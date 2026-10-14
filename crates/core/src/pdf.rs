//! Layout → PDF 1.7. Text stays text: every face is embedded as a subset
//! (CIDFontType2, Identity-H) with a ToUnicode map, so text can be
//! selected, searched and copied, Cyrillic included.

use crate::docx::Document;
use crate::fonts::{FaceId, Fonts};
use crate::layout::paint::{render, Sink};
use crate::layout::Layout;
use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
use pdf_writer::{Content, Date, Filter, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;

struct FontUse {
    remap: subsetter::GlyphRemapper,
    /// new glyph id → the char it stands for
    unicode: BTreeMap<u16, char>,
    name: String,
}

/// A picture ready to embed: decoded once, written after the pages.
enum Pixels2 {
    Jpeg { w: u32, h: u32, comps: u8 },
    Raw { px: Vec<u8>, alpha: Option<Vec<u8>>, w: u32, h: u32, gray: bool },
}

struct Img {
    id: Ref,
    name: String,
    data: Pixels2,
}

struct PageSink<'a> {
    fonts: &'a Fonts,
    doc: &'a Document,
    w: f32,
    h: f32,
    c: Content,
    used: &'a mut HashMap<FaceId, FontUse>,
    images: &'a mut HashMap<String, Option<Img>>,
    pending: &'a mut Vec<String>,
    next: &'a mut i32,
    page_fonts: Vec<FaceId>,
    page_images: Vec<String>,
}

fn rgb(c: u32) -> (f32, f32, f32) {
    (((c >> 16) & 0xFF) as f32 / 255.0, ((c >> 8) & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0)
}

impl Sink for PageSink<'_> {
    fn poly(&mut self, pts: &[(f32, f32)], color: u32) {
        let Some((first, rest)) = pts.split_first() else { return };
        let (r, g, b) = rgb(color);
        self.c.set_fill_rgb(r, g, b);
        self.c.move_to(first.0, self.h - first.1);
        for (x, y) in rest {
            self.c.line_to(*x, self.h - y);
        }
        self.c.close_path();
        self.c.fill_nonzero();
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        let (r, g, b) = rgb(color);
        self.c.set_fill_rgb(r, g, b);
        self.c.rect(x, self.h - y - h, w, h);
        self.c.fill_nonzero();
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, color: u32, style: u8) {
        let (r, g, b) = rgb(color);
        self.c.set_stroke_rgb(r, g, b);
        if style == 1 {
            // double: two thin lines
            let t = (w / 3.0).max(0.25);
            let (dx, dy) = if (y1 - y2).abs() < 0.01 { (0.0, w) } else { (w, 0.0) };
            self.c.set_line_width(t);
            for s in [-1.0, 1.0] {
                self.c.move_to(x1 + s * dx, self.h - (y1 + s * dy));
                self.c.line_to(x2 + s * dx, self.h - (y2 + s * dy));
                self.c.stroke();
            }
            return;
        }
        match style {
            2 => {
                self.c.set_dash_pattern([w, w * 2.0], 0.0);
            }
            3 => {
                self.c.set_dash_pattern([w * 4.0, w * 3.0], 0.0);
            }
            _ => {}
        }
        self.c.set_line_width(w);
        self.c.move_to(x1, self.h - y1);
        self.c.line_to(x2, self.h - y2);
        self.c.stroke();
        if style == 2 || style == 3 {
            self.c.set_dash_pattern([], 0.0);
        }
    }

    fn text(&mut self, x: f32, y: f32, text: &str, face: FaceId, size: f32, color: u32, adv: Option<&[f32]>) {
        if text.chars().all(|c| c.is_whitespace() || crate::fonts::invisible(c)) {
            return;
        }
        let f = self.fonts.face(face);
        let used = self.used.entry(face).or_insert_with(|| FontUse {
            remap: subsetter::GlyphRemapper::new(),
            unicode: BTreeMap::new(),
            name: format!("F{face}"),
        });
        if !self.page_fonts.contains(&face) {
            self.page_fonts.push(face);
        }
        let (r, g, b) = rgb(color);
        self.c.set_fill_rgb(r, g, b);
        self.c.begin_text();
        self.c.set_font(Name(used.name.as_bytes()), size);
        self.c.set_text_matrix([1.0, 0.0, 0.0, 1.0, x, self.h - y]);
        {
            let mut pos = self.c.show_positioned();
            let mut items = pos.items();
            let mut run: Vec<u8> = Vec::new();
            for (i, ch) in text.chars().enumerate() {
                if crate::fonts::invisible(ch) {
                    continue;
                }
                let gid = f.glyph(ch);
                // a character no font has: nothing rather than a .notdef box
                if gid == 0 {
                    continue;
                }
                let cid = used.remap.remap(gid);
                used.unicode.entry(cid).or_insert(ch);
                run.extend_from_slice(&cid.to_be_bytes());
                // keep the laid-out position: TJ numbers move left, in 1/1000 em
                if let Some(a) = adv.and_then(|a| a.get(i)) {
                    let adj = (f.glyph_advance(gid) * size - a) / size * 1000.0;
                    if adj.abs() > 0.5 {
                        items.show(Str(&run));
                        run.clear();
                        items.adjust(adj);
                    }
                }
            }
            if !run.is_empty() {
                items.show(Str(&run));
            }
        }
        self.c.end_text();
    }

    fn image(&mut self, x: f32, y: f32, w: f32, h: f32, src: Option<&str>) {
        let Some(src) = src else { return };
        let doc = self.doc;
        // pixels worth keeping: 300 per inch of what can show on the page
        let fit = |v: f32, page: f32| (v.abs().min(page) / 72.0 * 300.0).ceil().max(1.0) as u32;
        let fit = (fit(w, self.w), fit(h, self.h));
        let next = &mut *self.next;
        let pending = &mut *self.pending;
        let entry = self.images.entry(src.to_string()).or_insert_with(|| {
            // decoded now: a picture that cannot be embedded is never referenced
            let bytes = doc.media(src)?;
            let data = if is_jpeg(bytes) {
                let (w, h, comps) = jpeg_info(bytes)?;
                Pixels2::Jpeg { w, h, comps }
            } else if is_png(bytes) {
                let (px, alpha, w, h, gray) = png_pixels(bytes, fit)?;
                Pixels2::Raw { px, alpha, w, h, gray }
            } else {
                return None;
            };
            let id = Ref::new(*next);
            *next += 2; // picture + soft mask
            pending.push(src.to_string());
            Some(Img { id, name: format!("Im{}", id.get()), data })
        });
        let Some(img) = entry else { return };
        let name = img.name.clone();
        if !self.page_images.iter().any(|s| s == src) {
            self.page_images.push(src.to_string());
        }
        self.c.save_state();
        self.c.transform([w, 0.0, 0.0, h, x, self.h - y - h]);
        self.c.x_object(Name(name.as_bytes()));
        self.c.restore_state();
    }
}

