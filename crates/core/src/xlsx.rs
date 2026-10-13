//! XLSX for viewing: stored values (formula results; nothing is
//! recalculated), styles, number formats, merged cells, hidden rows and
//! columns, frozen panes. Sheets and shared strings are read with a
//! streaming parser, so a sheet of 100 000 rows stays within tens of MB.

use crate::docx::{self, Limits, Package};
use crate::numfmt;
use crate::xml::{self, Element};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum V {
    None,
    N(f64),
    S(Rc<str>),
    B(bool),
    E(Rc<str>),
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub c: u32,
    pub s: u32,
    pub v: V,
    pub f: Option<Rc<str>>,
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub r: u32,
    pub ht: Option<f32>,
    pub hidden: bool,
    pub style: Option<u32>,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug)]
pub struct Col {
    pub min: u32,
    pub max: u32,
    pub width: Option<f32>,
    pub hidden: bool,
    pub style: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub name: String,
    pub hidden: bool,
    pub rows: Vec<Row>,
    pub cols: Vec<Col>,
    /// column width in characters, row height in points
    pub default_w: f32,
    pub default_h: f32,
    /// r0, c0, r1, c1 (inclusive, 0-based)
    pub merges: Vec<[u32; 4]>,
    /// frozen rows and columns
    pub frozen: (u32, u32),
    pub filter: Option<[u32; 4]>,
    pub grid: bool,
    pub tab_color: Option<u32>,
    /// number of rows and columns in use
    pub nrows: u32,
    pub ncols: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Font {
    pub name: String,
    pub size: f32,
    pub b: bool,
    pub i: bool,
    pub u: bool,
    pub strike: bool,
    pub color: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Border {
    /// left, right, top, bottom: style name and colour
    pub sides: [Option<(String, u32)>; 4],
}

#[derive(Clone, Debug, Default)]
pub struct Xf {
    pub num: u32,
    pub font: u32,
    pub fill: Option<u32>,
    pub border: u32,
    pub h: Option<String>,
    pub v: Option<String>,
    pub wrap: bool,
    pub indent: u32,
    pub rot: i32,
}

#[derive(Debug, Default)]
pub struct Book {
    pub sheets: Vec<Sheet>,
    pub xfs: Vec<Xf>,
    pub fonts: Vec<Font>,
    pub borders: Vec<Border>,
    pub numfmts: HashMap<u32, String>,
    pub date1904: bool,
}

#[derive(Debug)]
pub enum Error {
    Package(String),
    Missing(&'static str),
    Xml(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Package(e) => write!(f, "package: {e}"),
            Error::Missing(p) => write!(f, "missing {p}"),
            Error::Xml(e) => write!(f, "xml: {e}"),
        }
    }
}

const INDEXED: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF,
    0x800000, 0x008000, 0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC, 0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF,
    0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF, 0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF, 0xFF99CC, 0xCC99FF, 0xFFCC99,
    0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600, 0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399, 0x333333,
];

/// Theme colours by index (lt1, dk1, lt2, dk2, accent1..6, hlink, folHlink).
fn theme_colors(pkg: &Package, wb_rels: &docx::Rels) -> Vec<u32> {
    let mut out = vec![0xFFFFFF, 0x000000, 0xE7E6E6, 0x44546A, 0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47, 0x0563C1, 0x954F72];
    let Some(part) = wb_rels.values().find(|r| r.kind == "theme").map(|r| r.target.clone()) else { return out };
    let Ok(Some(root)) = pkg.xml(&part) else { return out };
    fn find<'a>(e: &'a Element, n: &str) -> Option<&'a Element> {
        e.elems().find(|c| c.local == n).or_else(|| e.elems().find_map(|c| find(c, n)))
    }
    let Some(scheme) = find(&root, "clrScheme") else { return out };
    let order = ["lt1", "dk1", "lt2", "dk2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink"];
    for (i, n) in order.iter().enumerate() {
        if let Some(e) = scheme.elems().find(|c| c.local == *n) {
            let v = e.elems().find_map(|c| match c.local.as_str() {
                "srgbClr" => c.attr(xml::Ns::None, "val").and_then(|v| u32::from_str_radix(v, 16).ok()),
                "sysClr" => c.attr(xml::Ns::None, "lastClr").and_then(|v| u32::from_str_radix(v, 16).ok()),
                _ => None,
            });
            if let Some(v) = v {
                out[i] = v;
            }
        }
    }
    out
}

