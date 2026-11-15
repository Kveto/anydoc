# Anydoc object-processing pipeline for local AI & RAG

Anydoc extracts readable content and marks where pages or objects need further processing. The local workflow stores the original payloads, processes the marked objects in parallel, and assembles a new Markdown file for RAG or local reading. A visible link at each processed object's original position lets readers retrieve the stored payload.

## Open-source release scope

Use the existing Anydoc conversion for text and tables, including complex tables. The only deferred objects are images recognized by Anydoc, including image-only PDF pages identified for OCR. Store recognized embedded images as original bytes. For a deferred PDF page, retain the original PDF and page number, and store a rendered image when the OCR worker produces one.

The first working flow is extraction, image storage and manifest creation, image description or OCR, then one Markdown assembly pass. The assembler replaces each image marker with its result and a visible link to the stored payload. Tables keep their current Anydoc Markdown representation and create no processing jobs or HTML artifacts. Native chart and other non-image object processing, partitioned extraction, and more advanced layout routing are design options beyond this release.

Acceptance cases include an embedded image between paragraphs, a mixed PDF with a scanned page, and a document containing both an image and a table. For the last case, compare the table Markdown with normal Anydoc output and verify that only the image gets a deferred job.

## 1. Extract the document and store the payloads

The current Rust entry point is `anydoc::to_markdown_with_objects(&bytes, format)`. It returns Markdown, a list of marked objects, and embedded asset bytes. It handles whole PDF pages flagged for OCR and images or embedded objects recognized by the existing format parsers. The existing API uses visible markers. The demo converts those markers to the working format below.

Use a filesystem directory as the development bucket. Each source document revision has its own prefix. Use portable, slash-separated keys through a storage interface so the same pipeline can use a local folder, S3, or Cloudflare R2.

```text
documents/<document-id>/revisions/<revision-id>/
  source/original.rtf
  extraction/document.md
  extraction/objects.json
  assets/<sha256>/original.png
  assets/<sha256>/original.svg
  objects/<object-id>/input.png
  objects/<object-id>/results/describe.json
  objects/<object-id>/results/ocr.json
  assembled/document.md
```

Store original bytes for embedded images, SVGs, and other available payloads. Store complex native objects with the parts they need, such as a chart definition and its workbook. A preview can be stored separately. For PDF pages or regions without standalone payloads, store a rendered page or crop when materialized for processing. Record the original PDF key and page, and for crops record the bounding box and coordinate system. Each placement gets its own marker ID. Identical payload bytes can share a stored asset key.

Write the payload before making its job available. Keep the source file and first-pass Markdown under the revision prefix. The extraction manifest records what was found and can serve as the list of expected work for assembly.

## 2. Record markers and processing requirements

Store the first pass as raw Markdown. HTML comments mark the exact positions for the assembly pass. Rendering or sanitizing the working Markdown can remove comments.

```markdown
The graphic below compares April and May.

<!-- anydoc:object id="anydoc-object-1" -->

The next report covers June.
```

The manifest describes each object and points to a payload. Keys are relative to the revision prefix. Bucket names, endpoint URLs, credentials, and local filesystem roots belong in storage configuration.

```json
{
  "schema_version": 1,
  "source_revision": "v1",
  "source_key": "source/original.rtf",
  "objects": [
    {
      "id": "anydoc-object-1",
      "kind": "image",
      "asset_key": "assets/<sha256>/original.png",
      "media_type": "image/png",
      "processing": ["describe"],
      "status": "pending"
    }
  ]
}
```

Use OCR for scanned or otherwise unreadable text, visual description for figures, and a dedicated extractor for native chart data when available. The processing list can request multiple operations on the same object. A missing image payload can be marked for repair or review. Invalid input and exhausted parser safety limits remain document-level failures.

## 3. Process objects in parallel

Use a shared work queue. Each available worker claims a job, loads its payload by key, and writes a result for that job. A job identifies the revision, object ID, and processing type. Independent operations on the same object can run concurrently. Worker completion order has no bearing on output placement.

Use a claim timeout so an abandoned job can be retried. Key the result by revision, object ID, and operation so a retry can be handled without duplicate results. Each worker writes a separate result record, for example `objects/anydoc-object-1/results/ocr.json`. Include the object ID, operation, terminal status, output or error, and payload reference. The pipeline can build an enriched manifest from those records after processing. The extraction manifest remains the source of the expected objects and operations.

For the parallel simulation, use up to five workers and processors with different delays. Queue a scanned page, an embedded chart, and an SVG. Make the first job finish last. Verify that work overlaps, that results are stored separately, and that all links retrieve the correct payload. The simulation defines scheduling and handoff without selecting OCR or vision services or ordering dependent processing steps.

