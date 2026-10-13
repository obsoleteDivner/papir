//! Boxes (cells, headers), tables, pages.

use super::para::{self, ExKind, Excl, Label, ParaCtx, ParaLayout, Piece};
use super::{CacheKey, Env, color};
use crate::model::*;
use crate::props::{Border, Margins, ParaProps, RunProps, SectProps, TableProps, VMerge};
use crate::styles::format_counter;
use crate::xml::Ns;
use std::collections::HashMap;
use std::rc::Rc;

pub type PathRc = Rc<[u32]>;

#[derive(Clone, Debug)]
pub enum Placed {
    /// a line of a paragraph; `x`/`w` is the paragraph's column box
    Line { path: Option<PathRc>, para: Rc<ParaLayout>, idx: usize, x: f32, y: f32, w: f32 },
    Rect { x: f32, y: f32, w: f32, h: f32, color: u32 },
    Stroke { x1: f32, y1: f32, x2: f32, y2: f32, w: f32, color: u32, style: u8 },
    Image { x: f32, y: f32, w: f32, h: f32, src: Option<String> },
    /// text outside paragraphs (chart labels); `y` is the baseline
    Text { x: f32, y: f32, text: String, face: crate::fonts::FaceId, size: f32, color: u32 },
    /// filled polygon (chart areas and pie slices)
    Poly { pts: Vec<(f32, f32)>, color: u32 },
}

