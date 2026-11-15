"""Opt-in geometric extraction for mixed text and raster figures in PDFs."""

import json
import re
from bisect import bisect_left, bisect_right
from collections import Counter, defaultdict
from dataclasses import dataclass
from html import escape
from pathlib import Path
from typing import Any


CAPTION = re.compile(r"^\s*(?:fig(?:ure)?\.?\s+\d+)\b", re.I)
TABLE_CAPTION = re.compile(r"^table\s+(?:[a-z]?\d+(?:\.\d+)*|[a-z])(?:[.:\s]|$)", re.I)
CODE_FONT = re.compile(r"mono|courier|inconsolata|typewriter|consolas", re.I)
SECTION_HEADING = re.compile(r"^(\d+(?:\.\d+)*)(?:\.)?\s+([^\W\d_].*)$")
REFERENCE_LABEL = re.compile(r"^\[(\d+)\]\s+\S")
REFERENCE_LABEL_ONLY = re.compile(r"^\[\d+\]$")


@dataclass
class Block:
    box: Any
    text: str = ""
    images: tuple = ()
    heading_level: int = 0
    table: bool = False
    image_sizes: tuple = ()


def block_from_rows(pdf, rows, row_text):
    box = pdf.Rect()
    for _, spans in rows:
        for span in spans:
            box |= pdf.Rect(span["bbox"])
    return Block(box, "\n".join(row_text))


def reference_starts(rows, row_text):
    """Identify a sequence of aligned labels with hanging continuation lines."""
    labels = [(i, int(match[1])) for i, line in enumerate(row_text)
              if (match := REFERENCE_LABEL.match(line))]
    if len(labels) < 2 or labels[0][0] != 0:
        return []
    first = rows[0][1][0]
    # Hanging labels are often right-aligned, so [10] starts left of [9].
    separate_labels = all(
        REFERENCE_LABEL_ONLY.fullmatch(rows[i][1][0]["text"].strip())
        for i, _ in labels
    )
    label_x = first["bbox"][2] if separate_labels else first["origin"][0]
    for (i, number), (end, following) in zip(labels, labels[1:]):
        label = rows[end][1][0]
        edge = label["bbox"][2] if separate_labels else label["origin"][0]
        if following != number + 1 or abs(edge - label_x) > 2:
            return []
        if any(spans[0]["origin"][0] < label_x + 3 for _, spans in rows[i + 1:end]):
            return []
    return [i for i, _ in labels]


def ruled_tables(page, pdf, text):
    """Recognize simple tables with aligned top, header, and bottom rules."""
    rules = defaultdict(set)
    for drawing in page.get_drawings():
        for item in drawing["items"]:
            if item[0] == "l":
                start, end = item[1:]
                left, right = sorted((start.x, end.x))
                if abs(start.y - end.y) < 0.5 and right - left >= 45:
                    rules[round(left), round(right)].add(start.y)
    consumed = set()
    tables = []
    for (left, right), positions in rules.items():
        ys = sorted(positions)
        for index, (top, separator) in enumerate(zip(ys, ys[1:])):
            if not 6 <= separator - top <= 30 or index + 2 >= len(ys):
                continue
            header_box = pdf.Rect(left - 1, top, right + 1, separator)
            header = table_headers(page, pdf, header_box)
            if not header:
                continue
            labels, bold = header
            boundaries = [p[0] - 2 for p, _ in labels[1:]]
            rows = []
            candidate = None
            previous = separator
            for bottom in ys[index + 2:]:
                band = pdf.Rect(left - 1, previous, right + 1, bottom)
                cells = table_lines(page, band, boundaries)
                if not cells or not all(cells[0]):
                    break
                rows.append(cells)
                previous = bottom
                box = pdf.Rect(left - 1, top, right + 1, bottom)
                indices = {i for i, block in enumerate(text) if box.contains(block.box)}
                if indices & consumed:
                    break
                if indices and (bold or has_table_caption(text, box)) and not any(
                    i not in indices and box.intersects(block.box)
                    for i, block in enumerate(text)
                ):
                    candidate = (box, indices, len(rows))
            if candidate:
                box, indices, count = candidate
                body_box = pdf.Rect(box.x0, separator, box.x1, box.y1)
                literal = literal_columns(page, pdf, body_box, boundaries)
                markdown = table_grid(labels, rows[:count], literal)
                if markdown:
                    consumed.update(indices)
                    tables.append(Block(box, markdown, table=True))
    return [block for i, block in enumerate(text) if i not in consumed] + tables