## 4. Assemble once in document order

Wait for extraction to finish and for each requested operation to succeed or reach a recorded failure. Workers write result records. A single assembler reads the original first-pass Markdown from start to finish and writes a separate assembled file.

1. Match each marker to exactly one manifest entry and look up the results by ID. Validate that every expected placement appears once and that its result and asset references belong to the revision.
2. Replace each marker at its original position with the agreed content and a visible link to the stored object. Insert OCR text or an image description as appropriate. If processing fails, insert a visible notice with the payload link where available.
3. Publish the assembled Markdown after validation. If an object is processed again, reassemble from the original working Markdown and the updated results.

The current demo renders the link as **Processed image · anydoc-object-1**, followed by the description. In a local Markdown file the link can be relative to the asset key. For a private cloud bucket, use a stable application URL that authorizes the request and resolves the revision and key when opened.

## 5. Index the assembled Markdown

Store the assembled Markdown as the RAG input and chunk it after assembly so descriptions stay near the surrounding text. Keep the source revision, object ID, and asset key or page locator in chunk metadata so retrieved passages can cite the original payload. Retain the working Markdown, manifest, payloads, and results to support reprocessing.

## Large documents and memory budgets

The pipeline can scale with bounded extraction, processing, assembly, and indexing. The current Rust conversion API accepts the full input as bytes, retains the parsed document and embedded assets in memory, and builds a full Markdown string. The PDF fallback collects page Markdown before joining it. Moving payloads into the bucket after conversion still leaves that extraction peak. Keep the existing API for inputs within a measured size budget and add an extraction path that writes incremental output for larger inputs.

| Stage | Large document design |
|---|---|
| Intake | Stream the source to the bucket. Use bounded range reads where the format reader supports them. For formats needing random access, stage the source in a temporary file and open it through a seekable reader. Budget and clean up temporary disk space. |
| Extraction | Write ordered Markdown fragments to a buffered sink and emit one manifest record per placement. Stream each extracted asset into storage while hashing it and finalize its key before releasing the parser's reference. Use an incremental manifest such as JSON Lines or a disk-backed index for many objects. Keep the existing extraction manifest example for small documents. |
| Dispatch | Queue IDs, payload keys, and processing requests. Admit work using a shared memory budget, with a per-worker limit and bounded prefetch. Pause dispatch when the budget is full and resume when workers release capacity. |
| Processing | Load only the assigned payload or PDF region. Budget decoded pixels, page render buffers, and model memory as well as compressed bytes. Cap dimensions and decoded size. Tile or downsample when the operation can tolerate it. Persist results promptly and release buffers. |
| Assembly | Stream first-pass Markdown in source order. Parse reserved markers across read boundaries and resolve each ID from a disk-backed result index or a bounded batch of lookups. Copy normal text and insert the processed content and bucket link into a buffered output writer. Publish the new assembled file only after validating marker coverage and result references. |
| RAG | Read assembled Markdown incrementally. Bound chunk size and overlap, stream embeddings in bounded batches, and retain the revision, object ID, and asset key with each relevant chunk. |

For Office ZIP formats, a sink can reduce retained asset memory, but a truly bounded path also needs incremental handling of large XML parts and document blocks. Preserve lightweight indexes for features that need document-wide context, such as notes and anchors. For PDFs, check whether the reader actually bounds retained PDF objects, extracted text, and render buffers. Iterating over pages after an API has already materialized the whole result does not bound its peak memory. Use page or region batches through a suitable reader, or isolate conversion behind a hard process limit with a recorded failure if the input exceeds it.

Set a configurable default of **five active processing jobs per execution host**, shared across all documents on that host. The dispatcher grants a job both a worker slot and a memory reservation before it starts. Use one admission controller per host so several documents cannot each start five workers. In the simulation, use five slots and mock memory estimates. Production memory limits must be configured for the host and calibrated with measured jobs.

1. Reserve headroom for the runtime, extraction, assembly, and any loaded models. Set the worker memory budget from the remaining capacity. For example, with a 4 GiB worker budget, a job reserving 3 GiB can run alongside jobs whose reservations total at most 1 GiB, even with free worker slots.
2. Estimate memory for each job before starting it using decoded dimensions, render buffers, and the processor's working memory. When either limit is reached, keep jobs queued. Smaller jobs can start when they fit, with aging or reserved capacity to prevent larger jobs from waiting indefinitely.
3. Release the slot and reservation when the worker has finished or its process has stopped. Record observed peak memory and use process or container memory limits where available, since a reservation alone cannot enforce actual memory use.
4. Route a job that exceeds the entire worker budget to a bounded variant, such as tiled processing, or record an explicit failure. The assembler can then preserve its position and link without waiting indefinitely.

