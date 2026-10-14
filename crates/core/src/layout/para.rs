//! Paragraph → lines.

use super::{author_color, color, highlight, Env};
use crate::fonts::FaceId;
use crate::model::*;
use crate::props::{Borders, ParaProps, RunProps, TabStop};
use std::rc::Rc;

pub const DECO_U: u8 = 1;
pub const DECO_UU: u8 = 2;
pub const DECO_S: u8 = 4;
pub const DECO_SS: u8 = 8;

#[derive(Clone, Debug)]
pub enum Piece {
    Text {
        x: f32,
        text: String,
        face: FaceId,
        size: f32,
        color: u32,
        start: usize,
        adv: Vec<f32>,
        dy: f32,
        deco: u8,
        hl: Option<u32>,
        /// deleted text shown as a tracked change: visible, no caret positions
        phantom: bool,
    },
    Tab { x: f32, w: f32, start: usize, leader: Option<char>, face: FaceId, size: f32, color: u32, deco: u8 },
    Image { x: f32, w: f32, h: f32, src: Option<String>, start: usize, chart: Option<String> },
    /// line/page break or the end of a hidden/zero-width run of chars
    Mark { x: f32, start: usize },
    /// list label, not addressable
    Label { x: f32, text: String, face: FaceId, size: f32, color: u32, dy: f32 },
}

