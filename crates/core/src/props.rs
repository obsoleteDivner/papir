//! Run, paragraph and table properties: parsing from `w:rPr`/`w:pPr`/...,
//! merging along the style chain, and writing back in schema order.

use crate::xml::{Element, Node, Ns};
use std::rc::Rc;

/// Parses a measure in twips. Accepts plain numbers and universal measures
/// (`12pt`, `1in`, `2.5cm`, `10mm`, `1pc`, `1pi`).
pub fn twips(v: &str) -> Option<i32> {
    let v = v.trim();
    let (num, mul) = if let Some(n) = v.strip_suffix("pt") {
        (n, 20.0)
    } else if let Some(n) = v.strip_suffix("in") {
        (n, 1440.0)
    } else if let Some(n) = v.strip_suffix("cm") {
        (n, 1440.0 / 2.54)
    } else if let Some(n) = v.strip_suffix("mm") {
        (n, 144.0 / 2.54)
    } else if let Some(n) = v.strip_suffix("pc").or_else(|| v.strip_suffix("pi")) {
        (n, 240.0)
    } else {
        (v, 1.0)
    };
    num.trim().parse::<f64>().ok().map(|n| (n * mul).round() as i32)
}

fn wint(e: &Element, a: &str) -> Option<i32> {
    e.wattr(a).and_then(twips)
}