Apply separate limits to source size, decompressed bytes, decoded object size, queue depth, and temporary storage. Extraction needs its own budget because the current Anydoc API still retains the full parsed document and assets.

The large-file acceptance test should increase the number of assets while keeping their individual sizes constant, then test one very large compressed image and several concurrent heavy jobs. Measure peak process memory and temporary disk use separately for extraction, workers, assembly, and indexing. Verify that no payloads are lost, output order and links survive out-of-order completion, and budget exhaustion produces an explicit outcome. The current demo exercises placement and retrieval. It supplies no large-file benchmark.

## Partitioned extraction for large inputs

Select the extraction path before calling the in-memory Anydoc API. Stage the source in seekable storage and probe its format, compressed and declared expanded sizes, and unit counts where available. Use these signals to choose the normal path or the partitioned path. Treat metadata as an estimate and enforce limits on actual bytes, expansion, and allocations during parsing. A small compressed file can still need the partitioned path.

Build a lightweight, ordered extraction plan and persist it incrementally. Give each unit a stable ID and record the shared metadata it needs.

| Format | Extraction unit | Shared state |
|---|---|---|
| PDF | A page or a small batch of pages | Page tree, cross-references, fonts, and shared resources. Use a reader that can fetch page objects on demand with bounded caches. |
| PPTX and EPUB | A slide or spine item | Resolve relationships, styles, and ordering once. Read related assets on demand. |
| XLSX and CSV | Worksheet row batches or complete CSV record batches | Stream worksheet XML and CSV records. Use a disk-backed shared-string index when needed. Preserve row order and sheet identity. |
| DOCX | Consecutive block batches from a streaming XML parser | Carry section, list, and table state across batches. Resolve relationships, notes, and styles through bounded caches or disk-backed indexes. Spool oversized structures to disk or apply a per-unit limit. |

Each unit writes its Markdown fragment and manifest records to the revision prefix, stores its payloads, and records completion. Use unit IDs in placement IDs so fragments cannot collide. Release parse buffers before moving on. Resume from completed units where the reader supports safe checkpoints. For streaming formats, reopening may require replaying from a safe boundary. Publish the extraction completion record only when every planned unit has either completed or has an explicit failure.

Start with sequential extraction or a small bounded window of units, while already extracted objects enter the existing worker queue. Keep the five-job processing cap and coordinate extraction and workers through the host memory budget. Assemble the fragments in source order once extraction and object processing have settled. Handle cross-unit tables, list numbering, notes, and anchors before publishing Markdown for RAG.

A page batch only saves memory when the reader loads and releases that batch independently. The current PDF extraction API can still load and analyze the entire document before returning selected pages. Verify bounded behavior with peak-memory measurements before adopting a reader. If a format or unit cannot fit the budget with the available reader, record that outcome. Keep completed fragments and their provenance available.

### Alternative: map the format before partitioning

This is a design alternative. The existing converter and worker design remain as documented. The assumption is that a bounded discovery pass can identify the format and enough of its structure to plan extraction. The pass inventories logical units and dependencies. It does not decode all assets, render pages, or hold the document model in memory. A full upfront map is optional. Persist entries as discovered, or discover them alongside extraction when an upfront pass would duplicate work.

1. **Identify the format.** Probe the signature and, for containers such as Office ZIP packages, the internal metadata. Use the extension or an explicit hint for formats without signatures, such as CSV. Record whether the probe found encrypted, damaged, ambiguous, or unsupported input.
2. **Map the source units.** Record each unit's stable ID, source locator, logical order, shared dependencies, and a size estimate where available. PDF pages and PowerPoint slides already have fixed order. XLSX has sheets and row ranges, EPUB has spine items, and CSV has records. DOCX and similar flow documents can use sections or block ranges. Exact Word pages depend on a specific layout render and its settings.
3. **Choose the partition size.** Use estimated expanded bytes, render dimensions, and shared state to select one unit or a small batch. Avoid splitting raw bytes, XML elements, CSV records, or structures whose parser state cannot cross a boundary. Enforce actual limits during extraction. If one unit is oversized, split it at a supported boundary, use temporary storage or a bounded reader, or record the unit failure.
4. **Extract and record results.** Store each unit's Markdown fragment and object references under the revision. Keep source order in the unit map and preserve page, slide, sheet, or block locators for assembly and citations. An object spanning units gets one placement at its source anchor, with any additional locators recorded separately. Final assembly uses the recorded order and the object markers after workers settle.