fn tint(c: u32, t: f64) -> u32 {
    if t == 0.0 {
        return c;
    }
    let ch = |s: u32| (((c >> s) & 255) as f64) / 255.0;
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let mut l = (mx + mn) / 2.0;
    let d = mx - mn;
    let (mut h, s) = if d == 0.0 {
        (0.0, 0.0)
    } else {
        let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
        let h = if mx == r { (g - b) / d + if g < b { 6.0 } else { 0.0 } } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
        (h / 6.0, s)
    };
    l = if t < 0.0 { l * (1.0 + t) } else { l * (1.0 - t) + t };
    h = h.rem_euclid(1.0);
    let hue = |p: f64, q: f64, mut t: f64| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let (r, g, b) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        (hue(p, q, h + 1.0 / 3.0), hue(p, q, h), hue(p, q, h - 1.0 / 3.0))
    };
    let to = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    to(r) << 16 | to(g) << 8 | to(b)
}

fn color(e: &Element, theme: &[u32]) -> Option<u32> {
    let a = |n: &str| e.attr(xml::Ns::None, n);
    let base = if let Some(rgb) = a("rgb") {
        let rgb = if rgb.len() == 8 { &rgb[2..] } else { rgb };
        u32::from_str_radix(rgb, 16).ok()?
    } else if let Some(t) = a("theme") {
        *theme.get(t.parse::<usize>().ok()?)?
    } else if let Some(i) = a("indexed") {
        let i: usize = i.parse().ok()?;
        if i == 64 {
            return None;
        }
        *INDEXED.get(i)?
    } else {
        return None;
    };
    Some(tint(base, a("tint").and_then(|t| t.parse().ok()).unwrap_or(0.0)))
}

fn attr<'a>(e: &'a Element, n: &str) -> Option<&'a str> {
    e.attr(xml::Ns::None, n).or_else(|| e.attrs.iter().find(|x| x.local == n).map(|x| x.value.as_str()))
}

fn flag(e: &Element, n: &str) -> bool {
    matches!(attr(e, n), Some("1") | Some("true"))
}

fn val_flag(e: Option<&Element>) -> bool {
    e.is_some_and(|e| attr(e, "val").is_none_or(|v| v != "0" && v != "false" && v != "none"))
}