fn is_jpeg(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8])
}

fn is_png(b: &[u8]) -> bool {
    b.starts_with(&[0x89, b'P', b'N', b'G'])
}

/// (width, height, components) from a JPEG's frame header.
fn jpeg_info(b: &[u8]) -> Option<(u32, u32, u8)> {
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = b[i + 1];
        if m == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if (0xC0..=0xCF).contains(&m) && m != 0xC4 && m != 0xC8 && m != 0xCC {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            return Some((w, h, b[i + 9]));
        }
        i += 2 + len;
    }
    None
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

/// (colour or grey pixels, alpha, width, height, grey)
type Pixels = (Vec<u8>, Option<Vec<u8>>, u32, u32, bool);

/// Decodes a PNG, at most at `fit` (pixels the placed picture can show, 300
/// ppi of the visible size). A larger picture is reduced while it is read, row
/// by row (a box filter over whole blocks), so a 30000-pixel scan or a
/// decompression bomb costs a row of memory plus the reduced picture.
fn png_pixels(b: &[u8], fit: (u32, u32)) -> Option<Pixels> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(b));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let (w, h) = (r.info().width, r.info().height);
    let interlaced = r.info().interlaced;
    let (color, _) = r.output_color_type();
    use png::ColorType::*;
    let n = match color {
        Grayscale => 1,
        GrayscaleAlpha => 2,
        Rgb => 3,
        Rgba => 4,
        Indexed => return None,
    };
    // whole blocks of f x f pixels become one; never below what the page needs
    let f = (w / fit.0.max(1)).min(h / fit.1.max(1)).max(1);
    let (px, w, h) = if f == 1 {
        let mut buf = vec![0; r.output_buffer_size()?];
        let info = r.next_frame(&mut buf).ok()?;
        buf.truncate(info.buffer_size());
        (buf, w, h)
    } else {
        let (ow, oh) = (w.div_ceil(f) as usize, h.div_ceil(f) as usize);
        let mut out = Vec::with_capacity(ow * oh * n);
        let mut acc = vec![0u32; ow * n];
        let mut rows = 0u32;
        let mut block = 0u32;
        let flush = |acc: &mut Vec<u32>, rows: u32, out: &mut Vec<u8>| {
            for (i, v) in acc.iter_mut().enumerate() {
                let x = (i / n) as u32;
                let cols = (w - x * f).min(f);
                out.push((*v / (cols * rows).max(1)) as u8);
                *v = 0;
            }
        };
        // interlaced: only the last pass (every other row, every column), in order
        let skip = if interlaced { adam7_rows_before_last(w, h) } else { 0 };
        let mut index = 0u32;
        while let Some(row) = r.next_row().ok()? {
            index += 1;
            if index <= skip {
                continue;
            }
            let y = if interlaced { 2 * (index - skip - 1) + 1 } else { index - 1 };
            if y / f != block {
                flush(&mut acc, rows, &mut out);
                rows = 0;
                block = y / f;
            }
            for (x, p) in row.data().chunks_exact(n).enumerate() {
                let o = (x / f as usize) * n;
                for c in 0..n {
                    acc[o + c] += p[c] as u32;
                }
            }
            rows += 1;
        }
        if rows > 0 {
            flush(&mut acc, rows, &mut out);
        }
        out.resize(ow * oh * n, 255);
        (out, ow as u32, oh as u32)
    };
    let split = |n: usize| {
        let mut c = Vec::with_capacity(px.len());
        let mut a = Vec::with_capacity(px.len() / n);
        for p in px.chunks_exact(n) {
            c.extend_from_slice(&p[..n - 1]);
            a.push(p[n - 1]);
        }
        (c, a)
    };
    Some(match color {
        Rgba => {
            let (c, a) = split(4);
            (c, Some(a), w, h, false)
        }
        GrayscaleAlpha => {
            let (c, a) = split(2);
            (c, Some(a), w, h, true)
        }
        Rgb => (px, None, w, h, false),
        _ => (px, None, w, h, true),
    })
}

