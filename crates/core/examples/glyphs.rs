//! temporary: characters drawn with a glyph the font does not have (.notdef boxes)
use papir_core::fonts::{invisible, Fonts};
use papir_core::layout::{layout_document, Cache, Env, Piece, Placed};
use std::collections::BTreeMap;
fn main() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fonts");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("{dir}/fonts.json")).unwrap()).unwrap();
    let mut fonts = Fonts::new();
    for f in manifest.as_array().unwrap() {
        let data = std::fs::read(format!("{dir}/{}", f["file"].as_str().unwrap())).unwrap();
        fonts.add(f["family"].as_str().unwrap(), f["bold"].as_bool().unwrap(), f["italic"].as_bool().unwrap(), data).unwrap();
    }
    let mut files = Vec::new();
    for a in std::env::args().skip(1) {
        let p = std::path::PathBuf::from(&a);
        if p.is_dir() { for e in walk(&p) { files.push(e); } } else { files.push(p); }
    }
    let (mut ok, mut bad) = (0, 0);
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        let Ok(doc) = papir_core::Document::open(&bytes) else { continue };
        let cache = std::cell::RefCell::new(Cache::default());
        let env = Env { doc: &doc, fonts: &fonts, cache: &cache, markup: true, story: None };
        let l = layout_document(&env);
        let mut miss: BTreeMap<char, usize> = BTreeMap::new();
        let mut check = |text: &str, face| {
            for c in text.chars() {
                if c.is_whitespace() || invisible(c) || c.is_control() { continue; }
                if fonts.face(face).glyph(c) == 0 { *miss.entry(c).or_default() += 1; }
            }
        };
        for p in &l.pages {
            for it in &p.items {
                match it {
                    Placed::Line { para, idx, .. } => for pc in &para.lines[*idx].pieces {
                        match pc {
                            Piece::Text { text, face, .. } | Piece::Label { text, face, .. } => check(text, *face),
                            _ => {}
                        }
                    },
                    Placed::Text { text, face, .. } => check(text, *face),
                    _ => {}
                }
            }
        }
        if miss.is_empty() { ok += 1; } else {
            bad += 1;
            let list: Vec<String> = miss.iter().map(|(c, n)| format!("U+{:04X}{}x{n}", *c as u32, if c.is_alphanumeric() || c.is_ascii_punctuation() { format!(" '{c}' ") } else { " ".into() })).collect();
            println!("{}: {}", f.display(), list.join(", "));
        }
    }
    println!("clean {ok}, with missing glyphs {bad}");
}
fn walk(d: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(d) { for e in rd.flatten() { let p = e.path(); if p.is_dir() { v.extend(walk(&p)); } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("docx")) { v.push(p); } } }
    v.sort(); v
}
