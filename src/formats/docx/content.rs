//! Block and inline walking for WordprocessingML parts.

use crate::error::ConvertError;
use crate::formats::docx::numbering::{Counters, Numbering};
use crate::formats::docx::styles::{Styles, on_off, rpr_delta};
use crate::model::{
    Block, Cell, GridBuilder, ImageSource, Inline, LinkTarget, Style, TableKind, inlines_are_empty,
};
use crate::package::Package;
use crate::package::relationships::{RelTarget, Relationships, TargetMode, rel_target_bytes};
use crate::package::xml::{Element, ns};
use crate::shared::blockstyle::{BlockStyle, StyledRun};
use crate::shared::delta::rebase_emphasis;
use crate::shared::fields::{FieldFrame, field_result};
use crate::shared::header::resolve_header_rows;
use crate::shared::list::{ListEntry, ListKey, flush_list};
use crate::shared::math::{omath_para_to_tex, omath_to_tex};
use crate::shared::text::clean_text;
use std::cell::RefCell;
use std::collections::HashMap;

/// Namespaces whose markup this frontend understands; `mc:Choice` branches
/// requiring anything else fall back to `mc:Fallback`.
const SUPPORTED_NS: &[&str] = &[
    ns::W,
    ns::A,
    ns::PIC,
    ns::WP,
    ns::MC,
    ns::CHART,
    ns::DGM,
    ns::VML,
    ns::O_VML,
    ns::WPS,
    ns::WPG,
];

use crate::shared::assets::AssetSink;

pub(super) struct Ctx<'a, 'b> {
    pub pkg: &'b RefCell<Package<'a>>,
    pub rels: Relationships,
    pub base_part: String,
    pub styles: &'b Styles<'b>,
    pub numbering: &'b Numbering,
    pub counters: &'b RefCell<Counters>,
    pub assets: &'b RefCell<AssetSink>,
}

impl<'a, 'b> Ctx<'a, 'b> {
    /// The same document-wide dependencies scoped to another package part
    /// (notes parts): only the relationships and base path change.
    pub fn for_part(&self, rels: Relationships, base_part: String) -> Ctx<'a, 'b> {
        Ctx {
            pkg: self.pkg,
            rels,
            base_part,
            styles: self.styles,
            numbering: self.numbering,
            counters: self.counters,
            assets: self.assets,
        }
    }

    /// Load an internal relationship target's bytes, resolved against this
    /// part. Failures degrade (log + `None`) per the unified policy;
    /// resource-limit errors always propagate.
    fn rel_part(&self, rel_id: &str) -> Result<Option<RelTarget>, ConvertError> {
        rel_target_bytes(self.pkg, &self.rels, &self.base_part, rel_id)
    }

    fn add_asset(
        &self,
        media_type: String,
        part: String,
        bytes: &[u8],
    ) -> Result<crate::model::AssetId, ConvertError> {
        self.assets.borrow_mut().add(media_type, part, bytes)
    }

    /// Pick the branch of an `mc:AlternateContent` to process.
    fn alternate_branch<'e>(&self, alt: &'e Element) -> Option<&'e Element> {
        crate::shared::mc::alternate_branch(alt, SUPPORTED_NS)
    }
}

pub(super) enum ParaKind {
    Heading {
        level: u8,
        /// The visible number of a numbered heading (with its trailing
        /// separator), prepended to the content: headings have no native
        /// numbering in the output.
        label: Option<String>,
        /// The heading style's own emphasis, subtracted from its runs.
        base: Style,
    },
    ListItem {
        ilvl: usize,
        key: ListKey,
        number: u64,
        label: Option<String>,
    },
    /// A paragraph whose style names a block container.
    Styled(BlockStyle),
    Plain,
}

/// Block runs a following paragraph may extend: a list being built, and a
/// styled container. Only one is ever open, so starting either closes the
/// other.
#[derive(Default)]
pub(super) struct Runs {
    list: Vec<ListEntry>,
    styled: StyledRun,
}

impl Runs {
    fn flush(&mut self, blocks: &mut Vec<Block>) {
        self.styled.flush(blocks);
        flush_list(blocks, &mut self.list);
    }
}

/// A paragraph's content in source order: inline runs interleaved with
/// block attachments (text boxes, charts) at their anchor positions.
pub(super) enum Piece {
    Inlines(Vec<Inline>),
    Blocks(Vec<Block>),
}

pub(super) fn parse_blocks(parent: &Element, ctx: &Ctx) -> Result<Vec<Block>, ConvertError> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut runs = Runs::default();
    collect_blocks(parent, ctx, &mut blocks, &mut runs)?;
    runs.flush(&mut blocks);
    Ok(blocks)
}

fn collect_blocks(
    parent: &Element,
    ctx: &Ctx,
    blocks: &mut Vec<Block>,
    runs: &mut Runs,
) -> Result<(), ConvertError> {
    for child in parent.child_elems() {
        if child.is(ns::MC, "AlternateContent") {
            if let Some(branch) = ctx.alternate_branch(child) {
                collect_blocks(branch, ctx, blocks, runs)?;
            }
            continue;
        }
        if child.is(ns::M, "oMathPara") || child.is(ns::M, "oMath") {
            runs.flush(blocks);
            blocks.extend(omath_para_to_tex(child).into_iter().map(Block::Math));
            continue;
        }
        if child.ns.as_deref().is_none_or(|n| n != ns::W) {
            continue;
        }
        match child.local.as_str() {
            "p" => {
                let (kind, pieces) = parse_paragraph(child, ctx)?;
                emit_paragraph(kind, pieces, blocks, runs);
            }
            "tbl" => {
                runs.flush(blocks);
                blocks.extend(parse_table(child, ctx)?);
            }
            "sdt" => {
                if let Some(content) = child.find(ns::W, "sdtContent") {
                    collect_blocks(content, ctx, blocks, runs)?;
                }
            }
            "customXml" => collect_blocks(child, ctx, blocks, runs)?,
            _ => {}
        }
    }
    Ok(())
}

