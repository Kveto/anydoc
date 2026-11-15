"""Slide selection, picture placement, and bounded previews for the local workflow."""

from collections import defaultdict
import math
from pathlib import Path, PurePosixPath
import posixpath
import re
import shutil
import subprocess
import tempfile
from urllib.parse import unquote
from xml.etree import ElementTree as ET
from zipfile import ZipFile


SLIDE_MARKER = re.compile(r"^<!-- anydoc-slide:(\d+) -->[ \t]*$", re.MULTILINE)
OBJECT_MARKER = re.compile(r"`(anydoc-object-\d+)` \((?:image|object)\)")
EMU_PER_INCH = 914400
PREVIEW_DPI = 150
MAX_EDGE = 2560
MAX_DECODE_PIXELS = 20_000_000
MAX_BACKGROUND_BYTES = 32 * 1024 * 1024


def local_name(element):
    return element.tag.rsplit("}", 1)[-1]


def child(element, name):
    return next((item for item in element if local_name(item) == name), None) if element is not None else None


def qualified_attribute(element, name):
    return next((value for key, value in element.attrib.items() if key.endswith("}" + name)), None)


def xml_part(package, path):
    if package.getinfo(path).file_size > 16 * 1024 * 1024:
        raise ValueError(f"PowerPoint XML part exceeds the preview limit: {path}")
    return ET.fromstring(package.read(path))


def relationships(package, part):
    source = PurePosixPath(part)
    path = str(source.parent / "_rels" / (source.name + ".rels")) if part else "_rels/.rels"
    try:
        root = xml_part(package, path)
    except KeyError:
        return {}
    return {
        item.get("Id"): (
            posixpath.normpath(posixpath.join(str(source.parent), unquote(item.get("Target", "")))).lstrip("/"),
            item.get("Type", "").rsplit("/", 1)[-1],
        )
        for item in root if item.get("TargetMode") != "External"
    }


def transform_size(transform, extent_name="ext"):
    ext = child(transform, extent_name)
    if ext is None:
        return 0, 0
    return int(ext.get("cx", 0)), int(ext.get("cy", 0))


def picture_placements(tree, rels, scale=(1.0, 1.0)):
    for shape in tree if tree is not None else ():
        tag = local_name(shape)
        if tag == "grpSp":
            transform = child(child(shape, "grpSpPr"), "xfrm")
            outer = transform_size(transform)
            inner = transform_size(transform, "chExt")
            group_scale = tuple(s * a / b if a > 0 and b > 0 else s for s, a, b in zip(scale, outer, inner))
            yield from picture_placements(shape, rels, group_scale)
        elif tag == "pic":
            fill = child(shape, "blipFill")
            blip = child(fill, "blip")
            target, _ = rels.get(qualified_attribute(blip, "embed"), (None, None)) if blip is not None else (None, None)
            transform = child(child(shape, "spPr"), "xfrm")
            width, height = transform_size(transform)
            if not target or width <= 0 or height <= 0:
                continue
            crop = child(fill, "srcRect")
            yield target, {
                "width_emu": round(width * scale[0]),
                "height_emu": round(height * scale[1]),
                "rotation_degrees": int(transform.get("rot", 0)) / 60000,
                "flip_horizontal": transform.get("flipH") in ("1", "true"),
                "flip_vertical": transform.get("flipV") in ("1", "true"),
                "crop": {edge: int(crop.get(edge, 0)) / 100000 for edge in "ltrb"} if crop is not None else {},
            }


