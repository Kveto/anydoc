//! PDF via [pdf-inspector]: classification plus direct Markdown extraction.
//!
//! Unlike the other frontends, pdf-inspector emits Markdown itself, so PDFs
//! bypass the document model and the shared GFM writer. OCR is out of scope
//! here: a document with scanned or image-only pages errors naming them,
//! whether that is every page or one of a hundred, because output missing
//! those pages would read as complete.
//!
//! [pdf-inspector]: https://github.com/firecrawl/pdf-inspector

use crate::error::ConvertError;
use crate::{MarkdownWithObjects, PendingObject};
use pdf_inspector::PdfError;

pub fn to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    let mut result = pdf_inspector::process_pdf_mem(bytes).map_err(map_error)?;
    if !result.pages_needing_ocr.is_empty() {
        // Detection samples content streams and over-reports short or
        // image-heavy text pages; extraction knows which of them yielded none.
        let need_full_markdown = result.markdown.as_deref().is_none_or(|md| md.trim().is_empty());
        let flagged: Vec<u32> = result.pages_needing_ocr.iter().map(|page| page - 1).collect();
        let filter = (!need_full_markdown).then_some(flagged.as_slice());
        let extracted =
            pdf_inspector::extract_pages_markdown_mem(bytes, filter).map_err(map_error)?;
        if !extracted.pages_needing_ocr.is_empty() {
            return Err(ConvertError::NeedsOcr {
                pages: extracted.pages_needing_ocr,
                page_count: result.page_count,
            });
        }
        if need_full_markdown {
            let parts: Vec<&str> = extracted
                .pages
                .iter()
                .map(|p| p.markdown.trim_end())
                .filter(|md| !md.trim().is_empty())
                .collect();
            if !parts.is_empty() {
                result.markdown = Some(parts.join("\n\n"));
            }
        }
    }
    if result.has_encoding_issues {
        log::warn!("broken font encodings detected; extracted text may be garbled");
    }
    match result.markdown {
        Some(mut markdown) if !markdown.trim().is_empty() => {
            if !markdown.ends_with('\n') {
                markdown.push('\n');
            }
            Ok(markdown)
        }
        _ => Err(ConvertError::Unsupported(format!(
            "PDF has no extractable text ({:?}, {} pages)",
            result.pdf_type, result.page_count
        ))),
    }
}

fn map_error(e: PdfError) -> ConvertError {
    match e {
        PdfError::Encrypted => ConvertError::Encrypted,
        PdfError::Io(e) => ConvertError::Io(e),
        PdfError::NotAPdf(detail) => ConvertError::malformed(format!("not a PDF: {detail}")),
        PdfError::InvalidStructure => ConvertError::malformed("invalid PDF structure"),
        PdfError::Parse(detail) => ConvertError::malformed(detail),
    }
}

pub(crate) fn to_markdown_with_objects(bytes: &[u8]) -> Result<MarkdownWithObjects, ConvertError> {
    let result = pdf_inspector::process_pdf_mem(bytes).map_err(map_error)?;
    if result.has_encoding_issues {
        log::warn!("broken font encodings detected; extracted text may be garbled");
    }
    let mut objects = Vec::new();
    let markdown_empty = result.markdown.as_deref().is_none_or(|md| md.trim().is_empty());
    if !result.pages_needing_ocr.is_empty() || markdown_empty {
        let extracted = pdf_inspector::extract_pages_markdown_mem(bytes, None).map_err(map_error)?;
        if !extracted.pages_needing_ocr.is_empty() || markdown_empty {
            let mut parts = Vec::new();
            let all_pages_blank =
                extracted.pages.iter().all(|p| !p.needs_ocr && p.markdown.trim().is_empty());
            for page in extracted.pages {
                let number = page.page + 1;
                if page.needs_ocr || all_pages_blank {
                    let reason = page.ocr_reason.or_else(|| {
                        all_pages_blank
                            .then(|| format!("{:?}", result.pdf_type).to_ascii_lowercase())
                    });
                    objects.push(PendingObject::PdfPage { page: number, reason });
                    parts.push(format!(
                        "> Pending OCR · PDF page {number} · `anydoc-object-{}`",
                        objects.len()
                    ));
                } else if !page.markdown.trim().is_empty() {
                    parts.push(page.markdown.trim_end().to_owned());
                }
            }
            if parts.is_empty() && result.page_count > 0 {
                for number in 1..=result.page_count {
                    objects.push(PendingObject::PdfPage {
                        page: number,
                        reason: Some(format!("{:?}", result.pdf_type).to_ascii_lowercase()),
                    });
                    parts.push(format!(
                        "> Pending OCR · PDF page {number} · `anydoc-object-{}`",
                        objects.len()
                    ));
                }
            }
            if parts.is_empty() {
                return Err(ConvertError::Unsupported(format!(
                    "PDF has no extractable text ({:?}, {} pages)",
                    result.pdf_type, result.page_count
                )));
            }
            let markdown = parts.join("\n\n") + "\n";
            return Ok(MarkdownWithObjects { markdown, objects, assets: Vec::new() });
        }
    }
    match result.markdown {
        Some(mut markdown) if !markdown.trim().is_empty() => {
            if !markdown.ends_with('\n') {
                markdown.push('\n');
            }
            Ok(MarkdownWithObjects { markdown, objects, assets: Vec::new() })
        }
        _ => Err(ConvertError::Unsupported(format!(
            "PDF has no extractable text ({:?}, {} pages)",
            result.pdf_type, result.page_count
        ))),
    }
}