fn emit_paragraph(kind: ParaKind, pieces: Vec<Piece>, blocks: &mut Vec<Block>, runs: &mut Runs) {
    match kind {
        ParaKind::ListItem { ilvl, key, number, label } => {
            runs.styled.flush(blocks);
            let item = pieces_into_blocks(pieces);
            runs.list.push(ListEntry { level: ilvl, key, number, label, blocks: item });
        }
        ParaKind::Styled(style) => {
            flush_list(blocks, &mut runs.list);
            for piece in pieces {
                match piece {
                    Piece::Inlines(inlines) => runs.styled.push(style, inlines, blocks),
                    Piece::Blocks(attachments) => {
                        runs.styled.flush(blocks);
                        blocks.extend(attachments);
                    }
                }
            }
        }
        ParaKind::Heading { level, label, base } => {
            runs.flush(blocks);
            let mut label = label;
            let mut emitted_heading = false;
            for piece in pieces {
                match piece {
                    Piece::Inlines(mut content) if !inlines_are_empty(&content) => {
                        rebase_emphasis(&mut content, base);
                        if !emitted_heading {
                            if let Some(label) = label.take() {
                                content
                                    .insert(0, Inline::Text { text: label, style: Style::PLAIN });
                            }
                            blocks.push(Block::Heading { level, anchor: None, content });
                            emitted_heading = true;
                        } else {
                            // Attachments cannot be nested in a heading, so
                            // subsequent text becomes a paragraph.
                            blocks.push(Block::Paragraph(content));
                        }
                    }
                    Piece::Inlines(_) => {}
                    Piece::Blocks(attachments) => blocks.extend(attachments),
                }
            }
        }
        ParaKind::Plain => {
            runs.flush(blocks);
            blocks.extend(pieces_into_blocks(pieces));
        }
    }
}

fn parse_paragraph(p: &Element, ctx: &Ctx) -> Result<(ParaKind, Vec<Piece>), ConvertError> {
    let ppr = p.find(ns::W, "pPr");
    let pstyle_id = ppr.and_then(|pr| pr.find(ns::W, "pStyle")).and_then(|e| e.attr(ns::W, "val"));

    // Direct paragraph properties overlay the style chain: an explicit
    // outlineLvl of 9 ("no outline level") turns a style heading off.
    let direct_outline: Option<Option<u8>> = ppr
        .and_then(|pr| pr.find(ns::W, "outlineLvl"))
        .and_then(|e| e.attr(ns::W, "val"))
        .and_then(|v| v.parse::<u8>().ok())
        .map(|l| if l < 9 { Some(l + 1) } else { None });
    let style_heading = match pstyle_id {
        Some(id) => ctx.styles.heading_level(id)?,
        None => None,
    };
    let heading = direct_outline.or(style_heading).flatten();
    let style_block = match pstyle_id {
        Some(id) => ctx.styles.block_style(id)?,
        None => None,
    };

    // Numbering resolves independently of heading semantics: a numbered
    // heading advances its sequence and keeps its number visible.
    let numbering = resolve_numbering(ppr, pstyle_id, ctx)?;

    // Toggle properties: the paragraph style chain's true-parity flips the
    // docDefaults base. Headings use the same resolution as body text.
    let parity = match pstyle_id {
        Some(id) => ctx.styles.run_toggles(id)?,
        None => Default::default(),
    };
    let paragraph_level = parity.apply_over(ctx.styles.doc_defaults);

    let kind = match heading {
        Some(level) => {
            let label = match &numbering {
                Some((_, key, number, label)) if key.marker.ordered() => {
                    Some(format!("{} ", label.clone().unwrap_or_else(|| key.marker.label(*number))))
                }
                _ => None,
            };
            ParaKind::Heading { level, label, base: paragraph_level }
        }
        None => match numbering {
            Some((ilvl, key, number, label)) => ParaKind::ListItem { ilvl, key, number, label },
            None => match style_block {
                Some(style) => ParaKind::Styled(style),
                None => ParaKind::Plain,
            },
        },
    };

    let mut walker = InlineWalker::new(ctx, paragraph_level);
    walker.walk(p)?;
    Ok((kind, walker.finish()))
}

/// Resolve a paragraph's effective numbering per ECMA-376: the direct
/// `numPr` children are tri-state and merge property-by-property with the
/// style-inherited `numPr` (a missing `numId`/`ilvl` inherits; an explicit
/// `numId` of 0 suppresses). Returns the level, list identity, effective
/// number, and composite label, advancing the instance counters.
#[expect(clippy::type_complexity)]
fn resolve_numbering(
    ppr: Option<&Element>,
    pstyle_id: Option<&str>,
    ctx: &Ctx,
) -> Result<Option<(usize, ListKey, u64, Option<String>)>, ConvertError> {
    let direct = ppr.and_then(|pr| pr.find(ns::W, "numPr"));
    let direct_num_id: Option<u64> = direct
        .and_then(|numpr| numpr.find(ns::W, "numId"))
        .and_then(|e| e.attr(ns::W, "val"))
        .and_then(|v| v.parse().ok());
    let direct_ilvl: Option<usize> = direct
        .and_then(|numpr| numpr.find(ns::W, "ilvl"))
        .and_then(|e| e.attr(ns::W, "val"))
        .and_then(|v| v.parse().ok());

    let num_id = match direct_num_id {
        Some(id) => Some(id),
        None => match pstyle_id {
            Some(id) => ctx.styles.style_num_pr(id)?,
            None => None,
        },
    };
    let Some(num_id) = num_id else {
        return Ok(None);
    };
    if num_id == 0 {
        // numId 0 is explicitly suppressed numbering.
        return Ok(None);
    }
    let Some(instance) = ctx.numbering.instance(num_id) else {
        log::debug!("paragraph references undefined numbering instance {num_id}");
        return Ok(None);
    };
    let ilvl = match (direct_ilvl, pstyle_id) {
        (Some(l), _) => l,
        // Style-referenced numbering carries no usable ilvl (§17.3.1.19);
        // the level comes from the abstract levels' pStyle bindings.
        (None, Some(id)) => ctx.styles.style_numbering_level(id, instance)?.unwrap_or(0),
        (None, None) => 0,
    };
    let def = &instance.levels[ilvl.min(crate::formats::docx::numbering::LEVELS - 1)];
    // A numFmt of "none" is explicitly suppressed numbering.
    let Some(marker) = def.marker else {
        return Ok(None);
    };
    let (number, label) = if marker.ordered() {
        ctx.counters.borrow_mut().next(num_id, ilvl, instance)
    } else {
        (0, None)
    };
    Ok(Some((ilvl, ListKey { instance: num_id, marker }, number, label)))
}