def shape_boxes(tree):
    boxes = []
    text_regions = []
    for shape in tree if tree is not None else ():
        kind = local_name(shape)
        transform = child(child(shape, "spPr"), "xfrm") if kind != "graphicFrame" else child(shape, "xfrm")
        offset = child(transform, "off")
        width, height = transform_size(transform)
        if offset is None or min(width, height) <= 0:
            continue
        x, y = int(offset.get("x", 0)), int(offset.get("y", 0))
        text_body = child(shape, "txBody")
        text = " ".join((item.text or "").strip() for item in text_body.iter() if local_name(item) == "t").strip() if text_body is not None else ""
        if text:
            props = child(shape, "spPr")
            text_regions.append({
                "text": text,
                "bounds_emu": [x, y, x + width, y + height],
                "rotation_degrees": int(transform.get("rot", 0)) / 60000,
                "filled_shape": any(child(props, fill) is not None for fill in ("solidFill", "gradFill", "blipFill", "pattFill")),
            })
        if kind == "pic" or text:
            boxes.append((kind == "pic", x, y, width, height))
    return boxes, text_regions


def layout_signals(boxes, width, height):
    if not width or not height:
        return 0, False, False, False
    # Estimate union coverage on a fixed slide grid so overlapping pictures count once.
    row_masks = [0] * 64
    layered = columns = spatial_text = False
    for index, (picture, x, y, w, h) in enumerate(boxes):
        if picture:
            left, right = (max(0, min(64, round(64 * edge / width))) for edge in (x, x + w))
            top, bottom = (max(0, min(64, round(64 * edge / height))) for edge in (y, y + h))
            mask = ((1 << (right - left)) - 1) << left
            for row in range(top, bottom):
                row_masks[row] |= mask
        for other_picture, ox, oy, ow, oh in boxes[:index]:
            shared_y = max(0, min(y + h, oy + oh) - max(y, oy))
            shared_x = max(0, min(x + w, ox + ow) - max(x, ox))
            if picture != other_picture:
                layered |= shared_x * shared_y >= min(w * h, ow * oh) / 10
            elif not picture:
                side_by_side = shared_y >= min(h, oh) / 2 and shared_x == 0
                columns |= side_by_side and min(w, ow) >= width / 6
                spatial_text |= side_by_side or abs(x - ox) >= width * 0.08 or (
                    shared_x * shared_y >= min(w * h, ow * oh) / 5
                )
    return sum(mask.bit_count() for mask in row_masks) / 4096, layered, columns, spatial_text


def background_image(package, slide, rels):
    for parent_kind in ("slideLayout", "slideMaster", None):
        background = child(child(slide, "cSld"), "bg")
        if background is not None:
            for item in background.iter():
                if local_name(item) == "blip":
                    return rels.get(qualified_attribute(item, "embed"), (None, None))[0]
            return None
        parent = next((path for path, kind in rels.values() if kind == parent_kind), None)
        if parent is None:
            return None
        slide = xml_part(package, parent)
        rels = relationships(package, parent)
    return None


