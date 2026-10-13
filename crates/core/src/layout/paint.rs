//! Page → drawing operations. The same walk feeds the canvas display
//! list (JSON) and the PDF writer, so screen and PDF match.

use super::flow::{Page, Placed};
use super::para::{Piece, DECO_S, DECO_SS, DECO_U, DECO_UU};
use crate::fonts::{FaceId, Fonts};
use std::fmt::Write;

/// Drawing target. Coordinates in points from the page's top-left;
/// `y` of text is the baseline.
pub trait Sink {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32);
    /// style: 0 solid, 1 double, 2 dotted, 3 dashed
    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, color: u32, style: u8);
    /// `adv`: advance of every char as laid out (None: the font's own)
    fn text(&mut self, x: f32, y: f32, text: &str, face: FaceId, size: f32, color: u32, adv: Option<&[f32]>);
    fn image(&mut self, x: f32, y: f32, w: f32, h: f32, src: Option<&str>);
    fn poly(&mut self, pts: &[(f32, f32)], color: u32);
}

pub fn render(page: &Page, fonts: &Fonts, sink: &mut dyn Sink) {
    for it in &page.items {
        match it {
            Placed::Rect { x, y, w, h, color } => sink.rect(*x, *y, *w, *h, *color),
            Placed::Stroke { x1, y1, x2, y2, w, color, style } => sink.line(*x1, *y1, *x2, *y2, *w, *color, *style),
            Placed::Image { x, y, w, h, src } => sink.image(*x, *y, *w, *h, src.as_deref()),
            Placed::Text { x, y, text, face, size, color } => sink.text(*x, *y, text, *face, *size, *color, None),
            Placed::Poly { pts, color } => sink.poly(pts, *color),
            Placed::Line { para, idx, x, y, .. } => {
                let l = &para.lines[*idx];
                let base = y + l.base;
                if l.changed {
                    // change bar in the left margin
                    sink.line(x - 14.0, *y, x - 14.0, y + l.h, 0.75, 0x606060, 0);
                }
                for p in &l.pieces {
                    match p {
                        Piece::Text { x: px, text, face, size, color, adv, dy, deco, hl, .. } => {
                            let w: f32 = adv.iter().sum();
                            let f = fonts.face(*face);
                            if let Some(h) = hl {
                                sink.rect(x + px, base - f.ascent * size, w, (f.ascent + f.descent) * size, *h);
                            }
                            sink.text(x + px, base + dy, text, *face, *size, *color, Some(adv));
                            decorate(sink, x + px, w, base + dy, *size, f.underline_pos, f.underline_thick, *deco, *color);
                        }
                        Piece::Label { x: px, text, face, size, color, dy } => sink.text(x + px, base + dy, text, *face, *size, *color, None),
                        Piece::Tab { x: px, w, leader, face, size, color, deco, .. } => {
                            if let Some(ch) = leader {
                                let cw = fonts.width(*face, *ch, *size).max(0.5);
                                let n = ((w - cw * 0.5) / cw).floor().max(0.0) as usize;
                                if n > 0 {
                                    let s: String = std::iter::repeat_n(*ch, n).collect();
                                    sink.text(x + px + w - n as f32 * cw, base, &s, *face, *size, *color, None);
                                }
                            }
                            let f = fonts.face(*face);
                            decorate(sink, x + px, *w, base, *size, f.underline_pos, f.underline_thick, *deco, *color);
                        }
                        // charts are drawn by the layout next to the line
                        Piece::Image { chart: Some(_), .. } => {}
                        Piece::Image { x: px, w, h, src, .. } => sink.image(x + px, base - h, *w, *h, src.as_deref()),
                        Piece::Mark { .. } => {}
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decorate(sink: &mut dyn Sink, x: f32, w: f32, base: f32, size: f32, upos: f32, uth: f32, deco: u8, color: u32) {
    if w <= 0.0 || deco == 0 {
        return;
    }
    let th = (uth * size).max(0.5);
    let uy = base + (upos * size).max(th);
    if deco & DECO_U != 0 {
        sink.line(x, uy, x + w, uy, th, color, 0);
    }
    if deco & DECO_UU != 0 {
        sink.line(x, uy, x + w, uy, th, color, 0);
        sink.line(x, uy + th * 2.0, x + w, uy + th * 2.0, th, color, 0);
    }
    let sy = base - size * 0.28;
    if deco & DECO_S != 0 {
        sink.line(x, sy, x + w, sy, th, color, 0);
    }
    if deco & DECO_SS != 0 {
        sink.line(x, sy - th, x + w, sy - th, th, color, 0);
        sink.line(x, sy + th, x + w, sy + th, th, color, 0);
    }
}

fn hex(c: u32) -> String {
    format!("#{:06x}", c & 0xFFFFFF)
}

fn num(out: &mut String, v: f32) {
    let _ = write!(out, "{}", (v * 100.0).round() / 100.0);
}

fn jstr(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Canvas display list.
struct Json {
    o: String,
    first: bool,
}

impl Json {
    fn sep(&mut self) {
        if !self.first {
            self.o.push(',');
        }
        self.first = false;
    }
}

impl Sink for Json {
    fn poly(&mut self, pts: &[(f32, f32)], c: u32) {
        self.sep();
        self.o.push_str("[\"p\",");
        jstr(&mut self.o, &hex(c));
        for (x, y) in pts {
            self.o.push(',');
            num(&mut self.o, *x);
            self.o.push(',');
            num(&mut self.o, *y);
        }
        self.o.push(']');
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: u32) {
        self.sep();
        self.o.push_str("[\"r\",");
        for v in [x, y, w, h] {
            num(&mut self.o, v);
            self.o.push(',');
        }
        jstr(&mut self.o, &hex(c));
        self.o.push(']');
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, c: u32, st: u8) {
        self.sep();
        self.o.push_str("[\"l\",");
        for v in [x1, y1, x2, y2, w] {
            num(&mut self.o, v);
            self.o.push(',');
        }
        jstr(&mut self.o, &hex(c));
        let _ = write!(self.o, ",{st}]");
    }

    fn text(&mut self, x: f32, y: f32, t: &str, face: FaceId, size: f32, c: u32, _adv: Option<&[f32]>) {
        self.sep();
        self.o.push_str("[\"t\",");
        num(&mut self.o, x);
        self.o.push(',');
        num(&mut self.o, y);
        self.o.push(',');
        jstr(&mut self.o, t);
        let _ = write!(self.o, ",{face},");
        num(&mut self.o, size);
        self.o.push(',');
        jstr(&mut self.o, &hex(c));
        self.o.push(']');
    }

    fn image(&mut self, x: f32, y: f32, w: f32, h: f32, src: Option<&str>) {
        self.sep();
        self.o.push_str("[\"i\",");
        for v in [x, y, w, h] {
            num(&mut self.o, v);
            self.o.push(',');
        }
        jstr(&mut self.o, src.unwrap_or(""));
        self.o.push(']');
    }
}

/// Items: `["r",x,y,w,h,color]`, `["l",x1,y1,x2,y2,width,color,style]`,
/// `["t",x,y,text,face,size,color]`, `["i",x,y,w,h,src]`, `["p",color,x1,y1,x2,y2,...]`.
pub fn display_list(page: &Page, fonts: &Fonts) -> String {
    let mut j = Json { o: String::with_capacity(4096), first: true };
    j.o.push_str("{\"w\":");
    num(&mut j.o, page.w);
    j.o.push_str(",\"h\":");
    num(&mut j.o, page.h);
    j.o.push_str(",\"items\":[");
    render(page, fonts, &mut j);
    j.o.push_str("]}");
    j.o
}
