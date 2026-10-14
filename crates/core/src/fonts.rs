//! Font registry: faces from TrueType/OpenType bytes, metric-compatible
//! substitution and per-character fallback.

use std::cell::RefCell;
use std::collections::HashMap;

pub type FaceId = u16;

pub struct Face {
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    /// the font file, for embedding
    pub data: &'static [u8],
    face: ttf_parser::Face<'static>,
    upem: f32,
    /// em fractions
    pub ascent: f32,
    pub descent: f32,
    pub underline_pos: f32,
    pub underline_thick: f32,
    adv: RefCell<HashMap<char, Option<f32>>>,
}

impl Face {
    /// Advance of `c` as a fraction of the em, or None without a glyph.
    pub fn advance(&self, c: char) -> Option<f32> {
        if let Some(a) = self.adv.borrow().get(&c) {
            return *a;
        }
        let a = self
            .face
            .glyph_index(c)
            .filter(|g| g.0 != 0)
            .map(|g| self.face.glyph_hor_advance(g).unwrap_or(0) as f32 / self.upem);
        self.adv.borrow_mut().insert(c, a);
        a
    }

    pub fn has(&self, c: char) -> bool {
        self.advance(c).is_some()
    }

    /// Glyph id for `c` (0, the .notdef glyph, when missing).
    pub fn glyph(&self, c: char) -> u16 {
        self.face.glyph_index(c).map(|g| g.0).unwrap_or(0)
    }

    /// Advance of a glyph, em fraction.
    pub fn glyph_advance(&self, gid: u16) -> f32 {
        self.face.glyph_hor_advance(ttf_parser::GlyphId(gid)).unwrap_or(0) as f32 / self.upem
    }

    pub fn units_per_em(&self) -> f32 {
        self.upem
    }

    pub fn postscript_name(&self) -> String {
        self.face
            .names()
            .into_iter()
            .filter(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            // the first record may be in a Mac encoding: take one that reads
            .find_map(|n| n.to_string())
            .unwrap_or_else(|| self.family.replace(' ', ""))
    }

    /// (bbox in em, italic angle, cap height in em, serif)
    pub fn pdf_metrics(&self) -> ([f32; 4], f32, f32, bool) {
        let b = self.face.global_bounding_box();
        let u = self.upem;
        let cap = self.face.capital_height().map(|c| c as f32 / u).unwrap_or(self.ascent * 0.7);
        let serif = !self.family.to_lowercase().contains("sans") && !self.family.to_lowercase().contains("mono");
        ([b.x_min as f32 / u, b.y_min as f32 / u, b.x_max as f32 / u, b.y_max as f32 / u], self.face.italic_angle(), cap, serif)
    }
}

/// Word-compatible substitutes shipped with the editor.
const ALIASES: &[(&str, &str)] = &[
    ("times new roman", "Liberation Serif"),
    ("times", "Liberation Serif"),
    ("arial", "Liberation Sans"),
    ("helvetica", "Liberation Sans"),
    ("arial narrow", "Liberation Sans Narrow"),
    ("courier new", "Liberation Mono"),
    ("courier", "Liberation Mono"),
    ("calibri", "Carlito"),
    ("calibri light", "Carlito"),
    ("cambria", "Caladea"),
    ("cambria math", "Caladea"),
    ("verdana", "DejaVu Sans"),
    ("tahoma", "DejaVu Sans"),
    ("segoe ui", "DejaVu Sans"),
    ("symbol", "DejaVu Sans"),
    ("wingdings", "DejaVu Sans"),
    ("georgia", "DejaVu Serif"),
    ("book antiqua", "DejaVu Serif"),
    ("garamond", "Liberation Serif"),
    ("bookman old style", "DejaVu Serif"),
    ("consolas", "Liberation Mono"),
];

pub const DEFAULT_FAMILY: &str = "Liberation Serif";
pub const FALLBACK: &[&str] = &["DejaVu Sans", "Liberation Sans", "Liberation Serif"];

/// Family among `available` that renders `name` (metric-compatible
/// substitute), or None. Case-insensitive; `name` may list alternatives.
pub fn substitute<'a>(name: &str, available: &'a [String]) -> Option<&'a str> {
    let find = |n: &str| available.iter().find(|a| a.eq_ignore_ascii_case(n)).map(|s| s.as_str());
    for part in name.split(';') {
        let key = part.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        if let Some(a) = find(&key) {
            return Some(a);
        }
        if let Some((_, to)) = ALIASES.iter().find(|(from, _)| *from == key) {
            if let Some(a) = find(to) {
                return Some(a);
            }
        }
    }
    None
}

