//! Textual extraction of DrawingML rich objects shared by DOCX and PPTX:
//! charts (title, axis titles, cached series data) and SmartArt diagram
//! data (text points).

use crate::model::{Block, Cell, Inline, List, ListItem, Style, Table, TableKind};
use crate::package::xml::{Element, ns};
use crate::shared::text::clean_text;

const MAX_CHART_POINTS: usize = 10_000;

/// Extract cached chart points aligned by `<c:pt idx="N">` and `<c:ptCount>`.
/// Credit: addresses `firecrawl/anydoc` Issue #220 / PR #221.
fn chart_points(container: &Element) -> Vec<String> {
    let pts: Vec<&Element> = container.descendants(ns::CHART, "pt").collect();
    if pts.is_empty() {
        return container.descendants(ns::CHART, "v").map(|v| clean_text(&v.text())).collect();
    }
    let declared_len = container
        .first_descendant(ns::CHART, "ptCount")
        .and_then(|e| e.attr(ns::CHART, "val"))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_CHART_POINTS);
    let mut out = vec![String::new(); declared_len];
    for (seq_idx, pt) in pts.into_iter().enumerate() {
        let idx = pt
            .attr(ns::CHART, "idx")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(seq_idx);
        if idx >= MAX_CHART_POINTS {
            continue;
        }
        let val = pt.find(ns::CHART, "v").map(|v| clean_text(&v.text())).unwrap_or_default();
        if idx >= out.len() {
            out.resize(idx + 1, String::new());
        }
        out[idx] = val;
    }
    out
}

/// A chart part as blocks: bold title paragraph plus a categories x series
/// table built from the cached display strings.
pub fn chart_blocks(root: &Element) -> Vec<Block> {
    let mut blocks = Vec::new();
    let title = root
        .first_descendant(ns::CHART, "title")
        .map(|t| clean_text(&drawing_text(t)))
        .filter(|t| !t.trim().is_empty());
    if let Some(title) = title {
        blocks.push(Block::Paragraph(vec![Inline::Text {
            text: title,
            style: Style { bold: true, ..Style::PLAIN },
        }]));
    }
    struct Series {
        name: String,
        values: Vec<String>,
    }
    let mut categories: Vec<String> = Vec::new();
    let mut series: Vec<Series> = Vec::new();
    for ser in root.descendants(ns::CHART, "ser") {
        // Cached display strings only; `c:f` formula references are not text.
        let name = ser
            .find(ns::CHART, "tx")
            .and_then(|t| t.first_descendant(ns::CHART, "v"))
            .map(|v| clean_text(&v.text()))
            .unwrap_or_default();
        let cats: Vec<String> = ser.find(ns::CHART, "cat").map(chart_points).unwrap_or_default();
        if categories.is_empty() {
            categories = cats;
        } else {
            if cats.len() > categories.len() {
                categories.resize(cats.len(), String::new());
            }
            for (i, c) in cats.into_iter().enumerate() {
                if categories[i].is_empty() && !c.is_empty() {
                    categories[i] = c;
                }
            }
        }
        let values: Vec<String> = ser.find(ns::CHART, "val").map(chart_points).unwrap_or_default();
        series.push(Series { name, values });
    }
    if !series.is_empty() && !categories.is_empty() {
        let cat_title = root
            .first_descendant(ns::CHART, "catAx")
            .and_then(|ax| ax.find(ns::CHART, "title"))
            .map(|t| clean_text(&drawing_text(t)))
            .unwrap_or_default();
        let mut header: Vec<Cell> = vec![Cell::from_inlines(vec![Inline::plain(cat_title)])];
        header
            .extend(series.iter().map(|s| Cell::from_inlines(vec![Inline::plain(s.name.clone())])));
        let mut rows = vec![header];
        for (i, cat) in categories.iter().enumerate() {
            let mut row = vec![Cell::from_inlines(vec![Inline::plain(cat.clone())])];
            for s in &series {
                let v = s.values.get(i).cloned().unwrap_or_default();
                row.push(Cell::from_inlines(vec![Inline::plain(v)]));
            }
            rows.push(row);
        }
        blocks.push(Block::Table(Table::from_rows(rows, 1, TableKind::Data)));
    }
    blocks
}

/// A SmartArt data part as a bullet list of its text points in order.
pub fn diagram_blocks(root: &Element) -> Vec<Block> {
    let items: Vec<ListItem> = root
        .descendants(ns::DGM, "pt")
        .filter_map(|pt| {
            let text = clean_text(&pt.find(ns::DGM, "t")?.text());
            if text.trim().is_empty() {
                return None;
            }
            Some(ListItem {
                blocks: vec![Block::Paragraph(vec![Inline::plain(text)])],
                marker_label: None,
            })
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    vec![Block::List(List { marker: crate::model::MarkerKind::Bullet, start: 1, items })]
}

/// Text runs inside DrawingML rich text (`a:p`/`a:r`/`a:t`), joined.
pub fn drawing_text(elem: &Element) -> String {
    let mut parts: Vec<String> = Vec::new();
    for p in elem.descendants(ns::A, "p") {
        let text = p.text();
        if !text.trim().is_empty() {
            parts.push(text);
        }
    }
    if parts.is_empty() { elem.text() } else { parts.join(" ") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellSlot, inlines_to_plain_text};

    #[test]
    fn sparse_chart_points_align_by_idx() {
        let xml = br#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">
            <c:chart><c:plotArea><c:barChart>
                <c:ser>
                    <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>Revenue</c:v></c:pt></c:strCache></c:strRef></c:tx>
                    <c:cat><c:strRef><c:strCache>
                        <c:ptCount val="3"/>
                        <c:pt idx="0"><c:v>Q1</c:v></c:pt>
                        <c:pt idx="1"><c:v>Q2</c:v></c:pt>
                        <c:pt idx="2"><c:v>Q3</c:v></c:pt>
                    </c:strCache></c:strRef></c:cat>
                    <c:val><c:numRef><c:numCache>
                        <c:ptCount val="3"/>
                        <c:pt idx="0"><c:v>10</c:v></c:pt>
                        <c:pt idx="2"><c:v>30</c:v></c:pt>
                    </c:numCache></c:numRef></c:val>
                </c:ser>
            </c:barChart></c:plotArea></c:chart>
        </c:chartSpace>"#;
        let root = crate::package::xml::parse_xml(xml).unwrap();
        let blocks = chart_blocks(&root);
        let [Block::Table(table)] = &blocks[..] else { panic!("expected table: {blocks:?}") };
        let cell_str = |r: usize, c: usize| match &table.grid[r][c] {
            CellSlot::Origin(cell) => match cell.blocks.first() {
                Some(Block::Paragraph(inlines)) => inlines_to_plain_text(inlines),
                _ => String::new(),
            },
            _ => String::new(),
        };
        assert_eq!(cell_str(1, 0), "Q1");
        assert_eq!(cell_str(1, 1), "10");
        assert_eq!(cell_str(2, 0), "Q2");
        assert_eq!(cell_str(2, 1), "");
        assert_eq!(cell_str(3, 0), "Q3");
        assert_eq!(cell_str(3, 1), "30");
    }
}