/// Rows the Adam7 passes 1 to 6 hand out before the last one.
fn adam7_rows_before_last(w: u32, h: u32) -> u32 {
    // (x start, x step, y start, y step) of passes 1..6
    const P: [(u32, u32, u32, u32); 6] = [(0, 8, 0, 8), (4, 8, 0, 8), (0, 4, 4, 8), (2, 4, 0, 4), (0, 2, 2, 4), (1, 2, 0, 2)];
    P.iter().filter(|p| w > p.0).map(|&(_, _, y0, dy)| if h > y0 { (h - y0).div_ceil(dy) } else { 0 }).sum()
}

/// `2025-10-18T10:20:30Z` → PDF date.
fn pdf_date(s: &str) -> Option<Date> {
    let n = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<u16>().ok());
    let mut d = Date::new(n(0, 4)?);
    for (a, b, k) in [(5, 7, 0), (8, 10, 1), (11, 13, 2), (14, 16, 3), (17, 19, 4)] {
        if let Some(v) = n(a, b) {
            let v = v as u8;
            d = match k {
                0 => d.month(v),
                1 => d.day(v),
                2 => d.hour(v),
                3 => d.minute(v),
                _ => d.second(v),
            };
        }
    }
    if s.ends_with('Z') {
        d = d.utc_offset_hour(0).utc_offset_minute(0);
    }
    Some(d)
}

/// Six capitals marking a subset font name.
fn subset_tag(face: FaceId, n: usize) -> String {
    let mut v = (face as u64 + 1).wrapping_mul(2654435761) ^ (n as u64).wrapping_mul(40503);
    (0..6)
        .map(|_| {
            let c = (b'A' + (v % 26) as u8) as char;
            v /= 26;
            c
        })
        .collect()
}