def has_table_caption(text, box):
    for gap in (lambda b: b.box.y0 - box.y1, lambda b: box.y0 - b.box.y1):
        nearby = [b for b in text if 0 <= gap(b) <= 20
                  and min(b.box.x1, box.x1) - max(b.box.x0, box.x0)
                  >= 0.5 * min(b.box.width, box.width)]
        if nearby and TABLE_CAPTION.match(min(nearby, key=gap).text.strip()):
            return True
    return False


def table_headers(page, pdf, header_box):
    flags = pdf.TEXTFLAGS_DICT & ~pdf.TEXT_PRESERVE_IMAGES
    labels = []
    bold = True
    for block in page.get_text("dict", clip=header_box, flags=flags)["blocks"]:
        for line in block.get("lines", []):
            spans = [s for s in line["spans"] if s["text"].strip()]
            if not spans:
                continue
            bold &= all(s["flags"] & pdf.TEXT_FONT_BOLD for s in spans)
            labels.append((spans[0]["origin"], "".join(s["text"] for s in spans).strip()))
    labels.sort(key=lambda item: item[0][0])
    if len(labels) < 2 or max(p[1] for p, _ in labels) - min(p[1] for p, _ in labels) > 1:
        return None
    return labels, bold


def literal_columns(page, pdf, box, boundaries):
    flags = pdf.TEXTFLAGS_DICT & ~pdf.TEXT_PRESERVE_IMAGES
    styles = defaultdict(set)
    for block in page.get_text("dict", clip=box, flags=flags)["blocks"]:
        for line in block.get("lines", []):
            for span in line["spans"]:
                if span["text"].strip():
                    column = bisect_right(boundaries, span["origin"][0])
                    styles[column].add(bool(
                        span["flags"] & pdf.TEXT_FONT_MONOSPACED or CODE_FONT.search(span["font"])
                    ))
    return {column for column, kinds in styles.items() if kinds == {True}}


def table_lines(page, body_box, boundaries):
    rows = []
    baseline = None
    for word in sorted(page.get_text("words", clip=body_box), key=lambda w: (w[1], w[0])):
        if baseline is None or word[1] - baseline > 2:
            baseline = word[1]
            rows.append([])
        rows[-1].append(word)
    result = []
    for line in rows:
        cells = [[] for _ in range(len(boundaries) + 1)]
        for word in sorted(line, key=lambda w: w[0]):
            column = bisect_right(boundaries, word[0])
            if column < len(boundaries) and word[2] > boundaries[column] + 2:
                return None
            cells[column].append(word[4])
        result.append([" ".join(cell) for cell in cells])
    return result


def markdown_code_cell(lines):
    """Keep literal content in a portable single-line Markdown table cell."""
    content = " ".join(lines)
    fence = "`" * (1 + max((len(run) for run in re.findall(r"`+", content)), default=0))
    padding = " " if content.startswith("`") or content.endswith("`") else ""
    return f"{fence}{padding}{content}{padding}{fence}"


def table_grid(labels, bands, literal):
    body = []
    for band in bands:
        for index, cells in enumerate(band):
            # Explicit dividers define rows, including when the first cell
            # wraps. Without dividers a populated first cell marks a new row.
            if index == 0 or (len(bands) == 1 and cells[0]):
                if not all(cells):
                    return None
                body.append([[cell] for cell in cells])
            else:
                for value, cell in zip(body[-1], cells):
                    if cell:
                        value.append(cell)
    if len(body) < 2:
        return None

    def markdown_row(cells):
        return "| " + " | ".join(cell.replace("|", r"\|") for cell in cells) + " |"

    return "\n".join([
        markdown_row([escape(label, quote=False) for _, label in labels]),
        markdown_row(["---"] * len(labels)),
        *(markdown_row([
            markdown_code_cell(cell)
            if column in literal else escape(join_words("\n".join(cell)), quote=False)
            for column, cell in enumerate(row)
        ]) for row in body),
    ])


