#!/usr/bin/env python3
"""
Local AI Document Workflow Runner for anydoc (Local AI & Enthusiast Edition)
=============================================================================

A zero-dependency Python 3 runner that orchestrates the 4-stage "never-stop"
local document pipeline on top of `anydoc`:

  Stage 1: Run `anydoc` (`convert --with-objects`) to extract fast, accurate
           GitHub-Flavored Markdown, embedded image/object assets, and a
           structured JSON manifest (`objects.json`) — without stopping on
           `NeedsOcr`.
  Stage 2: Triage each marked `anydoc-object-N` item (`pdf_page`,
           `embedded_image`, `embedded_object`, `external_image`).
  Stage 3: Enrich visual objects locally via:
             - Ollama (`--provider ollama`, e.g. `llava`, `qwen2.5vl`, `minicpm-v`)
             - Local OpenAI-compatible server (`--provider lmstudio` / `llama.cpp`)
             - Local Tesseract CLI (`--provider tesseract`)
             - Or automatic `--provider offline` asset-linking mode when no
               local model is running.
  Stage 4: Assemble the final `assembled.md` in a single document-order pass
           so substituted text is never re-parsed for markers.

Usage examples:
  python examples/local_ai_workflow.py path/to/document.pdf
  python examples/local_ai_workflow.py path/to/report.docx --describe-image-context --provider ollama --model llava
  python examples/local_ai_workflow.py path/to/scan.pdf --provider tesseract
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Dict, List, Optional


MARKER_TOKEN_RE = re.compile(
    r"(?:^> Pending OCR · PDF page \d+ · `(anydoc-object-\d+)`\s*$)"
    r"|(?P<inline_prefix>[^\n]*?)`(?P<inline_id>anydoc-object-\d+)` \((?:image|object)\)",
    re.MULTILINE,
)


def find_repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def run_stage1_extract(
    input_path: Path,
    work_dir: Path,
    convert_bin: Optional[str] = None,
) -> tuple[Path, Path, Path]:
    """Run anydoc's `convert` example with `--with-objects` and `--manifest`."""
    work_dir.mkdir(parents=True, exist_ok=True)
    marked_md = work_dir / "marked.md"
    assets_dir = work_dir / "assets"
    manifest_json = work_dir / "objects.json"

    repo_root = find_repo_root()
    if convert_bin:
        cmd = [convert_bin]
    else:
        release_bin = repo_root / "target" / "release" / "examples" / (
            "convert.exe" if os.name == "nt" else "convert"
        )
        debug_bin = repo_root / "target" / "debug" / "examples" / (
            "convert.exe" if os.name == "nt" else "convert"
        )
        if release_bin.is_file():
            cmd = [str(release_bin)]
        elif debug_bin.is_file():
            cmd = [str(debug_bin)]
        else:
            cmd = ["cargo", "run", "--quiet", "--example", "convert", "--"]

    full_cmd = cmd + [
        str(input_path.resolve()),
        "--with-objects",
        "-o",
        str(marked_md),
        "--assets",
        str(assets_dir),
        "--manifest",
        str(manifest_json),
    ]
    print(f"[Stage 1] Extracting Markdown and object manifest via anydoc...")
    proc = subprocess.run(full_cmd, cwd=str(repo_root), text=True, capture_output=True)
    if proc.stderr:
        for line in proc.stderr.strip().splitlines():
            print(f"  anydoc: {line}")
    if proc.returncode != 0:
        raise RuntimeError(
            f"anydoc extraction failed (exit {proc.returncode}):\n{proc.stderr}"
        )
    return marked_md, assets_dir, manifest_json


def try_render_pdf_page(pdf_path: Path, page_num: int, out_dir: Path) -> Optional[Path]:
    """Attempt to rasterize a 1-based PDF page to PNG using local tools if installed."""
    out_dir.mkdir(parents=True, exist_ok=True)
    target_png = out_dir / f"{pdf_path.stem}-page-{page_num}.png"
    if target_png.is_file():
        return target_png

    pdftoppm = shutil.which("pdftoppm")
    if pdftoppm:
        prefix = out_dir / f"{pdf_path.stem}-page"
        cmd = [
            pdftoppm,
            "-png",
            "-r",
            "150",
            "-f",
            str(page_num),
            "-l",
            str(page_num),
            "-singlefile",
            str(pdf_path),
            str(prefix) + f"-{page_num}",
        ]
        res = subprocess.run(cmd, capture_output=True, text=True)
        if res.returncode == 0 and target_png.is_file():
            return target_png

    return None


