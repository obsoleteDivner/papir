//! Lays out .docx files and prints page counts and timings.
//! usage: cargo run --release --example pages -- file.docx...

use papir_core::fonts::Fonts;
use papir_core::layout::{layout_document, Cache, Env};
use std::cell::RefCell;

fn main() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fonts");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("{dir}/fonts.json")).unwrap()).unwrap();
    let mut fonts = Fonts::new();
    for f in manifest.as_array().unwrap() {
        let data = std::fs::read(format!("{dir}/{}", f["file"].as_str().unwrap())).unwrap();
        fonts.add(f["family"].as_str().unwrap(), f["bold"].as_bool().unwrap(), f["italic"].as_bool().unwrap(), data).unwrap();
    }
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let t0 = std::time::Instant::now();
        let doc = match papir_core::Document::open(&bytes) {
            Ok(d) => d,
            Err(e) => {
                println!("{path}: error {}", e.code());
                continue;
            }
        };
        let t1 = t0.elapsed();
        let cache = RefCell::new(Cache::default());
        let env = Env { doc: &doc, fonts: &fonts, cache: &cache, markup: true, story: None };
        let l = layout_document(&env);
        let t2 = t0.elapsed();
        // second pass hits the cache
        let _ = layout_document(&env);
        let t3 = t0.elapsed() - t2;
        let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy();
        println!("{name}\tpages={}\tparse={:?}\tlayout={:?}\trelayout={:?}", l.pages.len(), t1, t2 - t1, t3);
    }
}