struct InlineWalker<'a, 'b, 'e> {
    ctx: &'e Ctx<'a, 'b>,
    base: Style,
    pieces: Vec<Piece>,
    current: Vec<Inline>,
    fields: Vec<FieldFrame>,
}

impl<'a, 'b, 'e> InlineWalker<'a, 'b, 'e> {
    fn new(ctx: &'e Ctx<'a, 'b>, base: Style) -> Self {
        InlineWalker { ctx, base, pieces: Vec::new(), current: Vec::new(), fields: Vec::new() }
    }

    fn push(&mut self, inline: Inline) {
        match self.fields.last_mut() {
            Some(f) if f.in_result => f.inlines.push(inline),
            Some(_) => {}
            None => self.current.push(inline),
        }
    }

    /// Attach block content at the current position in run order.
    fn push_blocks(&mut self, blocks: Vec<Block>) {
        if blocks.is_empty() {
            return;
        }
        if !self.current.is_empty() {
            self.pieces.push(Piece::Inlines(std::mem::take(&mut self.current)));
        }
        self.pieces.push(Piece::Blocks(blocks));
    }

    fn walk(&mut self, elem: &Element) -> Result<(), ConvertError> {
        for child in elem.child_elems() {
            if child.is(ns::MC, "AlternateContent") {
                if let Some(branch) = self.ctx.alternate_branch(child) {
                    self.walk(branch)?;
                }
                continue;
            }
            if child.is(ns::M, "oMathPara") {
                // A math paragraph is displayed on its own line.
                self.push_blocks(omath_para_to_tex(child).into_iter().map(Block::Math).collect());
                continue;
            }
            if child.is(ns::M, "oMath") {
                let tex = omath_to_tex(child);
                if !tex.is_empty() {
                    self.push(Inline::Math(tex));
                }
                continue;
            }
            if child.ns.as_deref().is_none_or(|n| n != ns::W) {
                continue;
            }
            match child.local.as_str() {
                "pPr" => {}
                "r" => self.walk_run(child)?,
                "hyperlink" => {
                    let target = self.hyperlink_link_target(child);
                    let mut inner = InlineWalker::new(self.ctx, self.base);
                    inner.walk(child)?;
                    let (content, attachments) = split_pieces(inner.finish());
                    if let Some(target) = target {
                        // An empty label still keeps a resolved target: the
                        // renderer shows the URL as the link text.
                        self.push(Inline::Link { content, target });
                    } else {
                        for inline in content {
                            self.push(inline);
                        }
                    }
                    self.push_blocks(attachments);
                }
                "fldSimple" => {
                    let instr = child.attr(ns::W, "instr").unwrap_or("").to_string();
                    let mut inner = InlineWalker::new(self.ctx, self.base);
                    inner.walk(child)?;
                    let (content, attachments) = split_pieces(inner.finish());
                    self.push_field_result(&instr, content);
                    self.push_blocks(attachments);
                }
                "bookmarkStart" => {
                    if let Some(name) = child.attr(ns::W, "name")
                        && name != "_GoBack"
                    {
                        self.push(Inline::Anchor(name.to_string()));
                    }
                }
                "commentReference" => {
                    if let Some(id) = child.attr(ns::W, "id") {
                        self.push(Inline::NoteRef(format!("cm{id}")));
                    }
                }
                "sdt" => {
                    if let Some(content) = child.find(ns::W, "sdtContent") {
                        self.walk(content)?;
                    }
                }
                // `moveTo` is moved-in text — part of the final document;
                // `customXml` wraps ordinary run content.
                "smartTag" | "ins" | "bdo" | "dir" | "moveTo" | "customXml" => self.walk(child)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn hyperlink_link_target(&self, link: &Element) -> Option<LinkTarget> {
        if let Some(id) = link.attr_qualified(ns::R, "id")
            && let Some(rel) = self.ctx.rels.get(id)
        {
            return Some(crate::shared::fields::classify_rel_target(
                rel.mode == TargetMode::External,
                &rel.target,
            ));
        }
        link.attr(ns::W, "anchor").map(|a| LinkTarget::Anchor(a.to_string()))
    }

    fn walk_run(&mut self, run: &Element) -> Result<(), ConvertError> {
        let style = match run.find(ns::W, "rPr") {
            Some(rpr) => {
                // Character-style chain: another toggle layer over the
                // paragraph-level value. Direct formatting is absolute.
                let char_parity = match rpr.find(ns::W, "rStyle").and_then(|e| e.attr(ns::W, "val"))
                {
                    Some(id) => self.ctx.styles.run_toggles(id)?,
                    None => Default::default(),
                };
                let with_char = char_parity.apply_over(self.base);
                rpr_delta(rpr).apply(with_char)
            }
            None => self.base,
        };
        self.walk_run_content(run, style)
    }

    fn walk_run_content(&mut self, run: &Element, style: Style) -> Result<(), ConvertError> {
        for child in run.child_elems() {
            if child.is(ns::MC, "AlternateContent") {
                if let Some(branch) = self.ctx.alternate_branch(child) {
                    self.walk_run_content(branch, style)?;
                }
                continue;
            }
            let in_w = child.ns.as_deref().is_some_and(|n| n == ns::W);
            if !in_w {
                continue;
            }
            match child.local.as_str() {
                "t" => {
                    // Run edges carry the spacing between words in documents
                    // that never mark xml:space, and XML leaves unmarked
                    // whitespace to the application, so it is kept.
                    let text = clean_text(child.text().as_ref());
                    if !text.is_empty() {
                        self.push(Inline::Text { text, style });
                    }
                }
                "tab" | "ptab" => self.push(Inline::Text { text: " ".into(), style: Style::PLAIN }),
                // Markdown has no pages or columns, but every w:br still
                // separates the runs around it: dropping a page break
                // outright would join the words on either side. One left at
                // the end of a paragraph is trimmed when the block renders.
                "br" => self.push(Inline::LineBreak),
                "cr" => self.push(Inline::LineBreak),
                "footnoteReference" => {
                    if let Some(id) = child.attr(ns::W, "id") {
                        self.push(Inline::NoteRef(format!("fn{id}")));
                    }
                }
                "endnoteReference" => {
                    if let Some(id) = child.attr(ns::W, "id") {
                        self.push(Inline::NoteRef(format!("en{id}")));
                    }
                }
                "commentReference" => {
                    if let Some(id) = child.attr(ns::W, "id") {
                        self.push(Inline::NoteRef(format!("cm{id}")));
                    }
                }
                "sym" => {
                    if let Some(ch) = decode_w_sym(child) {
                        self.push(Inline::Text { text: ch.to_string(), style });
                    }
                }
                "drawing" | "pict" | "object" => self.walk_drawing(child)?,
                "fldChar" => match child.attr(ns::W, "fldCharType") {
                    Some("begin") => self.fields.push(FieldFrame::default()),
                    Some("separate") => {
                        if let Some(f) = self.fields.last_mut() {
                            f.in_result = true;
                        }
                    }
                    Some("end") => {
                        if let Some(FieldFrame { instr, inlines, .. }) = self.fields.pop() {
                            self.push_field_result(&instr, inlines);
                        }
                    }
                    _ => {}
                },
                "instrText" => {
                    if let Some(f) = self.fields.last_mut() {
                        f.instr.push_str(&child.text());
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Drawings, VML picts, and embedded objects: text boxes become block
    /// attachments at this position; images/charts/diagrams/objects resolve
    /// through relationships.
    fn walk_drawing(&mut self, elem: &Element) -> Result<(), ConvertError> {
        // Text boxes first: their content is the drawing's content.
        let mut boxes = Vec::new();
        collect_text_boxes(elem, &mut boxes);
        if !boxes.is_empty() {
            let mut blocks = Vec::new();
            for tb in boxes {
                blocks.extend(parse_blocks(tb, self.ctx)?);
            }
            self.push_blocks(blocks);
            return Ok(());
        }

        let descr = elem
            .first_descendant(ns::WP, "docPr")
            .and_then(|d| d.attr(ns::WP, "descr"))
            .map(clean_text)
            .unwrap_or_default();

        // Charts and SmartArt render their textual content.
        if let Some(chart_ref) = elem.first_descendant(ns::CHART, "chart")
            && let Some(rel_id) = chart_ref.attr_qualified(ns::R, "id")
        {
            self.push_blocks(self.chart_blocks(rel_id)?);
            return Ok(());
        }
        if let Some(rel_ids) = elem.first_descendant(ns::DGM, "relIds")
            && let Some(rel_id) = rel_ids.attr_qualified(ns::R, "dm")
        {
            self.push_blocks(self.diagram_blocks(rel_id)?);
            return Ok(());
        }

        // Embedded OLE objects come before images: standard Word OLE markup
        // carries a VML preview image next to the o:OLEObject, and the
        // object's identity and payload must win over its preview.
        if let Some(ole) = elem.first_descendant(ns::O_VML, "OLEObject") {
            let prog_id = ole.attr(ns::O_VML, "ProgID").unwrap_or("object").to_string();
            let alt = if descr.trim().is_empty() {
                format!("Embedded object: {prog_id}")
            } else {
                descr.clone()
            };
            let source = match ole.attr_qualified(ns::R, "id") {
                Some(rel_id) => match self.ctx.rel_part(rel_id)? {
                    Some((part, bytes)) => Some(ImageSource::Asset(self.ctx.add_asset(
                        "application/vnd.ms-ole-object".into(),
                        part,
                        &bytes,
                    )?)),
                    None => None,
                },
                None => None,
            };
            // Retain the companion VML preview image (`v:imagedata`) in the
            // asset store when present, and fall back to it if the OLE binary
            // stream itself is missing or external (`firecrawl/anydoc` Issue
            // #219 / PR #222).
            let preview_source = match elem
                .first_descendant(ns::VML, "imagedata")
                .and_then(|i| i.attr_qualified(ns::R, "id"))
            {
                Some(rel_id) => crate::shared::assets::rel_image_source(
                    self.ctx.pkg,
                    &self.ctx.rels,
                    &self.ctx.base_part,
                    self.ctx.assets,
                    rel_id,
                )?,
                None => None,
            };
            self.push(Inline::Image {
                alt,
                source: source.or(preview_source).unwrap_or(ImageSource::Unavailable),
            });
            return Ok(());
        }

        // Bitmap images: DrawingML blip or VML imagedata.
        let image_rel = elem
            .first_descendant(ns::A, "blip")
            .and_then(|b| {
                b.attr_qualified(ns::R, "embed").or_else(|| b.attr_qualified(ns::R, "link"))
            })
            .or_else(|| {
                elem.first_descendant(ns::VML, "imagedata")
                    .and_then(|i| i.attr_qualified(ns::R, "id"))
            });
        if let Some(rel_id) = image_rel {
            // External-mode targets (`r:link`) become external image
            // sources; embedded targets are retained as assets.
            let source = crate::shared::assets::rel_image_source(
                self.ctx.pkg,
                &self.ctx.rels,
                &self.ctx.base_part,
                self.ctx.assets,
                rel_id,
            )?;
            match source {
                Some(source) => self.push(Inline::Image { alt: descr, source }),
                None => {
                    if !descr.trim().is_empty() {
                        self.push(Inline::Image { alt: descr, source: ImageSource::Unavailable });
                    }
                }
            }
            return Ok(());
        }

        if !descr.trim().is_empty() {
            self.push(Inline::Image { alt: descr, source: ImageSource::Unavailable });
        }
        Ok(())
    }

    /// Textual extraction of a chart part via `shared::drawingml`.
    fn chart_blocks(&self, rel_id: &str) -> Result<Vec<Block>, ConvertError> {
        let Some((part, bytes)) = self.ctx.rel_part(rel_id)? else {
            return Ok(Vec::new());
        };
        match crate::package::xml::parse_xml(&bytes) {
            Ok(root) => Ok(crate::shared::drawingml::chart_blocks(&root)),
            Err(e) if e.is_fatal() => Err(e),
            Err(e) => {
                log::warn!("skipping corrupt chart part {part}: {e}");
                Ok(Vec::new())
            }
        }
    }

    /// Textual extraction of a SmartArt data part via `shared::drawingml`.
    fn diagram_blocks(&self, rel_id: &str) -> Result<Vec<Block>, ConvertError> {
        let Some((part, bytes)) = self.ctx.rel_part(rel_id)? else {
            return Ok(Vec::new());
        };
        match crate::package::xml::parse_xml(&bytes) {
            Ok(root) => Ok(crate::shared::drawingml::diagram_blocks(&root)),
            Err(e) if e.is_fatal() => Err(e),
            Err(e) => {
                log::warn!("skipping corrupt diagram part {part}: {e}");
                Ok(Vec::new())
            }
        }
    }

    fn push_field_result(&mut self, instr: &str, content: Vec<Inline>) {
        for inline in field_result(instr, content) {
            self.push(inline);
        }
    }

    fn finish(mut self) -> Vec<Piece> {
        while let Some(frame) = self.fields.pop() {
            for inline in frame.inlines {
                self.push(inline);
            }
        }
        if !self.current.is_empty() {
            self.pieces.push(Piece::Inlines(self.current));
        }
        self.pieces
    }
}

fn split_pieces(pieces: Vec<Piece>) -> (Vec<Inline>, Vec<Block>) {
    let mut inlines = Vec::new();
    let mut blocks = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Inlines(mut i) => inlines.append(&mut i),
            Piece::Blocks(b) => blocks.extend(b),
        }
    }
    (inlines, blocks)
}

fn pieces_into_blocks(pieces: Vec<Piece>) -> Vec<Block> {
    let mut blocks = Vec::new();
    for piece in pieces {
        match piece {
            // Visually empty inlines still become a paragraph: a
            // bookmark-only one carries the anchor link resolution binds to.
            Piece::Inlines(inlines) => blocks.push(Block::Paragraph(inlines)),
            Piece::Blocks(attachments) => blocks.extend(attachments),
        }
    }
    blocks
}

/// Find text-box content (`w:txbxContent`) in a drawing or VML pict, skipping
/// `mc:Fallback` so AlternateContent shapes aren't collected twice.
fn collect_text_boxes<'e>(elem: &'e Element, out: &mut Vec<&'e Element>) {
    for child in elem.child_elems() {
        if child.is(ns::MC, "Fallback") {
            continue;
        }
        if child.is(ns::W, "txbxContent") {
            out.push(child);
        } else {
            collect_text_boxes(child, out);
        }
    }
}

// ---------------------------------------------------------------------------
// Tables

struct TcInfo<'e> {
    elem: Option<&'e Element>,
    /// Legacy `hMerge` continuation cells folded into this origin.
    merged: Vec<&'e Element>,
    col_span: usize,
    row_span: usize,
    /// vMerge continuation: this position belongs to the origin above.
    covered: bool,
}

impl TcInfo<'_> {
    /// Empty single-column filler for `gridBefore`/`gridAfter` positions.
    fn filler() -> Self {
        TcInfo { elem: None, merged: Vec::new(), col_span: 1, row_span: 1, covered: false }
    }
}

/// A `w:trPr` grid filler count (`gridBefore`/`gridAfter`).
fn grid_filler(trpr: Option<&Element>, name: &str) -> usize {
    trpr.and_then(|p| p.find(ns::W, name))
        .and_then(|e| e.attr(ns::W, "val"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        .min(1000)
}

pub(super) fn parse_table(tbl: &Element, ctx: &Ctx) -> Result<Vec<Block>, ConvertError> {
    // Collect the raw cell matrix first so vertical merges can be resolved
    // into row spans before the grid is built. gridBefore/gridAfter filler
    // materializes as empty cells so every cell keeps its grid column.
    let mut matrix: Vec<Vec<TcInfo>> = Vec::new();
    for tr in tbl.find_all(ns::W, "tr") {
        let trpr = tr.find(ns::W, "trPr");
        let mut row = Vec::new();
        row.extend((0..grid_filler(trpr, "gridBefore")).map(|_| TcInfo::filler()));
        collect_row_cells(tr, &mut row);
        row.extend((0..grid_filler(trpr, "gridAfter")).map(|_| TcInfo::filler()));
        matrix.push(row);
    }
    // Active vertical-merge chains by grid column.
    let mut active: HashMap<usize, (usize, usize)> = HashMap::new();
    let rows_len = matrix.len();
    for r in 0..rows_len {
        let mut col = 0usize;
        let mut next_active: Vec<(usize, (usize, usize))> = Vec::new();
        for i in 0..matrix[r].len() {
            let (covered, span) = (matrix[r][i].covered, matrix[r][i].col_span);
            if covered {
                if let Some(&(orow, oidx)) = active.get(&col) {
                    matrix[orow][oidx].row_span += 1;
                    for c in col..col + span {
                        next_active.push((c, (orow, oidx)));
                    }
                } else {
                    matrix[r][i].covered = false; // stray continuation
                }
            } else {
                for c in col..col + span {
                    next_active.push((c, (r, i)));
                }
            }
            col += span;
        }
        active = next_active.into_iter().collect();
    }

    // tblHeader is ST_OnOff: an explicit false value is not a header row.
    let header_rows = tbl
        .find_all(ns::W, "tr")
        .take_while(|tr| tr.find(ns::W, "trPr").and_then(|p| on_off(p, "tblHeader")) == Some(true))
        .count();

    let mut builder = GridBuilder::new();
    for row in &matrix {
        builder.next_row();
        for tc in row {
            if tc.covered {
                for _ in 0..tc.col_span {
                    builder.covered();
                }
            } else {
                let mut blocks = match tc.elem {
                    Some(elem) => parse_blocks(elem, ctx)?,
                    None => Vec::new(),
                };
                for merged in &tc.merged {
                    blocks.extend(parse_blocks(merged, ctx)?);
                }
                builder.place(Cell::spanning(blocks, tc.col_span as u32, tc.row_span as u32))?;
            }
        }
    }
    let mut table = builder.finish(TableKind::Data);
    if table.grid.is_empty() {
        return Ok(Vec::new());
    }
    table.header_rows = resolve_header_rows(&table, header_rows);
    Ok(vec![Block::Table(table)])
}

fn collect_row_cells<'e>(parent: &'e Element, cells: &mut Vec<TcInfo<'e>>) {
    for child in parent.child_elems() {
        if child.ns.as_deref().is_none_or(|n| n != ns::W) {
            continue;
        }
        match child.local.as_str() {
            "tc" => {
                let tcpr = child.find(ns::W, "tcPr");
                let covered = tcpr
                    .and_then(|p| p.find(ns::W, "vMerge"))
                    .is_some_and(|v| !matches!(v.attr(ns::W, "val"), Some("restart")));
                let col_span: usize = tcpr
                    .and_then(|p| p.find(ns::W, "gridSpan"))
                    .and_then(|e| e.attr(ns::W, "val"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1)
                    .clamp(1, 1000);
                // Legacy hMerge: a non-restart hMerge cell folds into the
                // preceding origin, widening its span.
                let hmerge_cont = tcpr
                    .and_then(|p| p.find(ns::W, "hMerge"))
                    .is_some_and(|h| !matches!(h.attr(ns::W, "val"), Some("restart")));
                if hmerge_cont
                    && let Some(prev) = cells.last_mut()
                    && !prev.covered
                {
                    prev.col_span += col_span;
                    prev.merged.push(child);
                    continue;
                }
                cells.push(TcInfo {
                    elem: Some(child),
                    merged: Vec::new(),
                    col_span,
                    row_span: 1,
                    covered,
                });
            }
            "sdt" => {
                if let Some(content) = child.find(ns::W, "sdtContent") {
                    collect_row_cells(content, cells);
                }
            }
            "customXml" => collect_row_cells(child, cells),
            _ => {}
        }
    }
}

/// Decode a `<w:sym w:font="..." w:char="..."/>` glyph from legacy Symbol,
/// Wingdings, or Webdings code pages (either `0xF020..=0xF0FF` PUA or
/// `0x0020..=0x00FF`). Credit: @rahul05ranjan (`firecrawl/anydoc` Issue #193 /
/// PR #211).
fn decode_w_sym(elem: &Element) -> Option<char> {
    let raw = elem.attr(ns::W, "char")?.trim();
    let code = u32::from_str_radix(raw.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()?;
    let byte = match code {
        0xF020..=0xF0FF => (code - 0xF000) as u8,
        0x0020..=0x00FF => code as u8,
        _ => return char::from_u32(code).filter(|c| !c.is_control()),
    };
    let font = elem.attr(ns::W, "font").unwrap_or("Symbol").trim().to_ascii_lowercase();
    match font.as_str() {
        "symbol" => decode_symbol_byte(byte),
        "wingdings" => decode_wingdings_byte(byte),
        "wingdings 2" | "wingdings 3" | "webdings" => decode_dingbats_byte(&font, byte),
        _ => char::from_u32(byte as u32),
    }
}

fn decode_symbol_byte(b: u8) -> Option<char> {
    Some(match b {
        0x22 => '\u{2200}',
        0x24 => '\u{2203}',
        0x27 => '\u{220B}',
        0x2A => '\u{2217}',
        0x2D => '\u{2212}',
        0x20..=0x3F | 0x5B | 0x5D | 0x5F | 0x7B..=0x7E => b as char,
        0x40 => '\u{2245}',
        0x41 => '\u{0391}',
        0x42 => '\u{0392}',
        0x43 => '\u{03A7}',
        0x44 => '\u{0394}',
        0x45 => '\u{0395}',
        0x46 => '\u{03A6}',
        0x47 => '\u{0393}',
        0x48 => '\u{0397}',
        0x49 => '\u{0399}',
        0x4A => '\u{03D1}',
        0x4B => '\u{039A}',
        0x4C => '\u{039B}',
        0x4D => '\u{039C}',
        0x4E => '\u{039D}',
        0x4F => '\u{039F}',
        0x50 => '\u{03A0}',
        0x51 => '\u{0398}',
        0x52 => '\u{03A1}',
        0x53 => '\u{03A3}',
        0x54 => '\u{03A4}',
        0x55 => '\u{03A5}',
        0x56 => '\u{03C2}',
        0x57 => '\u{03A9}',
        0x58 => '\u{039E}',
        0x59 => '\u{03A8}',
        0x5A => '\u{0396}',
        0x5C => '\u{2234}',
        0x5E => '\u{22A5}',
        0x60 => '\u{F8E5}',
        0x61 => '\u{03B1}',
        0x62 => '\u{03B2}',
        0x63 => '\u{03C7}',
        0x64 => '\u{03B4}',
        0x65 => '\u{03B5}',
        0x66 => '\u{03C6}',
        0x67 => '\u{03B3}',
        0x68 => '\u{03B7}',
        0x69 => '\u{03B9}',
        0x6A => '\u{03D5}',
        0x6B => '\u{03BA}',
        0x6C => '\u{03BB}',
        0x6D => '\u{03BC}',
        0x6E => '\u{03BD}',
        0x6F => '\u{03BF}',
        0x70 => '\u{03C0}',
        0x71 => '\u{03B8}',
        0x72 => '\u{03C1}',
        0x73 => '\u{03C3}',
        0x74 => '\u{03C4}',
        0x75 => '\u{03C5}',
        0x76 => '\u{03D6}',
        0x77 => '\u{03C9}',
        0x78 => '\u{03BE}',
        0x79 => '\u{03C8}',
        0x7A => '\u{03B6}',
        0xA0 => '\u{20AC}',
        0xA1 => '\u{03D2}',
        0xA2 => '\u{2032}',
        0xA3 => '\u{2264}',
        0xA4 => '\u{2044}',
        0xA5 => '\u{221E}',
        0xA6 => '\u{0192}',
        0xA7 => '\u{2663}',
        0xA8 => '\u{2666}',
        0xA9 => '\u{2665}',
        0xAA => '\u{2660}',
        0xAB => '\u{2194}',
        0xAC => '\u{2190}',
        0xAD => '\u{2191}',
        0xAE => '\u{2192}',
        0xAF => '\u{2193}',
        0xB0 => '\u{00B0}',
        0xB1 => '\u{00B1}',
        0xB2 => '\u{2033}',
        0xB3 => '\u{2265}',
        0xB4 => '\u{00D7}',
        0xB5 => '\u{221D}',
        0xB6 => '\u{2202}',
        0xB7 => '\u{2022}',
        0xB8 => '\u{00F7}',
        0xB9 => '\u{2260}',
        0xBA => '\u{2261}',
        0xBB => '\u{2248}',
        0xBC => '\u{2026}',
        0xC4 => '\u{2297}',
        0xC5 => '\u{2295}',
        0xC6 => '\u{2205}',
        0xC7 => '\u{2229}',
        0xC8 => '\u{222A}',
        0xCC => '\u{2282}',
        0xCD => '\u{2286}',
        0xCE => '\u{2208}',
        0xCF => '\u{2209}',
        0xD0 => '\u{2220}',
        0xD1 => '\u{2207}',
        0xD2 | 0xE2 => '\u{00AE}',
        0xD3 | 0xE3 => '\u{00A9}',
        0xD4 | 0xE4 => '\u{2122}',
        0xD5 => '\u{220F}',
        0xD6 => '\u{221A}',
        0xD7 => '\u{22C5}',
        0xD8 => '\u{00AC}',
        0xD9 => '\u{2227}',
        0xDA => '\u{2228}',
        0xDB => '\u{21D4}',
        0xDC => '\u{21D0}',
        0xDD => '\u{21D1}',
        0xDE => '\u{21D2}',
        0xDF => '\u{21D3}',
        0xE0 => '\u{25CA}',
        0xE5 => '\u{2211}',
        0xF2 => '\u{222B}',
        _ => return None,
    })
}

fn decode_wingdings_byte(b: u8) -> Option<char> {
    Some(match b {
        0x20 => ' ',
        0x28 => '\u{260E}',
        0x2A => '\u{2709}',
        0x4A => '\u{263A}',
        0x4B => '\u{1F610}',
        0x4C => '\u{2639}',
        0x6C => '\u{25CF}',
        0x6E => '\u{25A0}',
        0x6F | 0x70 => '\u{25A1}',
        0x75 => '\u{25C6}',
        0x76 => '\u{2756}',
        0x9F | 0xA7 => '\u{2022}',
        0xA8 => '\u{25CB}',
        0xD8 => '\u{27A2}',
        0xDC => '\u{279C}',
        0xDF => '\u{2190}',
        0xE0 => '\u{2192}',
        0xE1 => '\u{2191}',
        0xE2 => '\u{2193}',
        0xE7 => '\u{2B05}',
        0xE8 => '\u{27A1}',
        0xE9 => '\u{2B06}',
        0xEA => '\u{2B07}',
        0xFB => '\u{2717}',
        0xFC => '\u{2713}',
        0xFD => '\u{2612}',
        0xFE => '\u{2611}',
        _ => return None,
    })
}

fn decode_dingbats_byte(font: &str, b: u8) -> Option<char> {
    match (font, b) {
        (_, 0x20) => Some(' '),
        ("wingdings 2", 0x50) => Some('\u{2713}'),
        ("wingdings 2", 0x52) => Some('\u{2611}'),
        ("wingdings 2", 0x53) => Some('\u{2612}'),
        ("wingdings 2", 0x97 | 0x98) => Some('\u{2022}'),
        ("wingdings 3", 0x61 | 0x66 | 0x75) => Some('\u{2190}'),
        ("wingdings 3", 0x62 | 0x67 | 0x76) => Some('\u{2192}'),
        ("wingdings 3", 0x63 | 0x68 | 0x77) => Some('\u{2191}'),
        ("wingdings 3", 0x64 | 0x69 | 0x78) => Some('\u{2193}'),
        ("webdings", 0x61) => Some('\u{2713}'),
        ("webdings", 0x72) => Some('\u{2717}'),
        ("webdings", 0x33) => Some('\u{25C0}'),
        ("webdings", 0x34) => Some('\u{25B6}'),
        ("webdings", 0x35) => Some('\u{25B2}'),
        ("webdings", 0x36) => Some('\u{25BC}'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MarkerKind;

    fn text(value: &str) -> Piece {
        Piece::Inlines(vec![Inline::plain(value)])
    }

    fn assert_text(block: &Block, expected: &str) {
        let Block::Paragraph(inlines) = block else { panic!("expected paragraph: {block:?}") };
        assert_eq!(crate::model::inlines_to_plain_text(inlines), expected);
    }

    #[test]
    fn heading_attachments_keep_source_order() {
        let mut blocks = Vec::new();
        emit_paragraph(
            ParaKind::Heading { level: 2, label: None, base: Style::PLAIN },
            vec![text("before"), Piece::Blocks(vec![Block::Rule]), text("after")],
            &mut blocks,
            &mut Runs::default(),
        );
        let [Block::Heading { content, .. }, Block::Rule, after] = &blocks[..] else {
            panic!("unexpected blocks: {blocks:?}")
        };
        assert_eq!(crate::model::inlines_to_plain_text(content), "before");
        assert_text(after, "after");
    }

    #[test]
    fn list_attachments_keep_source_order_inside_the_item() {
        let mut blocks = Vec::new();
        let mut runs = Runs::default();
        emit_paragraph(
            ParaKind::ListItem {
                ilvl: 0,
                key: ListKey { instance: 1, marker: MarkerKind::Bullet },
                number: 0,
                label: None,
            },
            vec![text("before"), Piece::Blocks(vec![Block::Rule]), text("after")],
            &mut blocks,
            &mut runs,
        );
        runs.flush(&mut blocks);
        let [Block::List(list)] = &blocks[..] else { panic!("unexpected blocks: {blocks:?}") };
        let [before, Block::Rule, after] = &list.items[0].blocks[..] else {
            panic!("unexpected item: {:?}", list.items[0].blocks)
        };
        assert_text(before, "before");
        assert_text(after, "after");
    }

    #[test]
    fn a_bookmark_only_paragraph_keeps_its_anchor() {
        // `inlines_are_empty` is true of a lone anchor, but dropping the
        // paragraph would strip the target a link resolves against.
        let mut blocks = Vec::new();
        emit_paragraph(
            ParaKind::Plain,
            vec![Piece::Inlines(vec![Inline::Anchor("mark".into())])],
            &mut blocks,
            &mut Runs::default(),
        );
        let [Block::Paragraph(inlines)] = &blocks[..] else { panic!("{blocks:?}") };
        assert!(matches!(inlines[..], [Inline::Anchor(_)]), "{inlines:?}");
    }

    #[test]
    fn an_empty_list_item_carries_no_blocks() {
        let mut blocks = Vec::new();
        let mut runs = Runs::default();
        emit_paragraph(
            ParaKind::ListItem {
                ilvl: 0,
                key: ListKey { instance: 1, marker: MarkerKind::Bullet },
                number: 0,
                label: None,
            },
            Vec::new(),
            &mut blocks,
            &mut runs,
        );
        runs.flush(&mut blocks);
        let [Block::List(list)] = &blocks[..] else { panic!("{blocks:?}") };
        assert!(list.items[0].blocks.is_empty(), "{:?}", list.items[0].blocks);
    }

    #[test]
    fn attachments_split_styled_runs_in_place() {
        let mut blocks = Vec::new();
        let mut runs = Runs::default();
        emit_paragraph(
            ParaKind::Styled(BlockStyle::Code),
            vec![text("before"), Piece::Blocks(vec![Block::Rule]), text("after")],
            &mut blocks,
            &mut runs,
        );
        runs.flush(&mut blocks);
        let [
            Block::CodeBlock { text: before, .. },
            Block::Rule,
            Block::CodeBlock { text: after, .. },
        ] = &blocks[..]
        else {
            panic!("unexpected blocks: {blocks:?}")
        };
        assert_eq!(before, "before");
        assert_eq!(after, "after");
    }

    #[test]
    fn w_sym_decodes_symbol_and_wingdings_glyphs() {
        let xml = br#"<w:r xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:sym w:font="Symbol" w:char="F0B7"/>
            <w:sym w:font="Symbol" w:char="F06D"/>
            <w:sym w:font="Wingdings" w:char="F0FC"/>
        </w:r>"#;
        let tree = crate::package::xml::parse_xml(xml).unwrap();
        let run = tree.find(ns::W, "r").unwrap();
        let glyphs: String = run.child_elems().filter_map(decode_w_sym).collect();
        assert_eq!(glyphs, "\u{2022}\u{03BC}\u{2713}");
    }
}
