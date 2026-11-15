//! Fixture corpus snapshot harness.
//!
//! One snapshot per fixture of the single conversion result: the Markdown
//! output, or the terminal error. Logs are never snapshotted. Fixtures under
//! `abuse/` are resource-abuse shapes and are exercised by dedicated tests once
//! limits exist, not by the corpus sweep. Malformed fixtures encode their
//! expected outcome as `name--<outcome>.ext` where outcome is one of
//! `recovers`, `skips`, `ignores`, `errors`.

mod common;

use common::{fixture_root, walk};
use std::fmt::Write as _;
use std::path::Path;

/// Convert one file, capturing panics so a bad parser records a baseline
/// instead of aborting the whole harness.
fn convert(path: &Path) -> String {
    let result = std::panic::catch_unwind(|| anydoc::to_markdown(path));
    match result {
        Ok(Ok(md)) => md,
        Ok(Err(e)) => format!("ERROR: {e:#}"),
        Err(_) => "PANIC".to_string(),
    }
}

fn expected_outcome(path: &Path) -> Option<&'static str> {
    let stem = path.file_stem()?.to_str()?;
    let (_, outcome) = stem.rsplit_once("--")?;
    match outcome {
        "recovers" => Some("recovers"),
        "skips" => Some("skips"),
        "ignores" => Some("ignores"),
        "errors" => Some("errors"),
        _ => None,
    }
}

#[test]
fn corpus() {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &mut files);
    for path in files {
        let rel = path.strip_prefix(&root).unwrap();
        if rel.starts_with("abuse") {
            continue;
        }
        let name = rel.to_string_lossy().replace(['\\', '/'], "__");
        let output = convert(&path);
        // Once the unified recovery policy exists (P1), annotated malformed
        // fixtures must match their single expected outcome.
        if let Some(outcome) = expected_outcome(&path) {
            let is_err = output.starts_with("ERROR: ") || output == "PANIC";
            match outcome {
                "errors" => {
                    assert!(is_err, "{name}: annotated `errors` but converted successfully")
                }
                // recovers/skips/ignores fixtures must produce usable output
                // post-refactor; during the baseline phase current behavior is
                // recorded as-is, so this stays a snapshot-only expectation
                // until P1 flips `STRICT_ANNOTATIONS` on.
                _ if STRICT_ANNOTATIONS => {
                    assert!(!is_err, "{name}: annotated `{outcome}` but failed: {output}")
                }
                _ => {}
            }
        }
        insta::assert_snapshot!(name, output);
    }
}

/// Annotated malformed fixtures must produce their single expected outcome:
/// `recovers`/`skips`/`ignores` convert successfully, `errors` return a typed
/// error. (Baseline recording ran with this off; it is enforced from P1 on.)
const STRICT_ANNOTATIONS: bool = true;

/// Every well-formed corpus fixture must be identified from its bytes
/// alone. Only the signature-less text formats (csv) legitimately need the
/// extension fallback.
#[test]
fn fixtures_detect_from_content() {
    use anydoc::Format;
    let expected = [
        ("doc", Some(Format::Doc)),
        ("docx", Some(Format::Docx)),
        ("epub", Some(Format::Epub)),
        ("odp", Some(Format::Odp)),
        ("ods", Some(Format::Ods)),
        ("odt", Some(Format::Odt)),
        ("pdf", Some(Format::Pdf)),
        ("ppt", Some(Format::Ppt)),
        ("pptx", Some(Format::Pptx)),
        ("rtf", Some(Format::Rtf)),
        ("xls", Some(Format::Excel)),
        ("xlsb", Some(Format::Excel)),
        ("xlsx", Some(Format::Excel)),
        ("csv", None),
    ];
    let root = fixture_root();
    for (dir, format) in expected {
        let mut files = Vec::new();
        walk(&root.join(dir), &mut files);
        assert!(!files.is_empty(), "no fixtures under {dir}/");
        for path in files {
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(
                Format::from_bytes(&bytes),
                format,
                "detection mismatch for {}",
                path.display()
            );
        }
    }
}