impl Piece {
    pub fn x(&self) -> f32 {
        match self {
            Piece::Text { x, .. } | Piece::Tab { x, .. } | Piece::Image { x, .. } | Piece::Mark { x, .. } | Piece::Label { x, .. } => *x,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Line {
    /// top, relative to the paragraph's first line
    pub y: f32,
    pub h: f32,
    /// baseline from the line top
    pub base: f32,
    pub start: usize,
    pub end: usize,
    pub pieces: Vec<Piece>,
    pub page_break: bool,
    /// x of the line's left edge and of its end (after the last char)
    pub x0: f32,
    pub x1: f32,
    /// ended by wrapping, not by a break or the paragraph end
    pub wrapped: bool,
    /// holds a tracked change (change bar in the margin)
    pub changed: bool,
}

#[derive(Clone, Debug)]
pub struct ParaLayout {
    pub props: ParaProps,
    pub lines: Vec<Line>,
    pub before: f32,
    pub after: f32,
    pub left: f32,
    pub right: f32,
    pub width: f32,
    pub len: usize,
    pub shd: Option<u32>,
    pub borders: Option<Borders>,
    /// floating pictures: (drawing, line index)
    pub anchors: Vec<(Rc<Drawing>, usize)>,
    pub height: f32,
    /// footnote references: text offset, footnote id
    pub notes: Vec<(usize, String)>,
}

#[derive(Clone, Copy, PartialEq)]
enum K {
    Text,
    Space,
    Tab,
    Break(BreakKind),
    Image,
    Label,
    LabelTab,
    Hidden,
}

#[derive(Clone)]
struct Sty {
    face: FaceId,
    size: f32,
    color: u32,
    deco: u8,
    hl: Option<u32>,
    dy: f32,
    spacing: f32,
}

struct Ch {
    c: char,
    off: usize,
    phantom: bool,
    tracked: bool,
    k: K,
    sty: usize,
    face: FaceId,
    size: f32,
    w: f32,
    img: Option<Rc<Drawing>>,
}

/// Label of a numbered paragraph: text and its run properties.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub rpr: RunProps,
    pub suff: String,
}

pub struct ParaCtx<'a> {
    pub table_ppr: Option<&'a ParaProps>,
    pub table_rpr: Option<&'a RunProps>,
    pub label: Option<&'a Label>,
    /// comment ranges open at the start of the paragraph
    pub comments: u8,
    /// areas taken by floating objects, relative to the paragraph's first line
    pub excl: &'a [Excl],
}

/// How text keeps out of a floating object's area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExKind {
    /// no text beside it: lines go below `y2`
    Above,
    /// text starts at or after x (object on the left)
    Left(f32),
    /// text ends before x (object on the right)
    Right(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Excl {
    pub y1: f32,
    pub y2: f32,
    pub kind: ExKind,
}

pub const COMMENT_SHADE: u32 = 0xFCEFB4;

/// Comment ids that are resolved (their text is not shaded).
fn comment_open(env: &Env, id: Option<String>) -> bool {
    !env.cache.borrow().hide_comments && id.is_some_and(|i| env.doc.comments.get(&i).is_none_or(|c| !c.done))
}

/// Effective paragraph properties including the numbering level's.
pub fn effective(doc: &crate::Document, p: &Para, table_ppr: Option<&ParaProps>) -> ParaProps {
    let styles = &doc.styles;
    let mut eff = styles.para(&p.props, table_ppr);
    if let (Some(n), Some(l)) = (eff.num_id, eff.ilvl) {
        if n > 0 {
            if let Some(lvl) = doc.numbering.level(n, l) {
                // numbering indents sit between the style and direct formatting
                let mut with = eff.clone();
                with.merge(&lvl.ppr);
                let mut direct = p.props.clone();
                direct.style = None;
                direct.num_id = None;
                direct.ilvl = None;
                with.merge(&direct);
                eff = with;
            }
        }
    }
    eff
}

pub fn run_style(env: &Env, rp: &RunProps) -> (String, bool, bool, f32) {
    let font = rp
        .font_hansi
        .clone()
        .or_else(|| rp.theme_hansi.as_deref().and_then(|t| env.doc.theme.resolve(t)).map(str::to_string))
        .or_else(|| rp.font_ascii.clone())
        .or_else(|| rp.theme_ascii.as_deref().and_then(|t| env.doc.theme.resolve(t)).map(str::to_string))
        .unwrap_or_else(|| "Times New Roman".into());
    let size = rp.sz.unwrap_or(20) as f32 / 2.0;
    (font, rp.b.unwrap_or(false), rp.i.unwrap_or(false), size)
}

fn sty_of(env: &Env, rp: &RunProps, link: bool) -> Sty {
    let (font, b, i, mut size) = run_style(env, rp);
    let face = env.fonts.select(&font, b, i);
    let mut dy = 0.0;
    match rp.valign.as_deref() {
        Some("superscript") => {
            dy = -size * 0.33;
            size *= 0.65;
        }
        Some("subscript") => {
            dy = size * 0.14;
            size *= 0.65;
        }
        _ => {}
    }
    let mut deco = 0;
    match rp.u.as_deref() {
        None | Some("none") => {}
        Some("double") => deco |= DECO_UU,
        Some(_) => deco |= DECO_U,
    }
    if rp.strike == Some(true) {
        deco |= DECO_S;
    }
    if rp.dstrike == Some(true) {
        deco |= DECO_SS;
    }
    let _ = link;
    Sty {
        face,
        size,
        color: rp.color.as_deref().and_then(color).unwrap_or(0),
        deco,
        hl: rp.highlight.as_deref().and_then(highlight).or_else(|| rp.shd.as_deref().and_then(color)),
        dy,
        spacing: rp.spacing.unwrap_or(0) as f32 / 20.0,
    }
}

/// Bullet glyphs from Symbol/Wingdings private-use code points.
fn map_bullet(c: char) -> char {
    match c as u32 {
        0xF0B7 | 0xF06C | 0xF09F => '•',
        0xF0A7 | 0xF06E => '▪',
        0xF0D8 | 0xF0E8 => '➢',
        0xF076 => '❖',
        0xF0FC => '✓',
        0xF0A8 => '◻',
        0xF06F => 'o',
        0xF02D => '–',
        u if (0xF000..=0xF0FF).contains(&u) => '•',
        _ => c,
    }
}

pub fn layout(env: &Env, p: &Para, width: f32, ctx: &ParaCtx) -> ParaLayout {
    let props = effective(env.doc, p, ctx.table_ppr);
    let styles = &env.doc.styles;
    let pstyle = props.style.clone();
    let rp = |direct: &RunProps| styles.run(pstyle.as_deref(), ctx.table_rpr, direct);

    let tw = |v: Option<i32>| v.unwrap_or(0) as f32 / 20.0;
    let left = tw(props.ind_left);
    let right = tw(props.ind_right);
    let first = tw(props.ind_first);

    let mut stys: Vec<Sty> = Vec::new();
    let mut chars: Vec<Ch> = Vec::new();
    let mark_rp = rp(&p.mark);
    let mark_sty = sty_of(env, &mark_rp, false);

    // list label
    if let Some(l) = ctx.label {
        let mut lrp = mark_rp.clone();
        lrp.merge(&l.rpr);
        lrp.u = None;
        let s = sty_of(env, &lrp, false);
        let si = stys.len();
        stys.push(s.clone());
        for c in l.text.chars().map(map_bullet) {
            let face = env.fonts.for_char(s.face, c);
            chars.push(Ch { c, off: 0, phantom: false, tracked: false, k: K::Label, sty: si, face, size: s.size, w: env.fonts.width(face, c, s.size), img: None });
        }
        match l.suff.as_str() {
            "space" => {
                chars.push(Ch { c: ' ', off: 0, phantom: false, tracked: false, k: K::Label, sty: si, face: s.face, size: s.size, w: env.fonts.width(s.face, ' ', s.size), img: None })
            }
            "nothing" => {}
            _ => chars.push(Ch { c: '\t', off: 0, phantom: false, tracked: false, k: K::LabelTab, sty: si, face: s.face, size: s.size, w: 0.0, img: None }),
        }
    }

    let mut off = 0usize;
    let mut note_refs: Vec<(usize, String)> = Vec::new();
    let mut anchors_raw: Vec<(Rc<Drawing>, usize)> = Vec::new();
    let mut link_depth = 0;
    // authors of the tracked insertions we are inside
    let mut ins: Vec<String> = Vec::new();
    let mut commented = ctx.comments as i32;
    for inl in &p.inlines {
        match inl {
            Inline::Raw(e) if comment_open(env, crate::comments::is_marker(e, "commentRangeStart")) => commented += 1,
            Inline::Raw(e) if comment_open(env, crate::comments::is_marker(e, "commentRangeEnd")) => commented = (commented - 1).max(0),
            Inline::Open(w) if w.kind == WrapKind::Hyperlink => link_depth += 1,
            Inline::Close(w) if w.kind == WrapKind::Hyperlink => link_depth -= 1,
            Inline::Open(w) => {
                if let WrapKind::Ins(a, _) = &w.kind {
                    ins.push(a.clone());
                }
            }
            Inline::Close(w) => {
                if matches!(w.kind, WrapKind::Ins(..)) {
                    ins.pop();
                }
            }
            Inline::Raw(e) if env.markup && crate::model::is_deletion(e) => {
                // deleted text: drawn struck through, takes no caret positions
                let author = e.wattr("author").unwrap_or("").to_string();
                for r in e.elems().filter(|x| x.is_w("r")) {
                    let mut eff = rp(&r.wchild("rPr").map(RunProps::parse).unwrap_or_default());
                    eff.color = Some(format!("{:06X}", author_color(&author)));
                    eff.strike = Some(true);
                    let s = sty_of(env, &eff, false);
                    let si = stys.len();
                    stys.push(s.clone());
                    for t in r.elems() {
                        let text = if t.is_w("delText") { t.text() } else if t.is_w("tab") { " ".into() } else { continue };
                        for c in text.chars() {
                            let face = env.fonts.for_char(s.face, c);
                            let w = env.fonts.width(face, c, s.size) + s.spacing;
                            chars.push(Ch { c, off, phantom: true, tracked: true, k: K::Text, sty: si, face, size: s.size, w, img: None });
                        }
                    }
                }
            }
            Inline::Run(r) => {
                let mut eff = rp(&r.props);
                let tracked = env.markup && !ins.is_empty();
                if tracked {
                    eff.color = Some(format!("{:06X}", author_color(ins.last().unwrap())));
                    eff.u = Some("single".into());
                }
                let hidden = eff.vanish == Some(true);
                let mut s = sty_of(env, &eff, link_depth > 0);
                if commented > 0 && s.hl.is_none() && !env.cache.borrow().hide_comments {
                    s.hl = Some(COMMENT_SHADE);
                }
                let caps = eff.caps == Some(true);
                let small = eff.small_caps == Some(true) && !caps;
                // text set in Symbol or Wingdings: what those fonts draw, in Unicode
                let sym_font = eff.font_ascii.clone().or_else(|| eff.font_hansi.clone()).filter(|f| crate::fonts::symbol_char(f, ' ').is_some());
                let si = stys.len();
                stys.push(s.clone());
                let mut small_si = None;
                for it in &r.items {
                    match it {
                        Item::Text(t) => {
                            for c in t.chars() {
                                let c = sym_font.as_deref().and_then(|f| crate::fonts::symbol_char(f, c)).unwrap_or(c);
                                let (mut dc, mut sty, mut size) = (c, si, s.size);
                                if caps {
                                    dc = c.to_uppercase().next().unwrap_or(c);
                                } else if small && c.is_lowercase() {
                                    dc = c.to_uppercase().next().unwrap_or(c);
                                    let idx = *small_si.get_or_insert_with(|| {
                                        let mut s2 = s.clone();
                                        s2.size *= 0.8;
                                        stys.push(s2);
                                        stys.len() - 1
                                    });
                                    sty = idx;
                                    size = stys[idx].size;
                                }
                                let face = env.fonts.for_char(s.face, dc);
                                let k = if hidden {
                                    K::Hidden
                                } else if dc == ' ' || dc == '\u{3000}' {
                                    K::Space
                                } else {
                                    K::Text
                                };
                                let w = if hidden { 0.0 } else { env.fonts.width(face, dc, size) + s.spacing };
                                chars.push(Ch { c: dc, off, phantom: false, tracked, k, sty, face, size, w, img: None });
                                off += 1;
                            }
                        }
                        Item::Tab => {
                            chars.push(Ch { c: '\t', off, phantom: false, tracked: false, k: if hidden { K::Hidden } else { K::Tab }, sty: si, face: s.face, size: s.size, w: 0.0, img: None });
                            off += 1;
                        }
                        Item::Break(b) => {
                            chars.push(Ch { c: '\n', off, phantom: false, tracked: false, k: K::Break(*b), sty: si, face: s.face, size: s.size, w: 0.0, img: None });
                            off += 1;
                        }
                        Item::Image(d) => {
                            if d.anchored {
                                anchors_raw.push((d.clone(), chars.len()));
                                chars.push(Ch { c: '\u{FFFC}', off, phantom: false, tracked: false, k: K::Hidden, sty: si, face: s.face, size: s.size, w: 0.0, img: None });
                            } else {
                                chars.push(Ch { c: '\u{FFFC}', off, phantom: false, tracked: false, k: K::Image, sty: si, face: s.face, size: s.size, w: d.width, img: Some(d.clone()) });
                            }
                            off += 1;
                        }
                        // footnote marks: the number, raised by the run's style
                        Item::Raw(e) if !hidden && (e.is_w("footnoteReference") || e.is_w("footnoteRef")) => {
                            let n = if e.is_w("footnoteRef") {
                                env.cache.borrow().cur_note
                            } else {
                                let id = e.wattr("id").unwrap_or("").to_string();
                                let n = env.cache.borrow().notes.get(&id).copied().unwrap_or(0);
                                note_refs.push((off, id));
                                n
                            };
                            if n > 0 {
                                for c in n.to_string().chars() {
                                    let face = env.fonts.for_char(s.face, c);
                                    chars.push(Ch { c, off, phantom: false, tracked: false, k: K::Label, sty: si, face, size: s.size, w: env.fonts.width(face, c, s.size), img: None });
                                }
                            }
                        }
                        // a symbol inserted from a font (Insert → Symbol): drawn, not editable as text
                        Item::Raw(e) if !hidden && e.is_w("sym") => {
                            let code = e.wattr("char").and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32);
                            if let Some(c) = code {
                                let c = e.wattr("font").and_then(|f| crate::fonts::symbol_char(f, c)).unwrap_or(c);
                                if !crate::fonts::invisible(c) {
                                    let face = env.fonts.for_char(s.face, c);
                                    chars.push(Ch { c, off, phantom: false, tracked: false, k: K::Label, sty: si, face, size: s.size, w: env.fonts.width(face, c, s.size), img: None });
                                }
                            }
                        }
                        Item::Raw(_) => {}
                    }
                }
            }
            _ => {}
        }
    }
    let len = off;

    // break opportunities: allowed[i] = a line may start at char i
    let text: String = chars.iter().map(|c| c.c).collect();
    let mut allowed = vec![false; chars.len() + 1];
    {
        let mut byte_to_char = vec![0usize; text.len() + 1];
        for (ci, (bi, _)) in text.char_indices().enumerate() {
            byte_to_char[bi] = ci;
        }
        byte_to_char[text.len()] = chars.len();
        for (bi, _) in unicode_linebreak::linebreaks(&text) {
            let ci = byte_to_char[bi];
            if ci < chars.len() {
                allowed[ci] = true;
            }
        }
        for (i, c) in chars.iter().enumerate() {
            if matches!(c.k, K::Label | K::LabelTab) {
                allowed[i] = false;
                allowed[i + 1] = false;
            }
        }
    }

    let default_tab = env.doc.settings.default_tab as f32 / 20.0;
    let tabs: Vec<TabStop> = props.tabs.clone().unwrap_or_default();
    let next_tab = |x: f32, first_line: bool| -> (f32, String, Option<char>) {
        let eps = 0.01;
        let mut best: Option<(f32, String, Option<char>)> = None;
        for t in &tabs {
            let pos = t.pos as f32 / 20.0;
            if pos > x + eps && t.kind != "clear" && t.kind != "bar" {
                let leader = match t.leader.as_deref() {
                    Some("dot") => Some('.'),
                    Some("hyphen") => Some('-'),
                    Some("underscore") | Some("heavy") => Some('_'),
                    Some("middleDot") => Some('·'),
                    _ => None,
                };
                best = Some((pos, t.kind.clone(), leader));
                break;
            }
        }
        // hanging indent acts as a tab stop on the first line
        if first_line && first < 0.0 && left > x + eps && best.as_ref().is_none_or(|b| left < b.0) {
            best = Some((left, "left".into(), None));
        }
        best.unwrap_or_else(|| {
            let n = ((x + eps) / default_tab).floor() + 1.0;
            (n * default_tab, "left".into(), None)
        })
    };

    let avail_right = width - right;
    let mut lines: Vec<Line> = Vec::new();
    let mut i = 0usize;
    let n = chars.len();
    let mut y = 0.0f32;
    let mut first_line = true;
    // widths of tabs as computed during breaking
    let mut tab_w = vec![0.0f32; n];
    // a typical line height, to find which areas a line will cross
    let est = {
        let f = env.fonts.face(mark_sty.face);
        let nat = (f.ascent + f.descent) * mark_sty.size;
        match (props.line_rule.as_deref(), props.line) {
            (Some("exact"), Some(v)) => v as f32 / 20.0,
            (Some("atLeast"), Some(v)) => nat.max(v as f32 / 20.0),
            (_, Some(v)) => nat * v as f32 / 240.0,
            _ => nat,
        }
    };
    loop {
        let base_start = if first_line { left + first } else { left };
        let mut start_x = base_start;
        let mut right_lim = avail_right;
        let mut gap = 0.0f32;
        if !ctx.excl.is_empty() {
            for _ in 0..16 {
                let yy = y + gap;
                start_x = base_start;
                right_lim = avail_right;
                let mut push: Option<f32> = None;
                for e in ctx.excl.iter().filter(|e| e.y1 < yy + est && e.y2 > yy) {
                    match e.kind {
                        ExKind::Above => push = Some(push.map_or(e.y2, |p: f32| p.max(e.y2))),
                        ExKind::Left(x) => start_x = start_x.max(x),
                        ExKind::Right(x) => right_lim = right_lim.min(x),
                    }
                }
                // too narrow beside the object: continue below it
                if push.is_none() && right_lim - start_x < 36.0 {
                    push = ctx.excl.iter().filter(|e| e.y1 < yy + est && e.y2 > yy).map(|e| e.y2).reduce(f32::max);
                }
                match push {
                    Some(p) if p > yy => gap = p - y,
                    _ => break,
                }
            }
        }
        let avail_right = right_lim;
        let mut x = start_x;
        let line_start = i;
        let mut last_break: Option<usize> = None;
        let mut end = n;
        let mut mandatory = None;
        let mut j = i;
        // after a right, centre or decimal tab the text ends at (or centres on) the
        // stop: (kind, stop, x where that text starts); a stop past the margin is
        // honoured, the text runs up to it as in Word
        let mut aligned: Option<(bool, f32, f32)> = None;
        let reach = |aligned: &Option<(bool, f32, f32)>, x: f32| match aligned {
            Some((true, pos, from)) => (pos + (x - from) / 2.0).max(x),
            Some((false, pos, _)) => pos.max(x),
            None => x,
        };
        while j < n {
            let c = &chars[j];
            if let K::Break(b) = c.k {
                end = j + 1;
                mandatory = Some(b);
                break;
            }
            let w = match c.k {
                K::Tab | K::LabelTab => {
                    x = reach(&aligned, x);
                    let (pos, kind, _) = next_tab(x, first_line);
                    match kind.as_str() {
                        "right" | "end" | "decimal" | "center" if pos > x => {
                            aligned = Some((kind == "center", pos, x));
                            tab_w[j] = 0.0;
                            0.0
                        }
                        _ => {
                            aligned = None;
                            let w = (pos - x).max(0.0);
                            tab_w[j] = w;
                            w
                        }
                    }
                }
                _ => c.w,
            };
            let limit = match aligned {
                Some((_, pos, _)) => avail_right.max(pos),
                None => avail_right,
            };
            if reach(&aligned, x + w) > limit + 0.01 && c.k != K::Space && j > line_start && !matches!(c.k, K::Label | K::LabelTab) {
                end = match last_break {
                    Some(b) if b > line_start => b,
                    _ => j,
                };
                break;
            }
            x += w;
            j += 1;
            if j < n && allowed[j] {
                last_break = Some(j);
            }
        }
        if j >= n && mandatory.is_none() {
            end = n;
        }
        let mut line = finish_line(env, &chars, &stys, &mut tab_w, line_start, end, start_x, avail_right, mandatory, &props, &next_tab, first_line, len, &mark_sty, y);
        // the line moved down below a floating object
        line.h += gap;
        line.base += gap;
        y += line.h;
        lines.push(line);
        i = end;
        first_line = false;
        if i >= n {
            // a paragraph ending with a line break still gets an empty last line
            if matches!(mandatory, Some(BreakKind::Line)) {
                continue;
            }
            break;
        }
    }

    // floating pictures: which line they are on
    let anchors = anchors_raw
        .into_iter()
        .map(|(d, ci)| {
            let off = chars[ci].off;
            let li = lines.iter().position(|l| off >= l.start && off < l.end.max(l.start + 1)).unwrap_or(0);
            (d, li)
        })
        .collect();

    let line_sum: f32 = lines.iter().map(|l| l.h).sum();
    let before = if props.before_auto == Some(true) { 14.0 } else { tw(props.before) };
    let after = if props.after_auto == Some(true) { 14.0 } else { tw(props.after) };
    ParaLayout {
        shd: props.shd.as_deref().and_then(color),
        borders: props.borders.clone(),
        props,
        lines,
        before,
        after,
        left,
        right,
        width,
        len,
        anchors,
        height: line_sum,
        notes: note_refs,
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_line(
    env: &Env,
    chars: &[Ch],
    stys: &[Sty],
    tab_w: &mut [f32],
    a: usize,
    b: usize,
    start_x: f32,
    avail_right: f32,
    mandatory: Option<BreakKind>,
    props: &ParaProps,
    next_tab: &dyn Fn(f32, bool) -> (f32, String, Option<char>),
    first_line: bool,
    para_len: usize,
    mark: &Sty,
    y: f32,
) -> Line {
    let last_line = b >= chars.len();
    // metrics
    let (mut asc, mut desc) = (0.0f32, 0.0f32);
    let mut any = false;
    for c in &chars[a..b] {
        match c.k {
            K::Image => {
                let h = c.img.as_ref().map(|d| d.height).unwrap_or(0.0);
                asc = asc.max(h);
                any = true;
            }
            K::Hidden => {}
            _ => {
                let f = env.fonts.face(stys[c.sty].face);
                asc = asc.max(f.ascent * c.size);
                desc = desc.max(f.descent * c.size);
                any = true;
            }
        }
    }
    if !any || last_line {
        // the paragraph mark counts for the last line
        let f = env.fonts.face(mark.face);
        asc = asc.max(f.ascent * mark.size);
        desc = desc.max(f.descent * mark.size);
    }
    let natural = asc + desc;
    let (h, base) = match (props.line_rule.as_deref(), props.line) {
        (Some("exact"), Some(v)) => {
            let h = v as f32 / 20.0;
            (h, (h - desc).max(asc.min(h)))
        }
        (Some("atLeast"), Some(v)) => {
            let h = natural.max(v as f32 / 20.0);
            (h, h - desc)
        }
        (_, Some(v)) => {
            // a multiple adds its space under the text (Word keeps the text at the
            // top of the line: a first line starts at the margin, a cell grows downwards);
            // a fraction takes it from above
            let h = natural * v as f32 / 240.0;
            (h, if h >= natural { asc } else { h - desc })
        }
        _ => (natural, asc),
    };

    // trailing spaces hang past the margin
    let mut content_end = b;
    while content_end > a && matches!(chars[content_end - 1].k, K::Space | K::Break(_) | K::Hidden) {
        content_end -= 1;
    }

    // x positions with left tabs
    let mut xs = vec![0.0f32; b - a + 1];
    let mut x = start_x;
    let mut tab_kinds: Vec<(usize, String, f32)> = Vec::new();
    for i in a..b {
        xs[i - a] = x;
        let w = match chars[i].k {
            K::Tab | K::LabelTab => {
                let (pos, kind, _) = next_tab(x, first_line);
                let w = (pos - x).max(0.0);
                tab_w[i] = w;
                if kind == "right" || kind == "center" || kind == "decimal" || kind == "end" {
                    tab_kinds.push((i, kind, pos));
                }
                w
            }
            K::Break(_) | K::Hidden => 0.0,
            _ => chars[i].w,
        };
        x += w;
    }
    xs[b - a] = x;

    // right/center tabs: pull the following segment back to the stop
    // left to right: each stop aligns what follows it after the earlier ones have moved it
    for (ti, kind, pos) in tab_kinds.iter() {
        let seg_end = (ti + 1..b).find(|&k| matches!(chars[k].k, K::Tab)).unwrap_or(content_end.max(ti + 1));
        let seg_w = xs[seg_end - a] - xs[ti + 1 - a];
        let want_start = if kind == "center" { pos - seg_w / 2.0 } else { pos - seg_w };
        let new_w = (want_start - xs[ti - a]).max(0.0);
        let delta = new_w - tab_w[*ti];
        tab_w[*ti] = new_w;
        for k in ti + 1..=b {
            xs[k - a] += delta;
        }
    }

    let line_w = xs[content_end - a] - start_x;
    let avail = avail_right - start_x;
    let jc = props.jc.as_deref().unwrap_or("left");
    let justify = matches!(jc, "both" | "distribute") && !last_line && mandatory.is_none();
    let mut shift = match jc {
        "center" => ((avail - line_w) / 2.0).max(0.0),
        "right" | "end" => (avail - line_w).max(0.0),
        _ => 0.0,
    };
    if !justify && line_w > avail {
        shift = 0.0;
    }
    let mut extra = 0.0;
    if justify {
        // only spaces after the last tab stretch
        let last_tab = (a..content_end).rev().find(|&k| matches!(chars[k].k, K::Tab | K::LabelTab)).map(|k| k + 1).unwrap_or(a);
        let spaces = (last_tab..content_end).filter(|&k| chars[k].k == K::Space).count();
        if spaces > 0 && line_w < avail {
            extra = (avail - line_w) / spaces as f32;
            let mut acc = 0.0;
            for k in a..=b {
                xs[k - a] += acc;
                if k < content_end && k >= last_tab && chars[k].k == K::Space {
                    acc += extra;
                }
            }
        }
    }

    // pieces
    let mut pieces = Vec::new();
    let mut k = a;
    while k < b {
        let c = &chars[k];
        let px = xs[k - a] + shift;
        match c.k {
            K::Tab | K::LabelTab => {
                let (_, _, leader) = next_tab(xs[k - a], first_line);
                let s = &stys[c.sty];
                if c.k == K::Tab {
                    pieces.push(Piece::Tab { x: px, w: tab_w[k], start: c.off, leader, face: s.face, size: s.size, color: s.color, deco: s.deco });
                }
                k += 1;
            }
            K::Break(_) => {
                pieces.push(Piece::Mark { x: px, start: c.off });
                k += 1;
            }
            K::Hidden => k += 1,
            K::Image => {
                let d = c.img.as_ref().unwrap();
                pieces.push(Piece::Image { x: px, w: d.width, h: d.height, src: d.src.clone(), start: c.off, chart: d.chart.clone() });
                k += 1;
            }
            K::Label => {
                let mut text = String::new();
                let s = &stys[c.sty];
                let face = c.face;
                while k < b && chars[k].k == K::Label && chars[k].face == face {
                    text.push(chars[k].c);
                    k += 1;
                }
                pieces.push(Piece::Label { x: px, text, face, size: s.size, color: s.color, dy: s.dy });
            }
            K::Text | K::Space => {
                let s = &stys[c.sty];
                let face = c.face;
                let mut text = String::new();
                let mut adv = Vec::new();
                let start = c.off;
                let sty = c.sty;
                let phantom = c.phantom;
                while k < b {
                    let d = &chars[k];
                    if !matches!(d.k, K::Text | K::Space) || d.sty != sty || d.face != face || d.phantom != phantom {
                        break;
                    }
                    // with justification every word is its own piece
                    if justify && !text.is_empty() && (d.k == K::Space) != (chars[k - 1].k == K::Space) {
                        break;
                    }
                    text.push(d.c);
                    adv.push(xs[k + 1 - a] - xs[k - a]);
                    k += 1;
                }
                pieces.push(Piece::Text { x: px, text, face, size: s.size, color: s.color, start, adv, dy: s.dy, deco: s.deco, hl: s.hl, phantom });
            }
        }
    }
    let start = if a < chars.len() { chars[a..].iter().find(|c| !matches!(c.k, K::Label | K::LabelTab)).map(|c| c.off).unwrap_or(para_len) } else { para_len };
    let end = if last_line { para_len } else { chars.get(b).map(|c| c.off).unwrap_or(para_len) };
    let _ = extra;
    Line {
        y,
        h,
        base,
        start: start.min(end),
        end,
        pieces,
        page_break: matches!(mandatory, Some(BreakKind::Page) | Some(BreakKind::Column)),
        x0: start_x + shift,
        x1: xs[b - a] + shift,
        wrapped: !last_line && mandatory.is_none(),
        changed: chars[a..b].iter().any(|c| c.tracked),
    }
}

impl Line {
    /// x of the caret before char `off` (clamped to the line).
    pub fn caret_x(&self, off: usize) -> f32 {
        let mut last = self.x0;
        for p in &self.pieces {
            match p {
                Piece::Text { x, start, adv, phantom, .. } => {
                    if off <= *start {
                        return *x;
                    }
                    // deleted text has no caret positions of its own
                    let n = if *phantom { 0 } else { adv.len() };
                    if off < start + n {
                        return x + adv[..off - start].iter().sum::<f32>();
                    }
                    last = x + adv.iter().sum::<f32>();
                }
                Piece::Tab { x, w, start, .. } => {
                    if off <= *start {
                        return *x;
                    }
                    last = x + w;
                }
                Piece::Image { x, w, start, .. } => {
                    if off <= *start {
                        return *x;
                    }
                    last = x + w;
                }
                Piece::Mark { x, start } => {
                    if off <= *start {
                        return *x;
                    }
                    last = *x;
                }
                Piece::Label { .. } => {}
            }
        }
        last
    }

    /// Offset nearest to `x`.
    pub fn hit(&self, px: f32) -> usize {
        for p in &self.pieces {
            match p {
                Piece::Text { x, start, adv, phantom: true, .. } => {
                    let w: f32 = adv.iter().sum();
                    if px < x + w {
                        return *start;
                    }
                }
                Piece::Text { x, start, adv, .. } => {
                    let mut cx = *x;
                    for (i, a) in adv.iter().enumerate() {
                        if px < cx + a / 2.0 {
                            return start + i;
                        }
                        cx += a;
                    }
                }
                Piece::Tab { x, w, start, .. } | Piece::Image { x, w, start, .. } => {
                    if px < x + w / 2.0 {
                        return *start;
                    }
                }
                Piece::Mark { start, .. } => return *start,
                Piece::Label { .. } => {}
            }
        }
        self.end_caret()
    }

    /// Last caret position on this line: before a trailing space or break
    /// of a wrapped line, the line end otherwise.
    pub fn end_caret(&self) -> usize {
        match self.pieces.last() {
            Some(Piece::Mark { start, .. }) => *start,
            Some(Piece::Text { text, start, adv, phantom: false, .. }) if self.wrapped && text.ends_with(' ') && start + adv.len() == self.end => self.end - 1,
            _ => self.end,
        }
    }
}