/// Families a document needs: every font it names, mapped to `available`,
/// plus the default and the first fallback.
pub fn needed_families(doc: &crate::Document, available: &[String]) -> Vec<String> {
    use crate::model::{Block, Container, Inline};
    use crate::props::RunProps;
    let mut names: Vec<String> = Vec::new();
    let mut add = |r: &RunProps| {
        for n in [&r.font_ascii, &r.font_hansi, &r.font_cs].into_iter().flatten() {
            names.push(n.clone());
        }
        for t in [&r.theme_ascii, &r.theme_hansi].into_iter().flatten() {
            if let Some(n) = doc.theme.resolve(t) {
                names.push(n.to_string());
            }
        }
    };
    add(&doc.styles.doc_rpr);
    for st in doc.styles.map.values() {
        add(&st.rpr);
    }
    for levels in doc.numbering.abs.values() {
        for l in levels.iter().flatten() {
            add(&l.rpr);
        }
    }
    fn walk(c: &Container, add: &mut dyn FnMut(&RunProps)) {
        for b in c {
            match &**b {
                Block::Para(p) => {
                    add(&p.mark);
                    for i in &p.inlines {
                        if let Inline::Run(r) = i {
                            add(&r.props);
                        }
                    }
                }
                Block::Table(t) => {
                    for r in t.rows() {
                        for cell in r.cells() {
                            walk(&cell.content, add);
                        }
                    }
                }
                Block::Sdt(s) => walk(&s.content, add),
                Block::Raw(_) => {}
            }
        }
    }
    walk(&doc.body, &mut add);
    for st in doc.stories.values() {
        walk(&st.content, &mut add);
    }
    // Word's default when nothing is named
    names.push("Times New Roman".into());
    // chart labels
    if !doc.charts.is_empty() {
        names.push("Calibri".into());
    }
    let mut out: Vec<String> = vec![DEFAULT_FAMILY.to_string(), FALLBACK[0].to_string()];
    for n in names {
        if let Some(f) = substitute(&n, available) {
            if !out.iter().any(|o| o == f) {
                out.push(f.to_string());
            }
        }
    }
    out.retain(|f| available.iter().any(|a| a == f));
    out
}

#[derive(Default)]
pub struct Fonts {
    pub faces: Vec<Face>,
    families: HashMap<String, Vec<FaceId>>,
    select_cache: RefCell<HashMap<(String, bool, bool), FaceId>>,
}

impl Fonts {
    pub fn new() -> Fonts {
        Fonts::default()
    }

    /// Registers a face. The bytes live as long as the process (fonts are
    /// loaded once), so the parsed face can borrow them for `'static`.
    pub fn add(&mut self, family: &str, bold: bool, italic: bool, data: Vec<u8>) -> Result<FaceId, String> {
        self.add_static(family, bold, italic, Box::leak(data.into_boxed_slice()))
    }

    /// Registers a face from bytes that live for the whole process (shared
    /// between threads that each build their own registry).
    pub fn add_static(&mut self, family: &str, bold: bool, italic: bool, data: &'static [u8]) -> Result<FaceId, String> {
        let face = ttf_parser::Face::parse(data, 0).map_err(|e| e.to_string())?;
        let upem = face.units_per_em() as f32;
        let (wa, wd) = face
            .tables()
            .os2
            .map(|o| (o.windows_ascender() as f32, o.windows_descender() as f32))
            .unwrap_or((face.ascender() as f32, -(face.descender() as f32)));
        let (ha, hd, gap) = (face.ascender() as f32, face.descender() as f32, face.line_gap() as f32);
        // GDI: tmHeight = winAscent + winDescent,
        // tmExternalLeading = max(0, lineGap - (tmHeight - (hheaAscent - hheaDescent)))
        let ext = (gap - ((wa + wd) - (ha - hd))).max(0.0);
        let ul = face.underline_metrics();
        let f = Face {
            family: family.to_string(),
            bold,
            italic,
            data,
            upem,
            ascent: wa / upem,
            descent: (wd + ext) / upem,
            underline_pos: ul.map(|u| -(u.position as f32) / upem).unwrap_or(0.1),
            underline_thick: ul.map(|u| u.thickness as f32 / upem).unwrap_or(0.05),
            face,
            adv: RefCell::new(HashMap::new()),
        };
        let id = self.faces.len() as FaceId;
        self.faces.push(f);
        self.families.entry(family.to_lowercase()).or_default().push(id);
        self.select_cache.borrow_mut().clear();
        Ok(id)
    }