def inspect_slides(input_path, markdown, objects, image_area_threshold):
    """Use presentation order and native markers to associate images with slides."""
    markers = list(SLIDE_MARKER.finditer(markdown))
    if not markers:
        return []
    by_marker = {obj["marker"]: obj for obj in objects}
    slides = []
    with ZipFile(input_path) as package:
        presentation = next((path for path, kind in relationships(package, "").values() if kind == "officeDocument"), "ppt/presentation.xml")
        root = xml_part(package, presentation)
        slide_list = child(root, "sldIdLst")
        rels = relationships(package, presentation)
        slide_paths = [rels[qualified_attribute(item, "id")][0] for item in slide_list if qualified_attribute(item, "id") in rels]
        size = child(root, "sldSz")
        slide_width, slide_height = (int(size.get("cx", 0)), int(size.get("cy", 0))) if size is not None else (0, 0)
        slide_area = max(1, slide_width * slide_height)
        for number, path in enumerate(slide_paths, 1):
            slide = xml_part(package, path)
            tree = child(child(slide, "cSld"), "spTree")
            relations = relationships(package, path)
            placements = list(picture_placements(tree, relations))
            words = sum(len((item.text or "").split()) for item in tree.iter() if local_name(item) == "t") if tree is not None else 0
            largest = max((p["width_emu"] * p["height_emu"] / slide_area for _, p in placements), default=0)
            background = background_image(package, slide, relations)
            boxes, text_regions = shape_boxes(tree)
            coverage, layered, columns, spatial_text = layout_signals(boxes, slide_width, slide_height)
            connectors = any(local_name(item) == "cxnSp" for item in tree.iter()) if tree is not None else False
            styled_labels = any(region["rotation_degrees"] or region["filled_shape"] for region in text_regions)
            if background:
                coverage = 1.0
            reasons = [reason for matched, reason in (
                (words < 20 and (placements or background or tree is not None and any(local_name(item) in ("graphicFrame", "grpSp", "AlternateContent") for item in tree)), "sparse_text_with_visuals"),
                (coverage >= image_area_threshold, "image_coverage"),
                (layered, "image_text_overlap"),
                (columns, "possible_text_columns"),
                (spatial_text or styled_labels or connectors and words > 0, "position_dependent_text"),
                (len(placements) >= 3 or any(local_name(item) in ("grpSp", "AlternateContent") for item in tree or ()), "complex_shapes"),
            ) if matched]
            single_image = words == 0 and (
                (len(placements) == 1 and largest >= 0.9) or (background and not placements)
            ) and not any(local_name(item) == "graphicFrame" for item in tree or ())
            slides.append({
                "slide": number,
                "hidden": slide.get("show") in ("0", "false"),
                "native_word_count": words,
                "background_source": background,
                "width_emu": slide_width,
                "height_emu": slide_height,
                "classification": "single_image" if single_image else "slide",
                "image_area_fraction": round(coverage, 4),
                "visual_reasons": reasons,
                "visual_candidate": bool(reasons) or single_image,
                "text_regions": text_regions,
                "placements": placements,
            })
    for index, marker in enumerate(markers):
        number = int(marker[1])
        end = markers[index + 1].start() if index + 1 < len(markers) else len(markdown)
        by_source = defaultdict(list)
        for source, placement in slides[number - 1]["placements"]:
            by_source[source].append(placement)
        slide_objects = defaultdict(list)
        for match in OBJECT_MARKER.finditer(markdown, marker.end(), end):
            obj = by_marker.get(match[1])
            if obj is None:
                continue
            obj["slide"] = number
            if obj["kind"] == "embedded_image":
                slide_objects[obj.get("source_path")].append(obj)
        for source, occurrences in slide_objects.items():
            if len(occurrences) == len(by_source[source]):
                for obj, placement in zip(occurrences, by_source[source]):
                    obj["placement"] = placement
    for slide in slides:
        del slide["placements"]
    return slides


def extract_backgrounds(input_path, directory, slides, objects):
    with ZipFile(input_path) as package:
        for slide in slides:
            source = slide["background_source"]
            if slide.get("rendered_file") or not source or min(slide["width_emu"], slide["height_emu"]) <= 0:
                continue
            suffix = PurePosixPath(source).suffix.lower()
            if not re.fullmatch(r"\.[a-z0-9]{1,8}", suffix):
                continue
            try:
                info = package.getinfo(source)
            except KeyError:
                continue
            if info.file_size > MAX_BACKGROUND_BYTES:
                slide["background_status"] = "asset_limit"
                continue
            number = slide["slide"]
            path = directory / f"slide-{number}-background{suffix}"
            with package.open(info) as original, path.open("wb") as destination:
                shutil.copyfileobj(original, destination)
            obj_id = len(objects) + 1
            objects.append({
                "id": obj_id, "marker": f"anydoc-object-{obj_id}", "kind": "embedded_image",
                "alt": f"Slide {number} background", "source_path": source, "extracted_file": str(path.resolve()),
                "slide": number, "role": "slide_background", "recommended_action": "describe_image",
                "placement": {
                    "width_emu": slide["width_emu"], "height_emu": slide["height_emu"],
                    "crop": {}, "rotation_degrees": 0, "flip_horizontal": False, "flip_vertical": False,
                },
            })