impl Placed {
    fn shift(&mut self, dx: f32, dy: f32) {
        match self {
            Placed::Line { x, y, .. } | Placed::Rect { x, y, .. } | Placed::Image { x, y, .. } | Placed::Text { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Placed::Poly { pts, .. } => {
                for p in pts.iter_mut() {
                    p.0 += dx;
                    p.1 += dy;
                }
            }
            Placed::Stroke { x1, y1, x2, y2, .. } => {
                *x1 += dx;
                *x2 += dx;
                *y1 += dy;
                *y2 += dy;
            }
        }
    }
}

/// Column boundaries of a table on a page (for resizing with the mouse).
#[derive(Clone, Debug)]
pub struct TableBox {
    pub path: PathRc,
    /// x of every grid boundary, points
    pub cols: Vec<f32>,
    pub y1: f32,
    pub y2: f32,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub w: f32,
    pub h: f32,
    pub items: Vec<Placed>,
    pub number: i32,
    pub sect: usize,
    pub first_of_sect: bool,
    /// body area, for the ruler
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
    pub tables: Vec<TableBox>,
    /// header and footer relationship ids used on this page
    pub header: Option<String>,
    pub footer: Option<String>,
}

pub struct Layout {
    pub pages: Vec<Page>,
    /// paragraph path → (page, item index) of each of its lines
    pub index: HashMap<PathRc, Vec<(usize, usize)>>,
    /// body paragraphs in document order
    pub order: Vec<PathRc>,
    pub order_pos: HashMap<PathRc, usize>,
}

/// Table style context for paragraphs in cells.
#[derive(Clone, Default)]
struct TCtx {
    id: Option<String>,
    ppr: Option<ParaProps>,
    rpr: Option<RunProps>,
}

fn para_layout(env: &Env, p: &Para, width: f32, t: &TCtx, label: Option<&Label>, comments: u8, excl: &[Excl]) -> Rc<ParaLayout> {
    let ex = excl.iter().fold(0u64, |h, e| {
        let k = match e.kind {
            ExKind::Above => 1.0f32,
            ExKind::Left(x) => 2.0 + x,
            ExKind::Right(x) => -2.0 - x,
        };
        [e.y1, e.y2, k].iter().fold(h, |h, v| h.wrapping_mul(1_000_003).wrapping_add(v.to_bits() as u64))
    });
    let notes = note_sig(env, p);
    let key = CacheKey { rev: p.rev, width: width.to_bits(), table: t.id.clone(), label: label.map(|l| l.text.clone()), comments, excl: ex, notes };
    let mut cache = env.cache.borrow_mut();
    let epoch = cache.epoch;
    if let Some((g, l)) = cache.map.get_mut(&key) {
        *g = epoch;
        return l.clone();
    }
    drop(cache);
    let ctx = ParaCtx { table_ppr: t.ppr.as_ref(), table_rpr: t.rpr.as_ref(), label, comments, excl };
    let l = Rc::new(para::layout(env, p, width, &ctx));
    env.cache.borrow_mut().map.insert(key, (epoch, l.clone()));
    l
}

/// The footnote numbers a paragraph shows, as one value (part of the cache key).
fn note_sig(env: &Env, p: &Para) -> u64 {
    let mut h = 0u64;
    for inl in &p.inlines {
        if let crate::model::Inline::Run(r) = inl {
            for it in &r.items {
                if let crate::model::Item::Raw(e) = it {
                    let n = if e.is_w("footnoteReference") {
                        env.cache.borrow().notes.get(e.wattr("id").unwrap_or("")).copied().unwrap_or(0)
                    } else if e.is_w("footnoteRef") {
                        env.cache.borrow().cur_note
                    } else {
                        continue;
                    };
                    h = h.wrapping_mul(1_000_003).wrapping_add(n as u64 + 1);
                }
            }
        }
    }
    h
}

/// Footnote numbers in the order of their references in the body.
fn number_notes(doc: &crate::docx::Document) -> HashMap<String, usize> {
    fn walk(c: &Container, out: &mut HashMap<String, usize>) {
        for b in c {
            match &**b {
                Block::Para(p) => {
                    for inl in &p.inlines {
                        if let crate::model::Inline::Run(r) = inl {
                            for it in &r.items {
                                if let crate::model::Item::Raw(e) = it {
                                    if e.is_w("footnoteReference") && e.wattr("customMarkFollows").is_none_or(|v| v == "0" || v == "false") {
                                        if let Some(id) = e.wattr("id") {
                                            let n = out.len() + 1;
                                            out.entry(id.to_string()).or_insert(n);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Block::Table(t) => {
                    for r in t.rows() {
                        for cell in r.cells() {
                            walk(&cell.content, out);
                        }
                    }
                }
                Block::Sdt(s) => walk(&s.content, out),
                Block::Raw(_) => {}
            }
        }
    }
    let mut out = HashMap::new();
    if !doc.footnotes.is_empty() {
        walk(&doc.body, &mut out);
    }
    out
}

/// Footnotes referenced from line `li`.
fn line_notes(pl: &ParaLayout, li: usize) -> Vec<String> {
    if pl.notes.is_empty() {
        return Vec::new();
    }
    let l = &pl.lines[li];
    let last = li + 1 == pl.lines.len();
    pl.notes.iter().filter(|(off, _)| *off >= l.start && (*off < l.end || (last && *off <= l.end))).map(|(_, id)| id.clone()).collect()
}

/// Space between the text and the footnotes (the separator line sits in it).
const NOTE_GAP: f32 = 12.0;

// ---------- numbering ----------

#[derive(Default)]
struct Counters {
    by_abs: HashMap<i32, [Option<i32>; 9]>,
    seen_num: std::collections::HashSet<i32>,
}

fn label_for(env: &Env, c: &mut Counters, eff: &ParaProps) -> Option<Label> {
    let (num, lvl) = (eff.num_id?, eff.ilvl.unwrap_or(0).clamp(0, 8));
    if num <= 0 {
        return None;
    }
    let nb = &env.doc.numbering;
    let level = nb.level(num, lvl)?;
    let abs = nb.abstract_of(num)?;
    let counters = c.by_abs.entry(abs).or_insert([None; 9]);
    if c.seen_num.insert(num) {
        if let Some((_, starts, _)) = nb.nums.get(&num) {
            for l in starts.keys() {
                if (0..9).contains(l) {
                    counters[*l as usize] = None;
                }
            }
        }
    }
    let li = lvl as usize;
    counters[li] = Some(counters[li].map(|v| v + 1).unwrap_or_else(|| nb.start(num, lvl)));
    for d in counters.iter_mut().skip(li + 1) {
        *d = None;
    }
    let text = if level.fmt == "bullet" {
        level.text.clone()
    } else {
        let mut t = level.text.clone();
        for k in (0..9).rev() {
            let pat = format!("%{}", k + 1);
            if t.contains(&pat) {
                let v = counters[k].unwrap_or_else(|| nb.start(num, k as i32));
                let fmt = nb.level(num, k as i32).map(|l| l.fmt.clone()).unwrap_or_else(|| "decimal".into());
                let fmt = if level.legal { "decimal".to_string() } else { fmt };
                t = t.replace(&pat, &format_counter(v, &fmt));
            }
        }
        t
    };
    Some(Label { text, rpr: level.rpr.clone(), suff: level.suff.clone() })
}

/// Per-paragraph context computed in document order: list labels and the
/// number of comment ranges open at the paragraph start.
#[derive(Default)]
struct Ordered {
    labels: HashMap<Vec<u32>, Label>,
    comments: HashMap<Vec<u32>, u8>,
    open: i32,
}

/// Labels of all numbered body paragraphs, in document order.
fn number_all(env: &Env) -> Ordered {
    let mut out = Ordered::default();
    let mut c = Counters::default();
    fn walk(env: &Env, cont: &Container, path: &mut Vec<u32>, t: &TCtx, c: &mut Counters, out: &mut Ordered) {
        for (i, b) in cont.iter().enumerate() {
            path.push(i as u32);
            match &**b {
                Block::Para(p) => {
                    let eff = para::effective(env.doc, p, t.ppr.as_ref());
                    if let Some(l) = label_for(env, c, &eff) {
                        out.labels.insert(path.clone(), l);
                    }
                    if out.open > 0 {
                        out.comments.insert(path.clone(), out.open.min(255) as u8);
                    }
                    for inl in &p.inlines {
                        if let Inline::Raw(e) = inl {
                            let open = |l: &str| crate::comments::is_marker(e, l).filter(|i| env.doc.comments.get(i).is_none_or(|c| !c.done)).is_some();
                            if open("commentRangeStart") {
                                out.open += 1;
                            } else if open("commentRangeEnd") {
                                out.open = (out.open - 1).max(0);
                            }
                        }
                    }
                }
                Block::Table(tb) => {
                    let tc = table_ctx(env, tb);
                    for (ri, r) in tb.rows().enumerate() {
                        for (ci, cell) in r.cells().enumerate() {
                            path.push(ri as u32);
                            path.push(ci as u32);
                            walk(env, &cell.content, path, &tc, c, out);
                            path.pop();
                            path.pop();
                        }
                    }
                }
                Block::Sdt(s) => walk(env, &s.content, path, t, c, out),
                Block::Raw(_) => {}
            }
            path.pop();
        }
    }
    walk(env, &env.doc.body, &mut Vec::new(), &TCtx::default(), &mut c, &mut out);
    out
}

fn table_ctx(env: &Env, t: &Table) -> TCtx {
    let (_, p, r) = env.doc.styles.table(t.props.style.as_deref());
    TCtx { id: t.props.style.clone().or_else(|| env.doc.styles.default_table.clone()), ppr: Some(p), rpr: Some(r) }
}

// ---------- boxes ----------

struct BoxCx<'a> {
    labels: Option<&'a Ordered>,
    order: Option<&'a mut Vec<PathRc>>,
}

/// Lays a container out without page breaks (cells, headers, footers).
/// Returns the height; items are relative to the box's top-left.
fn layout_box(env: &Env, cont: &Container, path: Option<&mut Vec<u32>>, width: f32, t: &TCtx, bx: &mut BoxCx, out: &mut Vec<Placed>) -> f32 {
    let mut y = 0.0f32;
    let mut prev: Option<(Option<String>, bool, f32)> = None;
    // automatic spacing (before/afterAutospacing) does not apply at the edges of a box, as in Word
    let mut last_auto = false;
    let mut dummy = Vec::new();
    let has_path = path.is_some();
    let path = path.unwrap_or(&mut dummy);
    for (i, b) in cont.iter().enumerate() {
        path.push(i as u32);
        match &**b {
            Block::Para(p) => {
                let label = if has_path { bx.labels.and_then(|l| l.labels.get(path.as_slice())) } else { None };
                let depth = if has_path { bx.labels.and_then(|l| l.comments.get(path.as_slice())).copied().unwrap_or(0) } else { 0 };
                let pl = para_layout(env, p, width, t, label, depth, &[]);
                let pr: Option<PathRc> = if has_path { Some(path.as_slice().into()) } else { None };
                if let (Some(o), Some(pr)) = (bx.order.as_deref_mut(), &pr) {
                    o.push(pr.clone());
                }
                let (before, prev_after) = spacing(&pl, prev.as_ref());
                let before = if i == 0 && pl.props.before_auto == Some(true) { 0.0 } else { before };
                y += prev_after + before;
                last_auto = pl.props.after_auto == Some(true);
                let top = y;
                for (li, l) in pl.lines.iter().enumerate() {
                    out.push(Placed::Line { path: pr.clone(), para: pl.clone(), idx: li, x: 0.0, y: y + 0.0 * l.y, w: width });
                    line_charts(env, &pl, li, 0.0, y, out);
                    y += l.h;
                }
                decorate(&pl, 0.0, width, top, y, out);
                for (d, li) in &pl.anchors {
                    let ly = top + pl.lines[..*li].iter().map(|l| l.h).sum::<f32>();
                    let g = FloatGeo { page_w: width, page_h: 0.0, left: 0.0, width, top_margin: 0.0, body_h: 0.0, para_top: top, line_top: ly };
                    if let Some((fx, fy)) = float_pos(d, &g) {
                        float_items(env, d, fx, fy, out);
                    }
                }
                prev = Some((pl.props.style.clone(), pl.props.contextual == Some(true), pl.after));
            }
            Block::Table(tb) => {
                if let Some((_, _, a)) = prev.take() {
                    y += a;
                }
                let g = table_geo(env, tb, if has_path { Some(path) } else { None }, width, bx);
                let rows: Vec<usize> = (0..g.rows.len()).collect();
                y += place_rows(&g, &rows, 0.0, y, out);
            }
            Block::Sdt(s) => {
                let mut sub = Vec::new();
                let h = layout_box(env, &s.content, if has_path { Some(path) } else { None }, width, t, bx, &mut sub);
                if let Some((_, _, a)) = prev.take() {
                    y += a;
                }
                for mut it in sub {
                    it.shift(0.0, y);
                    out.push(it);
                }
                y += h;
            }
            Block::Raw(_) => {}
        }
        path.pop();
    }
    if let Some((_, _, a)) = prev {
        if !last_auto {
            y += a;
        }
    }
    y
}

/// Where floating objects are positioned against.
struct FloatGeo {
    page_w: f32,
    page_h: f32,
    left: f32,
    width: f32,
    top_margin: f32,
    body_h: f32,
    para_top: f32,
    line_top: f32,
}

/// Top-left corner of a floating object.
fn float_pos(d: &Drawing, g: &FloatGeo) -> Option<(f32, f32)> {
    let f = d.anchor.as_ref()?;
    let (bx, span) = match f.rel_h.as_str() {
        "page" => (0.0, g.page_w),
        "leftMargin" | "insideMargin" => (0.0, g.left),
        "rightMargin" | "outsideMargin" => (g.left + g.width, (g.page_w - g.left - g.width).max(0.0)),
        _ => (g.left, g.width),
    };
    let x = match f.align_h.as_deref() {
        Some("left") | Some("inside") => bx,
        Some("center") => bx + (span - d.width) / 2.0,
        Some("right") | Some("outside") => bx + span - d.width,
        _ => bx + f.x,
    };
    let (by, vspan) = match f.rel_v.as_str() {
        "page" => (0.0, g.page_h),
        "margin" => (g.top_margin, g.body_h),
        "topMargin" => (0.0, g.top_margin),
        "bottomMargin" => (g.top_margin + g.body_h, (g.page_h - g.top_margin - g.body_h).max(0.0)),
        "line" => (g.line_top, 0.0),
        _ => (g.para_top, 0.0),
    };
    let y = match f.align_v.as_deref() {
        Some("top") => by,
        Some("center") => by + (vspan - d.height) / 2.0,
        Some("bottom") => by + vspan - d.height,
        _ => by + f.y,
    };
    Some((x, y))
}

/// Picture, shape fill and outline, text box content.
fn float_items(env: &Env, d: &Drawing, x: f32, y: f32, out: &mut Vec<Placed>) {
    let mut v = Vec::new();
    if let Some(c) = d.fill {
        v.push(Placed::Rect { x, y, w: d.width, h: d.height, color: c });
    }
    if let Some(c) = d.chart.as_ref().and_then(|c| env.doc.charts.get(c)) {
        v.extend(c.render(env.fonts, x, y, d.width, d.height));
    } else if d.src.is_some() {
        v.push(Placed::Image { x, y, w: d.width, h: d.height, src: d.src.clone() });
    }
    if let Some(content) = &d.txbx {
        let [t, _, l, r] = d.insets;
        let mut items = Vec::new();
        let mut bx = BoxCx { labels: None, order: None };
        layout_box(env, content, None, (d.width - l - r).max(8.0), &TCtx::default(), &mut bx, &mut items);
        for mut it in items {
            it.shift(x + l, y + t);
            v.push(it);
        }
    }
    if let Some((c, w)) = d.line {
        let (x2, y2) = (x + d.width, y + d.height);
        for (a, b, cc, e) in [(x, y, x2, y), (x2, y, x2, y2), (x2, y2, x, y2), (x, y2, x, y)] {
            v.push(Placed::Stroke { x1: a, y1: b, x2: cc, y2: e, w, color: c, style: 0 });
        }
    }
    if d.anchor.as_ref().is_some_and(|f| f.wrap == Wrap::Behind) {
        for (i, it) in v.into_iter().enumerate() {
            out.insert(i, it);
        }
    } else {
        out.extend(v);
    }
}

/// Charts among a line's inline pictures, placed with the line.
fn line_charts(env: &Env, pl: &ParaLayout, li: usize, x: f32, y: f32, out: &mut Vec<Placed>) {
    let l = &pl.lines[li];
    for p in &l.pieces {
        if let Piece::Image { x: px, w, h, chart: Some(c), .. } = p {
            if let Some(ch) = env.doc.charts.get(c) {
                out.extend(ch.render(env.fonts, x + px, y + l.base - h, *w, *h));
            }
        }
    }
}

/// An area of the page text keeps out of.
#[derive(Clone, Copy, Debug)]
struct PageExcl {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    wrap: Wrap,
}

/// (space before this paragraph, space after the previous one).
fn spacing(pl: &ParaLayout, prev: Option<&(Option<String>, bool, f32)>) -> (f32, f32) {
    match prev {
        None => (pl.before, 0.0),
        Some((style, ctx_prev, after)) => {
            let same = *style == pl.props.style;
            let after = if same && *ctx_prev { 0.0 } else { *after };
            let before = if same && pl.props.contextual == Some(true) { 0.0 } else { pl.before };
            (before, after)
        }
    }
}

/// Paragraph shading and borders.
fn decorate(pl: &ParaLayout, x: f32, w: f32, top: f32, bottom: f32, out: &mut Vec<Placed>) {
    let (l, r) = (x + pl.left, x + w - pl.right);
    if let Some(c) = pl.shd {
        out.insert(0, Placed::Rect { x: l, y: top, w: r - l, h: bottom - top, color: c });
    }
    if let Some(b) = &pl.borders {
        let line = |b: &Option<Border>, x1: f32, y1: f32, x2: f32, y2: f32, out: &mut Vec<Placed>| {
            if let Some(b) = b.as_ref().filter(|b| b.visible()) {
                out.push(Placed::Stroke { x1, y1, x2, y2, w: b.width(), color: b.color.as_deref().and_then(color).unwrap_or(0), style: style_of(b) });
            }
        };
        line(&b.top, l, top, r, top, out);
        line(&b.bottom, l, bottom, r, bottom, out);
        line(&b.left, l - 4.0, top, l - 4.0, bottom, out);
        line(&b.right, r + 4.0, top, r + 4.0, bottom, out);
    }
}

fn style_of(b: &Border) -> u8 {
    match b.style.as_str() {
        "double" => 1,
        "dotted" => 2,
        "dashed" | "dashSmallGap" => 3,
        _ => 0,
    }
}

// ---------- tables ----------

struct CellGeo {
    x: f32,
    w: f32,
    items: Vec<Placed>,
    content_h: f32,
    span_rows: usize,
    cont: bool,
    shd: Option<u32>,
    edges: [Option<Border>; 4],
    mar: (f32, f32, f32, f32),
    valign: Option<String>,
}

struct RowGeo {
    h: f32,
    header: bool,
    cells: Vec<CellGeo>,
}

struct TableGeo {
    rows: Vec<RowGeo>,
    /// boundaries of the grid columns relative to the column box
    cols: Vec<f32>,
}

fn eff_table(env: &Env, t: &Table) -> TableProps {
    let (mut tp, _, _) = env.doc.styles.table(t.props.style.as_deref());
    tp.borders.merge(&t.props.borders);
    tp.cell_mar.merge(&t.props.cell_mar);
    if t.props.ind.is_some() {
        tp.ind = t.props.ind;
    }
    if t.props.jc.is_some() {
        tp.jc = t.props.jc.clone();
    }
    tp.width = t.props.width.clone();
    tp
}

fn table_geo(env: &Env, t: &Table, path: Option<&mut Vec<u32>>, col_w: f32, bx: &mut BoxCx) -> TableGeo {
    let tp = eff_table(env, t);
    let tc = table_ctx(env, t);
    let mut mar = Margins { top: Some(0), left: Some(108), bottom: Some(0), right: Some(108) };
    mar.merge(&tp.cell_mar);
    let tw = |v: Option<i32>| v.unwrap_or(0) as f32 / 20.0;

    let rows: Vec<&Row> = t.rows().collect();
    let mut grid: Vec<f32> = t.grid.iter().map(|g| *g as f32 / 20.0).collect();
    if grid.iter().all(|g| *g <= 0.0) {
        // no grid: take the widest row's cell widths
        grid = rows
            .iter()
            .map(|r| r.cells().map(|c| c.props.width.unwrap_or(0) as f32 / 20.0).collect::<Vec<_>>())
            .max_by_key(|v| v.len())
            .unwrap_or_default();
        if grid.iter().all(|g| *g <= 0.0) && !grid.is_empty() {
            let n = grid.len() as f32;
            grid = vec![col_w / n; grid.len()];
        }
    }
    let ncols = grid.len();
    let mut col_x = vec![0.0f32; ncols + 1];
    for i in 0..ncols {
        col_x[i + 1] = col_x[i] + grid[i];
    }
    let table_w = col_x[ncols];
    let compat15 = true;
    let mut x0 = tw(tp.ind);
    if !compat15 {
        x0 -= tw(mar.left);
    }
    match tp.jc.as_deref() {
        Some("center") => x0 = (col_w - table_w) / 2.0,
        Some("right") | Some("end") => x0 = col_w - table_w,
        _ => {}
    }

    // grid column of every cell
    let pos: Vec<Vec<(usize, usize)>> = rows
        .iter()
        .map(|r| {
            let mut g = r.props.grid_before as usize;
            r.cells()
                .map(|c| {
                    let s = (g, c.props.span.max(1) as usize);
                    g += s.1;
                    s
                })
                .collect()
        })
        .collect();

    let mut has_path = path.is_some();
    let mut dummy = Vec::new();
    let path = path.unwrap_or(&mut dummy);
    let nrows = rows.len();
    let mut geo = TableGeo { rows: Vec::with_capacity(nrows), cols: col_x.iter().map(|c| x0 + c).collect() };
    for (ri, r) in rows.iter().enumerate() {
        let mut cells = Vec::new();
        for (ci, c) in r.cells().enumerate() {
            let (gx, span) = pos[ri][ci];
            let gx = gx.min(ncols);
            let ge = (gx + span).min(ncols);
            let x = x0 + col_x[gx];
            let w = if ge > gx { col_x[ge] - col_x[gx] } else { c.props.width.unwrap_or(0) as f32 / 20.0 };
            let mut m = mar;
            m.merge(&c.props.mar);
            let (mt, ml, mb, mr) = (tw(m.top), tw(m.left), tw(m.bottom), tw(m.right));
            let cont = c.props.vmerge == Some(VMerge::Continue);
            let mut span_rows = 1;
            if c.props.vmerge == Some(VMerge::Restart) {
                for (rj, row) in rows.iter().enumerate().skip(ri + 1) {
                    let hit = row.cells().zip(&pos[rj]).any(|(cc, (g, _))| *g == gx && cc.props.vmerge == Some(VMerge::Continue));
                    if hit {
                        span_rows += 1;
                    } else {
                        break;
                    }
                }
            }
            let mut items = Vec::new();
            path.push(ri as u32);
            path.push(ci as u32);
            let content_h = if cont {
                0.0
            } else {
                layout_box(env, &c.content, if has_path { Some(path) } else { None }, (w - ml - mr).max(1.0), &tc, bx, &mut items)
            };
            if cont {
                // keep caret targets for paragraphs of merged-away cells
                if let Some(o) = bx.order.as_deref_mut() {
                    let _ = o;
                }
            }
            path.pop();
            path.pop();
            let b = &c.props.borders;
            let last_row = ri + span_rows >= nrows;
            let edges = [
                b.top.clone().or_else(|| if ri == 0 { tp.borders.top.clone() } else { tp.borders.inside_h.clone() }),
                b.left.clone().or_else(|| if gx == 0 { tp.borders.left.clone() } else { tp.borders.inside_v.clone() }),
                b.bottom.clone().or_else(|| if last_row { tp.borders.bottom.clone() } else { tp.borders.inside_h.clone() }),
                b.right.clone().or_else(|| if ge >= ncols { tp.borders.right.clone() } else { tp.borders.inside_v.clone() }),
            ];
            cells.push(CellGeo {
                x,
                w,
                items,
                content_h: content_h + mt + mb,
                span_rows,
                cont,
                shd: c.props.shd.as_deref().and_then(color),
                edges,
                mar: (mt, ml, mb, mr),
                valign: c.props.valign.clone(),
            });
        }
        let mut h = cells.iter().filter(|c| !c.cont && c.span_rows == 1).map(|c| c.content_h).fold(0.0f32, f32::max);
        if let Some(rh) = r.props.height {
            let rh = rh as f32 / 20.0;
            h = if r.props.exact { rh } else { h.max(rh) };
        }
        if cells.is_empty() {
            h = h.max(1.0);
        }
        geo.rows.push(RowGeo { h, header: r.props.header, cells });
    }
    // vertically merged cells: grow the last spanned row if needed
    for ri in 0..geo.rows.len() {
        for ci in 0..geo.rows[ri].cells.len() {
            let (span, need) = {
                let c = &geo.rows[ri].cells[ci];
                (c.span_rows, c.content_h)
            };
            if span > 1 {
                let end = (ri + span).min(geo.rows.len());
                let have: f32 = geo.rows[ri..end].iter().map(|r| r.h).sum();
                if need > have {
                    geo.rows[end - 1].h += need - have;
                }
            }
        }
    }
    has_path = false;
    let _ = has_path;
    geo
}

/// Places the given rows starting at `y`; returns the height used.
fn place_rows(g: &TableGeo, rows: &[usize], x: f32, y: f32, out: &mut Vec<Placed>) -> f32 {
    let mut cy = y;
    let mut strokes = Vec::new();
    for (k, &ri) in rows.iter().enumerate() {
        let r = &g.rows[ri];
        for c in &r.cells {
            if c.cont {
                continue;
            }
            // height of the merged area among the rows placed here
            let mut h = r.h;
            let mut j = k + 1;
            while j < rows.len() && rows[j] < ri + c.span_rows && rows[j] == rows[j - 1] + 1 {
                h += g.rows[rows[j]].h;
                j += 1;
            }
            if let Some(col) = c.shd {
                out.push(Placed::Rect { x: x + c.x, y: cy, w: c.w, h, color: col });
            }
            let (mt, ml, _, _) = c.mar;
            let free = (h - c.content_h).max(0.0);
            let dy = match c.valign.as_deref() {
                Some("center") => free / 2.0,
                Some("bottom") => free,
                _ => 0.0,
            };
            for it in &c.items {
                let mut it = it.clone();
                it.shift(x + c.x + ml, cy + mt + dy);
                out.push(it);
            }
            let (x1, x2, y1, y2) = (x + c.x, x + c.x + c.w, cy, cy + h);
            let mut edge = |b: &Option<Border>, ax: f32, ay: f32, bx: f32, by: f32| {
                if let Some(b) = b.as_ref().filter(|b| b.visible()) {
                    strokes.push(Placed::Stroke { x1: ax, y1: ay, x2: bx, y2: by, w: b.width(), color: b.color.as_deref().and_then(color).unwrap_or(0), style: style_of(b) });
                }
            };
            edge(&c.edges[0], x1, y1, x2, y1);
            edge(&c.edges[1], x1, y1, x1, y2);
            edge(&c.edges[2], x1, y2, x2, y2);
            edge(&c.edges[3], x2, y1, x2, y2);
        }
        cy += r.h;
    }
    out.extend(strokes);
    cy - y
}

// ---------- pages ----------

struct Pager<'a, 'b> {
    env: &'a Env<'b>,
    pages: Vec<Page>,
    items: Vec<Placed>,
    sp: SectProps,
    sect: usize,
    y: f32,
    top: f32,
    bottom: f32,
    left: f32,
    width: f32,
    at_top: bool,
    first_of_sect: bool,
    page_no: i32,
    hdr: [Option<String>; 3],
    ftr: [Option<String>; 3],
    prev: Option<(Option<String>, bool, f32)>,
    labels: Ordered,
    order: Vec<PathRc>,
    started: bool,
    tables: Vec<TableBox>,
    /// floating objects on this page that text wraps around
    excl: Vec<PageExcl>,
    /// footnotes of this page and the room they take at its bottom
    page_notes: Vec<String>,
    notes_h: f32,
    note_heights: HashMap<String, f32>,
}

impl<'a, 'b> Pager<'a, 'b> {
    /// Height of a footnote at the text width.
    fn note_height(&mut self, id: &str) -> f32 {
        if let Some(h) = self.note_heights.get(id) {
            return *h;
        }
        let mut items = Vec::new();
        let h = self.note_items(id, &mut items);
        self.note_heights.insert(id.to_string(), h);
        h
    }

    fn note_items(&self, id: &str, out: &mut Vec<Placed>) -> f32 {
        let Some(c) = self.env.doc.footnotes.get(id) else { return 0.0 };
        let n = self.env.cache.borrow().notes.get(id).copied().unwrap_or(0);
        let prev = std::mem::replace(&mut self.env.cache.borrow_mut().cur_note, n);
        let mut bx = BoxCx { labels: None, order: None };
        let h = layout_box(self.env, c, None, self.width, &TCtx::default(), &mut bx, out);
        self.env.cache.borrow_mut().cur_note = prev;
        h
    }

    /// Room the footnotes of a line need on this page (with the separator for the first).
    fn notes_need(&mut self, pl: &ParaLayout, li: usize, pending: bool) -> f32 {
        let ids = line_notes(pl, li);
        if ids.is_empty() {
            return 0.0;
        }
        let h: f32 = ids.iter().map(|id| self.note_height(id)).sum();
        h + if self.page_notes.is_empty() && !pending { NOTE_GAP } else { 0.0 }
    }
    fn story_height(&self, rid: &Option<String>, w: f32) -> f32 {
        let Some(rid) = rid else { return 0.0 };
        let Some(st) = self.env.doc.stories.get(rid) else { return 0.0 };
        let mut items = Vec::new();
        let mut bx = BoxCx { labels: None, order: None };
        layout_box(self.env, &st.content, None, w, &TCtx::default(), &mut bx, &mut items)
    }

    fn refs(&self, first: bool) -> (Option<String>, Option<String>) {
        let i = if first && self.sp.title_pg { 1 } else { 0 };
        (self.hdr[i].clone(), self.ftr[i].clone())
    }

    fn flush(&mut self) {
        if !self.started {
            return;
        }
        // footnotes at the bottom: a short line, then each note
        if !self.page_notes.is_empty() {
            let top = self.bottom - self.notes_h;
            let len = (self.width / 3.0).min(144.0);
            self.items.push(Placed::Stroke { x1: self.left, y1: top + NOTE_GAP / 2.0, x2: self.left + len, y2: top + NOTE_GAP / 2.0, w: 0.5, color: 0, style: 0 });
            let mut y = top + NOTE_GAP;
            for id in std::mem::take(&mut self.page_notes) {
                let mut items = Vec::new();
                let h = self.note_items(&id, &mut items);
                for mut it in items {
                    it.shift(self.left, y);
                    self.items.push(it);
                }
                y += h;
            }
            self.notes_h = 0.0;
        }
        let sp = &self.sp;
        let tw = |v: i32| v as f32 / 20.0;
        self.pages.push(Page {
            w: tw(sp.w),
            h: tw(sp.h),
            items: std::mem::take(&mut self.items),
            number: self.page_no,
            sect: self.sect,
            first_of_sect: self.first_of_sect,
            left: tw(sp.left),
            right: tw(sp.w - sp.right),
            top: self.top,
            bottom: self.bottom,
            tables: std::mem::take(&mut self.tables),
            header: None,
            footer: None,
        });
        self.started = false;
    }

    fn new_page(&mut self) {
        self.flush();
        self.started = true;
        self.page_no += 1;
        let sp = self.sp.clone();
        let tw = |v: i32| v as f32 / 20.0;
        self.left = tw(sp.left);
        self.width = tw(sp.w - sp.left - sp.right).max(36.0);
        let (h, f) = self.refs(self.first_of_sect);
        let hh = self.story_height(&h, self.width);
        let fh = self.story_height(&f, self.width);
        // a header or footer pushes the body only when there is one (its distance alone does not)
        self.top = if h.is_some() { tw(sp.top).max(tw(sp.header) + hh) } else { tw(sp.top) };
        self.bottom = tw(sp.h) - if f.is_some() { tw(sp.bottom).max(tw(sp.footer) + fh) } else { tw(sp.bottom) };
        if self.bottom - self.top < 72.0 {
            self.bottom = self.top + 72.0;
        }
        self.y = self.top;
        self.at_top = true;
        self.prev = None;
        self.excl.clear();
        self.page_notes.clear();
        self.notes_h = 0.0;
    }

    fn next_page(&mut self) {
        self.first_of_sect = false;
        self.new_page();
    }

    fn container(&mut self, cont: &Container, path: &mut Vec<u32>) {
        let n = cont.len();
        for i in 0..n {
            path.push(i as u32);
            match &*cont[i] {
                Block::Para(p) => {
                    let keep_next_h = if p.props.keep_next == Some(true) || self.env.doc.styles.para(&p.props, None).keep_next == Some(true) {
                        cont.get(i + 1).map(|nb| self.first_line_h(nb))
                    } else {
                        None
                    };
                    self.para(p, path, keep_next_h);
                    if let Some(s) = p.sect() {
                        self.section_end(s);
                    }
                }
                Block::Table(t) => self.table(t, path),
                Block::Sdt(s) => self.container(&s.content, path),
                Block::Raw(_) => {}
            }
            path.pop();
        }
    }

    fn first_line_h(&self, b: &Block) -> f32 {
        match b {
            Block::Para(p) => {
                let pl = para_layout(self.env, p, self.width, &TCtx::default(), None, 0, &[]);
                pl.before + pl.lines.first().map(|l| l.h).unwrap_or(0.0)
            }
            _ => 0.0,
        }
    }

    fn section_end(&mut self, s: &crate::xml::Element) {
        // the paragraph's sectPr describes the section that ends here;
        // the next section's properties come from the next sectPr
        let _ = s;
        self.sect += 1;
        let next = self.next_sect_props();
        let continuous = next.kind == "continuous";
        self.apply_sect(next);
        if !continuous {
            self.first_of_sect = true;
            self.new_page();
        } else {
            self.first_of_sect = false;
        }
    }

    fn next_sect_props(&self) -> SectProps {
        sect_props(self.env.doc).get(self.sect).cloned().unwrap_or_else(|| self.env.doc.last_sect())
    }

    fn apply_sect(&mut self, sp: SectProps) {
        for i in 0..3 {
            if sp.headers[i].is_some() {
                self.hdr[i] = sp.headers[i].clone();
            }
            if sp.footers[i].is_some() {
                self.ftr[i] = sp.footers[i].clone();
            }
        }
        if let Some(s) = sp.pg_start {
            self.page_no = s - 1;
        }
        self.sp = sp;
    }

    fn para(&mut self, p: &Para, path: &mut Vec<u32>, keep_next_h: Option<f32>) {
        let label = self.labels.labels.get(path.as_slice()).cloned();
        let depth = self.labels.comments.get(path.as_slice()).copied().unwrap_or(0);
        let pl0 = para_layout(self.env, p, self.width, &TCtx::default(), label.as_ref(), depth, &[]);
        let pr: PathRc = path.as_slice().into();
        self.order.push(pr.clone());
        if pl0.props.page_break_before == Some(true) && !self.at_top {
            self.next_page();
        }
        let (before, prev_after) = spacing(&pl0, self.prev.as_ref());
        self.y += prev_after;
        let total: f32 = pl0.lines.iter().map(|l| l.h).sum();
        if !self.at_top {
            let need = before + total + keep_next_h.unwrap_or(0.0);
            let keep = pl0.props.keep_lines == Some(true) || keep_next_h.is_some();
            if keep && self.y + need > self.bottom && need <= self.bottom - self.top {
                self.next_page();
            }
        }
        // floating objects of this paragraph, then the lines around them
        let para_top = self.y + before;
        let tw = |v: i32| v as f32 / 20.0;
        for (d, li) in &pl0.anchors {
            let line_top = para_top + pl0.lines[..*li].iter().map(|l| l.h).sum::<f32>();
            let g = FloatGeo {
                page_w: tw(self.sp.w),
                page_h: tw(self.sp.h),
                left: self.left,
                width: self.width,
                top_margin: self.top,
                body_h: self.bottom - self.top,
                para_top,
                line_top,
            };
            let Some((fx, fy)) = float_pos(d, &g) else { continue };
            float_items(self.env, d, fx, fy, &mut self.items);
            if let Some(f) = d.anchor.as_ref().filter(|f| matches!(f.wrap, Wrap::Square | Wrap::TopBottom)) {
                let [dt, db, dl, dr] = f.dist;
                self.excl.push(PageExcl { x1: fx - dl, x2: fx + d.width + dr, y1: fy - dt, y2: fy + d.height + db, wrap: f.wrap });
            }
        }
        let rel: Vec<Excl> = self
            .excl
            .iter()
            .filter(|e| e.y2 > para_top)
            .map(|e| {
                let (cx1, cx2) = (e.x1 - self.left, e.x2 - self.left);
                let kind = if e.wrap == Wrap::TopBottom {
                    ExKind::Above
                } else if cx1 >= self.width - cx2 {
                    ExKind::Right(cx1)
                } else {
                    ExKind::Left(cx2)
                };
                Excl { y1: e.y1 - para_top, y2: e.y2 - para_top, kind }
            })
            .collect();
        let pl = if rel.is_empty() { pl0 } else { para_layout(self.env, p, self.width, &TCtx::default(), label.as_ref(), depth, &rel) };
        self.y += before;
        let widow = pl.props.widow != Some(false);
        let n = pl.lines.len();
        let mut i = 0;
        let mut seg_top = self.y;
        while i < n {
            // lines that fit here
            let mut k = 0;
            let mut hsum = 0.0;
            let mut notes = 0.0;
            while i + k < n {
                let need = self.notes_need(&pl, i + k, notes > 0.0);
                if self.y + hsum + pl.lines[i + k].h > self.bottom - self.notes_h - notes - need + 0.01 {
                    break;
                }
                notes += need;
                hsum += pl.lines[i + k].h;
                k += 1;
                if pl.lines[i + k - 1].page_break {
                    break;
                }
            }
            let forced_break = k > 0 && pl.lines[i + k - 1].page_break;
            if i + k < n && !forced_break && widow && n >= 2 {
                if i == 0 && k == 1 {
                    k = 0;
                } else if n - (i + k) == 1 && k >= 2 {
                    k -= 1;
                }
            }
            if k == 0 && self.at_top {
                k = 1;
            }
            for li in i..i + k {
                let l = &pl.lines[li];
                for id in line_notes(&pl, li) {
                    let h = self.note_height(&id);
                    if self.page_notes.is_empty() {
                        self.notes_h += NOTE_GAP;
                    }
                    self.notes_h += h;
                    self.page_notes.push(id);
                }
                self.items.push(Placed::Line { path: Some(pr.clone()), para: pl.clone(), idx: li, x: self.left, y: self.y, w: self.width });
                line_charts(self.env, &pl, li, self.left, self.y, &mut self.items);
                self.y += l.h;
                self.at_top = false;
            }
            i += k;
            if i < n || forced_break {
                let mut deco = Vec::new();
                decorate(&pl, self.left, self.width, seg_top, self.y, &mut deco);
                self.items.extend(deco);
                self.next_page();
                seg_top = self.y;
                if i >= n {
                    self.prev = None;
                    return;
                }
            }
        }
        let mut deco = Vec::new();
        decorate(&pl, self.left, self.width, seg_top, self.y, &mut deco);
        for d in deco {
            match d {
                Placed::Rect { .. } => self.items.insert(0, d),
                d => self.items.push(d),
            }
        }
        self.prev = Some((pl.props.style.clone(), pl.props.contextual == Some(true), pl.after));
    }

    fn table(&mut self, t: &Table, path: &mut Vec<u32>) {
        if let Some((_, _, a)) = self.prev.take() {
            self.y += a;
        }
        let g = {
            let mut bx = BoxCx { labels: Some(&self.labels), order: Some(&mut self.order) };
            table_geo(self.env, t, Some(path), self.width, &mut bx)
        };
        let headers: Vec<usize> = g.rows.iter().take_while(|r| r.header).enumerate().map(|(i, _)| i).collect();
        let tpath: PathRc = path.as_slice().into();
        let cols: Vec<f32> = g.cols.iter().map(|c| self.left + c).collect();
        let mut batch: Vec<usize> = Vec::new();
        let mut batch_y = self.y;
        let mut y = self.y;
        for ri in 0..g.rows.len() {
            let h = g.rows[ri].h;
            if y + h > self.bottom + 0.01 && !(self.at_top && batch.is_empty()) && !batch.is_empty() {
                let used = place_rows(&g, &batch, self.left, batch_y, &mut self.items);
                self.tables.push(TableBox { path: tpath.clone(), cols: cols.clone(), y1: batch_y, y2: batch_y + used });
                self.next_page();
                batch.clear();
                y = self.y;
                batch_y = y;
                // heading rows repeat only when the next row still fits under them (as Word does);
                // taller headings would fill every page and push the table on without end
                let heading: f32 = headers.iter().map(|&hr| g.rows[hr].h).sum();
                if !headers.contains(&ri) && heading + h <= self.bottom - y {
                    for &hr in &headers {
                        batch.push(hr);
                        y += g.rows[hr].h;
                    }
                }
            }
            batch.push(ri);
            y += h;
            self.at_top = false;
        }
        if !batch.is_empty() {
            let used = place_rows(&g, &batch, self.left, batch_y, &mut self.items);
            self.tables.push(TableBox { path: tpath, cols, y1: batch_y, y2: batch_y + used });
        }
        self.y = y;
        self.prev = None;
    }
}

/// Section properties in order: paragraph-level sectPrs, then the body's.
fn sect_props(doc: &crate::docx::Document) -> Vec<SectProps> {
    let mut v = Vec::new();
    fn walk(c: &Container, v: &mut Vec<SectProps>) {
        for b in c {
            match &**b {
                Block::Para(p) => {
                    if let Some(s) = p.sect() {
                        v.push(SectProps::parse(s));
                    }
                }
                Block::Sdt(s) => walk(&s.content, v),
                _ => {}
            }
        }
    }
    walk(&doc.body, &mut v);
    v.push(doc.last_sect());
    v
}

/// Replaces PAGE / NUMPAGES field results in a header or footer.
fn fill_fields(c: &Container, page: i32, total: i32) -> Container {
    fn para(p: &Para, page: i32, total: i32) -> Para {
        let mut p = p.clone();
        p.touch();
        let mut state = 0; // 0 none, 1 instr, 2 result
        let mut instr = String::new();
        let mut replaced = false;
        let mut simple: Option<String> = None;
        for inl in p.inlines.iter_mut() {
            match inl {
                Inline::Open(w) => {
                    if let WrapKind::Field(i) = &w.kind {
                        simple = Some(i.clone());
                        replaced = false;
                    }
                }
                Inline::Close(w) => {
                    if matches!(w.kind, WrapKind::Field(_)) {
                        simple = None;
                    }
                }
                Inline::Run(r) => {
                    for it in r.items.iter_mut() {
                        match it {
                            Item::Raw(e) if e.is_w("fldChar") => match e.wattr("fldCharType") {
                                Some("begin") => {
                                    state = 1;
                                    instr.clear();
                                    replaced = false;
                                }
                                Some("separate") => state = 2,
                                Some("end") => state = 0,
                                _ => {}
                            },
                            Item::Raw(e) if e.is_w("instrText") && state == 1 => instr.push_str(&e.text()),
                            Item::Text(t) => {
                                let code = if state == 2 { Some(instr.clone()) } else { simple.clone() };
                                if let Some(code) = code {
                                    let w = code.split_whitespace().next().unwrap_or("").to_uppercase();
                                    let v = match w.as_str() {
                                        "PAGE" => Some(page),
                                        "NUMPAGES" | "SECTIONPAGES" => Some(total),
                                        _ => None,
                                    };
                                    if let Some(v) = v {
                                        *t = if replaced { String::new() } else { v.to_string() };
                                        replaced = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    r.dirty = true;
                }
                _ => {}
            }
        }
        let _ = Ns::W;
        p
    }
    c.iter()
        .map(|b| {
            Rc::new(match &**b {
                Block::Para(p) => Block::Para(para(p, page, total)),
                Block::Table(t) => {
                    let mut t = t.clone();
                    for it in t.items.iter_mut() {
                        if let TableItem::Row(r) = it {
                            for ci in r.items.iter_mut() {
                                if let RowItem::Cell(cell) = ci {
                                    cell.content = fill_fields(&cell.content, page, total);
                                }
                            }
                        }
                    }
                    Block::Table(t)
                }
                Block::Sdt(s) => {
                    let mut s = s.clone();
                    s.content = fill_fields(&s.content, page, total);
                    Block::Sdt(s)
                }
                b => b.clone(),
            })
        })
        .collect()
}

pub fn layout_document(env: &Env) -> Layout {
    env.cache.borrow_mut().notes = number_notes(env.doc);
    let sects = sect_props(env.doc);
    let first = sects.first().cloned().unwrap_or_default();
    let mut pg = Pager {
        env,
        pages: Vec::new(),
        items: Vec::new(),
        sp: SectProps::default(),
        sect: 0,
        y: 0.0,
        top: 0.0,
        bottom: 0.0,
        left: 0.0,
        width: 0.0,
        at_top: true,
        first_of_sect: true,
        page_no: 0,
        hdr: [None, None, None],
        ftr: [None, None, None],
        prev: None,
        labels: number_all(env),
        order: Vec::new(),
        started: false,
        tables: Vec::new(),
        excl: Vec::new(),
        page_notes: Vec::new(),
        notes_h: 0.0,
        note_heights: HashMap::new(),
    };
    pg.apply_sect(first);
    pg.new_page();
    let mut path = Vec::new();
    pg.container(&env.doc.body, &mut path);
    pg.flush();
    let mut pages = pg.pages;

    // headers and footers with page numbers
    let total = pages.len() as i32;
    let all = sect_props(env.doc);
    let mut hdr: [Option<String>; 3] = [None, None, None];
    let mut ftr: [Option<String>; 3] = [None, None, None];
    let mut cur_sect = usize::MAX;
    let mut story_order: Vec<PathRc> = Vec::new();
    for (pi, page) in pages.iter_mut().enumerate() {
        if page.sect != cur_sect {
            for s in all.iter().take(page.sect + 1).skip(if cur_sect == usize::MAX { 0 } else { cur_sect + 1 }) {
                for i in 0..3 {
                    if s.headers[i].is_some() {
                        hdr[i] = s.headers[i].clone();
                    }
                    if s.footers[i].is_some() {
                        ftr[i] = s.footers[i].clone();
                    }
                }
            }
            cur_sect = page.sect;
        }
        let sp = all.get(page.sect).cloned().unwrap_or_default();
        let i = if page.first_of_sect && sp.title_pg { 1 } else { 0 };
        let tw = |v: i32| v as f32 / 20.0;
        let width = tw(sp.w - sp.left - sp.right).max(36.0);
        page.header = hdr[i].clone();
        page.footer = ftr[i].clone();
        for (is_hdr, rid) in [(true, &hdr[i]), (false, &ftr[i])] {
            let Some(rid) = rid else { continue };
            let Some(st) = env.doc.stories.get(rid) else { continue };
            let mut items = Vec::new();
            let mut bx = BoxCx { labels: None, order: None };
            // the header being edited is laid out as it is stored, with paths
            let editing = env.story.as_ref().is_some_and(|(r, p)| r == rid && *p == pi);
            let h = if editing {
                let idx = crate::edit::story_index(env.doc, rid).unwrap_or(0);
                let mut path = vec![crate::edit::STORY | idx];
                let mut order: Vec<PathRc> = Vec::new();
                let mut bx = BoxCx { labels: None, order: Some(&mut order) };
                let h = layout_box(env, &st.content, Some(&mut path), width, &TCtx::default(), &mut bx, &mut items);
                story_order = order;
                h
            } else {
                let content = fill_fields(&st.content, page.number, total);
                layout_box(env, &content, None, width, &TCtx::default(), &mut bx, &mut items)
            };
            let y = if is_hdr { tw(sp.header) } else { tw(sp.h) - tw(sp.footer) - h };
            for mut it in items {
                it.shift(tw(sp.left), y);
                page.items.push(it);
            }
        }
    }

    let mut index: HashMap<PathRc, Vec<(usize, usize)>> = HashMap::new();
    for (pi, p) in pages.iter().enumerate() {
        for (ii, it) in p.items.iter().enumerate() {
            if let Placed::Line { path: Some(path), .. } = it {
                index.entry(path.clone()).or_default().push((pi, ii));
            }
        }
    }
    let mut order = pg.order;
    order.extend(story_order);
    let order_pos = order.iter().enumerate().map(|(i, p)| (p.clone(), i)).collect();
    env.cache.borrow_mut().sweep();
    Layout { pages, index, order, order_pos }
}

// ---------- geometry queries ----------

fn in_scope(path: &[u32], scope: Option<u32>) -> bool {
    match (path.first(), scope) {
        (Some(f), None) => f & crate::edit::STORY == 0,
        (Some(f), Some(s)) => *f == s,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Caret {
    pub page: usize,
    pub x: f32,
    pub y: f32,
    pub h: f32,
}

impl Layout {
    /// Box of the inline picture at `off` of a paragraph: (page, x, y, w, h).
    pub fn image_box(&self, path: &[u32], off: usize) -> Option<(usize, f32, f32, f32, f32)> {
        for &(pg, it) in self.index.get(path)? {
            let Some((_, para, idx, x, y, _)) = self.line_at(pg, it) else { continue };
            let l = &para.lines[idx];
            for p in &l.pieces {
                if let Piece::Image { x: px, w, h, start, .. } = p {
                    if *start == off {
                        return Some((pg, x + px, y + l.base - h, *w, *h));
                    }
                }
            }
        }
        None
    }

    /// Inline picture under a point: (paragraph path, offset).
    pub fn image_at(&self, page: usize, px: f32, py: f32) -> Option<(PathRc, usize)> {
        let p = self.pages.get(page)?;
        for it in &p.items {
            if let Placed::Line { path: Some(path), para, idx, x, y, .. } = it {
                let l = &para.lines[*idx];
                for pc in &l.pieces {
                    if let Piece::Image { x: ix, w, h, start, .. } = pc {
                        let (bx, by) = (x + ix, y + l.base - h);
                        if px >= bx && px <= bx + w && py >= by && py <= by + h {
                            return Some((path.clone(), *start));
                        }
                    }
                }
            }
        }
        None
    }

    /// Table column boundary near (x, y): (table path, boundary index, x).
    pub fn column_border(&self, page: usize, x: f32, y: f32, tol: f32) -> Option<(PathRc, usize, f32)> {
        let p = self.pages.get(page)?;
        for t in &p.tables {
            if y < t.y1 || y > t.y2 {
                continue;
            }
            for (i, bx) in t.cols.iter().enumerate().skip(1) {
                if (x - bx).abs() <= tol {
                    return Some((t.path.clone(), i, *bx));
                }
            }
        }
        None
    }

    fn line_at(&self, page: usize, item: usize) -> Option<(&PathRc, &Rc<ParaLayout>, usize, f32, f32, f32)> {
        match self.pages.get(page)?.items.get(item)? {
            Placed::Line { path: Some(p), para, idx, x, y, w } => Some((p, para, *idx, *x, *y, *w)),
            _ => None,
        }
    }

    /// (page, item) of the line holding `off`.
    pub fn locate(&self, path: &[u32], off: usize) -> Option<(usize, usize)> {
        let lines = self.index.get(path)?;
        let mut best = *lines.first()?;
        for &(pg, it) in lines {
            if let Some((_, para, idx, ..)) = self.line_at(pg, it) {
                let l = &para.lines[idx];
                if off >= l.start && (off < l.end || idx + 1 == para.lines.len()) {
                    return Some((pg, it));
                }
                if off >= l.start {
                    best = (pg, it);
                }
            }
        }
        Some(best)
    }

    pub fn caret(&self, path: &[u32], off: usize) -> Option<Caret> {
        let (pg, it) = self.locate(path, off)?;
        let (_, para, idx, x, y, _) = self.line_at(pg, it)?;
        let l = &para.lines[idx];
        let cx = x + l.caret_x(off);
        // caret spans the text height, not the line spacing
        let (mut asc, mut desc) = (l.base * 0.8, (l.h - l.base).max(l.h * 0.2));
        for p in &l.pieces {
            if let Piece::Text { start, adv, size, .. } = p {
                if off >= *start && off <= start + adv.len() {
                    asc = size * 0.95;
                    desc = size * 0.25;
                    break;
                }
            }
        }
        let top = (y + l.base - asc).max(y);
        Some(Caret { page: pg, x: cx, y: top, h: (asc + desc).min(l.h.max(asc + desc)) })
    }

    pub fn hit(&self, page: usize, px: f32, py: f32) -> Option<(PathRc, usize)> {
        self.hit_in(page, px, py, None)
    }

    /// Hit test among the lines of one scope: the body (None) or the header
    /// or footer whose paths start with `scope`.
    pub fn hit_in(&self, page: usize, px: f32, py: f32, scope: Option<u32>) -> Option<(PathRc, usize)> {
        let p = self.pages.get(page)?;
        let mut best: Option<(f32, usize)> = None;
        for (ii, it) in p.items.iter().enumerate() {
            if let Placed::Line { path: Some(lp), para, idx, x, y, w } = it {
                if !in_scope(lp, scope) {
                    continue;
                }
                let l = &para.lines[*idx];
                let dy = if py < *y { y - py } else if py > y + l.h { py - y - l.h } else { 0.0 };
                let dx = if px < *x { x - px } else if px > x + w { px - x - w } else { 0.0 };
                let score = dy * 4.0 + dx;
                if best.is_none_or(|b| score < b.0) {
                    best = Some((score, ii));
                }
            }
        }
        let (_, ii) = best?;
        let (path, para, idx, x, ..) = self.line_at(page, ii)?;
        Some((path.clone(), para.lines[idx].hit(px - x)))
    }

    /// Start and end caret offsets of the visual line holding `off`.
    pub fn line_bounds(&self, path: &[u32], off: usize) -> Option<(usize, usize)> {
        let (pg, it) = self.locate(path, off)?;
        let (_, para, idx, ..) = self.line_at(pg, it)?;
        let l = &para.lines[idx];
        Some((l.start, l.end_caret()))
    }

    /// Caret position one line up (`dir < 0`) or down at horizontal `px`.
    pub fn vertical(&self, path: &[u32], off: usize, px: f32, dir: i32) -> Option<(PathRc, usize)> {
        let scope = path.first().filter(|f| *f & crate::edit::STORY != 0).copied();
        let (pg, it) = self.locate(path, off)?;
        let (_, para, idx, _, y, _) = self.line_at(pg, it)?;
        let cur_top = y;
        let cur_bot = y + para.lines[idx].h;
        let mut page = pg;
        loop {
            let p = self.pages.get(page)?;
            let mut best: Option<(f32, f32, usize)> = None;
            for (ii, it) in p.items.iter().enumerate() {
                if let Placed::Line { path: Some(lp), para, idx, x, y, w } = it {
                    if !in_scope(lp, scope) {
                        continue;
                    }
                    let l = &para.lines[*idx];
                    let ok = if page != pg {
                        true
                    } else if dir > 0 {
                        *y >= cur_bot - 0.5
                    } else {
                        y + l.h <= cur_top + 0.5
                    };
                    if !ok {
                        continue;
                    }
                    let vy = if dir > 0 { *y } else { -(*y + l.h) };
                    let dx = if px < *x { x - px } else if px > x + w { px - x - w } else { 0.0 };
                    let better = match best {
                        None => true,
                        Some((by, bdx, _)) => vy < by - 0.5 || ((vy - by).abs() <= 0.5 && dx < bdx),
                    };
                    if better {
                        best = Some((vy, dx, ii));
                    }
                }
            }
            if let Some((_, _, ii)) = best {
                let (path, para, idx, x, ..) = self.line_at(page, ii)?;
                return Some((path.clone(), para.lines[idx].hit(px - x)));
            }
            if dir > 0 {
                page += 1;
            } else {
                page = page.checked_sub(1)?;
            }
        }
    }

    /// Rectangles (page, x, y, w, h) covering the range between two positions
    /// given in document order.
    pub fn sel_rects(&self, a: (&[u32], usize), b: (&[u32], usize)) -> Vec<(usize, f32, f32, f32, f32)> {
        let mut out = Vec::new();
        let (Some(&ia), Some(&ib)) = (self.order_pos.get(a.0), self.order_pos.get(b.0)) else { return out };
        for pi in ia..=ib {
            let path = &self.order[pi];
            let from = if pi == ia { a.1 } else { 0 };
            let to = if pi == ib { b.1 } else { usize::MAX };
            for &(pg, it) in self.index.get(path).map(|v| v.as_slice()).unwrap_or(&[]) {
                let Some((_, para, idx, x, y, _)) = self.line_at(pg, it) else { continue };
                let l = &para.lines[idx];
                let last = idx + 1 == para.lines.len();
                let (s, e) = (from.max(l.start), to.min(l.end));
                if s > e || (s == e && !(last && to == usize::MAX && s == l.end)) {
                    if !(s == e && s == l.start && to > l.end) {
                        continue;
                    }
                }
                let x1 = x + l.caret_x(s);
                let mut x2 = x + l.caret_x(e);
                if to > l.end || (last && to == usize::MAX) {
                    x2 = x2.max(x + l.x1) + 4.0;
                }
                if x2 > x1 {
                    out.push((pg, x1, y, x2 - x1, l.h));
                }
            }
        }
        out
    }
}