    pub fn face(&self, id: FaceId) -> &Face {
        &self.faces[id as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    pub fn has_family(&self, name: &str) -> bool {
        self.families.contains_key(&name.to_lowercase())
    }

    fn pick(&self, ids: &[FaceId], bold: bool, italic: bool) -> FaceId {
        let score = |id: &FaceId| {
            let f = self.face(*id);
            (f.bold == bold) as u8 * 2 + (f.italic == italic) as u8
        };
        *ids.iter().max_by_key(|id| score(id)).expect("non-empty family")
    }

    /// Family name actually used for `name`. A name may list alternatives
    /// separated by `;` (e.g. "Liberation Sans;Arial").
    pub fn resolve_family(&self, name: &str) -> String {
        for part in name.split(';') {
            let key = part.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            if self.families.contains_key(&key) {
                return part.trim().to_string();
            }
            if let Some((_, to)) = ALIASES.iter().find(|(from, _)| *from == key) {
                if self.families.contains_key(&to.to_lowercase()) {
                    return to.to_string();
                }
            }
        }
        DEFAULT_FAMILY.to_string()
    }

    pub fn select(&self, name: &str, bold: bool, italic: bool) -> FaceId {
        let k = (name.to_string(), bold, italic);
        if let Some(id) = self.select_cache.borrow().get(&k) {
            return *id;
        }
        let fam = self.resolve_family(name).to_lowercase();
        let ids = self
            .families
            .get(&fam)
            .or_else(|| self.families.get(&DEFAULT_FAMILY.to_lowercase()))
            .or_else(|| self.families.values().next())
            .expect("no fonts loaded");
        let id = self.pick(ids, bold, italic);
        self.select_cache.borrow_mut().insert(k, id);
        id
    }

    /// Face to draw `c` with: `primary` if it has the glyph, else a fallback.
    pub fn for_char(&self, primary: FaceId, c: char) -> FaceId {
        if c.is_control() || invisible(c) || self.face(primary).has(c) {
            return primary;
        }
        let f = self.face(primary);
        for fam in FALLBACK {
            if let Some(ids) = self.families.get(&fam.to_lowercase()) {
                let id = self.pick(ids, f.bold, f.italic);
                if self.face(id).has(c) {
                    return id;
                }
            }
        }
        primary
    }

    /// Advance width in points.
    pub fn width(&self, face: FaceId, c: char, size: f32) -> f32 {
        if invisible(c) {
            return 0.0;
        }
        let f = self.face(face);
        f.advance(c).or_else(|| f.advance('?')).unwrap_or(0.5) * size
    }
}

/// Characters that are never drawn (Unicode default-ignorable: zero-width
/// joiners and spaces, direction marks, variation selectors, BOM, the soft
/// hyphen...). Word shows nothing for them; a font has no glyph and would draw
/// its .notdef box (or, for the soft hyphen, a hyphen in the middle of a word).
pub fn invisible(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F | 0x200B..=0x200F | 0x202A..=0x202E
        | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFF8 | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

/// What a character of a symbol font (Symbol, Wingdings) stands for in
/// Unicode, as Word draws it. Word keeps these as private-use code points
/// (U+F0xx) or plain codes in a run set in that font; our fonts have the
/// Unicode characters. None: not a symbol font, the character stays. A code
/// without an equivalent maps to U+200B and is not drawn (never a box).
pub fn symbol_char(font: &str, c: char) -> Option<char> {
    let font = font.trim().to_ascii_lowercase();
    let table: &[(u8, char)] = match font.as_str() {
        "symbol" => SYMBOL,
        "wingdings" => WINGDINGS,
        "wingdings 2" => WINGDINGS2,
        "wingdings 3" | "webdings" => &[],
        _ => return None,
    };
    let mut u = c as u32;
    if (0xF020..=0xF0FF).contains(&u) {
        u -= 0xF000;
    }
    if !(0x20..=0xFF).contains(&u) {
        return Some(c);
    }
    if u == 0x20 {
        return Some(' ');
    }
    Some(table.iter().find(|(k, _)| *k as u32 == u).map(|(_, v)| *v).unwrap_or('\u{200B}'))
}

const SYMBOL: &[(u8, char)] = &[
    (0x21, '!'), (0x22, '∀'), (0x23, '#'), (0x24, '∃'), (0x25, '%'), (0x26, '&'), (0x27, '∋'), (0x28, '('), (0x29, ')'), (0x2A, '∗'), (0x2B, '+'), (0x2C, ','), (0x2D, '−'), (0x2E, '.'), (0x2F, '/'),
    (0x30, '0'), (0x31, '1'), (0x32, '2'), (0x33, '3'), (0x34, '4'), (0x35, '5'), (0x36, '6'), (0x37, '7'), (0x38, '8'), (0x39, '9'), (0x3A, ':'), (0x3B, ';'), (0x3C, '<'), (0x3D, '='), (0x3E, '>'), (0x3F, '?'),
    (0x40, '≅'), (0x41, 'Α'), (0x42, 'Β'), (0x43, 'Χ'), (0x44, 'Δ'), (0x45, 'Ε'), (0x46, 'Φ'), (0x47, 'Γ'), (0x48, 'Η'), (0x49, 'Ι'), (0x4A, 'ϑ'), (0x4B, 'Κ'), (0x4C, 'Λ'), (0x4D, 'Μ'), (0x4E, 'Ν'), (0x4F, 'Ο'),
    (0x50, 'Π'), (0x51, 'Θ'), (0x52, 'Ρ'), (0x53, 'Σ'), (0x54, 'Τ'), (0x55, 'Υ'), (0x56, 'ς'), (0x57, 'Ω'), (0x58, 'Ξ'), (0x59, 'Ψ'), (0x5A, 'Ζ'), (0x5B, '['), (0x5C, '∴'), (0x5D, ']'), (0x5E, '⊥'), (0x5F, '_'),
    (0x61, 'α'), (0x62, 'β'), (0x63, 'χ'), (0x64, 'δ'), (0x65, 'ε'), (0x66, 'φ'), (0x67, 'γ'), (0x68, 'η'), (0x69, 'ι'), (0x6A, 'ϕ'), (0x6B, 'κ'), (0x6C, 'λ'), (0x6D, 'μ'), (0x6E, 'ν'), (0x6F, 'ο'),
    (0x70, 'π'), (0x71, 'θ'), (0x72, 'ρ'), (0x73, 'σ'), (0x74, 'τ'), (0x75, 'υ'), (0x76, 'ϖ'), (0x77, 'ω'), (0x78, 'ξ'), (0x79, 'ψ'), (0x7A, 'ζ'), (0x7B, '{'), (0x7C, '|'), (0x7D, '}'), (0x7E, '∼'),
    (0xA0, '€'), (0xA1, 'ϒ'), (0xA2, '′'), (0xA3, '≤'), (0xA4, '⁄'), (0xA5, '∞'), (0xA6, 'ƒ'), (0xA7, '♣'), (0xA8, '♦'), (0xA9, '♥'), (0xAA, '♠'), (0xAB, '↔'), (0xAC, '←'), (0xAD, '↑'), (0xAE, '→'), (0xAF, '↓'),
    (0xB0, '°'), (0xB1, '±'), (0xB2, '″'), (0xB3, '≥'), (0xB4, '×'), (0xB5, '∝'), (0xB6, '∂'), (0xB7, '•'), (0xB8, '÷'), (0xB9, '≠'), (0xBA, '≡'), (0xBB, '≈'), (0xBC, '…'), (0xBD, '⏐'), (0xBE, '⎯'), (0xBF, '↵'),
    (0xC0, 'ℵ'), (0xC1, 'ℑ'), (0xC2, 'ℜ'), (0xC3, '℘'), (0xC4, '⊗'), (0xC5, '⊕'), (0xC6, '∅'), (0xC7, '∩'), (0xC8, '∪'), (0xC9, '⊃'), (0xCA, '⊇'), (0xCB, '⊄'), (0xCC, '⊂'), (0xCD, '⊆'), (0xCE, '∈'), (0xCF, '∉'),
    (0xD0, '∠'), (0xD1, '∇'), (0xD2, '®'), (0xD3, '©'), (0xD4, '™'), (0xD5, '∏'), (0xD6, '√'), (0xD7, '⋅'), (0xD8, '¬'), (0xD9, '∧'), (0xDA, '∨'), (0xDB, '⇔'), (0xDC, '⇐'), (0xDD, '⇑'), (0xDE, '⇒'), (0xDF, '⇓'),
    (0xE0, '◊'), (0xE1, '〈'), (0xE2, '®'), (0xE3, '©'), (0xE4, '™'), (0xE5, '∑'), (0xE6, '⎛'), (0xE7, '⎜'), (0xE8, '⎝'), (0xE9, '⎡'), (0xEA, '⎢'), (0xEB, '⎣'), (0xEC, '⎧'), (0xED, '⎨'), (0xEE, '⎩'), (0xEF, '⎪'),
    (0xF1, '〉'), (0xF2, '∫'), (0xF3, '⌠'), (0xF4, '⎮'), (0xF5, '⌡'), (0xF6, '⎞'), (0xF7, '⎟'), (0xF8, '⎠'), (0xF9, '⎤'), (0xFA, '⎥'), (0xFB, '⎦'), (0xFC, '⎫'), (0xFD, '⎬'), (0xFE, '⎭'),
];

// the characters our fonts have for what Wingdings draws (bullets, boxes, ticks, arrows, hands, numbers)
const WINGDINGS: &[(u8, char)] = &[
    (0x21, '✏'), (0x22, '✂'), (0x23, '✁'), (0x28, '☎'), (0x29, '✆'), (0x2A, '✉'), (0x36, '⌛'), (0x3E, '✇'), (0x3F, '✍'),
    (0x41, '✌'), (0x45, '☜'), (0x46, '☞'), (0x47, '☝'), (0x48, '☟'), (0x4A, '☺'), (0x4C, '☹'), (0x4E, '☠'), (0x51, '✈'), (0x52, '☼'), (0x54, '❄'), (0x56, '✞'), (0x58, '✠'), (0x59, '✡'), (0x5A, '☪'), (0x5B, '☯'), (0x5E, '♈'),
    (0x6C, '●'), (0x6D, '○'), (0x6E, '■'), (0x6F, '□'), (0x70, '◻'), (0x71, '❑'), (0x72, '❒'), (0x73, '⬧'), (0x74, '⧫'), (0x75, '◆'), (0x76, '❖'), (0x77, '⬥'), (0x78, '⌧'), (0x7A, '⌘'),
    (0x80, '⓪'), (0x81, '①'), (0x82, '②'), (0x83, '③'), (0x84, '④'), (0x85, '⑤'), (0x86, '⑥'), (0x87, '⑦'), (0x88, '⑧'), (0x89, '⑨'), (0x8A, '⑩'), (0x8B, '⓿'),
    (0x8C, '❶'), (0x8D, '❷'), (0x8E, '❸'), (0x8F, '❹'), (0x90, '❺'), (0x91, '❻'), (0x92, '❼'), (0x93, '❽'), (0x94, '❾'), (0x95, '❿'),
    (0x9E, '·'), (0x9F, '•'), (0xA0, '▪'), (0xA1, '○'), (0xA4, '◉'), (0xA5, '◎'), (0xA7, '▪'), (0xA8, '◻'), (0xAA, '✦'), (0xAB, '★'), (0xAC, '✶'), (0xAD, '✴'), (0xAE, '✹'), (0xAF, '✵'), (0xB1, '⌖'), (0xB5, '✪'), (0xB6, '✰'),
    (0xD5, '⌫'), (0xD6, '⌦'), (0xD7, '➢'), (0xD8, '➢'), (0xD9, '➢'), (0xDA, '➢'), (0xDF, '←'), (0xE0, '→'), (0xE1, '↑'), (0xE2, '↓'), (0xE3, '↖'), (0xE4, '↗'), (0xE5, '↙'), (0xE6, '↘'),
    (0xE7, '←'), (0xE8, '➔'), (0xE9, '↑'), (0xEA, '↓'), (0xEB, '↖'), (0xEC, '↗'), (0xED, '↙'), (0xEE, '↘'), (0xEF, '⇦'), (0xF0, '⇨'), (0xF1, '⇧'), (0xF2, '⇩'), (0xF3, '⬄'), (0xF4, '⇳'),
    (0xFB, '✗'), (0xFC, '✔'), (0xFD, '☒'), (0xFE, '☑'),
];

// Wingdings 2: what forms use (ticks, crosses, check boxes, bullets)
const WINGDINGS2: &[(u8, char)] = &[
    (0x4F, '✗'), (0x50, '✓'), (0x51, '☒'), (0x52, '☑'), (0x53, '☒'), (0x54, '☒'), (0x55, '☒'), (0x56, '☒'), (0x97, '•'), (0x98, '●'), (0x99, '●'), (0x9A, '○'), (0xA2, '□'), (0xA3, '☐'), (0xA4, '◻'), (0xA5, '◼'),
];
