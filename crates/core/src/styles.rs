//! styles.xml, numbering.xml, theme fonts, settings.

use crate::props::{ParaProps, RunProps, TableProps, twips};
use crate::xml::{Element, Ns};
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct Style {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub based_on: Option<String>,
    pub next: Option<String>,
    pub ppr: ParaProps,
    pub rpr: RunProps,
    pub tbl: TableProps,
    pub default: bool,
    pub hidden: bool,
    pub ui_priority: i32,
    pub q_format: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Styles {
    pub map: HashMap<String, Style>,
    pub order: Vec<String>,
    pub doc_rpr: RunProps,
    pub doc_ppr: ParaProps,
    pub default_para: Option<String>,
    pub default_char: Option<String>,
    pub default_table: Option<String>,
}

impl Styles {
    pub fn parse(root: &Element) -> Styles {
        let mut s = Styles::default();
        if let Some(dd) = root.wchild("docDefaults") {
            if let Some(r) = dd.wchild("rPrDefault").and_then(|x| x.wchild("rPr")) {
                s.doc_rpr = RunProps::parse(r);
            }
            if let Some(p) = dd.wchild("pPrDefault").and_then(|x| x.wchild("pPr")) {
                s.doc_ppr = ParaProps::parse(p);
            }
        }
        for st in root.wchildren("style") {
            let id = match st.wattr("styleId") {
                Some(i) => i.to_string(),
                None => continue,
            };
            let kind = st.wattr("type").unwrap_or("paragraph").to_string();
            let default = matches!(st.wattr("default"), Some("1") | Some("true"));
            let val = |n: &str| st.wchild(n).and_then(|c| c.wattr("val")).map(str::to_string);
            let style = Style {
                id: id.clone(),
                name: val("name").unwrap_or_else(|| id.clone()),
                kind: kind.clone(),
                based_on: val("basedOn"),
                next: val("next"),
                ppr: st.wchild("pPr").map(ParaProps::parse).unwrap_or_default(),
                rpr: st.wchild("rPr").map(RunProps::parse).unwrap_or_default(),
                tbl: st.wchild("tblPr").map(TableProps::parse).unwrap_or_default(),
                default,
                hidden: st.wchild("semiHidden").is_some() || st.wchild("hidden").is_some(),
                ui_priority: val("uiPriority").and_then(|v| v.parse().ok()).unwrap_or(99),
                q_format: st.wchild("qFormat").is_some(),
            };
            if default {
                match kind.as_str() {
                    "paragraph" => s.default_para = Some(id.clone()),
                    "character" => s.default_char = Some(id.clone()),
                    "table" => s.default_table = Some(id.clone()),
                    _ => {}
                }
            }
            s.order.push(id.clone());
            s.map.insert(id, style);
        }
        s
    }

    /// Styles from the root of the `basedOn` chain down to `id`.
    fn chain(&self, id: &str) -> Vec<&Style> {
        let mut v = Vec::new();
        let mut cur = self.map.get(id);
        while let Some(st) = cur {
            if v.len() > 16 || v.iter().any(|x: &&Style| x.id == st.id) {
                break;
            }
            v.push(st);
            cur = st.based_on.as_deref().and_then(|b| self.map.get(b));
        }
        v.reverse();
        v
    }

    pub fn table(&self, id: Option<&str>) -> (TableProps, ParaProps, RunProps) {
        let mut t = TableProps::default();
        let mut p = ParaProps::default();
        let mut r = RunProps::default();
        if let Some(id) = id.or(self.default_table.as_deref()) {
            for st in self.chain(id) {
                t.borders.merge(&st.tbl.borders);
                t.cell_mar.merge(&st.tbl.cell_mar);
                if st.tbl.ind.is_some() {
                    t.ind = st.tbl.ind;
                }
                if st.tbl.jc.is_some() {
                    t.jc = st.tbl.jc.clone();
                }
                p.merge(&st.ppr);
                r.merge(&st.rpr);
            }
        }
        (t, p, r)
    }

    /// Effective paragraph properties: docDefaults → default paragraph style
    /// → table style → the rest of the paragraph style chain → direct
    /// formatting. A table style beats the default style (Normal) but not a
    /// style chosen for the paragraph, as in Word: in a "Table Grid" table,
    /// Normal paragraphs lose their spacing after and take single spacing.
    pub fn para(&self, direct: &ParaProps, table: Option<&ParaProps>) -> ParaProps {
        let mut p = self.doc_ppr.clone();
        let chain = direct.style.as_deref().or(self.default_para.as_deref()).map(|id| self.chain(id)).unwrap_or_default();
        let normal = self.default_para.as_deref();
        let mut table = table;
        if !chain.iter().any(|st| Some(st.id.as_str()) == normal) {
            if let Some(t) = table.take() {
                p.merge(t);
            }
        }
        for st in chain {
            p.merge(&st.ppr);
            if Some(st.id.as_str()) == normal {
                if let Some(t) = table.take() {
                    p.merge(t);
                }
            }
        }
        p.merge(direct);
        p
    }

    /// Effective run properties: docDefaults → table style → paragraph style
    /// chain → character style chain → direct formatting.
    pub fn run(&self, para_style: Option<&str>, table: Option<&RunProps>, direct: &RunProps) -> RunProps {
        let mut r = self.doc_rpr.clone();
        if let Some(t) = table {
            r.merge(t);
        }
        if let Some(id) = para_style.or(self.default_para.as_deref()) {
            for st in self.chain(id) {
                r.merge(&st.rpr);
            }
        }
        if let Some(id) = direct.style.as_deref() {
            for st in self.chain(id) {
                r.merge(&st.rpr);
            }
        }
        let style = r.style.take();
        r.merge(direct);
        r.style = direct.style.clone().or(style);
        r
    }

    /// Paragraph styles to offer in the UI, Word-like order.
    pub fn quick_styles(&self) -> Vec<&Style> {
        let mut v: Vec<&Style> = self
            .order
            .iter()
            .filter_map(|id| self.map.get(id))
            .filter(|s| s.kind == "paragraph" && !s.hidden)
            .collect();
        v.sort_by_key(|s| (!s.q_format, s.ui_priority));
        v
    }
}

#[derive(Clone, Debug, Default)]
pub struct Level {
    pub start: i32,
    pub fmt: String,
    pub text: String,
    pub jc: String,
    pub suff: String,
    pub restart: Option<i32>,
    pub legal: bool,
    pub ppr: ParaProps,
    pub rpr: RunProps,
}

#[derive(Clone, Debug, Default)]
pub struct Numbering {
    pub abs: HashMap<i32, Vec<Option<Level>>>,
    /// numId -> (abstractNumId, per-level start override, per-level level override)
    pub nums: HashMap<i32, (i32, HashMap<i32, i32>, HashMap<i32, Level>)>,
}

fn parse_level(l: &Element) -> Level {
    let val = |n: &str| l.wchild(n).and_then(|c| c.wattr("val")).map(str::to_string);
    Level {
        start: val("start").and_then(|v| v.parse().ok()).unwrap_or(0),
        fmt: val("numFmt").unwrap_or_else(|| "decimal".into()),
        text: val("lvlText").unwrap_or_default(),
        jc: val("lvlJc").unwrap_or_else(|| "left".into()),
        suff: val("suff").unwrap_or_else(|| "tab".into()),
        restart: val("lvlRestart").and_then(|v| v.parse().ok()),
        legal: l.wchild("isLgl").is_some(),
        ppr: l.wchild("pPr").map(ParaProps::parse).unwrap_or_default(),
        rpr: l.wchild("rPr").map(RunProps::parse).unwrap_or_default(),
    }
}

impl Numbering {
    pub fn parse(root: &Element) -> Numbering {
        let mut n = Numbering::default();
        for a in root.wchildren("abstractNum") {
            let id: i32 = a.wattr("abstractNumId").and_then(|v| v.parse().ok()).unwrap_or(-1);
            let mut levels = vec![None; 9];
            for l in a.wchildren("lvl") {
                let i: usize = l.wattr("ilvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                if i < 9 {
                    levels[i] = Some(parse_level(l));
                }
            }
            n.abs.insert(id, levels);
        }
        for num in root.wchildren("num") {
            let id: i32 = num.wattr("numId").and_then(|v| v.parse().ok()).unwrap_or(-1);
            let abs: i32 = num
                .wchild("abstractNumId")
                .and_then(|c| c.wattr("val"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(-1);
            let mut starts = HashMap::new();
            let mut lvls = HashMap::new();
            for o in num.wchildren("lvlOverride") {
                let i: i32 = o.wattr("ilvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                if let Some(s) = o.wchild("startOverride").and_then(|c| c.wattr("val")).and_then(|v| v.parse().ok()) {
                    starts.insert(i, s);
                }
                if let Some(l) = o.wchild("lvl") {
                    lvls.insert(i, parse_level(l));
                }
            }
            n.nums.insert(id, (abs, starts, lvls));
        }
        n
    }

    pub fn level(&self, num_id: i32, ilvl: i32) -> Option<&Level> {
        let (abs, _, over) = self.nums.get(&num_id)?;
        if let Some(l) = over.get(&ilvl) {
            return Some(l);
        }
        self.abs.get(abs)?.get(ilvl as usize)?.as_ref()
    }

    pub fn abstract_of(&self, num_id: i32) -> Option<i32> {
        self.nums.get(&num_id).map(|n| n.0)
    }

    pub fn start(&self, num_id: i32, ilvl: i32) -> i32 {
        if let Some(s) = self.nums.get(&num_id).and_then(|n| n.1.get(&ilvl)) {
            return *s;
        }
        self.level(num_id, ilvl).map(|l| l.start).unwrap_or(1)
    }
}

/// Theme font scheme: (major latin, minor latin).
#[derive(Clone, Debug, Default)]
pub struct ThemeFonts {
    pub major: Option<String>,
    pub minor: Option<String>,
}

impl ThemeFonts {
    pub fn parse(root: &Element) -> ThemeFonts {
        let face = |kind: &str| {
            root.find(Ns::A, kind)
                .and_then(|f| f.child(Ns::A, "latin"))
                .and_then(|l| l.attr(Ns::None, "typeface"))
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        };
        ThemeFonts { major: face("majorFont"), minor: face("minorFont") }
    }

    pub fn resolve(&self, theme: &str) -> Option<&str> {
        if theme.starts_with("major") {
            self.major.as_deref()
        } else {
            self.minor.as_deref()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub default_tab: i32,
    /// w:trackRevisions: edits are recorded as tracked changes
    pub track: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { default_tab: 708, track: false }
    }
}

impl Settings {
    pub fn parse(root: &Element) -> Settings {
        let mut s = Settings::default();
        if let Some(t) = root.wchild("defaultTabStop").and_then(|c| c.wattr("val")).and_then(twips) {
            if t > 0 {
                s.default_tab = t;
            }
        }
        s.track = root.wchild("trackRevisions").is_some_and(|c| c.wattr("val").is_none_or(|v| v != "0" && v != "false"));
        s
    }
}

/// Formats a list counter.
pub fn format_counter(n: i32, fmt: &str) -> String {
    match fmt {
        "lowerLetter" => letters(n, false),
        "upperLetter" => letters(n, true),
        "lowerRoman" => roman(n).to_lowercase(),
        "upperRoman" => roman(n),
        "decimalZero" => format!("{n:02}"),
        "none" => String::new(),
        "bullet" => String::new(),
        "russianLower" | "ukrainianLower" => cyr(n, false),
        "russianUpper" | "ukrainianUpper" => cyr(n, true),
        _ => n.to_string(),
    }
}

fn letters(n: i32, upper: bool) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let c = (b'a' + ((n - 1) % 26) as u8) as char;
    let s: String = std::iter::repeat_n(c, ((n - 1) / 26 + 1) as usize).collect();
    if upper { s.to_uppercase() } else { s }
}

fn cyr(n: i32, upper: bool) -> String {
    const L: &[char] = &['а', 'б', 'в', 'г', 'д', 'е', 'є', 'ж', 'з', 'и', 'і', 'ї', 'й', 'к', 'л', 'м', 'н', 'о', 'п', 'р', 'с', 'т', 'у', 'ф', 'х', 'ц', 'ч', 'ш', 'щ', 'ю', 'я'];
    if n <= 0 {
        return n.to_string();
    }
    let c = L[((n - 1) as usize) % L.len()];
    let s: String = std::iter::repeat_n(c, ((n - 1) as usize) / L.len() + 1).collect();
    if upper { s.to_uppercase() } else { s }
}

fn roman(mut n: i32) -> String {
    if n <= 0 || n > 3999 {
        return n.to_string();
    }
    let t = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut s = String::new();
    for (v, r) in t {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}
