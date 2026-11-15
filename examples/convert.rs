//! Convert a document to Markdown (with optional never-stop object markers,
//! extracted assets, and a JSON manifest for local AI workflows):
//! `cargo run --example convert -- <file> [-f csv] [-o out.md] [--assets dir] [--with-objects] [--manifest objects.json] [--strict]`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anydoc::model::{Asset, ImageSource};
use anydoc::{ConvertError, Format, MarkdownWithObjects, PendingObject};

const USAGE: &str = "\
usage: convert <file> [options]

options:
  -f, --format <ext>       Explicit input format (e.g. csv, docx, pdf)
  -o, --output <file.md>   Write Markdown to file instead of stdout
  --assets <dir>           Directory to write extracted images & embedded objects
  -w, --with-objects       Emit `anydoc-object-N` markers for PDF OCR pages, images, and objects
  --manifest <file.json>   Write a JSON manifest of all marked objects (implies --with-objects)
  --strict                 Fail on PDFs needing OCR instead of falling back to page markers";

struct CliOptions {
    input: PathBuf,
    output: Option<PathBuf>,
    format: Option<Format>,
    assets: Option<PathBuf>,
    manifest: Option<PathBuf>,
    with_objects: bool,
    strict: bool,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut format: Option<Format> = None;
    let mut assets: Option<PathBuf> = None;
    let mut manifest: Option<PathBuf> = None;
    let mut with_objects = false;
    let mut strict = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-o" | "--output" => {
                i += 1;
                output = args.get(i).map(PathBuf::from);
            }
            "-f" | "--format" => {
                i += 1;
                let Some(name) = args.get(i) else { break };
                let Some(named) = Format::from_extension(name) else {
                    eprintln!("error: unknown format: {name}");
                    return ExitCode::FAILURE;
                };
                format = Some(named);
            }
            "--assets" => {
                i += 1;
                assets = args.get(i).map(PathBuf::from);
            }
            "--manifest" => {
                i += 1;
                manifest = args.get(i).map(PathBuf::from);
                with_objects = true;
            }
            "-w" | "--with-objects" => {
                with_objects = true;
            }
            "--strict" => {
                strict = true;
            }
            other => input = Some(PathBuf::from(other)),
        }
        i += 1;
    }
    let Some(input) = input else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let opts = CliOptions { input, output, format, assets, manifest, with_objects, strict };
    match run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(opts: &CliOptions) -> Result<(), ConvertError> {
    let bytes = std::fs::read(&opts.input)?;
    let resolved_format = opts
        .format
        .or_else(|| Format::from_bytes(&bytes))
        .or_else(|| Format::from_path(&opts.input));

    let start = std::time::Instant::now();
    let result = match resolved_format {
        Some(format) => convert_known_format(&bytes, format, opts.with_objects, opts.strict)?,
        None if !opts.strict && looks_like_plain_text(&opts.input, &bytes) => {
            eprintln!(
                "note: {} is plain text; passing through directly",
                opts.input.display()
            );
            let mut markdown = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            if !markdown.ends_with('\n') {
                markdown.push('\n');
            }
            MarkdownWithObjects { markdown, objects: Vec::new(), assets: Vec::new() }
        }
        None => {
            return Err(ConvertError::Unsupported(format!(
                "unrecognized file content and extension: {}",
                opts.input.display()
            )));
        }
    };
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "converted {} in {} ({} marked object(s), {} embedded asset(s))",
        opts.input.display(),
        millis(elapsed),
        result.objects.len(),
        result.assets.len()
    );

    match opts.output.as_deref() {
        Some(out) => {
            if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(out, &result.markdown)?;
        }
        None => {
            use std::io::Write;
            let _ = std::io::stdout().write_all(result.markdown.as_bytes());
        }
    }

    let mut written_assets: HashMap<usize, PathBuf> = HashMap::new();
    if let Some(dir) = opts.assets.as_deref() {
        let assets = if !result.assets.is_empty() || opts.with_objects {
            result.assets.clone()
        } else if let Some(format) = resolved_format.filter(|f| *f != Format::Pdf) {
            anydoc::to_document(&bytes, format)?.assets
        } else {
            Vec::new()
        };
        std::fs::create_dir_all(dir)?;
        let stem = opts.input.file_stem().unwrap_or_default().to_string_lossy();
        for asset in &assets {
            let ext = asset_extension(asset);
            let path = dir.join(format!("{stem}-{}.{ext}", asset.id.0));
            std::fs::write(&path, &asset.bytes)?;
            written_assets.insert(asset.id.0, path);
        }
        eprintln!("wrote {} assets to {}", assets.len(), dir.display());
    }

    if let Some(manifest_path) = opts.manifest.as_deref() {
        if let Some(parent) = manifest_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let json = build_manifest_json(&opts.input, &result, &written_assets);
        std::fs::write(manifest_path, json)?;
        eprintln!("wrote object manifest to {}", manifest_path.display());
    }

    Ok(())
}