def call_ollama_vision(
    image_path: Path,
    prompt: str,
    model: str,
    endpoint: str,
) -> Optional[str]:
    """Send an image to a local Ollama vision model (`/api/generate`)."""
    b64 = base64.b64encode(image_path.read_bytes()).decode("ascii")
    payload = json.dumps(
        {
            "model": model,
            "prompt": prompt,
            "images": [b64],
            "stream": False,
        }
    ).encode("utf-8")
    req = urllib.request.Request(
        f"{endpoint.rstrip('/')}/api/generate",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            data = json.loads(resp.read().decode("utf-8"))
            return (data.get("response") or "").strip() or None
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
        print(f"  [notice] Ollama request failed ({exc}); falling back to offline card.")
        return None


def call_openai_local_vision(
    image_path: Path,
    prompt: str,
    model: str,
    endpoint: str,
) -> Optional[str]:
    """Send an image to a local OpenAI-compatible server (LM Studio, llama.cpp, vLLM)."""
    b64 = base64.b64encode(image_path.read_bytes()).decode("ascii")
    ext = image_path.suffix.lower().lstrip(".") or "png"
    mime = "image/jpeg" if ext in ("jpg", "jpeg") else f"image/{ext}"
    payload = json.dumps(
        {
            "model": model,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": prompt},
                        {
                            "type": "image_url",
                            "image_url": {"url": f"data:{mime};base64,{b64}"},
                        },
                    ],
                }
            ],
            "temperature": 0.1,
        }
    ).encode("utf-8")
    req = urllib.request.Request(
        f"{endpoint.rstrip('/')}/v1/chat/completions",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            data = json.loads(resp.read().decode("utf-8"))
            choices = data.get("choices") or []
            if choices:
                return (choices[0].get("message", {}).get("content") or "").strip() or None
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
        print(f"  [notice] Local OpenAI server failed ({exc}); falling back to offline card.")
    return None


def call_tesseract_ocr(image_path: Path) -> Optional[str]:
    """Run local `tesseract` CLI on an image if available."""
    tesseract = shutil.which("tesseract")
    if not tesseract:
        return None
    res = subprocess.run(
        [tesseract, str(image_path), "stdout", "--oem", "1"],
        capture_output=True,
        text=True,
    )
    if res.returncode == 0 and res.stdout.strip():
        return res.stdout.strip()
    return None


def record_embedded_image_size(obj):
    """Read embedded-image dimensions without loading the decoded pixels."""
    if obj["kind"] != "embedded_image" or "asset_size" in obj or not obj.get("extracted_file"):
        return False
    try:
        from PIL import Image
    except ImportError:
        return False
    try:
        with Image.open(obj["extracted_file"]) as image:
            size = {"width_px": image.width, "height_px": image.height}
    except (OSError, ValueError, Image.DecompressionBombError):
        return False
    obj["asset_size"] = size
    obj["source_image_sizes"] = [size]
    return True


def image_size_label(obj):
    def dimensions(size):
        return f"{size['width_px']} × {size['height_px']} px"

    asset = obj.get("asset_size")
    originals = obj.get("source_image_sizes") or []
    if not asset:
        return ""
    if "panel_bboxes" not in obj:
        return f"Original image: {dimensions(asset)}."
    source = ", ".join(dimensions(size) for size in originals)
    return f"Original image panels: {source}. Rendered crop: {dimensions(asset)}."


def enrich_object(
    obj: Dict[str, Any],
    input_doc: Path,
    work_dir: Path,
    provider: str,
    model: str,
    endpoint: str,
) -> str:
    """Produce the Markdown replacement block for a single marked PendingObject."""
    marker = obj["marker"]
    kind = obj["kind"]

    if kind == "pdf_page":
        page = obj.get("page", 1)
        reason = obj.get("reason") or "scanned_or_image_page"
        rendered_png = try_render_pdf_page(input_doc, page, work_dir / "assets")
        extracted_text: Optional[str] = None
        if rendered_png and provider != "offline":
            prompt = (
                f"Extract all readable text and tables from PDF page {page} as clean "
                "GitHub-Flavored Markdown. Preserve headings, lists, and table columns."
            )
            if provider == "ollama":
                extracted_text = call_ollama_vision(rendered_png, prompt, model, endpoint)
            elif provider == "lmstudio":
                extracted_text = call_openai_local_vision(rendered_png, prompt, model, endpoint)
            elif provider == "tesseract":
                extracted_text = call_tesseract_ocr(rendered_png)

        if extracted_text:
            return (
                f"<!-- {marker}: PDF page {page} ({reason}) processed via {provider} -->\n"
                f"{extracted_text}"
            )
        if rendered_png:
            rel = os.path.relpath(rendered_png, work_dir).replace("\\", "/")
            return (
                f"> **[PDF Page {page} — Pending Local OCR (`{reason}`)]**\n"
                f"> Rendered page image: ![PDF page {page}]({rel})"
            )
        return (
            f"> **[PDF Page {page} — Pending Local OCR (`{reason}`)]**\n"
            f"> *Tip: Install `pdftoppm` (Poppler) or run with `--provider ollama --model llava` to OCR this page locally.*"
        )

    alt = (obj.get("alt") or "").strip()
    extracted_file = obj.get("extracted_file")
    source_path = obj.get("source_path")
    media_type = obj.get("media_type") or "application/octet-stream"

    rel_asset: Optional[str] = None
    asset_path: Optional[Path] = None
    if extracted_file:
        asset_path = Path(extracted_file)
        if asset_path.is_file():
            try:
                rel_asset = os.path.relpath(asset_path, work_dir).replace("\\", "/")
            except ValueError:
                rel_asset = str(asset_path).replace("\\", "/")

    if kind == "embedded_image":
        size_label = image_size_label(obj)
        ai_summary: Optional[str] = None
        if (
            asset_path
            and asset_path.suffix.lower() in (".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp")
            and provider != "offline"
        ):
            prompt = (
                "Describe this document figure or extract any text inside it concisely in 1-3 sentences."
                + (f" Existing alt text: {alt}" if alt else "")
                + (f" Image metadata: {size_label}" if size_label else "")
            )
            if provider == "ollama":
                ai_summary = call_ollama_vision(asset_path, prompt, model, endpoint)
            elif provider == "lmstudio":
                ai_summary = call_openai_local_vision(asset_path, prompt, model, endpoint)
            elif provider == "tesseract":
                ai_summary = call_tesseract_ocr(asset_path)

        label = alt if alt else f"Image {obj['id']}"
        lines: List[str] = []
        if rel_asset:
            lines.append(f"![{label}]({rel_asset})")
        else:
            lines.append(f"![{label}]()")
        if size_label:
            lines.append(f"\n*{size_label}*")
        if ai_summary:
            lines.append(f"\n> **Local AI Vision ({provider}):** {ai_summary}")
        return "\n".join(lines)

    if kind == "embedded_object":
        label = alt if alt else f"Embedded object ({media_type})"
        if rel_asset:
            return f"> **{label}** — extracted payload: [`{Path(rel_asset).name}`]({rel_asset}) (`{media_type}`)"
        return f"> **{label}** (`{media_type}`)"

    if kind == "external_image":
        label = alt if alt else "External image"
        url = source_path or ""
        return f"![{label}]({url})"

    return f"![{alt}]()" if alt else f"<!-- {marker}: unavailable visual object -->"


def assemble_markdown_single_pass(
    marked_markdown: str,
    objects_by_marker: Dict[str, Dict[str, Any]],
    replacements: Dict[str, str],
) -> str:
    """
    Stage 4: Single-pass document-order substitution.
    Replaces all `> Pending OCR · PDF page N · \`anydoc-object-K\`` lines and
    inline `\`anydoc-object-K\` (image|object)` markers in one pass so that
    model output containing marker-like strings is never re-expanded.
    """
    def _replace(match: re.Match[str]) -> str:
        pdf_marker = match.group(1)
        if pdf_marker:
            return replacements.get(pdf_marker, match.group(0))

        inline_id = match.group("inline_id")
        prefix = match.group("inline_prefix") or ""
        replacement = replacements.get(inline_id)
        if replacement is None:
            return match.group(0)

        obj = objects_by_marker.get(inline_id, {})
        alt = (obj.get("alt") or "").strip()
        if alt and prefix.rstrip().endswith(alt):
            trimmed_prefix = prefix.rstrip()[: -len(alt)]
            return f"{trimmed_prefix}{replacement}"
        return f"{prefix}{replacement}"

    return MARKER_TOKEN_RE.sub(_replace, marked_markdown)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run the 4-stage local-first anydoc extraction & enrichment workflow."
    )
    parser.add_argument("input", type=Path, help="Path to input document (PDF, DOCX, PPTX, XLSX, ODT, RTF, ...)")
    parser.add_argument(
        "-o",
        "--output-dir",
        type=Path,
        default=None,
        help="Directory for marked.md, assembled.md, objects.json, and assets/ (default: <input_stem>_anydoc_out)",
    )
    parser.add_argument(
        "--provider",
        choices=["offline", "ollama", "lmstudio", "tesseract"],
        default=None,
        help="Provider for scanned-page OCR and optional image descriptions",
    )
    parser.add_argument(
        "--describe-image-context",
        action="store_true",
        help="Add local vision descriptions to embedded images (default: link images only)",
    )
    parser.add_argument(
        "--model",
        default=None,
        help="Vision model (default for image descriptions: qwen3.5:9b)",
    )
    parser.add_argument(
        "--endpoint",
        default=None,
        help="Custom local endpoint URL (default: http://localhost:11434 for ollama, http://localhost:1234 for lmstudio)",
    )
    parser.add_argument(
        "--convert-bin",
        default=None,
        help="Optional path to a prebuilt `convert` binary",
    )
    parser.add_argument(
        "--pdf-layout", action="store_true",
        help="Order PDF text by column and extract raster figures (requires PyMuPDF)",
    )
    args = parser.parse_args()
    if args.pdf_layout and args.input.suffix.lower() != ".pdf":
        parser.error("--pdf-layout requires a PDF input")
    if args.describe_image_context and args.provider in ("offline", "tesseract"):
        parser.error("--describe-image-context requires ollama or lmstudio")

    if not args.input.is_file():
        print(f"error: input file not found: {args.input}", file=sys.stderr)
        return 1

    work_dir = args.output_dir or Path.cwd() / f"{args.input.stem}_anydoc_out"
    page_provider = args.provider or "offline"
    image_provider = (args.provider or "ollama") if args.describe_image_context else "offline"
    active_provider = image_provider if args.describe_image_context else page_provider
    model = args.model or ("qwen3.5:9b" if args.describe_image_context else "llava")
    endpoint = args.endpoint or (
        "http://localhost:11434" if active_provider == "ollama" else "http://localhost:1234"
    )

    if args.pdf_layout:
        from pdf_layout import extract_pdf_layout

        marked_md_path, _, manifest_path = extract_pdf_layout(args.input, work_dir)
    else:
        marked_md_path, _, manifest_path = run_stage1_extract(
            args.input, work_dir, args.convert_bin
        )
    marked_markdown = marked_md_path.read_text(encoding="utf-8")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    objects: List[Dict[str, Any]] = manifest.get("objects", [])
    sizes_added = False
    for obj in objects:
        sizes_added |= record_embedded_image_size(obj)
    if sizes_added:
        manifest_path.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    print(
        f"[Stage 2 & 3] Processing {len(objects)} marked object(s) "
        f"(image descriptions: {image_provider}, page OCR: {page_provider})..."
    )
    replacements: Dict[str, str] = {}
    objects_by_marker: Dict[str, Dict[str, Any]] = {}
    for obj in objects:
        marker = obj["marker"]
        objects_by_marker[marker] = obj
        replacements[marker] = enrich_object(
            obj,
            args.input,
            work_dir,
            page_provider if obj.get("kind") == "pdf_page" else image_provider,
            model,
            endpoint,
        )

    print("[Stage 4] Assembling final Markdown in single-pass document order...")
    assembled_markdown = assemble_markdown_single_pass(
        marked_markdown, objects_by_marker, replacements
    )
    assembled_path = work_dir / "assembled.md"
    assembled_path.write_text(assembled_markdown, encoding="utf-8")

    print(f"Done! Outputs written to: {work_dir}")
    print(f"  - Raw marked Markdown : {marked_md_path}")
    print(f"  - Object manifest     : {manifest_path}")
    print(f"  - Assembled Markdown  : {assembled_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
