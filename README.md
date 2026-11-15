# Anydoc with local image processing

This fork builds on Firecrawl's Anydoc, a fast Rust converter for Word, PowerPoint, Excel, OpenDocument, RTF, EPUB, CSV, and PDFs. Upstream contributors built the format parsers, shared document model, Markdown writer, and Node.js, Python, and WebAssembly bindings. PDF extraction builds on Firecrawl's pdf-inspector. [Original Anydoc](https://github.com/firecrawl/anydoc) · [pdf-inspector](https://github.com/firecrawl/pdf-inspector) · [License](LICENSE)

I wanted a simple way to turn documents into Markdown for local search and AI without losing the figures that give the text context. This fork keeps the fast text workflow and saves links to figures. Optional local processing adds recognized text from scans and descriptions of images, charts, and diagrams to the Markdown, so more of the document's information is available for search and AI. In strict PDF mode, a single page that needs text recognition stops the entire conversion. The fork's default workflow keeps extracting the rest of the PDF and leaves a note where that page belongs. With local OCR enabled, the recognized text goes in that spot.

The fork also includes parser fixes informed by upstream community work. Thanks to [rahul05ranjan](https://github.com/rahul05ranjan) for work on Word comments and symbol glyphs, [Aschenbath](https://github.com/Aschenbath) for work on OpenDocument nested lists and RTF tracked deletions, [crown-sports](https://github.com/crown-sports) for the OpenDocument annotation fix, [Yzzz88](https://github.com/Yzzz88) for chart alignment and OLE previews, and [sean0407](https://github.com/sean0407) for spreadsheet hyperlinks. [Issue and PR credits](docs/local-ai-edition.md#42-upstream-community-findings--prs-credited-in-this-fork)

## Choose how to convert

Run these commands from the root of a checkout of this fork. The Rust example requires a Rust toolchain. The workflow examples also require Python 3. For PDF layout and image dimensions, install the optional Python packages.

```sh
python -m pip install pymupdf pillow
```

### 1. Text only

Use this for searchable document text, headings, lists, and supported tables, with no asset export or model processing.

```sh
cargo run --locked --release --example convert -- report.docx -o report.md
```

A PDF page that needs OCR gets a marker by default. Use `--strict` if you want conversion to return an error instead.

### 2. Text and linked images

Save recognized images and link them at their reading positions. This is the default for the Python workflow and runs without a model.

```sh
python examples/local_ai_workflow.py report.docx -o report-out
python examples/local_ai_workflow.py paper.pdf --pdf-layout -o paper-out
```

For PDFs with embedded raster figures, `--pdf-layout` uses geometric text and image positions to recover common two-column layouts, detect headings and regular tables, and group figure panels. It uses PyMuPDF. Open `assembled.md` and keep its `assets/` folder alongside it. `marked.md` and `objects.json` preserve the positions, saved asset references, and available source and crop dimensions for downstream processing.

### 3. Text, images, and local descriptions

Use this when you want a chart or diagram to be searchable by what it shows. Start Ollama locally and use an installed vision model. Image descriptions appear next to the original figure and its link.

```sh
ollama pull qwen3.5:9b
python examples/local_ai_workflow.py paper.pdf --pdf-layout --describe-image-context -o paper-described
```

For an Office document, leave out `--pdf-layout`. Use `--model` to select another installed vision model. For scanned-page OCR, select a provider explicitly, for example `--provider tesseract` with Tesseract and Poppler installed. Local descriptions can misread figures or estimate values incorrectly. The linked figure is available for verification.

The layout path uses heuristics. It handles embedded raster figures and many ruled tables. Vector-only figures, ambiguous tables, and layouts with hard-to-resolve reading order can require a separate pass. [Local workflow details](docs/local-ai-edition.md) · [Pipeline design and planned work](docs/object-processing-pipeline.md)

### PowerPoint pictures and slide rendering

Every mode extracts editable text, tables, chart data, and speaker notes. Choose how to capture the visual content.

| Switch | What appears in `assembled.md` |
| --- | --- |
| Default (omit `--pptx-visual`) | Individual previews of supported images, scaled to their displayed size and adjusted for the recorded crop and rotation. Links to the original image files remain available. |
| `--pptx-visual` or `--pptx-visual auto` | One full-slide image for each visible slide selected by the layout heuristics. Other slides use the default picture workflow. |
| `--pptx-visual all` | One full-slide image for every visible slide. Hidden slides stay on the native extraction path. |

Auto mode can select a slide because it is image-only, because images cover at least 40% of the slide, or because placement may carry meaning. Examples include text over a graphic, staggered callouts, cards, columns, rotated labels, and connected shapes. `--pptx-image-area-percent 60` changes the image coverage trigger to 60%. The other layout signals still apply.

```sh
# Extract native content and save placement-sized picture previews.
python examples/local_ai_workflow.py deck.pptx -o deck-out

# Render selected slides. Save the images alongside native content.
python examples/local_ai_workflow.py deck.pptx --pptx-visual auto -o deck-visual

# Render every visible slide and add descriptions from the local Ollama model.
python examples/local_ai_workflow.py deck.pptx --pptx-visual all --describe-image-context -o deck-described
```

`--describe-image-context` enables local vision independently of the render mode. On a rendered slide, the model sees the full-slide image, native text, and available text-box coordinates. On other slides, it receives the individual picture previews. Descriptions appear in Markdown alongside the extracted text. For text extraction from rendered slides with Tesseract, use `--provider tesseract` with `--pptx-visual auto` or `all`.

Install Pillow for picture previews and LibreOffice plus PyMuPDF for slide rendering. `--soffice PATH` selects a LibreOffice executable in a custom location. Previews target 150 DPI with a 2560-pixel longest-edge limit. Keep `assets/` alongside `assembled.md`. Use `objects.json` to inspect original image sizes, picture placement, available text-box coordinates, and each slide's selection reasons and rendered dimensions.

## Roadmap for RAG

Future work would extend extraction to the full page structure, including running headers and footers, page numbers, section headings, tables, figures, and captions. Recording their roles, positions, and reading order would help retrieval systems keep section context and cite the right page while filtering repeated page furniture from search results.

Deeper parsing of graphs, charts, and diagrams would recover labels, legends, relationships, and data values where available. Values estimated from graphics would be marked as approximate. Keeping these structured results alongside searchable descriptions and links to the original visuals would give RAG systems more complete evidence for answers grounded in the document.

## Upstream packages and APIs

The package names and browser demo below are published by Firecrawl. Use this checkout for the fork-specific workflow above.

## Quick start

### Agent skill

anydoc ships as an [Agent Skill](https://agentskills.io), so your agent can read any document it runs into:

```bash
npx skills add firecrawl/anydoc
```

The [skill](skills/convert-documents-to-markdown/SKILL.md) teaches the agent to convert documents with the anydoc CLI. Works with [Claude Code](https://claude.ai/code), [Codex](https://openai.com/codex/), [Cursor](https://cursor.com), [OpenCode](https://opencode.ai), and any other [compatible agent](https://agentskills.io/clients).

### CLI

```bash
npx @firecrawl/anydoc report.docx               # Markdown to stdout
npx @firecrawl/anydoc slides.pptx -o slides.md  # or to a file
npx @firecrawl/anydoc - --format csv < data.csv # read stdin
npx @firecrawl/anydoc scan.pdf --ocr hosted     # scanned pages via Firecrawl Parse
```

`npx` downloads the prebuilt binary for your platform on first run. For a permanent `anydoc` command, install globally with `npm install -g @firecrawl/anydoc`. Run `anydoc --help` for all options.

### Node.js

```bash
npm install @firecrawl/anydoc
```

```js
import { toDocument, toMarkdown, toMarkdownBytes } from '@firecrawl/anydoc';

// From a file path:
const markdown = await toMarkdown('report.docx');

// To enable OCR:
const markdown = await toMarkdown('report.docx', { ocr: 'hosted' );

// From bytes, with the format detected from the content:
const fromBytes = await toMarkdownBytes(bytes);

// Or name it, which signature-less formats (CSV) need:
const fromCsv = await toMarkdownBytes(bytes, 'csv');

// Or stop at the document model, which also carries embedded assets:
const document = await toDocument(bytes);
```

> Full API reference: [node/README.md](node/README.md)

### Python

```bash
pip install firecrawl-anydoc
```

```python
import anydoc

# From a file path:
markdown = anydoc.to_markdown("report.docx")

# To enable OCR:
markdown = anydoc.to_markdown("report.docx", ocr="hosted")

# From bytes, with the format detected from the content:
markdown = anydoc.to_markdown_bytes(data)

# Or name it, which signature-less formats (CSV) need:
markdown = anydoc.to_markdown_bytes(data, "csv")

# Or stop at the document model, which also carries embedded assets:
document = anydoc.to_document(data)
```

> Full API reference: [python/README.md](python/README.md)

### Browser (WebAssembly)

```bash
npm install @firecrawl/anydoc-wasm
```

```js
import init, { toMarkdownBytes, toDocument } from '@firecrawl/anydoc-wasm';

await init();

// From bytes, with the format detected from the content:
const markdown = toMarkdownBytes(bytes);

// Or name it, which signature-less formats (CSV) need:
const fromCsv = toMarkdownBytes(bytes, 'csv');

// Or stop at the document model, which also carries embedded assets:
const document = toDocument(bytes);
```

> Full API reference: [wasm/README.md](wasm/README.md)

### Rust

```bash
cargo add anydoc
```

```rust
// From a file path:
let markdown = anydoc::to_markdown("report.docx")?;

// From bytes, with the format detected from the content:
let markdown = anydoc::to_markdown_bytes(&bytes, None)?;

// Or name it, which signature-less formats (CSV) need:
let markdown = anydoc::to_markdown_bytes(&bytes, anydoc::Format::Csv)?;

// Or stop at the document model, which also carries embedded assets:
let document = anydoc::to_document(&bytes, None)?;
```

## OCR

anydoc reads text-based PDFs locally but does no OCR, so a PDF with scanned or image-only pages fails with `NeedsOcr`. Opt in and those documents go to [Firecrawl Parse](https://firecrawl.dev/parse), which OCRs them and returns the same Markdown. No signup needed; set `FIRECRAWL_API_KEY` for higher limits.

|        | Opt in                                         | Key, else `FIRECRAWL_API_KEY` |
| ------ | ---------------------------------------------- | ----------------------------- |
| CLI    | `anydoc scan.pdf --ocr hosted`                 | `--api-key <key>`             |
| Node   | `toMarkdown('scan.pdf', { ocr: 'hosted' })`    | `apiKey`                      |
| Python | `anydoc.to_markdown("scan.pdf", ocr="hosted")` | `api_key`                     |

Only documents that need OCR leave the machine, and the whole document goes, since Parse has no page selection. If Parse cannot convert it, Node rejects with `code: 'hosted'` and Python raises `HostedError`. `--api-url`, `apiUrl` and `api_url`, else `FIRECRAWL_API_URL`, point at another Parse deployment. `--parse-mode`, `parseMode` and `parse_mode` pick the Parse PDF mode: `auto` (the default) extracts text and falls back to OCR, `fast` extracts text only, `ocr` OCRs every page. The Rust crate has no `ocr` option and never makes network calls.

## Features

- **One output for every format.** Each format parses into a shared document model and renders through a single Markdown serializer, so escaping, tables, heading anchors, and footnotes behave identically whether the input was a `.doc` from 2003 or a `.pptx` from yesterday.
- **Full document structure.** Headings with anchors, bold/italic/strikethrough, inline code and code blocks, links and internal cross-references, bulleted/numbered/nested/task lists with the source's own numbering, tables with merged cells and header rows, block quotes, footnotes and endnotes, and speaker notes.
- **Equations as LaTeX.** Word and PowerPoint (OMML), OpenDocument and EPUB (MathML), and RTF equations convert to GitHub-flavored math: `$...$` inline and `$$` blocks.
- **Embedded assets.** Images and embedded objects render as their alt text in the Markdown, and the raw bytes stay available on the document model, tagged with their media type. Images with an external URL become ordinary Markdown images.
- **Content-based format detection.** The format is read from the bytes themselves (PDF header, RTF open group, OLE stream names, ZIP package mimetype), so mislabeled files still convert correctly.
- **Fast.** Pure Rust, no ML models, no external services. Median conversion time is under 5ms per document.
- **Bindings that stay out of the way.** Node.js conversion runs on the libuv thread pool and never blocks the event loop; Python releases the GIL so other threads keep running. TypeScript types and Python stubs ship with the packages.
- **PDF support built in.** Text-based PDFs convert locally through [pdf-inspector](https://github.com/firecrawl/pdf-inspector), no OCR service required. Scanned pages can opt into [hosted OCR](#scanned-pdfs-ocr).
- **Agent ready.** Ships as an [Agent Skill](#agent-skill): one `npx skills add firecrawl/anydoc` and any agent can read office documents.

## Supported formats

| Format           | Extensions                                                 |
| ---------------- | ---------------------------------------------------------- |
| Word             | `.doc`, `.docx`, `.docm`                                   |
| PowerPoint       | `.ppt`, `.pps`, `.pot`, `.pptx`, `.pptm`, `.ppsx`, `.ppsm` |
| Excel            | `.xls`, `.xlsx`, `.xlsm`, `.xlsb`                          |
| OpenDocument     | `.odt`, `.ods`, `.odp`                                     |
| Rich Text Format | `.rtf`                                                     |
| EPUB             | `.epub`                                                    |
| CSV              | `.csv`                                                     |
| PDF              | `.pdf`                                                     |

## Benchmark

anydoc is measured against six other converters on 100 real-world documents spanning fourteen formats. Scores run from 0 to 100, higher is better; speed is the median time to convert one document.

| tool         | formats   | median ms | docs judged | score  | completeness | structure | formatting | cleanliness |
| ------------ | --------- | --------- | ----------- | ------ | ------------ | --------- | ---------- | ----------- |
| anydoc       | **14/14** | **4.4**   | 94          | **81** | **87**       | **79**    | **78**     | **81**      |
| libreoffice  | 12/14     | 1129.5    | 87          | 40     | 59           | 42        | 40         | 24          |
| unstructured | 8/14      | 572.9     | 58          | 63     | 76           | 59        | 51         | 63          |
| markitdown   | 6/14      | 134.8     | 33          | 65     | 78           | 66        | 60         | 52          |
| pandoc       | 5/14      | 102.1     | 34          | 56     | 74           | 57        | 56         | 38          |
| docling      | 4/14      | 513.6     | 21          | 57     | 60           | 60        | 57         | 51          |
| mammoth      | 1/14      | 52.5      | 8           | 70     | 84           | 71        | 75         | 51          |

Per format, like for like:

| format | anydoc | libreoffice | unstructured | markitdown | pandoc | docling | mammoth |
| ------ | ------ | ----------- | ------------ | ---------- | ------ | ------- | ------- |
| doc    | **87** | 57          | 67           | -          | -      | -       | -       |
| docm   | **84** | 48          | -            | -          | -      | -       | -       |
| docx   | **88** | 56          | 53           | 71         | 68     | 71      | 70      |
| epub   | **77** | -           | 72           | 72         | 52     | -       | -       |
| odp    | **86** | 23          | -            | -          | -      | -       | -       |
| ods    | **82** | 38          | -            | -          | -      | -       | -       |
| odt    | **80** | 51          | 68           | -          | 60     | -       | -       |
| ppt    | **80** | 26          | -            | -          | -      | -       | -       |
| pptx   | **74** | 24          | -            | 66         | -      | 52      | -       |
| rtf    | **88** | 53          | 46           | -          | 45     | -       | -       |
| xls    | **80** | 38          | 66           | 62         | -      | -       | -       |
| xlsm   | **76** | 32          | -            | -          | -      | -       | -       |
| xlsx   | **72** | 30          | 66           | 55         | -      | 47      | -       |

**How quality was scored:** an LLM judge (Claude Sonnet 5) compares two tools' outputs blind against ground truth: the document's first six pages, rendered to images by LibreOffice. Each output is scored on completeness, structure, formatting, and cleanliness. Every pair is judged twice with the outputs swapped to cancel position bias, for 482 verdicts in total. Each tool's `score` averages its per-format scores over the formats it supports, so a corpus heavy in one format can't skew it. It also means each row averages a different set of formats (mammoth's 69 is docx alone, while anydoc's 81 spans all fourteen), so the per-format table is the fair comparison.

Speed is one warm conversion per document on a Ryzen 9 9950X3D (Windows 11, 64 GB DDR5-6400). anydoc and the Python libraries are timed with process spawn excluded; the CLI tools include it, since that is how they are used. The harness lives in [`bench/`](bench/README.md); the corpus is not redistributable and is not in the repo.

**Best fit:** pipelines that receive a mixed bag of office documents and need one consistent, structured Markdown output. In this comparison, anydoc was the only tool to cover all fourteen formats, scored highest on every judged format, and converted documents an order of magnitude faster than the next-fastest tool.

## Format detection

The format is read from the file content, using the marker its specification designates: the PDF header, the RTF open group, OLE stream names, the ZIP package mimetype and content types. CSV has no such marker, so the extension or an explicit format names it instead.

```rust
Format::from_bytes(&bytes); // Some(Format::Docx), or None when nothing matches
Format::from_extension("pptm"); // Some(Format::Pptx)
Format::from_path(Path::new("report.odt")); // Some(Format::Odt)
```

The same three functions exist in Node (`formatFromBytes`, ...) and Python (`anydoc.format_from_bytes`, ...).

## Errors

A conversion returns `Err` only when no complete Markdown could come out of the file. `ConvertError` names what went wrong:

```rust
match anydoc::to_markdown(path) {
    Ok(markdown) => Some(markdown),
    // No document comes out of these, so record the file and take the next one.
    Err(
        error @ (ConvertError::Encrypted
        | ConvertError::Unsupported(_)
        | ConvertError::NeedsOcr { .. }),
    ) => {
        unconverted.push((path, error));
        None
    }
    Err(error) => return Err(error),
}
```

| Variant         | Meaning                                                             |
| --------------- | ------------------------------------------------------------------- |
| `Unsupported`   | Unknown format, or one that cannot be converted                     |
| `NeedsOcr`      | Scanned or image-only pages of a PDF, listed in `pages`             |
| `Malformed`     | Structurally unusable: no meaningful content could be extracted     |
| `Encrypted`     | Encrypted or password-protected                                     |
| `ResourceLimit` | Crossed a fixed safety limit (decompression, nesting, node count)   |
| `MissingPart`   | A part required for any meaningful output is absent                 |
| `Io`            | The file could not be read, from `to_markdown` only                 |

Node and wasm publish the variant name on `error.code`; Python raises one `anydoc.ConvertError` subclass per variant, or `OSError` when the file cannot be read.

anydoc converts locally and does not do OCR, so a PDF with scanned pages fails with `NeedsOcr`. Opt in with `ocr: 'hosted'` in Node, `ocr="hosted"` in Python or `--ocr hosted` on the CLI to send that document to [Firecrawl Parse](https://firecrawl.dev/parse). No signup needed. Set `FIRECRAWL_API_KEY` for higher limits.

## How it works

```
document bytes
  │
  ├─► format detection      → content markers, not the extension
  │
  ├─► format parser          → one per format (doc, docx, ppt, pptx, xls,
  │                            xlsx, odt/ods/odp, rtf, epub, csv)
  │         │
  │         └─► Document     → shared model: blocks, inlines, tables,
  │                            footnotes, assets
  │               │
  │               └─► GFM serializer → Markdown
  │
  └─► PDF → pdf-inspector    → Markdown directly
```

Because every format funnels through the same document model and serializer, output quirks get fixed once. A table-escaping fix for docx is automatically a table-escaping fix for rtf, odt, and everything else.

## Development

```bash
cargo test
cd node && npm install && npm run build && npm test
cd python && pip install maturin && maturin develop && python -m unittest discover -s tests
wasm-pack build wasm --release --target web --scope firecrawl && node --test wasm/test.mjs  # see wasm/README.md
```

A committed fixture corpus under `tests/fixtures/` is snapshot-tested, `tests/robustness.rs` mutation-tests every fixture, and `fuzz/` carries cargo-fuzz targets per format. The speed and quality benchmark lives in [`bench/`](bench/README.md).

Releases are tagged `v<version>`, which publishes the crate, the npm package, and the PyPI wheels from [`.github/workflows/release.yml`](.github/workflows/release.yml). The version lives in three places, bumped together for a release:

- [`Cargo.toml`](Cargo.toml): the crate
- [`node/package.json`](node/package.json): the npm package
- [`python/Cargo.toml`](python/Cargo.toml): the wheel (`python/pyproject.toml` reads it)

## License

[MIT](LICENSE)