/// A PDF with scanned pages is reported, not silently shortened: the error
/// names the pages that need OCR.
#[test]
fn scanned_pages_are_reported_not_dropped() {
    let mixed = std::fs::read(fixture_root().join("pdf/handmade-mixed.pdf")).unwrap();
    match anydoc::to_markdown_bytes(&mixed, anydoc::Format::Pdf) {
        Err(anydoc::ConvertError::NeedsOcr { pages, page_count }) => {
            assert_eq!((pages, page_count), (vec![2], 2));
        }
        other => panic!("expected NeedsOcr, got {other:?}"),
    }
}

#[test]
fn marked_pdf_keeps_readable_text_and_scanned_pages() {
    use anydoc::{Format, PendingObject, to_markdown_with_objects};

    let mixed = std::fs::read(fixture_root().join("pdf/handmade-mixed.pdf")).unwrap();
    let result = to_markdown_with_objects(&mixed, Format::Pdf).unwrap();
    assert!(matches!(result.objects.as_slice(), [PendingObject::PdfPage { page: 2, .. }]));
    let (text, marker) = result.markdown.split_once("> Pending OCR").unwrap();
    assert!(!text.trim().is_empty());
    assert!(marker.contains("PDF page 2 · `anydoc-object-1`"));

    let scan = std::fs::read(fixture_root().join("pdf/handmade-scanned.pdf")).unwrap();
    let result = to_markdown_with_objects(&scan, Format::Pdf).unwrap();
    assert!(matches!(result.objects.first(), Some(PendingObject::PdfPage { page: 1, .. })));
    assert!(result.markdown.contains("PDF page 1 · `anydoc-object-1`"));
}

#[test]
fn marked_presentation_exposes_embedded_object_payload() {
    use anydoc::model::ImageSource;
    use anydoc::{Format, PendingObject, to_markdown_with_objects};

    let bytes = std::fs::read(fixture_root().join("pptx/handmade-order.pptx")).unwrap();
    let result = to_markdown_with_objects(&bytes, Format::Pptx).unwrap();
    let (index, _) = result
        .objects
        .iter()
        .enumerate()
        .find(|(_, object)| {
            matches!(object, PendingObject::Image { source: ImageSource::Asset(id), .. }
            if result.assets[id.0].media_type == "application/vnd.ms-ole-object")
        })
        .expect("embedded object marker");
    assert!(result.markdown.contains(&format!("`anydoc-object-{}` (object)", index + 1)));
}

