//! Document model.
//!
//! Every container is a `Vec<Rc<Block>>`: an undo snapshot is a cheap clone of
//! the outer vectors, and an edit copies only the blocks it touches
//! (`Rc::make_mut`). Anything the editor does not understand is kept as the
//! original XML element and written back untouched.

use crate::props::{CellProps, ParaProps, RowProps, RunProps, TableProps};
use crate::xml::Element;
use std::cell::Cell as StdCell;
use std::rc::Rc;

pub type Container = Vec<Rc<Block>>;

thread_local! {
    static REV: StdCell<u64> = const { StdCell::new(1) };
}

/// Monotonic revision used as a layout-cache key: every created or mutated
/// paragraph gets a fresh one.
pub fn next_rev() -> u64 {
    REV.with(|r| {
        let v = r.get();
        r.set(v + 1);
        v
    })
}

#[derive(Clone, Debug)]
pub enum Block {
    Para(Para),
    Table(Table),
    Sdt(SdtBlock),
    /// Unknown block-level element (bookmarks, customXml, altChunk, ...).
    Raw(Rc<Element>),
}

#[derive(Clone, Debug)]
pub struct SdtBlock {
    /// `<w:sdt>` + sdtPr/sdtEndPr + `<w:sdtContent>`
    pub open: Rc<str>,
    /// `</w:sdtContent></w:sdt>`
    pub close: Rc<str>,
    pub content: Container,
}

#[derive(Clone, Debug)]
pub struct Para {
    pub rev: u64,
    pub open: Rc<str>,
    pub close: Rc<str>,
    /// original `w:pPr`; unknown children are kept on save
    pub ppr: Option<Rc<Element>>,
    pub props: ParaProps,
    /// run properties of the paragraph mark (`w:pPr/w:rPr`)
    pub mark: RunProps,
    pub props_dirty: bool,
    pub inlines: Vec<Inline>,
    /// original markup while the paragraph is untouched (text, not a tree: a
    /// large document would otherwise be held twice)
    pub orig: Option<Rc<str>>,
}

