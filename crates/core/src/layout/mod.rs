//! Layout: paragraphs into lines, lines and tables into pages.

mod flow;
pub mod para;
pub mod paint;

pub use flow::{layout_document, Caret, Layout, Page, PathRc, Placed, TableBox};
pub use para::{Line, ParaLayout, Piece};

use crate::docx::Document;
use crate::fonts::Fonts;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Hash, PartialEq, Eq, Clone)]
pub struct CacheKey {
    pub rev: u64,
    pub width: u32,
    pub table: Option<String>,
    pub label: Option<String>,
    /// open comment ranges at the paragraph start
    pub comments: u8,
    /// floating objects around the paragraph (hash)
    pub excl: u64,
    /// numbers of the footnotes the paragraph shows (hash)
    pub notes: u64,
}

#[derive(Default)]
pub struct Cache {
    pub map: HashMap<CacheKey, (u32, Rc<ParaLayout>)>,
    pub epoch: u32,
    /// footnote id → its number (order of the references in the text)
    pub notes: HashMap<String, usize>,
    /// number of the footnote being laid out (its own mark, w:footnoteRef)
    pub cur_note: usize,
    /// comments are not shown (no shading of their ranges)
    pub hide_comments: bool,
}

impl Cache {
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Drops entries not used by the last layout pass.
    pub fn sweep(&mut self) {
        let g = self.epoch;
        self.map.retain(|_, (epoch, _)| *epoch == g);
        self.epoch += 1;
    }
}

pub struct Env<'a> {
    pub doc: &'a Document,
    pub fonts: &'a Fonts,
    pub cache: &'a RefCell<Cache>,
    /// show tracked changes (insertions marked, deletions struck through)
    pub markup: bool,
    /// header/footer being edited and the page it is edited on
    pub story: Option<(String, usize)>,
}

/// Colour of a reviewer, stable per name.
pub fn author_color(name: &str) -> u32 {
    const P: [u32; 6] = [0xB5082E, 0x2E75B6, 0x548235, 0x7030A0, 0xC55A11, 0x1F7A7A];
    let h = name.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    P[(h as usize) % P.len()]
}

/// `RRGGBB` → 0xRRGGBB; None for `auto`.
pub fn color(s: &str) -> Option<u32> {
    if s.len() == 6 {
        u32::from_str_radix(s, 16).ok()
    } else {
        None
    }
}

pub fn highlight(s: &str) -> Option<u32> {
    Some(match s {
        "yellow" => 0xFFFF00,
        "green" => 0x00FF00,
        "cyan" => 0x00FFFF,
        "magenta" => 0xFF00FF,
        "blue" => 0x0000FF,
        "red" => 0xFF0000,
        "darkBlue" => 0x000080,
        "darkCyan" => 0x008080,
        "darkGreen" => 0x008000,
        "darkMagenta" => 0x800080,
        "darkRed" => 0x800000,
        "darkYellow" => 0x808000,
        "darkGray" => 0x808080,
        "lightGray" => 0xC0C0C0,
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        _ => return None,
    })
}