#[test]
fn image_heavy_text_pdf_keeps_extractable_text() {
    use anydoc::{Format, to_markdown_bytes, to_markdown_with_objects};
    use std::io::Write as _;

    // A PDF page with both selectable text and a large raster XObject can be
    // classified as `ImageBased` by `process_pdf_mem` (leaving `result.markdown`
    // as `None`), while `extract_pages_markdown_mem` extracts the text and
    // clears `pages_needing_ocr`. Both entry points must return that text.
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let mut push_obj = |out: &mut Vec<u8>, body: &[u8]| {
        let id = offsets.len() + 1;
        offsets.push(out.len());
        writeln!(out, "{id} 0 obj").unwrap();
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    let pixels = vec![128u8; 200 * 200];
    let mut img_obj = format!(
        "<< /Type /XObject /Subtype /Image /Width 200 /Height 200 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {} >>\nstream\n",
        pixels.len()
    )
    .into_bytes();
    img_obj.extend_from_slice(&pixels);
    img_obj.extend_from_slice(b"\nendstream");

    let stream = b"BT /F1 12 Tf 72 700 Td (Page one has selectable text and a figure.) Tj ET\nq 300 0 0 300 72 200 cm /Im0 Do Q\n";
    let content_obj =
        format!("<< /Length {} >>\nstream\n{}endstream", stream.len(), String::from_utf8_lossy(stream))
            .into_bytes();

    push_obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    push_obj(&mut out, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>");
    push_obj(&mut out, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    push_obj(&mut out, &img_obj);
    push_obj(
        &mut out,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> /XObject << /Im0 4 0 R >> >> /Contents 6 0 R >>",
    );
    push_obj(&mut out, &content_obj);

    let xref_offset = out.len();
    writeln!(out, "xref\n0 {}", offsets.len() + 1).unwrap();
    writeln!(out, "0000000000 65535 f ").unwrap();
    for off in &offsets {
        writeln!(out, "{off:010} 00000 n ").unwrap();
    }
    write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        offsets.len() + 1
    )
    .unwrap();

    let md = to_markdown_bytes(&out, Format::Pdf).unwrap();
    assert!(md.contains("Page one has selectable text and a figure."), "{md}");
    let marked = to_markdown_with_objects(&out, Format::Pdf).unwrap();
    assert_eq!(marked.markdown, md);
    assert!(marked.objects.is_empty());
}

/// Embedded object payloads land in `Document::assets` with their identity
/// and media type (the Markdown output shows only the alt text).
#[test]
fn embedded_ole_payload_is_retained() {
    let path = fixture_root().join("pptx").join("handmade-order.pptx");
    let bytes = std::fs::read(&path).unwrap();
    let doc = anydoc::to_document(&bytes, anydoc::Format::Pptx).unwrap();
    let ole = doc
        .assets
        .iter()
        .find(|a| a.media_type == "application/vnd.ms-ole-object")
        .expect("OLE payload retained as an asset");
    assert_eq!(ole.bytes, b"OLE-PAYLOAD-STAND-IN".repeat(4));
}

/// Standard Word OLE markup places a VML preview image next to the
/// `o:OLEObject`; the object payload (not the preview) must be retained,
/// and the companion preview image is also kept in the asset store.
#[test]
fn docx_ole_payload_wins_over_its_preview_image() {
    let path = fixture_root().join("docx").join("handmade-ole.docx");
    let bytes = std::fs::read(&path).unwrap();
    let doc = anydoc::to_document(&bytes, anydoc::Format::Docx).unwrap();
    let ole = doc
        .assets
        .iter()
        .find(|a| a.media_type == "application/vnd.ms-ole-object")
        .expect("OLE payload retained as an asset");
    assert_eq!(ole.bytes, b"DOCX-OLE-PAYLOAD".repeat(4));
    assert!(
        doc.assets.iter().any(|a| a.media_type.starts_with("image/")),
        "companion VML preview image is also retained in assets: {:?}",
        doc.assets
    );
}

/// Even when a valid multi-page PDF contains zero extractable text and no
/// pre-flagged OCR pages (e.g. blank or unflagged raster pages),
/// `to_markdown_with_objects` emits page markers instead of failing.
#[test]
fn blank_or_image_only_pdf_emits_page_markers_in_never_stop_mode() {
    use anydoc::{Format, PendingObject, to_markdown_with_objects};
    use std::io::Write as _;

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let mut push_obj = |out: &mut Vec<u8>, body: &[u8]| {
        let id = offsets.len() + 1;
        offsets.push(out.len());
        writeln!(out, "{id} 0 obj").unwrap();
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    push_obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    push_obj(&mut out, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
    push_obj(
        &mut out,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>",
    );
    push_obj(
        &mut out,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>",
    );
    push_obj(&mut out, b"<< /Length 0 >>\nstream\n\nendstream");

    let xref_offset = out.len();
    writeln!(out, "xref\n0 {}", offsets.len() + 1).unwrap();
    writeln!(out, "0000000000 65535 f ").unwrap();
    for off in &offsets {
        writeln!(out, "{off:010} 00000 n ").unwrap();
    }
    write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        offsets.len() + 1
    )
    .unwrap();

    let marked = to_markdown_with_objects(&out, Format::Pdf).unwrap();
    assert_eq!(marked.objects.len(), 2);
    assert!(matches!(marked.objects[0], PendingObject::PdfPage { page: 1, .. }));
    assert!(matches!(marked.objects[1], PendingObject::PdfPage { page: 2, .. }));
    assert!(marked.markdown.contains("PDF page 1 · `anydoc-object-1`"));
    assert!(marked.markdown.contains("PDF page 2 · `anydoc-object-2`"));
}

/// Repeated references to one part must neither re-decompress against the
/// archive budget nor duplicate the retained asset (S12).
#[test]
fn repeated_part_references_convert_and_dedupe() {
    let path = fixture_root().join("docx").join("handmade-manyrefs.docx");
    let bytes = std::fs::read(&path).unwrap();
    let doc = anydoc::to_document(&bytes, anydoc::Format::Docx).unwrap();
    assert_eq!(doc.assets.len(), 1, "one shared asset for seventy references");
}

/// The binary DOC inline picture (sprmCPicLocation -> PICF + OfficeArt in
/// the Data stream) is retained as an asset.
#[test]
fn doc_inline_picture_is_retained() {
    let bytes = std::fs::read(fixture_root().join("doc").join("text.doc")).unwrap();
    let doc = anydoc::to_document(&bytes, anydoc::Format::Doc).unwrap();
    assert!(
        doc.assets.iter().any(|a| a.media_type.starts_with("image/")),
        "expected an extracted picture asset, got {:?}",
        doc.assets.iter().map(|a| (&a.media_type, a.bytes.len())).collect::<Vec<_>>()
    );
}

/// The RTF `\pict` payload is retained as an asset (the Markdown output
/// shows only the alt text, which is empty for pictures without one).
#[test]
fn rtf_inline_picture_is_retained() {
    let bytes = std::fs::read(fixture_root().join("rtf").join("text.rtf")).unwrap();
    let doc = anydoc::to_document(&bytes, anydoc::Format::Rtf).unwrap();
    let png = doc.assets.iter().find(|a| a.media_type == "image/png");
    let png = png.expect("png pict retained as an asset");
    assert!(png.bytes.starts_with(&[0x89, b'P', b'N', b'G']), "payload decodes from hex");
}

/// Resource-abuse fixtures must hard-fail with `ResourceLimit` - the one
/// class of malformed input that never converts.
#[test]
fn abuse_fixtures_hard_fail() {
    let root = fixture_root().join("abuse");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(!files.is_empty(), "abuse fixture set is missing");
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let result = anydoc::to_markdown(&path);
        assert!(
            matches!(result, Err(anydoc::ConvertError::ResourceLimit { .. })),
            "{name}: expected a ResourceLimit error, got {result:?}"
        );
    }
}

/// Local-only sweep over the ~100 real-world files in `samples/` (gitignored).
/// Snapshots a compact digest (length + hash + head) per file so drift is
/// detected without committing megabytes of output. Run with:
/// `cargo test --test snapshots -- --ignored`
#[test]
#[ignore = "requires the local gitignored samples/ directory"]
fn samples_sweep() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    if !root.is_dir() {
        eprintln!("samples/ not present; skipping sweep");
        return;
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    for path in files {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("pdf") {
            continue; // pdf is an output target, not an input format
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let output = convert(&path);
        let digest = digest(&output);
        insta::assert_snapshot!(format!("sample__{name}"), digest);
    }
}

fn digest(output: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash: String =
        Sha256::digest(output.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let mut s = String::new();
    writeln!(s, "chars: {}", output.chars().count()).unwrap();
    writeln!(s, "sha256: {hash}").unwrap();
    writeln!(s, "--- head ---").unwrap();
    let head: String = output.chars().take(2000).collect();
    s.push_str(&head);
    s
}