Prefer the normal path for small inputs. Evaluate this alternative for formats with cheap structural discovery, stable units, and bounded reads. When a reader needs substantial global state, include that state in the memory budget and measure the cost of the map itself. Compare peak memory, temporary disk use, and elapsed time against the normal path. Verify output order, shared references, cross-unit structures, and source links with representative files before selecting it.

## Routing by layout and text quality

Choose the extraction path for the smallest unit whose reading order can be resolved reliably. Use source blocks and sections for Word until pagination is known, slides for PowerPoint, and physical pages for PDF. A single document can use more than one path.

| Path | When to use it | What to keep |
|---|---|---|
| Native extraction | Source markup or PDF text gives reliable text and reading order. Normal flowing Word columns can qualify. | Native text, headings, lists, tables, and source locators. Retain markers for embedded objects with reliable anchors. |
| Rendered layout with native text | Visual placement affects order or meaning, as with floating text boxes, side-by-side panels, or figures with captions. Native text remains usable. | Render for layout, match native text to regions, and establish reading order from the region structure. Send only unresolved regions or objects for OCR, description, or specialized extraction. |
| Full-page image with OCR and layout analysis | A scan, outlined or garbled text, or a complex page whose native text and visual regions cannot be reconciled reliably. | Render the affected page to an image. Recover text and regions with OCR and page layout analysis. Route non-text regions for description or structured extraction where needed. Store the page image and relevant crops with their source coordinates. |

Use structural hints such as floating anchors, text boxes, section and column definitions, and table structure as evidence. For PDF, also consider text coverage, decoding quality, and scanned-page signals. Multiple columns or an embedded image alone do not require full-page OCR. When the choice is uncertain, render the affected unit and compare native text with visible regions before escalating. Record the selected path, reason, source locator, and unresolved items in the revision's processing records.

Rendered pages can contain usable native text alongside images and scanned text. In the hybrid path, reconcile results by region so OCR does not duplicate native text. Keep caption and figure associations and use layout groups to determine reading order. A global top-to-bottom, left-to-right sort can interleave independent columns. For Word, map blocks to page and region positions from a specific rendition. Record the renderer and settings needed to reproduce page references.

Queue rendering and OCR under the host memory budget and the five-job cap. Render only the pages that need visual analysis where the renderer supports it. Some Word renderers must paginate the whole document to locate those pages. Treat that layout pass as its own budgeted job. Keep one assembly pass in document order and retain links to the stored objects in the Markdown used for RAG.

Validate the routing with a flowing two-column Word section, a page with floating text boxes and an embedded figure, and a PDF containing both selectable text and a scanned page. Check reading order, duplicate text, preserved object links, and peak memory. This is a proposed design.

### Deterministic routing rules

Evaluate the rules in order for each page or logical source unit. Each check reports pass, fail, or unknown. The rules are deterministic for a given set of observations and a versioned routing policy. The observations can be incomplete or heuristic, so the chosen path is not a guarantee of perfect extraction.

**Single-image page override.** Before evaluating the three options below, route a page recognized as one image carrying the entire visible page content directly to **Option 3**. This also applies when the image has an invisible OCR text layer. An image occupying one region alongside independently visible native text is evaluated by the normal rules. If detection cannot establish that the image is the whole page, record the check as unknown and use the layout inspection step.

1. **Option 1 — native text only.** Native text passes decoding and coverage checks, source order is reliable, and there are no meaningful unresolved regions or objects. An independently verified blank unit also takes this path and emits no text. Normal flowing Word columns can qualify. Decorative elements and fully extracted native tables do not create pending work.
2. **Option 2 — native text plus object or region processing.** Native text is usable and any missing content can be isolated to addressable objects or regions. Each pending item has a source anchor or page coordinates, and the resulting reading order is reliable. Use the source anchor when sufficient. Render for layout only when needed to establish region boundaries, order, or caption associations. A region can use OCR while the rest of the page keeps its native text.
3. **Option 3 — full-page OCR and layout analysis.** The unit contains meaningful visible content and native text is unusable across the page, or the hybrid path cannot reliably isolate missing content and reconcile it with the native text and reading order. Analyze the whole rendered page and keep any remaining objects available for follow-up. An inaccessible or unrenderable unit is recorded as a failure, with its source reference where available.

For an unknown check, inspect the rendered layout if available and reapply the rules. A missing text layer on a verified scan qualifies for option 3. A chart with a reliable anchor qualifies for option 2 even if its description needs visual processing. Empty extraction alone cannot distinguish a blank page from a scan. Define and calibrate format-specific thresholds for text quality, region coverage, and alignment against representative fixtures. Record the policy version, checks, selected path, and reason for each unit.