def text_blocks(page, pdf):
    flags = pdf.TEXTFLAGS_DICT & ~pdf.TEXT_PRESERVE_IMAGES
    blocks = [b for b in page.get_text("dict", flags=flags)["blocks"] if b["type"] == 0]
    sizes = Counter()
    for block in blocks:
        for line in block["lines"]:
            for span in line["spans"]:
                sizes[round(span["size"], 1)] += sum(ch.isalpha() for ch in span["text"])
    if not sizes or not sum(sizes.values()):
        body_size = 0
    else:
        halfway = sum(sizes.values()) / 2
        total = 0
        for body_size, count in sorted(sizes.items()):
            total += count
            if total >= halfway:
                break
    groups = []
    for block in blocks:
        rows = []
        row_text = []
        horizontal = []
        for line in block["lines"]:
            spans = [span for span in line["spans"] if span["text"].strip()]
            if spans:
                baseline = spans[0]["origin"][1]
                value = "".join(span["text"] for span in line["spans"]).strip()
                # A section number and its title can be separate PDF lines
                # on the same visual baseline.
                if rows and abs(rows[-1][0] - baseline) <= 0.75:
                    rows[-1][1].extend(spans)
                    row_text[-1] += " " + value
                else:
                    rows.append((baseline, spans))
                    row_text.append(value)
                    horizontal.append(abs(line["dir"][1]) < 0.1)
        if not rows:
            continue
        groups.append((rows, row_text, horizontal))

    for group_index, (rows, row_text, horizontal) in enumerate(groups):
        start = 0
        for i, (baseline, spans) in enumerate(rows):
            if i < start:
                continue
            title = row_text[i]
            numbered = SECTION_HEADING.fullmatch(title)
            bibliography = title.casefold() in {"references", "bibliography", "works cited"}
            bold = all(s["flags"] & pdf.TEXT_FONT_BOLD for s in spans)
            level = 0
            if i == 0 and bold and len(title) <= 100 and (numbered or bibliography):
                level = min(6, numbered[1].count(".") + 2) if numbered else 2
            elif horizontal[i] and body_size and 1 <= len(title.split()) <= 12 and len(title) <= 100:
                size = min(s["size"] for s in spans)
                # A short display line may have unreliable bold flags,
                # especially when the PDF uses a custom Type 3 font.
                if size >= body_size * 1.65 and any(ch.isalpha() for ch in title):
                    level = 2
                elif (len(rows) == 1 and size >= body_size * 1.16 and title[:1].isupper()
                      and not title.endswith((".", ",", ";")) and group_index + 1 < len(groups)):
                    next_y, next_spans = groups[group_index + 1][0][0]
                    if (size >= max(s["size"] for s in next_spans) * 1.15
                            and spans[0]["font"] != next_spans[0]["font"]
                            and abs(spans[0]["origin"][0] - next_spans[0]["origin"][0]) <= 24
                            and size <= next_y - baseline <= size * 3):
                        level = 3
            if level:
                end = i + 1
                if level == 2 and not numbered and not bibliography:
                    while end < len(rows) and horizontal[end]:
                        next_y, next_spans = rows[end]
                        if (next_y - rows[end - 1][0] > spans[0]["size"] * 1.3
                                or abs(next_spans[0]["origin"][0] - spans[0]["origin"][0]) > 2
                                or any(s["font"] != spans[0]["font"] or
                                       abs(s["size"] - spans[0]["size"]) > 0.5 for s in next_spans)):
                            break
                        end += 1
                if start < i:
                    yield from paragraph_blocks(pdf, rows[start:i], row_text[start:i])
                heading = block_from_rows(pdf, rows[i:end], [" ".join(row_text[i:end])])
                heading.heading_level = level
                yield heading
                start = end
        if start < len(rows):
            yield from paragraph_blocks(pdf, rows[start:], row_text[start:])


def paragraph_blocks(pdf, rows, row_text):
    starts = reference_starts(rows, row_text) or [0]
    for start, end in zip(starts, [*starts[1:], len(rows)]):
        yield block_from_rows(pdf, rows[start:end], row_text[start:end])


def column_gutter(text, bounds):
    """Find a central split supported by several blocks on both sides."""
    narrow = [b for b in text if b.box.width <= bounds.width * 0.55]
    starts = sorted(b.box.x0 for b in narrow)
    ends = sorted(b.box.x1 for b in narrow)
    edges = sorted(set(starts + ends))
    center = (bounds.x0 + bounds.x1) / 2
    candidates = []
    for low, high in zip(edges, edges[1:]):
        cut = (low + high) / 2
        if high - low < 10 or abs(cut - center) > bounds.width * 0.1:
            continue
        left = bisect_right(ends, cut)
        right = len(starts) - bisect_left(starts, cut)
        if min(left, right) >= 2:
            candidates.append((left + right, min(left, right), -abs(cut - center), cut))
    return max(candidates)[-1] if candidates else None