fn styles(book: &mut Book, root: &Element, theme: &[u32]) {
    let child = |e: &Element, n: &str| e.elems().find(|c| c.local == n).cloned();
    if let Some(nf) = child(root, "numFmts") {
        for f in nf.elems() {
            if let (Some(id), Some(code)) = (attr(f, "numFmtId").and_then(|v| v.parse().ok()), attr(f, "formatCode")) {
                book.numfmts.insert(id, code.to_string());
            }
        }
    }
    if let Some(fs) = child(root, "fonts") {
        for f in fs.elems() {
            let g = |n: &str| f.elems().find(|c| c.local == n);
            book.fonts.push(Font {
                name: g("name").and_then(|e| attr(e, "val")).unwrap_or("Calibri").to_string(),
                size: g("sz").and_then(|e| attr(e, "val")).and_then(|v| v.parse().ok()).unwrap_or(11.0),
                b: val_flag(g("b")),
                i: val_flag(g("i")),
                u: val_flag(g("u")),
                strike: val_flag(g("strike")),
                color: g("color").and_then(|e| color(e, theme)),
            });
        }
    }
    let mut fills: Vec<Option<u32>> = Vec::new();
    if let Some(fs) = child(root, "fills") {
        for f in fs.elems() {
            let pf = f.elems().find(|c| c.local == "patternFill");
            let solid = pf.is_some_and(|p| attr(p, "patternType").is_some_and(|t| t != "none"));
            let c = pf.filter(|_| solid).and_then(|p| p.elems().find(|c| c.local == "fgColor").and_then(|e| color(e, theme)));
            // gradients: their first stop
            let c = c.or_else(|| f.elems().find(|c| c.local == "gradientFill").and_then(|g| g.elems().next()).and_then(|s| s.elems().next()).and_then(|e| color(e, theme)));
            fills.push(c);
        }
    }
    if let Some(bs) = child(root, "borders") {
        for b in bs.elems() {
            let mut out = Border::default();
            for (i, n) in ["left", "right", "top", "bottom"].iter().enumerate() {
                let side = b.elems().find(|c| c.local == *n).or_else(|| b.elems().find(|c| c.local == if *n == "left" { "start" } else if *n == "right" { "end" } else { "" }));
                if let Some(s) = side {
                    if let Some(st) = attr(s, "style").filter(|s| *s != "none") {
                        let c = s.elems().find(|c| c.local == "color").and_then(|e| color(e, theme)).unwrap_or(0);
                        out.sides[i] = Some((st.to_string(), c));
                    }
                }
            }
            book.borders.push(out);
        }
    }
    if let Some(xs) = child(root, "cellXfs") {
        for x in xs.elems() {
            let al = x.elems().find(|c| c.local == "alignment");
            let num = |n: &str| attr(x, n).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            book.xfs.push(Xf {
                num: num("numFmtId"),
                font: num("fontId"),
                fill: fills.get(num("fillId") as usize).copied().flatten(),
                border: num("borderId"),
                h: al.and_then(|a| attr(a, "horizontal")).map(str::to_string),
                v: al.and_then(|a| attr(a, "vertical")).map(str::to_string),
                wrap: al.is_some_and(|a| flag(a, "wrapText")),
                indent: al.and_then(|a| attr(a, "indent")).and_then(|v| v.parse().ok()).unwrap_or(0),
                rot: al.and_then(|a| attr(a, "textRotation")).and_then(|v| v.parse().ok()).unwrap_or(0),
            });
        }
    }
    if book.fonts.is_empty() {
        book.fonts.push(Font { name: "Calibri".into(), size: 11.0, ..Default::default() });
    }
    if book.xfs.is_empty() {
        book.xfs.push(Xf::default());
    }
}

/// "AB12" → (row, col), 0-based.
pub fn parse_ref(s: &str) -> Option<(u32, u32)> {
    let s = s.trim_start_matches('$');
    let letters: String = s.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits = s[letters.len()..].trim_start_matches('$');
    let mut c: u32 = 0;
    for ch in letters.chars() {
        c = c.checked_mul(26)?.checked_add(ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
    }
    let r: u32 = digits.parse().ok()?;
    if c == 0 || r == 0 {
        return None;
    }
    Some((r - 1, c - 1))
}

pub fn col_name(mut c: u32) -> String {
    let mut s = Vec::new();
    c += 1;
    while c > 0 {
        let m = (c - 1) % 26;
        s.push((b'A' + m as u8) as char);
        c = (c - 1) / 26;
    }
    s.iter().rev().collect()
}

fn parse_range(s: &str) -> Option<[u32; 4]> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let (r0, c0) = parse_ref(a)?;
    let (r1, c1) = parse_ref(b)?;
    Some([r0.min(r1), c0.min(c1), r0.max(r1), c0.max(c1)])
}

fn att(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes().with_checks(false).flatten().find(|a| a.key.local_name().as_ref() == name).map(|a| {
        #[allow(deprecated)]
        a.unescape_value().map(|v| v.into_owned()).unwrap_or_else(|_| a.value.to_string())
    })
}

fn text_of(reader: &mut Reader<&[u8]>, end: &str) -> Result<String, Error> {
    let mut s = String::new();
    loop {
        match reader.read_event().map_err(|e| Error::Xml(e.to_string()))? {
            Event::Text(t) => {
                let raw = t.as_ref().to_string();
                s.push_str(&xml::unescape_text(&raw).unwrap_or(raw));
            }
            Event::CData(t) => s.push_str(t.as_ref()),
            Event::GeneralRef(r) => s.push_str(&xml::resolve_entity(r.as_ref()).unwrap_or_default()),
            Event::End(e) if e.local_name().as_ref() == end => return Ok(s),
            Event::Eof => return Ok(s),
            _ => {}
        }
    }
}