## Page rendering

Use headless LibreOffice for Word layout, following the renderer used in the previous project. Export the original DOCX to a PDF once per source revision and render configuration. For an input PDF, use the original directly. Use a PDF rasterizer such as PDFium to render only the pages or regions selected for Options 2 and 3. Keep Option 1 on native extraction when its checks pass.

- Run LibreOffice in an isolated worker with a dedicated profile, pinned version and fonts, and explicit memory, time, and temporary disk limits. Budget the pagination job for the whole document. LibreOffice's PDF page-range setting limits export output and does not establish a bounded pagination cost.
- Store the PDF rendition and render metadata under the document revision. Record the renderer version, font set, export settings, source hash, rendition key, and page count. Reuse that rendition across jobs. Word page references refer to this rendition. Use reference PDFs supplied with the fixtures when exact pagination must be checked.
- For Option 2, align native blocks to positioned PDF text and render selected pages or crops for unresolved visual regions. For Option 3, rasterize the full selected page for OCR and layout analysis. Choose resolution for the task and bound pixel dimensions before allocation. Store images and crops by key and release raster buffers after writing.
- Record the rendition page number, crop box, rotation, raster dimensions, and transform between PDF coordinates and image pixels. Use those records for OCR alignment, object links, and citations. For Word, retain the mapping from source block IDs to rendition regions, including ambiguous mappings.
- Validate against fixtures with flowing columns, floating text boxes, tables spanning pages, mixed orientations, and fonts that affect line wrapping. Check page count, clipping, reading order, and whether highlighted regions land on the expected content.

[LibreOffice PDF export options](https://help.libreoffice.org/latest/en-US/text/shared/guide/pdf_params.html) · [PDFium rendering API](https://pdfium.googlesource.com/pdfium/+/0ab17240e21eb625350cfcc622b8aeedf67eb4b9/public/fpdfview.h)

**Authority for visual extraction.** When visual routing is needed, the stored PDF rendition (or the original PDF for PDF inputs) is the source of truth for page boundaries, visible content, reading order, and region coordinates. Use matching native text and original object payloads to enrich the extraction, with provenance back to the source. Resolve disagreements about visible layout against the PDF. Rasterize pages or regions only when visual analysis requires pixels. For native-only extraction, use the original document structure.

### Self-hosted renderer alternatives

Expose one rendering interface to the host pipeline. An adapter can produce a fixed-layout PDF rendition, positioned text, or a raster of a requested page or region. This lets routing and assembly use one interface while the deployment can choose its rendering backend.

| Candidate | How it fits | What to verify |
|---|---|---|
| LibreOffice with PDFium behind one service | Retain the existing Office-to-PDF approach. Rasterize requested pages or crops and keep the PDF for native text alignment. | Fidelity against reference PDFs, font substitution, and peak memory for pagination and rasterization separately. |
| Self-hosted ONLYOFFICE Docs | One conversion service supports Office-to-PDF and direct page images. The documented thumbnail switch offers the first page or an archive of all pages. For selective rendering, evaluate a page-level API or rasterize the exported PDF separately. | Compare fidelity and memory with LibreOffice. Review AGPL terms or the commercial edition for the planned private deployment and distribution. |

Test the same complex documents across the candidates. Include multiple columns, floating objects, charts, tables crossing pages, and substituted fonts. Measure page count and visual differences against fixture reference PDFs, region alignment, peak memory, and elapsed time. Record whether each backend can render a requested page or crop without rasterizing all pages. Keep the backend decision open until these checks and the license review are complete.

Sources: [LibreOffice PDF export](https://help.libreoffice.org/latest/en-US/text/shared/guide/pdf_params.html), [ONLYOFFICE conversion options](https://api.onlyoffice.com/docs/docs-api/additional-api/conversion-api/request), [ONLYOFFICE core license](https://github.com/ONLYOFFICE/core).

## Demo files and implementation status

- The [mixed PDF input](../tests/fixtures/pdf/handmade-mixed.pdf) and [generated Markdown](../examples/marked-pdf/output/output.md) show readable text followed by a whole-page OCR marker.
- The [local bucket demo](../examples/bucket-demo/bucket/documents/figure-demo/revisions/v1/) contains the original RTF, working Markdown, manifest, content-addressed PNG, example description result, and assembled Markdown with a verified relative link. Its single result uses a manually written description and the demo path `objects/anydoc-object-1/result.json`.
- The production queue, parallel simulation, automated assembler, cloud storage backend, and RAG indexing are design steps. PDF region markers and extraction of native objects that the current parsers skip also require implementation.