/// `<w:b/>` is true, `w:val="0|false|off"` is false.
pub fn on_off(e: &Element) -> bool {
    !matches!(e.wattr("val"), Some("0") | Some("false") | Some("off") | Some("none"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunProps {
    pub style: Option<String>,
    pub font_ascii: Option<String>,
    pub font_hansi: Option<String>,
    pub font_cs: Option<String>,
    pub font_east: Option<String>,
    pub theme_ascii: Option<String>,
    pub theme_hansi: Option<String>,
    pub b: Option<bool>,
    pub i: Option<bool>,
    pub caps: Option<bool>,
    pub small_caps: Option<bool>,
    pub strike: Option<bool>,
    pub dstrike: Option<bool>,
    pub vanish: Option<bool>,
    pub color: Option<String>,
    /// character spacing, twips
    pub spacing: Option<i32>,
    /// size, half-points
    pub sz: Option<u32>,
    pub highlight: Option<String>,
    pub u: Option<String>,
    pub shd: Option<String>,
    pub valign: Option<String>,
}

macro_rules! merge_fields {
    ($dst:ident, $src:ident; $($f:ident),*) => {
        $( if $src.$f.is_some() { $dst.$f = $src.$f.clone(); } )*
    };
}

impl RunProps {
    pub fn merge(&mut self, o: &RunProps) {
        merge_fields!(self, o; style, b, i, caps, small_caps, strike, dstrike, vanish,
            color, spacing, sz, highlight, u, shd, valign);
        // fonts: an explicit font name cancels the theme font of the same slot
        if o.font_ascii.is_some() || o.theme_ascii.is_some() {
            self.font_ascii = o.font_ascii.clone();
            self.theme_ascii = o.theme_ascii.clone();
        }
        if o.font_hansi.is_some() || o.theme_hansi.is_some() {
            self.font_hansi = o.font_hansi.clone();
            self.theme_hansi = o.theme_hansi.clone();
        }
        merge_fields!(self, o; font_cs, font_east);
    }

    pub fn parse(e: &Element) -> RunProps {
        let mut p = RunProps::default();
        for c in e.elems() {
            if c.ns != Ns::W {
                continue;
            }
            let val = || c.wattr("val").map(str::to_string);
            match c.local.as_str() {
                "rStyle" => p.style = val(),
                "rFonts" => {
                    p.font_ascii = c.wattr("ascii").map(str::to_string);
                    p.font_hansi = c.wattr("hAnsi").map(str::to_string);
                    p.font_cs = c.wattr("cs").map(str::to_string);
                    p.font_east = c.wattr("eastAsia").map(str::to_string);
                    p.theme_ascii = c.wattr("asciiTheme").map(str::to_string);
                    p.theme_hansi = c.wattr("hAnsiTheme").map(str::to_string);
                }
                "b" => p.b = Some(on_off(c)),
                "i" => p.i = Some(on_off(c)),
                "caps" => p.caps = Some(on_off(c)),
                "smallCaps" => p.small_caps = Some(on_off(c)),
                "strike" => p.strike = Some(on_off(c)),
                "dstrike" => p.dstrike = Some(on_off(c)),
                "vanish" => p.vanish = Some(on_off(c)),
                "color" => p.color = val(),
                "spacing" => p.spacing = wint(c, "val"),
                "sz" => p.sz = c.wattr("val").and_then(|v| v.parse::<f32>().ok()).map(|v| v as u32),
                "highlight" => p.highlight = val(),
                "u" => p.u = Some(val().unwrap_or_else(|| "single".into())),
                "shd" => p.shd = c.wattr("fill").filter(|f| *f != "auto").map(str::to_string),
                "vertAlign" => p.valign = val(),
                _ => {}
            }
        }
        p
    }

    /// Writes `w:rPr`, keeping children of `orig` this editor does not manage.
    pub fn write(&self, orig: Option<&Element>) -> Option<Element> {
        let mut out: Vec<Element> = orig
            .map(|o| o.elems().filter(|c| !(c.ns == Ns::W && RPR_MANAGED.contains(&c.local.as_str()))).cloned().collect())
            .unwrap_or_default();
        let v = |name: &str, val: &str| Element::new_w(name).with_wattr("val", val);
        let flag = |name: &str, on: bool| if on { Element::new_w(name) } else { Element::new_w(name).with_wattr("val", "0") };
        if let Some(s) = &self.style {
            out.push(v("rStyle", s));
        }
        if self.font_ascii.is_some() || self.font_hansi.is_some() || self.font_cs.is_some()
            || self.font_east.is_some() || self.theme_ascii.is_some() || self.theme_hansi.is_some()
        {
            let mut f = Element::new_w("rFonts");
            for (k, val) in [
                ("ascii", &self.font_ascii),
                ("hAnsi", &self.font_hansi),
                ("eastAsia", &self.font_east),
                ("cs", &self.font_cs),
                ("asciiTheme", &self.theme_ascii),
                ("hAnsiTheme", &self.theme_hansi),
            ] {
                if let Some(x) = val {
                    f.set_wattr(k, x);
                }
            }
            out.push(f);
        }
        for (name, val) in [
            ("b", self.b),
            ("bCs", self.b),
            ("i", self.i),
            ("iCs", self.i),
            ("caps", self.caps),
            ("smallCaps", self.small_caps),
            ("strike", self.strike),
            ("dstrike", self.dstrike),
            ("vanish", self.vanish),
        ] {
            if let Some(on) = val {
                out.push(flag(name, on));
            }
        }
        if let Some(c) = &self.color {
            out.push(v("color", c));
        }
        if let Some(s) = self.spacing {
            out.push(v("spacing", &s.to_string()));
        }
        if let Some(s) = self.sz {
            out.push(v("sz", &s.to_string()));
            out.push(v("szCs", &s.to_string()));
        }
        if let Some(h) = &self.highlight {
            out.push(v("highlight", h));
        }
        if let Some(u) = &self.u {
            out.push(v("u", u));
        }
        if let Some(s) = &self.shd {
            out.push(Element::new_w("shd").with_wattr("val", "clear").with_wattr("color", "auto").with_wattr("fill", s));
        }
        if let Some(a) = &self.valign {
            out.push(v("vertAlign", a));
        }
        if out.is_empty() {
            return None;
        }
        sort_schema(&mut out, RPR_ORDER);
        let mut el = orig.cloned().unwrap_or_else(|| Element::new_w("rPr"));
        el.children = out.into_iter().map(Node::Elem).collect();
        Some(el)
    }
}

const RPR_MANAGED: &[&str] = &[
    "rStyle", "rFonts", "b", "bCs", "i", "iCs", "caps", "smallCaps", "strike", "dstrike", "vanish",
    "color", "spacing", "sz", "szCs", "highlight", "u", "shd", "vertAlign",
];

const RPR_ORDER: &[&str] = &[
    "rStyle", "rFonts", "b", "bCs", "i", "iCs", "caps", "smallCaps", "strike", "dstrike", "outline",
    "shadow", "emboss", "imprint", "noProof", "snapToGrid", "vanish", "webHidden", "color", "spacing",
    "w", "kern", "position", "sz", "szCs", "highlight", "u", "effect", "bdr", "shd", "fitText",
    "vertAlign", "rtl", "cs", "em", "lang", "eastAsianLayout", "specVanish", "oMath", "rPrChange",
];

const PPR_ORDER: &[&str] = &[
    "pStyle", "keepNext", "keepLines", "pageBreakBefore", "framePr", "widowControl", "numPr",
    "suppressLineNumbers", "pBdr", "shd", "tabs", "suppressAutoHyphens", "kinsoku", "wordWrap",
    "overflowPunct", "topLinePunct", "autoSpaceDE", "autoSpaceDN", "bidi", "adjustRightInd",
    "snapToGrid", "spacing", "ind", "contextualSpacing", "mirrorIndents", "suppressOverlap", "jc",
    "textDirection", "textAlignment", "textboxTightWrap", "outlineLvl", "divId", "cnfStyle", "rPr",
    "sectPr", "pPrChange",
];

fn sort_schema(v: &mut [Element], order: &[&str]) {
    let rank = |e: &Element| {
        if e.ns != Ns::W {
            return order.len();
        }
        order.iter().position(|n| *n == e.local).unwrap_or(order.len())
    };
    v.sort_by_key(rank);
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabStop {
    pub kind: String,
    /// twips
    pub pos: i32,
    pub leader: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Border {
    pub style: String,
    /// eighths of a point
    pub sz: u32,
    pub color: Option<String>,
    /// twips/points depending on context; kept for spacing
    pub space: u32,
}

impl Border {
    pub fn parse(e: &Element) -> Border {
        Border {
            style: e.wattr("val").unwrap_or("none").to_string(),
            sz: e.wattr("sz").and_then(|v| v.parse().ok()).unwrap_or(4),
            color: e.wattr("color").filter(|c| *c != "auto").map(str::to_string),
            space: e.wattr("space").and_then(|v| v.parse().ok()).unwrap_or(0),
        }
    }

    pub fn visible(&self) -> bool {
        !matches!(self.style.as_str(), "none" | "nil" | "")
    }

    /// line width in points
    pub fn width(&self) -> f32 {
        (self.sz as f32 / 8.0).max(0.25)
    }
}

/// top, left, bottom, right, insideH, insideV
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Borders {
    pub top: Option<Border>,
    pub left: Option<Border>,
    pub bottom: Option<Border>,
    pub right: Option<Border>,
    pub inside_h: Option<Border>,
    pub inside_v: Option<Border>,
}

impl Borders {
    pub fn parse(e: &Element) -> Borders {
        let mut b = Borders::default();
        for c in e.elems() {
            let v = Some(Border::parse(c));
            match c.local.as_str() {
                "top" => b.top = v,
                "left" | "start" => b.left = v,
                "bottom" => b.bottom = v,
                "right" | "end" => b.right = v,
                "insideH" => b.inside_h = v,
                "insideV" => b.inside_v = v,
                _ => {}
            }
        }
        b
    }

    pub fn merge(&mut self, o: &Borders) {
        merge_fields!(self, o; top, left, bottom, right, inside_h, inside_v);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParaProps {
    pub style: Option<String>,
    pub jc: Option<String>,
    pub ind_left: Option<i32>,
    pub ind_right: Option<i32>,
    /// first-line indent; negative for hanging
    pub ind_first: Option<i32>,
    pub before: Option<i32>,
    pub after: Option<i32>,
    pub before_auto: Option<bool>,
    pub after_auto: Option<bool>,
    pub line: Option<i32>,
    pub line_rule: Option<String>,
    pub contextual: Option<bool>,
    pub keep_next: Option<bool>,
    pub keep_lines: Option<bool>,
    pub page_break_before: Option<bool>,
    pub widow: Option<bool>,
    /// (numId, ilvl)
    pub num_id: Option<i32>,
    pub ilvl: Option<i32>,
    pub tabs: Option<Vec<TabStop>>,
    pub shd: Option<String>,
    pub borders: Option<Borders>,
}

impl ParaProps {
    pub fn merge(&mut self, o: &ParaProps) {
        merge_fields!(self, o; style, jc, ind_left, ind_right, ind_first, before, after,
            before_auto, after_auto, line, line_rule, contextual, keep_next, keep_lines,
            page_break_before, widow, num_id, ilvl, shd, borders);
        if let Some(t) = &o.tabs {
            let mut tabs = self.tabs.take().unwrap_or_default();
            for s in t {
                tabs.retain(|x| x.pos != s.pos);
                if s.kind != "clear" {
                    tabs.push(s.clone());
                }
            }
            tabs.sort_by_key(|t| t.pos);
            self.tabs = Some(tabs);
        }
    }

    pub fn parse(e: &Element) -> ParaProps {
        let mut p = ParaProps::default();
        for c in e.elems() {
            if c.ns != Ns::W {
                continue;
            }
            match c.local.as_str() {
                "pStyle" => p.style = c.wattr("val").map(str::to_string),
                "jc" => p.jc = c.wattr("val").map(str::to_string),
                "ind" => {
                    p.ind_left = wint(c, "left").or_else(|| wint(c, "start"));
                    p.ind_right = wint(c, "right").or_else(|| wint(c, "end"));
                    if let Some(h) = wint(c, "hanging") {
                        p.ind_first = Some(-h);
                    } else if let Some(f) = wint(c, "firstLine") {
                        p.ind_first = Some(f);
                    }
                }
                "spacing" => {
                    p.before = wint(c, "before");
                    p.after = wint(c, "after");
                    p.before_auto = c.wattr("beforeAutospacing").map(|v| v == "1" || v == "true");
                    p.after_auto = c.wattr("afterAutospacing").map(|v| v == "1" || v == "true");
                    p.line = wint(c, "line");
                    p.line_rule = c.wattr("lineRule").map(str::to_string);
                    if p.line.is_some() && p.line_rule.is_none() {
                        p.line_rule = Some("auto".into());
                    }
                }
                "contextualSpacing" => p.contextual = Some(on_off(c)),
                "keepNext" => p.keep_next = Some(on_off(c)),
                "keepLines" => p.keep_lines = Some(on_off(c)),
                "pageBreakBefore" => p.page_break_before = Some(on_off(c)),
                "widowControl" => p.widow = Some(on_off(c)),
                "numPr" => {
                    p.num_id = c.wchild("numId").and_then(|n| wint(n, "val"));
                    p.ilvl = c.wchild("ilvl").and_then(|n| wint(n, "val"));
                    if p.ilvl.is_none() && p.num_id.is_some() {
                        p.ilvl = Some(0);
                    }
                }
                "tabs" => {
                    p.tabs = Some(
                        c.wchildren("tab")
                            .map(|t| TabStop {
                                kind: t.wattr("val").unwrap_or("left").to_string(),
                                pos: wint(t, "pos").unwrap_or(0),
                                leader: t.wattr("leader").map(str::to_string),
                            })
                            .collect(),
                    )
                }
                "shd" => p.shd = c.wattr("fill").filter(|f| *f != "auto").map(str::to_string),
                "pBdr" => p.borders = Some(Borders::parse(c)),
                _ => {}
            }
        }
        p
    }

    /// Writes `w:pPr`. Children of `orig` this editor does not manage
    /// (numPr, tabs, borders, rPr of the mark, sectPr, ...) are kept.
    pub fn write(&self, orig: Option<&Element>, mark: Option<Element>) -> Option<Element> {
        let managed = ["pStyle", "jc", "ind", "spacing", "numPr", "rPr"];
        let mut out: Vec<Element> = orig
            .map(|o| o.elems().filter(|c| !(c.ns == Ns::W && managed.contains(&c.local.as_str()))).cloned().collect())
            .unwrap_or_default();
        if let Some(s) = &self.style {
            out.push(Element::new_w("pStyle").with_wattr("val", s));
        }
        if self.num_id.is_some() || self.ilvl.is_some() {
            let mut np = Element::new_w("numPr");
            if let Some(l) = self.ilvl {
                np.children.push(Node::Elem(Element::new_w("ilvl").with_wattr("val", &l.to_string())));
            }
            if let Some(n) = self.num_id {
                np.children.push(Node::Elem(Element::new_w("numId").with_wattr("val", &n.to_string())));
            }
            out.push(np);
        }
        if let Some(j) = &self.jc {
            out.push(Element::new_w("jc").with_wattr("val", j));
        }
        let orig_child = |n: &str| orig.and_then(|o| o.wchild(n)).cloned();
        if self.before.is_some() || self.after.is_some() || self.line.is_some() {
            let mut s = orig_child("spacing").unwrap_or_else(|| Element::new_w("spacing"));
            if let Some(v) = self.before {
                s.set_wattr("before", &v.to_string());
            }
            if let Some(v) = self.after {
                s.set_wattr("after", &v.to_string());
            }
            if let Some(v) = self.line {
                s.set_wattr("line", &v.to_string());
                s.set_wattr("lineRule", self.line_rule.as_deref().unwrap_or("auto"));
            }
            out.push(s);
        }
        if self.ind_left.is_some() || self.ind_right.is_some() || self.ind_first.is_some() {
            let mut s = Element::new_w("ind");
            if let Some(v) = self.ind_left {
                s.set_wattr("left", &v.to_string());
            }
            if let Some(v) = self.ind_right {
                s.set_wattr("right", &v.to_string());
            }
            match self.ind_first {
                Some(v) if v < 0 => s.set_wattr("hanging", &(-v).to_string()),
                Some(v) => s.set_wattr("firstLine", &v.to_string()),
                None => {}
            }
            out.push(s);
        }
        if let Some(m) = mark {
            out.push(m);
        }
        if out.is_empty() {
            return None;
        }
        sort_schema(&mut out, PPR_ORDER);
        let mut el = orig.cloned().unwrap_or_else(|| Element::new_w("pPr"));
        el.children = out.into_iter().map(Node::Elem).collect();
        Some(el)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Margins {
    pub top: Option<i32>,
    pub left: Option<i32>,
    pub bottom: Option<i32>,
    pub right: Option<i32>,
}

impl Margins {
    pub fn parse(e: &Element) -> Margins {
        let mut m = Margins::default();
        for c in e.elems() {
            let v = wint(c, "w");
            match c.local.as_str() {
                "top" => m.top = v,
                "left" | "start" => m.left = v,
                "bottom" => m.bottom = v,
                "right" | "end" => m.right = v,
                _ => {}
            }
        }
        m
    }

    pub fn merge(&mut self, o: &Margins) {
        merge_fields!(self, o; top, left, bottom, right);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableProps {
    pub style: Option<String>,
    pub width: Option<(i32, String)>,
    pub ind: Option<i32>,
    pub jc: Option<String>,
    pub borders: Borders,
    pub cell_mar: Margins,
    pub fixed: bool,
}

impl TableProps {
    pub fn parse(e: &Element) -> TableProps {
        let mut p = TableProps::default();
        for c in e.elems() {
            match c.local.as_str() {
                "tblStyle" => p.style = c.wattr("val").map(str::to_string),
                "tblW" => {
                    p.width = wint(c, "w").map(|w| (w, c.wattr("type").unwrap_or("dxa").to_string()))
                }
                "tblInd" => p.ind = wint(c, "w"),
                "jc" => p.jc = c.wattr("val").map(str::to_string),
                "tblBorders" => p.borders = Borders::parse(c),
                "tblCellMar" => p.cell_mar = Margins::parse(c),
                "tblLayout" => p.fixed = c.wattr("type") == Some("fixed"),
                _ => {}
            }
        }
        p
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowProps {
    /// twips
    pub height: Option<i32>,
    pub exact: bool,
    pub header: bool,
    pub cant_split: bool,
    pub grid_before: u32,
    pub grid_after: u32,
}

impl RowProps {
    pub fn parse(e: &Element) -> RowProps {
        let mut p = RowProps::default();
        for c in e.elems() {
            match c.local.as_str() {
                "trHeight" => {
                    p.height = wint(c, "val");
                    p.exact = c.wattr("hRule") == Some("exact");
                }
                "tblHeader" => p.header = on_off(c),
                "cantSplit" => p.cant_split = on_off(c),
                "gridBefore" => p.grid_before = wint(c, "val").unwrap_or(0).max(0) as u32,
                "gridAfter" => p.grid_after = wint(c, "val").unwrap_or(0).max(0) as u32,
                _ => {}
            }
        }
        p
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VMerge {
    Restart,
    Continue,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellProps {
    pub width: Option<i32>,
    pub span: u32,
    pub vmerge: Option<VMerge>,
    pub valign: Option<String>,
    pub shd: Option<String>,
    pub borders: Borders,
    pub mar: Margins,
}

impl CellProps {
    pub fn parse(e: &Element) -> CellProps {
        let mut p = CellProps { span: 1, ..Default::default() };
        for c in e.elems() {
            match c.local.as_str() {
                "tcW" => p.width = if c.wattr("type") == Some("pct") { None } else { wint(c, "w") },
                "gridSpan" => p.span = wint(c, "val").unwrap_or(1).max(1) as u32,
                "vMerge" => {
                    p.vmerge = Some(if c.wattr("val") == Some("restart") { VMerge::Restart } else { VMerge::Continue })
                }
                "vAlign" => p.valign = c.wattr("val").map(str::to_string),
                "shd" => p.shd = c.wattr("fill").filter(|f| *f != "auto").map(str::to_string),
                "tcBorders" => p.borders = Borders::parse(c),
                "tcMar" => p.mar = Margins::parse(c),
                _ => {}
            }
        }
        p
    }
}

/// Page setup of a section, twips.
#[derive(Clone, Debug, PartialEq)]
pub struct SectProps {
    pub w: i32,
    pub h: i32,
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
    pub header: i32,
    pub footer: i32,
    pub title_pg: bool,
    pub kind: String,
    pub headers: [Option<String>; 3],
    pub footers: [Option<String>; 3],
    pub pg_start: Option<i32>,
}

impl Default for SectProps {
    fn default() -> Self {
        // A4 with Word's default margins
        SectProps {
            w: 11906,
            h: 16838,
            top: 1134,
            bottom: 1134,
            left: 1701,
            right: 850,
            header: 708,
            footer: 708,
            title_pg: false,
            kind: "nextPage".into(),
            headers: [None, None, None],
            footers: [None, None, None],
            pg_start: None,
        }
    }
}

impl SectProps {
    pub fn parse(e: &Element) -> SectProps {
        let mut s = SectProps::default();
        for c in e.elems() {
            match c.local.as_str() {
                "pgSz" => {
                    if let Some(w) = wint(c, "w") {
                        s.w = w;
                    }
                    if let Some(h) = wint(c, "h") {
                        s.h = h;
                    }
                }
                "pgMar" => {
                    let g = |a: &str, d: i32| wint(c, a).unwrap_or(d);
                    s.top = g("top", s.top).abs();
                    s.bottom = g("bottom", s.bottom).abs();
                    s.left = g("left", s.left);
                    s.right = g("right", s.right);
                    s.header = g("header", s.header);
                    s.footer = g("footer", s.footer);
                    s.left += wint(c, "gutter").unwrap_or(0);
                }
                "titlePg" => s.title_pg = on_off(c),
                "type" => s.kind = c.wattr("val").unwrap_or("nextPage").to_string(),
                "pgNumType" => s.pg_start = wint(c, "start"),
                "headerReference" | "footerReference" => {
                    let idx = match c.wattr("type") {
                        Some("first") => 1,
                        Some("even") => 2,
                        _ => 0,
                    };
                    let rid = c.attr(Ns::R, "id").map(str::to_string);
                    if c.local == "headerReference" {
                        s.headers[idx] = rid;
                    } else {
                        s.footers[idx] = rid;
                    }
                }
                _ => {}
            }
        }
        s
    }
}

pub fn rc(e: &Element) -> Rc<Element> {
    Rc::new(e.clone())
}