struct PageOut {
    id: Ref,
    content: Ref,
    w: f32,
    h: f32,
    data: Vec<u8>,
    fonts: Vec<FaceId>,
    images: Vec<String>,
    /// link annotations: object and (x, top, w, h, URL)
    links: Vec<(Ref, (f32, f32, f32, f32, String))>,
}

/// External links in a paragraph: char ranges and their URLs.
fn link_ranges(doc: &Document, path: &[u32]) -> Vec<(usize, usize, String)> {
    use crate::model::{Inline, WrapKind};
    let Some(p) = crate::edit::dpara(doc, path) else { return Vec::new() };
    // links of headers and footers resolve through their own part
    let story_rels = crate::edit::story_rid(doc, path).and_then(|rid| doc.stories.get(&rid)).and_then(|s| crate::docx::read_rels(&doc.pkg, &s.part).ok());
    let rels = story_rels.as_ref().unwrap_or(&doc.rels);
    let attr = |tag: &str, name: &str| -> Option<String> {
        let i = tag.find(&format!("{name}=\""))? + name.len() + 2;
        Some(crate::xml::unescape_text(&tag[i..i + tag[i..].find('"')?]).unwrap_or_default())
    };
    let target = |w: &crate::model::Wrapper| -> Option<String> {
        match &w.kind {
            WrapKind::Hyperlink => {
                let r = rels.get(&attr(&w.open, "r:id")?)?;
                r.external.then(|| r.target.clone())
            }
            // fldSimple HYPERLINK "url"
            WrapKind::Field(instr) => {
                let t = instr.trim();
                let rest = t.strip_prefix("HYPERLINK")?.trim();
                let url = rest.strip_prefix('"')?.split('"').next()?.to_string();
                (!url.is_empty()).then_some(url)
            }
            _ => None,
        }
    };
    let mut out = Vec::new();
    let mut stack: Vec<(usize, Option<String>)> = Vec::new();
    let mut off = 0;
    for inl in &p.inlines {
        match inl {
            Inline::Open(w) => stack.push((off, target(w))),
            Inline::Close(_) => {
                if let Some((a, Some(u))) = stack.pop() {
                    if off > a {
                        out.push((a, off, u));
                    }
                }
            }
            other => off += other.len(),
        }
    }
    out
}

/// Clickable areas of a page: x, top, w, h (points) and the URL.
fn page_links(doc: &Document, page: &crate::layout::Page) -> Vec<(f32, f32, f32, f32, String)> {
    use crate::layout::{Piece, Placed};
    let mut cache: HashMap<Vec<u32>, Vec<(usize, usize, String)>> = HashMap::new();
    let mut out = Vec::new();
    for it in &page.items {
        let Placed::Line { path: Some(path), para, idx, x, y, .. } = it else { continue };
        let ranges = cache.entry(path.to_vec()).or_insert_with(|| link_ranges(doc, path));
        let l = &para.lines[*idx];
        for (a, b, url) in ranges.iter() {
            if *b <= l.start || *a >= l.end.max(l.start + 1) {
                continue;
            }
            let (mut x0, mut x1) = (f32::MAX, f32::MIN);
            for p in &l.pieces {
                if let Piece::Text { x: px, start, adv, phantom: false, .. } = p {
                    let mut cx = x + px;
                    for (k, w) in adv.iter().enumerate() {
                        if (*a..*b).contains(&(start + k)) {
                            x0 = x0.min(cx);
                            x1 = x1.max(cx + w);
                        }
                        cx += w;
                    }
                }
            }
            if x1 > x0 {
                out.push((x0, *y, x1 - x0, l.h, url.clone()));
            }
        }
    }
    out
}