/// Shared strings: the plain text of every `si` (runs joined, phonetic hints skipped).
fn shared_strings(data: &[u8]) -> Result<Vec<Rc<str>>, Error> {
    let mut out = Vec::new();
    let text = String::from_utf8_lossy(data);
    let mut r = Reader::from_str(&text);
    r.config_mut().trim_text(false);
    let mut cur: Option<String> = None;
    let mut skip = 0;
    loop {
        match r.read_event().map_err(|e| Error::Xml(e.to_string()))? {
            Event::Start(e) => match e.local_name().as_ref() {
                "si" => cur = Some(String::new()),
                "rPh" => skip += 1,
                "t" if skip == 0 => {
                    let t = text_of(&mut r, "t")?;
                    if let Some(c) = cur.as_mut() {
                        c.push_str(&t);
                    }
                }
                _ => {}
            },
            Event::Empty(e) if e.local_name().as_ref() == "si" => out.push(Rc::from("")),
            Event::End(e) => match e.local_name().as_ref() {
                "si" => out.push(Rc::from(cur.take().unwrap_or_default())),
                "rPh" => skip -= 1,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// A shared formula moved from its anchor by (dr, dc): relative references shift.
fn shift_formula(f: &str, dr: i64, dc: i64) -> String {
    let ch: Vec<char> = f.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut q = false;
    while i < ch.len() {
        let c = ch[i];
        if c == '"' {
            q = !q;
        }
        let prev_ident = i > 0 && (ch[i - 1].is_alphanumeric() || ch[i - 1] == '_' || ch[i - 1] == '.');
        if !q && !prev_ident && (c == '$' || c.is_ascii_alphabetic()) {
            // [$]LETTERS[$]DIGITS not followed by "(" or a letter
            let mut j = i;
            let ca = ch[j] == '$';
            if ca {
                j += 1;
            }
            let ls = j;
            while j < ch.len() && ch[j].is_ascii_alphabetic() {
                j += 1;
            }
            let letters: String = ch[ls..j].iter().collect();
            let ra = j < ch.len() && ch[j] == '$';
            if ra {
                j += 1;
            }
            let ds = j;
            while j < ch.len() && ch[j].is_ascii_digit() {
                j += 1;
            }
            let digits: String = ch[ds..j].iter().collect();
            let ok = !letters.is_empty() && letters.len() <= 3 && !digits.is_empty() && !(j < ch.len() && (ch[j].is_alphanumeric() || ch[j] == '(' || ch[j] == '_'));
            if ok {
                if let Some((r, c0)) = parse_ref(&format!("{letters}{digits}")) {
                    let nc = if ca { c0 as i64 } else { c0 as i64 + dc };
                    let nr = if ra { r as i64 } else { r as i64 + dr };
                    if nc >= 0 && nr >= 0 {
                        out.push_str(&format!("{}{}{}{}", if ca { "$" } else { "" }, col_name(nc as u32), if ra { "$" } else { "" }, nr + 1));
                        i = j;
                        continue;
                    }
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn sheet(data: &[u8], strings: &[Rc<str>]) -> Result<Sheet, Error> {
    let mut sh = Sheet { default_w: 8.43, default_h: 15.0, grid: true, ..Default::default() };
    let text = String::from_utf8_lossy(data);
    let mut r = Reader::from_str(&text);
    r.config_mut().trim_text(false);
    let mut row: Option<Row> = None;
    let mut next_row = 0u32;
    let mut next_col = 0u32;
    // shared formulas: si → (anchor row, anchor col, text)
    let mut shared: HashMap<String, (u32, u32, Rc<str>)> = HashMap::new();
    let mut cell: Option<(Cell, Option<String>)> = None;
    loop {
        let ev = r.read_event().map_err(|e| Error::Xml(e.to_string()))?;
        let (e, empty) = match &ev {
            Event::Start(e) => (Some(e.clone()), false),
            Event::Empty(e) => (Some(e.clone()), true),
            _ => (None, false),
        };
        if let Some(e) = e {
            match e.local_name().as_ref() {
                "tabColor" => sh.tab_color = att(&e, "rgb").and_then(|v| u32::from_str_radix(v.get(v.len().saturating_sub(6)..).unwrap_or(""), 16).ok()),
                "sheetView" => sh.grid = att(&e, "showGridLines").is_none_or(|v| v != "0" && v != "false"),
                "pane" => {
                    if att(&e, "state").is_some_and(|s| s == "frozen" || s == "frozenSplit") {
                        let x = att(&e, "xSplit").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as u32;
                        let y = att(&e, "ySplit").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as u32;
                        sh.frozen = (y, x);
                    }
                }
                "sheetFormatPr" => {
                    if let Some(h) = att(&e, "defaultRowHeight").and_then(|v| v.parse().ok()) {
                        sh.default_h = h;
                    }
                    if let Some(w) = att(&e, "defaultColWidth").and_then(|v| v.parse().ok()) {
                        sh.default_w = w;
                    } else if let Some(b) = att(&e, "baseColWidth").and_then(|v| v.parse::<f32>().ok()) {
                        sh.default_w = b + 0.71;
                    }
                }
                "col" => {
                    let n = |k: &str| att(&e, k).and_then(|v| v.parse::<u32>().ok());
                    if let (Some(a), Some(b)) = (n("min"), n("max")) {
                        sh.cols.push(Col {
                            min: a.saturating_sub(1),
                            max: b.saturating_sub(1).min(16_383),
                            width: att(&e, "width").and_then(|v| v.parse().ok()),
                            hidden: att(&e, "hidden").is_some_and(|v| v == "1" || v == "true"),
                            style: n("style"),
                        });
                    }
                }
                "row" => {
                    let rr = att(&e, "r").and_then(|v| v.parse::<u32>().ok()).map(|v| v.saturating_sub(1)).unwrap_or(next_row);
                    next_row = rr + 1;
                    next_col = 0;
                    let custom = att(&e, "customHeight").is_some_and(|v| v == "1" || v == "true");
                    let ht = att(&e, "ht").and_then(|v| v.parse::<f32>().ok());
                    let rw = Row {
                        r: rr,
                        ht: if custom || ht.is_some_and(|h| (h - sh.default_h).abs() > 0.01) { ht } else { None },
                        hidden: att(&e, "hidden").is_some_and(|v| v == "1" || v == "true"),
                        style: if att(&e, "customFormat").is_some_and(|v| v == "1" || v == "true") { att(&e, "s").and_then(|v| v.parse().ok()) } else { None },
                        cells: Vec::new(),
                    };
                    if empty {
                        if rw.ht.is_some() || rw.hidden || rw.style.is_some() {
                            sh.rows.push(rw);
                        }
                    } else {
                        row = Some(rw);
                    }
                }
                "c" => {
                    let (rr, cc) = att(&e, "r").and_then(|v| parse_ref(&v)).unwrap_or((row.as_ref().map(|r| r.r).unwrap_or(0), next_col));
                    next_col = cc + 1;
                    let c = Cell { c: cc, s: att(&e, "s").and_then(|v| v.parse().ok()).unwrap_or(0), v: V::None, f: None };
                    let t = att(&e, "t");
                    let _ = rr;
                    if empty {
                        if let Some(row) = row.as_mut() {
                            row.cells.push(c);
                        }
                    } else {
                        cell = Some((c, t));
                    }
                }
                "v" if cell.is_some() && !empty => {
                    let text = text_of(&mut r, "v")?;
                    let (c, t) = cell.as_mut().unwrap();
                    c.v = match t.as_deref() {
                        Some("s") => text.trim().parse::<usize>().ok().and_then(|i| strings.get(i).cloned()).map(V::S).unwrap_or(V::None),
                        Some("str") | Some("inlineStr") => V::S(Rc::from(text)),
                        Some("b") => V::B(text.trim() == "1"),
                        Some("e") => V::E(Rc::from(text)),
                        Some("d") => V::S(Rc::from(text)),
                        _ => text.trim().parse::<f64>().map(V::N).unwrap_or(V::None),
                    };
                }
                "is" if cell.is_some() && !empty => {
                    // inline string: all t inside
                    let mut s = String::new();
                    loop {
                        match r.read_event().map_err(|e| Error::Xml(e.to_string()))? {
                            Event::Start(x) if x.local_name().as_ref() == "t" => s.push_str(&text_of(&mut r, "t")?),
                            Event::End(x) if x.local_name().as_ref() == "is" => break,
                            Event::Eof => break,
                            _ => {}
                        }
                    }
                    cell.as_mut().unwrap().0.v = V::S(Rc::from(s));
                }
                "f" if cell.is_some() => {
                    let si = att(&e, "si");
                    let shared_t = att(&e, "t").is_some_and(|t| t == "shared");
                    let text = if empty { String::new() } else { text_of(&mut r, "f")? };
                    let (c, _) = cell.as_mut().unwrap();
                    let row_i = row.as_ref().map(|r| r.r).unwrap_or(0);
                    if !text.is_empty() {
                        let f: Rc<str> = Rc::from(text);
                        if shared_t {
                            if let Some(si) = si {
                                shared.insert(si, (row_i, c.c, f.clone()));
                            }
                        }
                        c.f = Some(f);
                    } else if let Some((ar, ac, f)) = si.and_then(|si| shared.get(&si)) {
                        c.f = Some(Rc::from(shift_formula(f, row_i as i64 - *ar as i64, c.c as i64 - *ac as i64)));
                    }
                }
                "mergeCell" => {
                    if let Some(m) = att(&e, "ref").and_then(|v| parse_range(&v)) {
                        sh.merges.push(m);
                    }
                }
                "autoFilter" => sh.filter = att(&e, "ref").and_then(|v| parse_range(&v)),
                _ => {}
            }
            continue;
        }
        match ev {
            Event::End(e) => match e.local_name().as_ref() {
                "c" => {
                    if let (Some((c, _)), Some(row)) = (cell.take(), row.as_mut()) {
                        row.cells.push(c);
                    }
                }
                "row" => {
                    if let Some(mut rw) = row.take() {
                        rw.cells.sort_by_key(|c| c.c);
                        sh.rows.push(rw);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    sh.rows.sort_by_key(|r| r.r);
    sh.rows.dedup_by_key(|r| r.r);
    sh.nrows = sh.rows.iter().filter(|r| !r.cells.is_empty() || r.ht.is_some()).map(|r| r.r + 1).max().unwrap_or(0);
    sh.ncols = sh.rows.iter().filter_map(|r| r.cells.last().map(|c| c.c + 1)).max().unwrap_or(0);
    for m in &sh.merges {
        sh.nrows = sh.nrows.max(m[2] + 1);
        sh.ncols = sh.ncols.max(m[3] + 1);
    }
    Ok(sh)
}

impl Book {
    pub fn open(bytes: &[u8]) -> Result<Book, Error> {
        let lim = Limits::default();
        let pkg = Package::read(bytes, &lim).map_err(|e| Error::Package(e.to_string()))?;
        let root_rels = docx::read_rels(&pkg, "").map_err(|e| Error::Package(e.to_string()))?;
        let wb_part = root_rels.values().find(|r| r.kind == "officeDocument").map(|r| r.target.clone()).unwrap_or_else(|| "xl/workbook.xml".into());
        let wb = pkg.xml(&wb_part).map_err(|e| Error::Package(e.to_string()))?.ok_or(Error::Missing("xl/workbook.xml"))?;
        let rels = docx::read_rels(&pkg, &wb_part).map_err(|e| Error::Package(e.to_string()))?;
        let mut book = Book::default();
        book.date1904 = wb.elems().find(|e| e.local == "workbookPr").is_some_and(|e| flag(e, "date1904"));
        let theme = theme_colors(&pkg, &rels);
        if let Some(p) = rels.values().find(|r| r.kind == "styles") {
            if let Ok(Some(root)) = pkg.xml(&p.target) {
                styles(&mut book, &root, &theme);
            }
        }
        if book.xfs.is_empty() {
            styles(&mut book, &Element { qname: "styleSheet".into(), ns: xml::Ns::None, local: "styleSheet".into(), attrs: vec![], children: vec![] }, &theme);
        }
        let strings = match rels.values().find(|r| r.kind == "sharedStrings").and_then(|r| pkg.get(&r.target)) {
            Some(d) => shared_strings(d)?,
            None => Vec::new(),
        };
        let Some(sheets) = wb.elems().find(|e| e.local == "sheets") else { return Err(Error::Missing("sheets")) };
        for s in sheets.elems().filter(|e| e.local == "sheet") {
            let rid = s.attrs.iter().find(|a| a.local == "id").map(|a| a.value.clone()).unwrap_or_default();
            let Some(rel) = rels.get(&rid) else { continue };
            if rel.kind != "worksheet" {
                continue;
            }
            let Some(data) = pkg.get(&rel.target) else { continue };
            let mut sh = sheet(data, &strings)?;
            sh.name = attr(s, "name").unwrap_or("Аркуш").to_string();
            sh.hidden = attr(s, "state").is_some_and(|v| v != "visible");
            book.autofit(&mut sh);
            book.sheets.push(sh);
        }
        Ok(book)
    }

    /// Heights Excel would give rows that do not set one: room for wrapped
    /// text and for large fonts (estimated from average glyph widths).
    fn autofit(&self, sh: &mut Sheet) {
        let col_chars = |c: u32| sh.cols.iter().find(|x| x.min <= c && c <= x.max).and_then(|x| x.width).unwrap_or(sh.default_w);
        let merged = |r: u32, c: u32| sh.merges.iter().any(|m| m[0] <= r && r <= m[2] && m[1] <= c && c <= m[3]);
        let base = self.fonts.first().map(|f| f.size).unwrap_or(11.0);
        let mut fits = Vec::new();
        for (i, row) in sh.rows.iter().enumerate() {
            if row.ht.is_some() || row.hidden {
                continue;
            }
            let mut need = sh.default_h;
            for c in &row.cells {
                if matches!(c.v, V::None) {
                    continue;
                }
                let Some(xf) = self.xfs.get(c.s as usize) else { continue };
                let size = self.fonts.get(xf.font as usize).map(|f| f.size).unwrap_or(11.0);
                let line = size * 1.32;
                let mut lines = 1usize;
                if xf.wrap && !merged(row.r, c.c) {
                    let text = self.display(c).0;
                    // a column is `width` digits of the default font (7 px); Cyrillic text averages ~6.6 px at 11 pt
                    let per_line = ((col_chars(c.c) * 7.0 - 5.0) / (6.6 * size / 11.0)).max(1.0);
                    lines = text.split('\n').map(|p| ((p.chars().count() as f32) / per_line).ceil().max(1.0) as usize).sum();
                }
                if lines == 1 && size <= base + 0.5 {
                    continue;
                }
                need = need.max(lines.min(40) as f32 * line + 2.0);
            }
            if need > sh.default_h + 0.5 {
                fits.push((i, need.round()));
            }
        }
        for (i, h) in fits {
            sh.rows[i].ht = Some(h);
        }
    }

    pub fn format_code(&self, xf: u32) -> String {
        let id = self.xfs.get(xf as usize).map(|x| x.num).unwrap_or(0);
        self.numfmts.get(&id).cloned().or_else(|| numfmt::builtin(id).map(str::to_string)).unwrap_or_else(|| "General".into())
    }

    /// Text shown in a cell, its colour from the format, and whether it is a number.
    pub fn display(&self, c: &Cell) -> (String, Option<u32>, bool) {
        match &c.v {
            V::None => (String::new(), None, false),
            V::N(n) => {
                let (t, col) = numfmt::format(*n, &self.format_code(c.s), self.date1904);
                (t, col, true)
            }
            V::S(s) => (numfmt::format_text(s, &self.format_code(c.s)), None, false),
            V::B(b) => ((if *b { "ІСТИНА" } else { "ХИБНІСТЬ" }).into(), None, false),
            V::E(e) => (e.to_string(), None, false),
        }
    }
}

impl Sheet {
    pub fn row(&self, r: u32) -> Option<&Row> {
        self.rows.binary_search_by_key(&r, |x| x.r).ok().map(|i| &self.rows[i])
    }

    pub fn cell(&self, r: u32, c: u32) -> Option<&Cell> {
        let row = self.row(r)?;
        row.cells.binary_search_by_key(&c, |x| x.c).ok().map(|i| &row.cells[i])
    }

    /// Rows r0..=r1 present in the sheet.
    pub fn rows_in(&self, r0: u32, r1: u32) -> &[Row] {
        let a = self.rows.partition_point(|x| x.r < r0);
        let b = self.rows.partition_point(|x| x.r <= r1);
        &self.rows[a..b]
    }
}