#[derive(Clone, Debug)]
pub enum Inline {
    Run(Run),
    /// Start of an inline wrapper (sdt, hyperlink, fldSimple, smartTag, ins...).
    Open(Rc<Wrapper>),
    Close(Rc<Wrapper>),
    /// Zero-width element at paragraph level (bookmarks, proofErr, math...).
    Raw(Rc<Element>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum WrapKind {
    Sdt,
    Hyperlink,
    Field(String),
    /// tracked insertion (w:ins, w:moveTo): author, date
    Ins(String, String),
    Other,
}

/// A tracked deletion (w:del, w:moveFrom) kept as raw markup.
pub fn is_deletion(e: &crate::xml::Element) -> bool {
    e.is_w("del") || e.is_w("moveFrom")
}

#[derive(Clone, Debug)]
pub struct Wrapper {
    pub kind: WrapKind,
    pub open: String,
    pub close: String,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub open: Rc<str>,
    pub rpr: Option<Rc<Element>>,
    pub props: RunProps,
    pub dirty: bool,
    pub items: Vec<Item>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakKind {
    Line,
    Page,
    Column,
}

#[derive(Clone, Debug)]
pub enum Item {
    Text(String),
    Tab,
    Break(BreakKind),
    Image(Rc<Drawing>),
    /// zero-width run content kept as-is (fldChar, instrText, footnoteReference...)
    Raw(Rc<Element>),
}

#[derive(Clone, Debug)]
pub struct Drawing {
    pub el: Element,
    /// relationship id of the picture (`a:blip/@r:embed`)
    pub rid: Option<String>,
    /// resolved part name of the picture in the package
    pub src: Option<String>,
    /// position and wrapping of a floating object
    pub anchor: Option<Float>,
    /// text box content (wps:txbx / v:textbox)
    pub txbx: Option<Container>,
    /// chart part this drawing shows
    pub chart: Option<String>,
    /// shape fill and outline (colour, width in points)
    pub fill: Option<u32>,
    pub line: Option<(u32, f32)>,
    /// text box insets, points: top, bottom, left, right
    pub insets: [f32; 4],
    /// size in points
    pub width: f32,
    pub height: f32,
    pub anchored: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    /// in front of the text, no wrapping
    Front,
    /// behind the text
    Behind,
    /// text on one side (square, tight, through)
    Square,
    /// text above and below only
    TopBottom,
}

/// Position of a floating object (`wp:anchor`).
#[derive(Clone, Debug, PartialEq)]
pub struct Float {
    /// offsets in points when there is no alignment
    pub x: f32,
    pub y: f32,
    pub rel_h: String,
    pub rel_v: String,
    pub align_h: Option<String>,
    pub align_v: Option<String>,
    pub wrap: Wrap,
    /// distance from text: top, bottom, left, right (points)
    pub dist: [f32; 4],
}

impl Drawing {
    /// The same picture at another size (points): every extent in the
    /// drawing markup is rewritten, the rest is kept.
    pub fn resized(&self, w: f32, h: f32) -> Drawing {
        fn walk(e: &mut crate::xml::Element, cx: &str, cy: &str) {
            let ext = (e.ns == crate::xml::Ns::Wp && e.local == "extent") || (e.ns == crate::xml::Ns::A && e.local == "ext");
            if ext {
                for a in e.attrs.iter_mut() {
                    match a.local.as_str() {
                        "cx" => a.value = cx.to_string(),
                        "cy" => a.value = cy.to_string(),
                        _ => {}
                    }
                }
            }
            for c in e.children.iter_mut() {
                if let crate::xml::Node::Elem(x) = c {
                    walk(x, cx, cy);
                }
            }
        }
        let mut d = self.clone();
        let (cx, cy) = (((w * 12700.0) as i64).to_string(), ((h * 12700.0) as i64).to_string());
        walk(&mut d.el, &cx, &cy);
        d.width = w;
        d.height = h;
        d
    }
}

impl Item {
    /// Number of caret positions the item occupies.
    pub fn len(&self) -> usize {
        match self {
            Item::Text(t) => t.chars().count(),
            Item::Raw(_) => 0,
            _ => 1,
        }
    }
}

impl Run {
    pub fn len(&self) -> usize {
        self.items.iter().map(Item::len).sum()
    }

    pub fn new(props: RunProps) -> Run {
        Run { open: "<w:r>".into(), rpr: None, props, dirty: true, items: Vec::new() }
    }

    /// Same run shell (tag, properties), no content.
    pub fn shell(&self) -> Run {
        Run {
            open: self.open.clone(),
            rpr: self.rpr.clone(),
            props: self.props.clone(),
            dirty: self.dirty,
            items: Vec::new(),
        }
    }
}

impl Inline {
    pub fn len(&self) -> usize {
        match self {
            Inline::Run(r) => r.len(),
            _ => 0,
        }
    }
}

impl Para {
    pub fn new(props: ParaProps, mark: RunProps) -> Para {
        Para {
            rev: next_rev(),
            open: "<w:p>".into(),
            close: "</w:p>".into(),
            ppr: None,
            props,
            mark,
            props_dirty: true,
            inlines: Vec::new(),
            orig: None,
        }
    }

    pub fn len(&self) -> usize {
        self.inlines.iter().map(Inline::len).sum()
    }

    /// Call before any mutation.
    pub fn touch(&mut self) {
        self.rev = next_rev();
        self.orig = None;
    }

    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for i in &self.inlines {
            if let Inline::Run(r) = i {
                for it in &r.items {
                    match it {
                        Item::Text(t) => s.push_str(t),
                        Item::Tab => s.push('\t'),
                        Item::Break(_) => s.push('\n'),
                        Item::Image(_) => s.push('\u{FFFC}'),
                        Item::Raw(_) => {}
                    }
                }
            }
        }
        s
    }

    /// Whether the paragraph carries a section break (`w:pPr/w:sectPr`).
    pub fn sect(&self) -> Option<&Element> {
        self.ppr.as_ref().and_then(|p| p.wchild("sectPr"))
    }
}

#[derive(Clone, Debug)]
pub struct Table {
    pub open: Rc<str>,
    pub close: Rc<str>,
    pub props: TableProps,
    /// grid column widths, twips
    pub grid: Vec<i32>,
    pub items: Vec<TableItem>,
    pub orig: Option<Rc<str>>,
}

#[derive(Clone, Debug)]
pub enum TableItem {
    Row(Row),
    Raw(Rc<Element>),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub open: Rc<str>,
    pub close: Rc<str>,
    /// sdt/customXml wrapping the row: (open, close)
    pub wrap: Option<(Rc<str>, Rc<str>)>,
    pub props: RowProps,
    pub items: Vec<RowItem>,
}

#[derive(Clone, Debug)]
pub enum RowItem {
    Cell(TableCell),
    Raw(Rc<Element>),
}

#[derive(Clone, Debug)]
pub struct TableCell {
    pub open: Rc<str>,
    pub close: Rc<str>,
    pub wrap: Option<(Rc<str>, Rc<str>)>,
    pub tcpr: Option<Rc<Element>>,
    pub props: CellProps,
    pub content: Container,
}

impl Table {
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.items.iter().filter_map(|i| match i {
            TableItem::Row(r) => Some(r),
            _ => None,
        })
    }

    pub fn row_mut(&mut self, n: usize) -> Option<&mut Row> {
        self.items
            .iter_mut()
            .filter_map(|i| match i {
                TableItem::Row(r) => Some(r),
                _ => None,
            })
            .nth(n)
    }
}

impl Row {
    pub fn cells(&self) -> impl Iterator<Item = &TableCell> {
        self.items.iter().filter_map(|i| match i {
            RowItem::Cell(c) => Some(c),
            _ => None,
        })
    }

    pub fn cell_mut(&mut self, n: usize) -> Option<&mut TableCell> {
        self.items
            .iter_mut()
            .filter_map(|i| match i {
                RowItem::Cell(c) => Some(c),
                _ => None,
            })
            .nth(n)
    }
}

/// Header or footer part.
#[derive(Clone, Debug)]
pub struct Story {
    pub part: String,
    pub root: Rc<Element>,
    pub content: Container,
    /// edited: the part is written again on save
    pub dirty: bool,
}