pub fn write(doc: &Document, layout: &Layout, fonts: &Fonts) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let catalog = Ref::new(1);
    let tree = Ref::new(2);
    let info = Ref::new(3);
    let mut next = 4;
    let alloc = |n: &mut i32| {
        let r = Ref::new(*n);
        *n += 1;
        r
    };

    let mut used: HashMap<FaceId, FontUse> = HashMap::new();
    let mut images: HashMap<String, Option<Img>> = HashMap::new();
    let mut pending: Vec<String> = Vec::new();
    let mut pages = Vec::new();
    for p in &layout.pages {
        let id = alloc(&mut next);
        let content = alloc(&mut next);
        let mut sink = PageSink {
            fonts,
            doc,
            w: p.w,
            h: p.h,
            c: Content::new(),
            used: &mut used,
            images: &mut images,
            pending: &mut pending,
            next: &mut next,
            page_fonts: Vec::new(),
            page_images: Vec::new(),
        };
        render(p, fonts, &mut sink);
        let PageSink { c, page_fonts, page_images, .. } = sink;
        let data = deflate(&c.finish());
        let links = page_links(doc, p).into_iter().map(|l| (alloc(&mut next), l)).collect();
        pages.push(PageOut { id, content, w: p.w, h: p.h, data, fonts: page_fonts, images: page_images, links });
    }
    let mut faces: Vec<FaceId> = used.keys().copied().collect();
    faces.sort();
    let font_refs: HashMap<FaceId, Ref> = faces.iter().map(|f| (*f, alloc(&mut next))).collect();

    for p in &pages {
        let mut page = pdf.page(p.id);
        page.media_box(Rect::new(0.0, 0.0, p.w, p.h));
        page.parent(tree);
        page.contents(p.content);
        let mut res = page.resources();
        {
            let mut d = res.fonts();
            for f in &p.fonts {
                d.pair(Name(used[f].name.as_bytes()), font_refs[f]);
            }
        }
        {
            let mut x = res.x_objects();
            for src in &p.images {
                if let Some(Some(img)) = images.get(src) {
                    x.pair(Name(img.name.as_bytes()), img.id);
                }
            }
        }
        res.finish();
        if !p.links.is_empty() {
            page.annotations(p.links.iter().map(|(r, _)| *r));
        }
        page.finish();
        pdf.stream(p.content, &p.data).filter(Filter::FlateDecode);
        for (r, (x, y, w, h, url)) in &p.links {
            let mut a = pdf.annotation(*r);
            a.subtype(pdf_writer::types::AnnotationType::Link).rect(Rect::new(*x, p.h - y - h, x + w, p.h - y)).border(0.0, 0.0, 0.0, None);
            a.action().action_type(pdf_writer::types::ActionType::Uri).uri(Str(url.as_bytes()));
        }
    }
    pdf.catalog(catalog).pages(tree);
    let ids: Vec<Ref> = pages.iter().map(|p| p.id).collect();
    pdf.pages(tree).kids(ids.iter().copied()).count(ids.len() as i32);

    for f in &faces {
        let face = fonts.face(*f);
        let u = &used[f];
        let cid = alloc(&mut next);
        let desc = alloc(&mut next);
        let file = alloc(&mut next);
        let cmap = alloc(&mut next);
        let base = format!("{}+{}", subset_tag(*f, u.unicode.len()), face.postscript_name());
        let base_name = Name(base.as_bytes());

        pdf.type0_font(font_refs[f])
            .base_font(base_name)
            .encoding_predefined(Name(b"Identity-H"))
            .descendant_font(cid)
            .to_unicode(cmap);

        // after subsetting glyph ids are 0..n in order of first use
        let widths: Vec<f32> = u.remap.remapped_gids().map(|old| face.glyph_advance(old) * 1000.0).collect();
        let mut cf = pdf.cid_font(cid);
        cf.subtype(CidFontType::Type2);
        cf.base_font(base_name);
        cf.system_info(SystemInfo { registry: Str(b"Adobe"), ordering: Str(b"Identity"), supplement: 0 });
        cf.font_descriptor(desc);
        cf.default_width(0.0);
        cf.cid_to_gid_map_predefined(Name(b"Identity"));
        cf.widths().consecutive(0, widths);
        cf.finish();

        let (bbox, italic, cap, serif) = face.pdf_metrics();
        let mut flags = FontFlags::SYMBOLIC;
        if serif {
            flags |= FontFlags::SERIF;
        }
        if italic != 0.0 || face.italic {
            flags |= FontFlags::ITALIC;
        }
        pdf.font_descriptor(desc)
            .name(base_name)
            .flags(flags)
            .bbox(Rect::new(bbox[0] * 1000.0, bbox[1] * 1000.0, bbox[2] * 1000.0, bbox[3] * 1000.0))
            .italic_angle(italic)
            .ascent(face.ascent * 1000.0)
            .descent(-face.descent * 1000.0)
            .cap_height(cap * 1000.0)
            .stem_v(if face.bold { 140.0 } else { 80.0 })
            .font_file2(file);

        let subset = subsetter::subset(face.data, 0, &u.remap).unwrap_or_else(|_| face.data.to_vec());
        let packed = deflate(&subset);
        pdf.stream(file, &packed).filter(Filter::FlateDecode).pair(Name(b"Length1"), subset.len() as i32);

        let mut cm = UnicodeCmap::new(Name(b"Custom"), SystemInfo { registry: Str(b"Adobe"), ordering: Str(b"UCS"), supplement: 0 });
        for (g, c) in &u.unicode {
            cm.pair(*g, *c);
        }
        let cm_data = deflate(&cm.finish());
        pdf.cmap(cmap, &cm_data).filter(Filter::FlateDecode);
    }

    for src in pending {
        let Some(Some(img)) = images.get(&src) else { continue };
        let id = img.id;
        match &img.data {
            Pixels2::Jpeg { w, h, comps } => {
                let Some(bytes) = doc.media(&src) else { continue };
                let mut im = pdf.image_xobject(id, bytes);
                im.filter(Filter::DctDecode);
                im.width(*w as i32);
                im.height(*h as i32);
                match comps {
                    1 => {
                        im.color_space().device_gray();
                    }
                    4 => {
                        // Adobe CMYK JPEGs are stored inverted
                        im.decode([1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
                        im.color_space().device_cmyk();
                    }
                    _ => {
                        im.color_space().device_rgb();
                    }
                }
                im.bits_per_component(8);
            }
            Pixels2::Raw { px, alpha, w, h, gray } => {
                let mask = Ref::new(id.get() + 1);
                let data = deflate(px);
                let mut im = pdf.image_xobject(id, &data);
                im.filter(Filter::FlateDecode);
                im.width(*w as i32);
                im.height(*h as i32);
                if *gray {
                    im.color_space().device_gray();
                } else {
                    im.color_space().device_rgb();
                }
                im.bits_per_component(8);
                if alpha.is_some() {
                    im.s_mask(mask);
                }
                im.finish();
                if let Some(a) = alpha {
                    let ad = deflate(a);
                    let mut sm = pdf.image_xobject(mask, &ad);
                    sm.filter(Filter::FlateDecode);
                    sm.width(*w as i32);
                    sm.height(*h as i32);
                    sm.color_space().device_gray();
                    sm.bits_per_component(8);
                }
            }
        }
    }

    let m = &doc.meta;
    let mut di = pdf.document_info(info);
    if let Some(t) = &m.title {
        di.title(TextStr(t));
    }
    if let Some(a) = &m.author {
        di.author(TextStr(a));
    }
    di.producer(TextStr("Papir docconv"));
    if let Some(d) = m.created.as_deref().and_then(pdf_date) {
        di.creation_date(d);
    }
    di.finish();
    pdf.set_version(1, 7);
    pdf.finish()
}

/// Options for `convert_docx_to_pdf`.
pub struct Options<'a> {
    pub fonts: &'a Fonts,
    pub limits: crate::docx::Limits,
}

#[derive(Debug)]
pub enum ConvertError {
    Docx(crate::docx::Error),
}

impl ConvertError {
    pub fn code(&self) -> &'static str {
        match self {
            ConvertError::Docx(e) => e.code(),
        }
    }
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::Docx(e) => write!(f, "{e}"),
        }
    }
}

/// .docx bytes → PDF bytes.
pub fn convert_docx_to_pdf(input: &[u8], options: &Options) -> Result<Vec<u8>, ConvertError> {
    convert(input, options).map(|(pdf, _)| pdf)
}

/// .docx bytes → (PDF bytes, page count).
pub fn convert(input: &[u8], options: &Options) -> Result<(Vec<u8>, usize), ConvertError> {
    let doc = Document::open_with(input, &options.limits).map_err(ConvertError::Docx)?;
    let cache = std::cell::RefCell::new(crate::layout::Cache::default());
    let env = crate::layout::Env { doc: &doc, fonts: options.fonts, cache: &cache, markup: true, story: None };
    let layout = crate::layout::layout_document(&env);
    Ok((write(&doc, &layout, options.fonts), layout.pages.len()))
}