fn convert_known_format(
    bytes: &[u8],
    format: Format,
    with_objects: bool,
    strict: bool,
) -> Result<MarkdownWithObjects, ConvertError> {
    if with_objects {
        return anydoc::to_markdown_with_objects(bytes, format);
    }
    match anydoc::to_markdown_bytes(bytes, format) {
        Ok(markdown) => Ok(MarkdownWithObjects { markdown, objects: Vec::new(), assets: Vec::new() }),
        Err(ConvertError::NeedsOcr { pages, page_count }) if !strict => {
            eprintln!(
                "note: PDF has {} of {page_count} page(s) needing OCR ({pages:?}); continuing in never-stop mode with page markers",
                pages.len()
            );
            anydoc::to_markdown_with_objects(bytes, format)
        }
        Err(ConvertError::Unsupported(msg))
            if !strict && format == Format::Pdf && msg.starts_with("PDF has no extractable text") =>
        {
            eprintln!("note: {msg}; continuing in never-stop mode with page markers");
            anydoc::to_markdown_with_objects(bytes, format)
        }
        Err(e) => Err(e),
    }
}

fn looks_like_plain_text(path: &Path, bytes: &[u8]) -> bool {
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "txt" | "md" | "markdown" | "rst" | "log" | "json" | "yaml" | "yml" | "toml" | "tsv"
            )
        });
    ext_ok && !bytes.contains(&0)
}

fn asset_extension(asset: &Asset) -> String {
    let (kind, subtype) = asset.media_type.split_once('/').unwrap_or(("", ""));
    match (kind, subtype) {
        ("image", "jpeg") => "jpg".to_string(),
        ("image", "svg+xml") => "svg".to_string(),
        ("image", "x-emf") => "emf".to_string(),
        ("image", "x-wmf") => "wmf".to_string(),
        ("image", "x-pict") => "pct".to_string(),
        ("image", s) => s.chars().filter(char::is_ascii_alphanumeric).collect(),
        _ => "bin".to_string(),
    }
}

fn build_manifest_json(
    input: &Path,
    result: &MarkdownWithObjects,
    written_assets: &HashMap<usize, PathBuf>,
) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!(
        "  \"source_document\": {},\n  \"object_count\": {},\n  \"asset_count\": {},\n  \"objects\": [\n",
        json_str(&input.display().to_string()),
        result.objects.len(),
        result.assets.len()
    ));
    for (idx, obj) in result.objects.iter().enumerate() {
        let number = idx + 1;
        let marker = format!("anydoc-object-{number}");
        let comma = if number < result.objects.len() { "," } else { "" };
        match obj {
            PendingObject::PdfPage { page, reason } => {
                let reason_json = match reason {
                    Some(r) => json_str(r),
                    None => "null".to_string(),
                };
                out.push_str(&format!(
                    "    {{\n      \"id\": {number},\n      \"marker\": {},\n      \"kind\": \"pdf_page\",\n      \"page\": {page},\n      \"reason\": {reason_json},\n      \"recommended_action\": \"ocr_page\"\n    }}{comma}\n",
                    json_str(&marker),
                ));
            }
            PendingObject::Image { alt, source } => {
                let (source_kind, media_type, source_path, extracted_file, action) = match source {
                    ImageSource::Asset(id) => {
                        let asset = &result.assets[id.0];
                        let extracted = written_assets
                            .get(&id.0)
                            .map(|p| json_str(&p.display().to_string()))
                            .unwrap_or_else(|| "null".to_string());
                        let is_ole = asset.media_type == "application/vnd.ms-ole-object";
                        let action = if is_ole {
                            "inspect_embedded_object"
                        } else if alt.trim().is_empty() {
                            "ocr_or_describe_image"
                        } else {
                            "describe_image"
                        };
                        (
                            if is_ole { "embedded_object" } else { "embedded_image" },
                            json_str(&asset.media_type),
                            if asset.origin_part.is_empty() {
                                "null".to_string()
                            } else {
                                json_str(&asset.origin_part)
                            },
                            extracted,
                            action,
                        )
                    }
                    ImageSource::External(url) => (
                        "external_image",
                        "null".to_string(),
                        json_str(url),
                        "null".to_string(),
                        "fetch_or_link_external_image",
                    ),
                    ImageSource::Unavailable => (
                        "unavailable_image",
                        "null".to_string(),
                        "null".to_string(),
                        "null".to_string(),
                        "keep_alt_text",
                    ),
                };
                out.push_str(&format!(
                    "    {{\n      \"id\": {number},\n      \"marker\": {},\n      \"kind\": {},\n      \"alt\": {},\n      \"media_type\": {media_type},\n      \"source_path\": {source_path},\n      \"extracted_file\": {extracted_file},\n      \"recommended_action\": {}\n    }}{comma}\n",
                    json_str(&marker),
                    json_str(source_kind),
                    json_str(alt),
                    json_str(action),
                ));
            }
        }
    }
    out.push_str("  ]\n}\n");
    out
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn millis(ms: f64) -> String {
    if ms < 10.0 { format!("{ms:.2}ms") } else { format!("{ms:.0}ms") }
}
