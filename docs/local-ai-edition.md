# anydoc — Never-Stop & Local AI Edition

## PDF columns and embedded figures

Use the optional geometric PDF path when a page contains multiple columns and raster figures.

```sh
python -m pip install pymupdf
python examples/local_ai_workflow.py paper.pdf --pdf-layout
python examples/local_ai_workflow.py paper.pdf --pdf-layout --describe-image-context
```

This path reads native text positions and raster image bounds using PyMuPDF. It looks for a central gutter, orders each column independently, and groups consecutive image panels under a shared following figure caption. It writes figure crops, page coordinates, and markers into the existing local workflow. The assembled Markdown embeds the crops in reading order. Image descriptions are off by default. Use `--describe-image-context` to add them with local Ollama and `qwen3.5:9b`. Use `--model` to choose another installed vision model, or `--provider lmstudio --model <name>` with the flag to use LM Studio. The flag alone leaves scanned-page OCR off. Explicit provider selection still applies to scanned pages.

The layout rules target common left-to-right two-column PDFs. The layout path emits Markdown tables when aligned horizontal rules, bold column headers, and cell positions give a clear grid. Wrapped lines stay in their cells. Other tables fall back to native text blocks. Vector-only figures are outside the current layout path. Textless pages with raster images retain the existing whole-page OCR marker. The Rust converter remains available through the workflow's default path.

The Markdown and object manifest record source pixel dimensions and the size of the PNG passed to the vision model. For grouped PDF figures, source dimensions are listed per embedded panel. For embedded images in Office and other files, install Pillow to enable dimension lookup. When descriptions are enabled, the same dimensions are included in the prompt.

## 1. Appreciation for the Original Project & Why This Edition Exists