def picture_preview(obj, directory):
    placement = obj.get("placement")
    source = obj.get("extracted_file")
    if not placement or not source:
        if source:
            obj["preview_status"] = "placement_unavailable"
        return
    try:
        from PIL import Image, ImageOps
    except ImportError:
        obj["preview_status"] = "pillow_required"
        return
    dimensions = [placement[key] * PREVIEW_DPI / EMU_PER_INCH for key in ("width_emu", "height_emu")]
    scale = min(1, MAX_EDGE / max(dimensions))
    target = tuple(max(1, round(value * scale)) for value in dimensions)
    crop = placement["crop"]
    l, t, r, b = (crop.get(edge, 0) for edge in "ltrb")
    if any(value < 0 for value in (l, t, r, b)) or l + r >= 1 or t + b >= 1:
        obj["preview_status"] = "unsupported_crop"
        return
    try:
        with Image.open(source) as image:
            if image.width * image.height > MAX_DECODE_PIXELS and image.format == "JPEG":
                image.draft("RGB", (max(1, round(target[0] / (1 - l - r))), max(1, round(target[1] / (1 - t - b)))))
            if image.width * image.height > MAX_DECODE_PIXELS:
                obj["preview_status"] = "decode_limit"
                return
            width, height = image.size
            box = (l * width, t * height, (1 - r) * width, (1 - b) * height)
            preview = image.convert("RGBA").resize(target, Image.Resampling.LANCZOS, box=box)
            if placement["flip_horizontal"]:
                preview = ImageOps.mirror(preview)
            if placement["flip_vertical"]:
                preview = ImageOps.flip(preview)
            if placement["rotation_degrees"]:
                preview = preview.rotate(-placement["rotation_degrees"], resample=Image.Resampling.BICUBIC, expand=True)
                preview.thumbnail((MAX_EDGE, MAX_EDGE), Image.Resampling.LANCZOS)
            path = directory / f"slide-{obj['slide']}-picture-{obj['id']}.png"
            preview.save(path)
            obj["preview_file"] = str(path.resolve())
            obj["preview_size"] = {"width_px": preview.width, "height_px": preview.height}
    except (OSError, ValueError, Image.DecompressionBombError):
        obj["preview_status"] = "unsupported_image"


def render_slides(input_path, directory, slides, mode, soffice):
    visible = [slide for slide in slides if not slide["hidden"]]
    selected = {slide["slide"] for slide in visible if mode == "all" or slide["visual_candidate"]}
    if not selected:
        return
    import fitz

    office = soffice or shutil.which("soffice.com") or shutil.which("soffice")
    if not office:
        installed = Path("C:/Program Files/LibreOffice/program/soffice.com")
        if installed.is_file():
            office = str(installed)
    if not office:
        raise ValueError("Install LibreOffice or pass --soffice to render PowerPoint slides.")

    with tempfile.TemporaryDirectory(prefix="anydoc-pptx-", dir=directory) as tmp:
        temporary = Path(tmp)
        profile = (temporary / "profile").resolve().as_uri()
        result = subprocess.run(
            [office, f"-env:UserInstallation={profile}", "--headless", "--convert-to",
             'pdf:impress_pdf_Export:{"ExportHiddenSlides":{"type":"boolean","value":"false"}}',
             "--outdir", str(temporary.resolve()), str(input_path.resolve())],
            capture_output=True, text=True, timeout=120,
        )
        pdf = temporary / f"{input_path.stem}.pdf"
        if result.returncode or not pdf.is_file():
            raise ValueError(f"LibreOffice could not render the presentation: {result.stderr.strip()}")
        with fitz.open(pdf) as document:
            if len(document) != len(visible):
                raise ValueError("Rendered PDF page count differs from the visible slide count.")
            for page, slide in zip(document, visible):
                number = slide["slide"]
                if number not in selected:
                    continue
                zoom = min(PREVIEW_DPI / 72, MAX_EDGE / math.ceil(max(page.rect.width, page.rect.height)))
                image = page.get_pixmap(matrix=fitz.Matrix(zoom, zoom), alpha=False)
                path = directory / f"slide-{number}.png"
                image.save(path)
                slide["rendered_file"] = str(path.resolve())
                slide["rendered_size"] = {"width_px": image.width, "height_px": image.height}
