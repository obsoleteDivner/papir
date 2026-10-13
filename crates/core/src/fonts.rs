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