[`firecrawl/anydoc`](https://github.com/firecrawl/anydoc) and its companion PDF engine [`firecrawl/pdf-inspector`](https://github.com/firecrawl/pdf-inspector) are among the cleanest, fastest document conversion libraries in the open-source ecosystem:

- **14 formats into one shared AST and GitHub-Flavored Markdown writer**: Word (`.doc`, `.docx`, `.docm`), PowerPoint (`.ppt`, `.pptx`, `.pps`, `.pot`), Excel (`.xls`, `.xlsx`, `.xlsm`, `.xlsb`), OpenDocument (`.odt`, `.ods`, `.odp`), RTF (`.rtf`), EPUB (`.epub`), CSV (`.csv`), and PDF (`.pdf`).
- **Single-digit millisecond median latency** in pure Rust, with content-based signature detection (`Format::from_bytes`) and strict decompression/recursion safety budgets.
- **High structural fidelity**: merged table cells, numbered/nested/task lists, OMML/MathML/RTF equations converted to LaTeX (`$...$` / `$$...$$`), footnotes/endnotes, and embedded binary assets.

### Why Upstream `anydoc` Stops on Scanned Pages

Because the upstream project powers the commercial **[Firecrawl Parse](https://firecrawl.dev/parse)** cloud service (`--ocr hosted`), its default `to_markdown` entry point enforces an all-or-nothing rule on PDFs:

1. If a 100-page PDF has **99 pages of clean, selectable text and 1 scanned appendix page**, `anydoc::to_markdown` aborts the entire conversion with `ConvertError::NeedsOcr { pages: [100], page_count: 100 }` so callers are prompted to send the document to `--ocr hosted`.
2. If a PDF is 100% scanned or image-only, conversion aborts with `NeedsOcr` or `Unsupported("PDF has no extractable text...")`.
3. In Office and RTF documents, embedded images without alt text and embedded OLE objects are stored in `Document::assets` in memory, but omitted from the rendered Markdown output.

While that design makes complete sense as an entry point for a hosted cloud OCR API, **local AI enthusiasts, researchers, privacy-sensitive users, and non-Rust developers** often want the opposite behavior:

> *"Extract everything that is already readable right now in milliseconds, never throw away 99 good pages because of 1 scanned page, extract all embedded images and objects to disk, and give me a clear JSON manifest and Markdown markers so my local tools (Ollama, LM Studio, llama.cpp, Tesseract, or a simple Python script) can fill in the visual parts locally."*

This fork (**Never-Stop & Local AI Edition**) extends `anydoc` with that exact local-first workflow while preserving 100% backward compatibility with the upstream Rust API and test suite, and contributing a set of upstream parser accuracy fixes.

---

## 2. How the "Never-Stop" Mode Works

Instead of halting when a page or visual object needs OCR or vision analysis, the never-stop pipeline separates **fast structural extraction** from **local visual enrichment**:

```text
Input Document (.pdf, .docx, .pptx, .xlsx, .odt, .rtf, ...)
  │
  ├─► Stage 1: anydoc Never-Stop Extraction (`--with-objects`)
  │     ├── Instant Markdown for all selectable text, tables, lists, math, notes
  │     ├── Inline markers (`anydoc-object-1`, `anydoc-object-2`, ...) at exact reading positions
  │     ├── Extracted binary assets (`assets/<stem>-<id>.png`, `.svg`, `.bin`)
  │     └── Structured JSON manifest (`objects.json`) describing every pending item
  │
  ├─► Stage 2 & 3: Local AI / OCR Enrichment (`examples/local_ai_workflow.py`)
  │     ├── Local Ollama Vision (`--describe-image-context --provider ollama --model llava`)
  │     ├── Local OpenAI-Compatible Server (`--provider lmstudio` / `llama.cpp`)
  │     ├── Local Tesseract OCR (`--provider tesseract`)
  │     └── Offline Asset-Linking Mode (`--provider offline`, zero AI models required)
  │
  └─► Stage 4: Single-Pass Document-Order Assembly (`assembled.md`)
        └── Replaces markers in one pass from start to finish with OCR text,
            AI figure descriptions, and clickable relative links to extracted assets
```

### Example Marker Output (`marked.md`)

For a mixed PDF where page 1 has selectable text and page 2 is a scan:

```markdown
# Quarterly Engineering Summary

All selectable text, tables, and headings on page 1 are extracted immediately.

> Pending OCR · PDF page 2 · `anydoc-object-1`
```

For a Word, PowerPoint, ODT, or RTF document containing an embedded diagram (`image/png`) and an embedded Excel sheet (`application/vnd.ms-ole-object`):

```markdown
The architecture diagram below shows the local ingestion flow:

Architecture diagram `anydoc-object-1` (image)

Embedded object: Excel.Sheet.8 `anydoc-object-2` (object)
```

---

## 3. How to Use It

### Option A: Zero-Dependency Python Workflow Runner (No Rust Knowledge Needed)

[`examples/local_ai_workflow.py`](../examples/local_ai_workflow.py) uses only the Python 3 standard library (`json`, `subprocess`, `urllib.request`, `pathlib`)—no `pip install` required.

```bash
# 1. Offline mode (default): extracts Markdown, saves all images/assets,
#    and links them in `assembled.md` without needing any AI server running:
python examples/local_ai_workflow.py path/to/document.docx

# 2. Local Ollama vision model (e.g. llava, qwen2.5vl, minicpm-v):
python examples/local_ai_workflow.py path/to/report.docx --describe-image-context --provider ollama --model llava

# 3. Local LM Studio / llama.cpp / vLLM server (OpenAI-compatible /v1/chat/completions):
python examples/local_ai_workflow.py path/to/slides.pptx --describe-image-context --provider lmstudio --model local-vision

# 4. Local Tesseract CLI for scanned PDF pages:
python examples/local_ai_workflow.py path/to/scan.pdf --provider tesseract
```

Each run creates an output directory (`<stem>_anydoc_out/`) containing:
- `marked.md` — first-pass Markdown with `anydoc-object-N` markers
- `objects.json` — machine-readable manifest of every pending PDF page, image, or embedded object
- `assets/` — all extracted images and embedded object payloads
- `assembled.md` — final Markdown with markers replaced by local AI descriptions, OCR text, and relative asset links

### Option B: Command-Line Converter (`examples/convert.rs`)

```bash
# Automatic never-stop fallback: if a PDF has scanned pages, `convert` logs a
# note and automatically emits page markers instead of failing with NeedsOcr:
cargo run --release --example convert -- scan.pdf -o scan.md

# Explicit `--with-objects` mode with extracted assets and JSON manifest:
cargo run --release --example convert -- report.docx \
  --with-objects \
  -o out/marked.md \
  --assets out/assets \
  --manifest out/objects.json

# Strict upstream behavior (fail on NeedsOcr):
cargo run --release --example convert -- scan.pdf --strict
```

### Option C: Rust Library API

```rust
use anydoc::{Format, PendingObject, to_markdown_with_objects, to_markdown_with_objects_file};

// From a file path (detects format from content bytes, falling back to extension):
let result = to_markdown_with_objects_file("mixed-scan.pdf")?;
println!("{}", result.markdown);

for (idx, object) in result.objects.iter().enumerate() {
    let marker_id = idx + 1;
    match object {
        PendingObject::PdfPage { page, reason } => {
            println!("Marker anydoc-object-{marker_id}: PDF page {page} needs OCR ({reason:?})");
        }
        PendingObject::Image { alt, source } => {
            println!("Marker anydoc-object-{marker_id}: Image {alt:?} -> {source:?}");
        }
    }
}
```

---

## 4. Credits, Attribution & Summary of Contributions

We believe strongly in transparent open-source attribution. This repository combines the foundational work of the **Firecrawl** team, bug reports and fixes identified by the **upstream `firecrawl/anydoc` community**, and **original extensions** built for this Never-Stop & Local AI Edition.

### 4.1 Foundational Upstream Project
- **[Firecrawl](https://firecrawl.dev) (`firecrawl/anydoc` & `firecrawl/pdf-inspector`)**: Created and open-sourced the core Rust conversion engine, shared `Document` AST, GitHub-Flavored Markdown serializer, content-based format detector, and `pdf-inspector` text/layout analyzer under the MIT License.

### 4.2 Upstream Community Findings & PRs Credited in This Fork
Several format-parser fixes included here were informed by issues and pull requests opened on [`firecrawl/anydoc`](https://github.com/firecrawl/anydoc) after `v0.2.4` (`e7178e0`). Full credit for identifying and addressing these upstream parser edge cases goes to their respective authors:

| Area | Upstream Issue / PR | Credited Contributor(s) | Summary of Fix |
| :--- | :--- | :--- | :--- |
| **DOCX Comments (`word/comments.xml`)** | [Issue `#191`](https://github.com/firecrawl/anydoc/issues/191) · [PR `#210`](https://github.com/firecrawl/anydoc/pull/210) | [`@rahul05ranjan`](https://github.com/rahul05ranjan) | Parses `word/comments.xml` (`rel_type::COMMENTS`) and `<w:commentReference>` in `src/formats/docx/` so Word review comments are preserved as structured notes (`cm{id}`). |
| **DOCX `<w:sym>` Symbol & Dingbat Glyphs** | [Issue `#193`](https://github.com/firecrawl/anydoc/issues/193) · [PR `#211`](https://github.com/firecrawl/anydoc/pull/211) | [`@rahul05ranjan`](https://github.com/rahul05ranjan) | Decodes `<w:sym w:font="..." w:char="..."/>` glyphs from legacy `Symbol`, `Wingdings`, `Wingdings 2`, `Wingdings 3`, and `Webdings` code pages (`0xF020..=0xF0FF` PUA and `0x0020..=0x00FF`). |
| **ODT Continued Sublist Reset & `<office:annotation>`** | [Issue `#203`](https://github.com/firecrawl/anydoc/issues/203) · [PR `#206`](https://github.com/firecrawl/anydoc/pull/206) · [PR `#215`](https://github.com/firecrawl/anydoc/pull/215) | [`@crown-sports`](https://github.com/crown-sports) for annotations, [`@Aschenbath`](https://github.com/Aschenbath) for numbering | Prevents `<office:annotation>` (`ns::OFFICE`) from leaking comment text into paragraph words, and resets deeper `list_counters` when a parent list item advances in `src/formats/odf/text.rs`. |
| **RTF Tracked-Change Deletions (`\deleted`)** | [Issue `#202`](https://github.com/firecrawl/anydoc/issues/202) · [PR `#216`](https://github.com/firecrawl/anydoc/pull/216) | [`@Aschenbath`](https://github.com/Aschenbath) | Suppresses `\deleted` revision groups in `src/formats/rtf/mod.rs` so tracked deletions are not emitted as live text. |
| **DrawingML Sparse Chart Point Alignment** | [Issue `#220`](https://github.com/firecrawl/anydoc/issues/220) · [PR `#221`](https://github.com/firecrawl/anydoc/pull/221) | [`@Yzzz88`](https://github.com/Yzzz88) | Uses `<c:pt idx="N">` and `<c:ptCount>` in `src/shared/drawingml.rs` so sparse chart series values do not shift into earlier category rows. |
| **DOCX Embedded OLE Preview Images** | [Issue `#219`](https://github.com/firecrawl/anydoc/issues/219) · [PR `#222`](https://github.com/firecrawl/anydoc/pull/222) | [`@Yzzz88`](https://github.com/Yzzz88) | Retains companion VML preview images (`v:imagedata`) in `Document::assets` alongside `<o:OLEObject>` payloads (and falls back to the preview when the OLE stream is absent). |
| **XLSX Worksheet Cell Hyperlinks** | [Issue `#192`](https://github.com/firecrawl/anydoc/issues/192) · [PR `#198`](https://github.com/firecrawl/anydoc/pull/198) | [`@sean0407`](https://github.com/sean0407) | Resolves `<hyperlinks>` in `src/formats/sheet/xlsx.rs` against worksheet relationships and sheet anchors so linked cells render as `[text](url)` Markdown links. |

### 4.3 Original Enhancements Added in This Edition
1. **Never-Stop PDF & Object-Marking Pipeline (`src/lib.rs`, `src/formats/pdf.rs`, `src/render/markdown/`)**:
   - Designed `to_markdown_with_objects` and `to_markdown_with_objects_file` (`MarkdownWithObjects`, `PendingObject::PdfPage`, `PendingObject::Image`) so mixed PDFs, 100%-scanned PDFs, and image-only PDFs emit `anydoc-object-N` page markers instead of aborting with `ConvertError::NeedsOcr` or `ConvertError::Unsupported`.
   - Fixed mixed-PDF duplicate page text emission and preserved selectable text on image-heavy PDF pages classified as `ImageBased`.
   - Deferred footnote/endnote definition serialization until after body blocks so `anydoc-object-N` markers are numbered in strict reading order, normalized multiline image alt text, and preserved paragraph separation around block attachments (`Piece::Blocks`).
2. **Upgraded Never-Stop CLI (`examples/convert.rs`)**:
   - Added `--with-objects` (`-w`), `--manifest <file.json>`, automatic `NeedsOcr` fallback to page-marker mode, and plain-text passthrough for signature-less text files.
3. **Zero-Dependency Python 3 Local AI Workflow Runner (`examples/local_ai_workflow.py`) & Pipeline Architecture (`docs/object-processing-pipeline.md`)**:
   - Built a 4-stage local workflow runner (Extraction → Triage → Local Vision/OCR via Ollama, LM Studio, Tesseract, or Offline Asset Linking → Single-Pass Document-Order Assembly) requiring no Rust knowledge and zero third-party Python packages.