def order_regions(blocks, cut, width):
    if cut is None:
        return [sorted(blocks, key=lambda b: (b.box.y0, b.box.x0))]
    spanning = sorted(
        [b for b in blocks if b.box.x0 < cut - 8 and b.box.x1 > cut + 8
         and b.box.width > width * 0.55],
        key=lambda b: b.box.y0,
    )
    remaining = [b for b in blocks if b not in spanning]
    regions = []
    for boundary in [*spanning, None]:
        band = [b for b in remaining if boundary is None or b.box.y0 < boundary.box.y0]
        for side in (True, False):
            column = [b for b in band if ((b.box.x0 + b.box.x1) / 2 < cut) == side]
            if column:
                regions.append(sorted(column, key=lambda b: (b.box.y0, b.box.x0)))
        remaining = [b for b in remaining if b not in band]
        if boundary is not None:
            regions.append([boundary])
    return regions


def join_words(text):
    def unwrap(match):
        word = match[1]
        return word + ("-" if "-" in word else "")

    return re.sub(r"([\w-]+)-\n(?=\w)", unwrap, text.strip()).replace("\n", " ")


def group_figures(region):
    """Group consecutive panels when they share a following figure caption."""
    pending = []
    for block in region:
        if block.images:
            pending.append(block)
            continue
        if len(pending) > 1 and CAPTION.match(block.text):
            box = pending[0].box
            for panel in pending[1:]:
                box = box | panel.box
            yield Block(
                box, images=tuple(p.box for p in pending),
                image_sizes=tuple(size for panel in pending for size in panel.image_sizes),
            )
        else:
            yield from pending
        pending.clear()
        yield block
    yield from pending


def extract_pdf_layout(input_path: Path, work_dir: Path):
    try:
        import pymupdf as pdf
    except ImportError as error:
        raise RuntimeError("--pdf-layout requires PyMuPDF (pip install pymupdf)") from error

    work_dir = work_dir.resolve()
    assets = work_dir / "assets"
    assets.mkdir(parents=True, exist_ok=True)
    marked = work_dir / "marked.md"
    manifest = work_dir / "objects.json"
    objects = []
    with pdf.open(input_path) as document, marked.open("w", encoding="utf-8") as output:
        for page in document:
            number = page.number + 1
            bounds = page.rect * page.derotation_matrix
            text = list(text_blocks(page, pdf))
            text = ruled_tables(page, pdf, text)
            images = []
            for info in page.get_image_info():
                box = pdf.Rect(info["bbox"]) & bounds
                if not box.is_empty:
                    images.append(Block(
                        box, images=(box,), image_sizes=((info["width"], info["height"]),),
                    ))
            if not text and images:
                marker = f"anydoc-object-{len(objects) + 1}"
                objects.append({"marker": marker, "id": marker, "kind": "pdf_page", "page": number})
                output.write(f"> Pending OCR · PDF page {number} · `{marker}`\n\n")
                continue
            cut = column_gutter(text, bounds)
            page_parts = []
            previous = None
            for region in order_regions(text + images, cut, bounds.width):
                for index, block in enumerate(group_figures(region)):
                    if not block.images:
                        value = join_words(block.text)
                        if block.table:
                            page_parts.append(block.text)
                        elif block.heading_level:
                            page_parts.append(f"{'#' * block.heading_level} {value}")
                        elif (index == 0 and cut is not None and previous is not None
                                and previous.text and not previous.heading_level and not previous.table
                                and previous.box.x1 <= cut <= block.box.x0
                                and previous.box.y0 > block.box.y0
                                and page_parts[-1][-1:].isalnum() and value[:1].islower()):
                            page_parts[-1] += " " + value
                        else:
                            page_parts.append(value)
                        previous = block
                        continue
                    marker = f"anydoc-object-{len(objects) + 1}"
                    path = assets / f"page-{number}-{marker}.png"
                    clip = block.box * page.rotation_matrix
                    scale = min(3, 2400 / max(clip.width, clip.height))
                    crop = page.get_pixmap(matrix=pdf.Matrix(scale, scale), clip=clip, alpha=False)
                    crop.save(path)
                    objects.append({
                        "marker": marker, "id": marker, "kind": "embedded_image",
                        "page": number, "bbox": list(block.box),
                        "coordinate_system": "unrotated page points, top-left origin",
                        "panel_bboxes": [list(box) for box in block.images],
                        "source_image_sizes": [
                            {"width_px": width, "height_px": height}
                            for width, height in block.image_sizes
                        ],
                        "asset_size": {"width_px": crop.width, "height_px": crop.height},
                        "media_type": "image/png", "extracted_file": str(path),
                    })
                    page_parts.append(f"`{marker}` (image)")
                    previous = block
            output.write("\n\n".join(page_parts) + "\n\n")
    manifest.write_text(json.dumps({"objects": objects}, indent=2) + "\n", encoding="utf-8")
    return marked, assets, manifest
