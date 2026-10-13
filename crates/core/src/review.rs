//! Tracked changes from .docx: accept, reject, navigate.
//!
//! Insertions are `Inline::Open/Close` wrappers of kind `Ins`, deletions are
//! raw `w:del`/`w:moveFrom` elements, formatting changes are `w:rPrChange`
//! inside run properties and `w:pPrChange` inside paragraph properties, and a
//! paragraph mark may carry `w:ins`/`w:del` in `w:pPr/w:rPr`.

use crate::edit::{container_mut, para_at, para_mut, para_paths};
use crate::editor::{Editor, Kind, Pos};
use crate::model::*;
use crate::props::{ParaProps, RunProps};
use crate::xml::{Element, Node};
use std::rc::Rc;

/// A change found in a paragraph: (offset, length, kind, author, date).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub off: usize,
    pub len: usize,
    pub kind: &'static str,
    pub author: String,
    pub date: String,
}

fn attr(e: &Element, n: &str) -> String {
    e.wattr(n).unwrap_or("").to_string()
}

/// Changes of a paragraph in order.
pub fn changes(p: &Para) -> Vec<Change> {
    let mut out = Vec::new();
    let mut pos = 0;
    let mut open: Vec<(usize, String, String)> = Vec::new();
    for inl in &p.inlines {
        match inl {
            Inline::Open(w) => {
                if let WrapKind::Ins(a, d) = &w.kind {
                    open.push((pos, a.clone(), d.clone()));
                }
            }
            Inline::Close(w) => {
                if matches!(w.kind, WrapKind::Ins(..)) {
                    if let Some((s, a, d)) = open.pop() {
                        out.push(Change { off: s, len: pos - s, kind: "ins", author: a, date: d });
                    }
                }
            }
            Inline::Raw(e) if is_deletion(e) => out.push(Change { off: pos, len: 0, kind: "del", author: attr(e, "author"), date: attr(e, "date") }),
            Inline::Run(r) => {
                if let Some(c) = r.rpr.as_ref().and_then(|x| x.wchild("rPrChange")) {
                    out.push(Change { off: pos, len: r.len(), kind: "format", author: attr(c, "author"), date: attr(c, "date") });
                }
                pos += r.len();
            }
            _ => {}
        }
    }
    if let Some(ppr) = &p.ppr {
        if let Some(c) = ppr.wchild("pPrChange") {
            out.push(Change { off: 0, len: pos, kind: "paragraph", author: attr(c, "author"), date: attr(c, "date") });
        }
        if let Some(m) = ppr.wchild("rPr").and_then(|r| r.wchild("del").or_else(|| r.wchild("ins"))) {
            out.push(Change { off: pos, len: 0, kind: if m.is_w("del") { "del" } else { "ins" }, author: attr(m, "author"), date: attr(m, "date") });
        }
    }
    out.sort_by_key(|c| c.off);
    out
}

/// Runs of a `w:del` element as ordinary runs (`w:delText` → `w:t`).
fn restore(del: &Element, doc: &crate::Document) -> Vec<Inline> {
    fn undel(e: &Element) -> Element {
        let mut e = e.clone();
        if e.is_w("delText") {
            e.local = "t".into();
            e.qname = e.qname.replace("delText", "t");
        } else if e.is_w("delInstrText") {
            e.local = "instrText".into();
            e.qname = e.qname.replace("delInstrText", "instrText");
        }
        e.children = e
            .children
            .into_iter()
            .map(|n| match n {
                Node::Elem(x) => Node::Elem(undel(&x)),
                t => t,
            })
            .collect();
        e
    }
    let mut p = Element::new_w("p");
    for c in del.elems() {
        p.children.push(Node::Elem(undel(c)));
    }
    let mut para = crate::docx::parse_para(&p, doc);
    for i in para.inlines.iter_mut() {
        if let Inline::Run(r) = i {
            r.open = "<w:r>".into();
        }
    }
    para.inlines
}

fn strip_child(e: &Rc<Element>, local: &str) -> Rc<Element> {
    let mut x = (**e).clone();
    x.children.retain(|n| !matches!(n, Node::Elem(c) if c.is_w(local)));
    Rc::new(x)
}

/// Accepts or rejects changes of `p` touching `[from, to]` (all when None).
/// Returns (changed, the paragraph mark goes away: merge with the next).
fn review_para(p: &mut Para, accept: bool, range: Option<(usize, usize)>, doc: &crate::Document) -> (bool, bool) {
    let hit = |a: usize, b: usize| match range {
        None => true,
        Some((f, t)) => a <= t && b >= f,
    };
    let mut changed = false;
    let mut i = 0;
    let mut pos = 0;
    while i < p.inlines.len() {
        match &p.inlines[i] {
            Inline::Open(w) if matches!(w.kind, WrapKind::Ins(..)) => {
                // the matching close and the length of the insertion
                let mut depth = 0;
                let mut j = i;
                let mut len = 0;
                for (k, x) in p.inlines.iter().enumerate().skip(i) {
                    match x {
                        Inline::Open(o) if Rc::ptr_eq(o, w) => depth += 1,
                        Inline::Close(c) if Rc::ptr_eq(c, w) => {
                            depth -= 1;
                            if depth == 0 {
                                j = k;
                                break;
                            }
                        }
                        x => len += x.len(),
                    }
                }
                if j > i && hit(pos, pos + len) {
                    if accept {
                        p.inlines.remove(j);
                        p.inlines.remove(i);
                    } else {
                        p.inlines.drain(i..=j);
                    }
                    changed = true;
                    continue;
                }
                i += 1;
            }
            Inline::Raw(e) if is_deletion(e) => {
                if hit(pos, pos) {
                    let e = e.clone();
                    p.inlines.remove(i);
                    if !accept {
                        let back = restore(&e, doc);
                        let n = back.len();
                        for (k, x) in back.into_iter().enumerate() {
                            p.inlines.insert(i + k, x);
                        }
                        i += n;
                        pos += p.inlines[i - n..i].iter().map(|x| x.len()).sum::<usize>();
                    }
                    changed = true;
                    continue;
                }
                i += 1;
            }
            Inline::Run(r) => {
                let len = r.len();
                if let Some(rpr) = r.rpr.clone().filter(|x| x.wchild("rPrChange").is_some()) {
                    if hit(pos, pos + len) {
                        let Inline::Run(r) = &mut p.inlines[i] else { unreachable!() };
                        if accept {
                            r.rpr = Some(strip_child(&rpr, "rPrChange"));
                        } else {
                            // the old properties come back
                            let old = rpr.wchild("rPrChange").and_then(|c| c.wchild("rPr")).cloned().unwrap_or_else(|| Element::new_w("rPr"));
                            r.props = RunProps::parse(&old);
                            r.rpr = Some(Rc::new(old));
                        }
                        r.dirty = false;
                        changed = true;
                    }
                }
                pos += len;
                i += 1;
            }
            _ => i += 1,
        }
    }
    let mut merge = false;
    if let Some(ppr) = p.ppr.clone() {
        let total = p.len();
        if ppr.wchild("pPrChange").is_some() && hit(0, total) {
            if accept {
                p.ppr = Some(strip_child(&ppr, "pPrChange"));
            } else {
                let mut old = ppr.wchild("pPrChange").and_then(|c| c.wchild("pPr")).cloned().unwrap_or_else(|| Element::new_w("pPr"));
                // the mark's run properties and the section break are not part of the change
                for keep in ["rPr", "sectPr"] {
                    if let Some(k) = ppr.wchild(keep) {
                        old.children.push(Node::Elem(k.clone()));
                    }
                }
                p.props = ParaProps::parse(&old);
                p.ppr = Some(Rc::new(old));
            }
            p.props_dirty = false;
            changed = true;
        }
        let ppr = p.ppr.clone().unwrap();
        if let Some(rpr) = ppr.wchild("rPr") {
            let mark = rpr.wchild("del").map(|_| "del").or_else(|| rpr.wchild("ins").map(|_| "ins"));
            if let Some(kind) = mark {
                if hit(total, total) {
                    let mut e = (*ppr).clone();
                    for n in e.children.iter_mut() {
                        if let Node::Elem(x) = n {
                            if x.is_w("rPr") {
                                x.children.retain(|c| !matches!(c, Node::Elem(y) if y.is_w("del") || y.is_w("ins")));
                            }
                        }
                    }
                    p.ppr = Some(Rc::new(e));
                    p.props_dirty = false;
                    // a deleted mark accepted, or an inserted one rejected, joins the paragraphs
                    merge = (kind == "del") == accept;
                    changed = true;
                }
            }
        }
    }
    if changed {
        p.touch();
    }
    (changed, merge)
}

impl Editor {
    /// Accepts or rejects tracked changes: the ones touching the selection
    /// (or the caret), or all of them.
    pub fn review(&mut self, accept: bool, all: bool) -> usize {
        self.checkpoint(Kind::Other);
        let (a, b) = self.ordered();
        let doc = self.doc.clone();
        let mut count = 0;
        let mut merges: Vec<Vec<u32>> = Vec::new();
        for path in para_paths(&self.doc.body) {
            if !all && (path < a.path || path > b.path) {
                continue;
            }
            let range = if all {
                None
            } else {
                let len = para_at(&self.doc.body, &path).map(|p| p.len()).unwrap_or(0);
                let from = if path == a.path { a.off } else { 0 };
                let to = if path == b.path { b.off } else { len };
                Some((from, to))
            };
            if para_at(&self.doc.body, &path).is_none_or(|p| changes(p).is_empty()) {
                continue;
            }
            if let Some(p) = para_mut(&mut self.doc.body, &path) {
                let (c, m) = review_para(p, accept, range, &doc);
                if c {
                    count += 1;
                }
                if m {
                    merges.push(path.clone());
                }
            }
        }
        if all {
            count += self.review_rows(accept);
        }
        // join paragraphs whose mark was removed, last first
        for path in merges.iter().rev() {
            let mut next = path.clone();
            *next.last_mut().unwrap() += 1;
            let Some(n) = para_at(&self.doc.body, &next).cloned() else { continue };
            if let Some(p) = para_mut(&mut self.doc.body, path) {
                p.append(&n);
            }
            if let Some(c) = container_mut(&mut self.doc.body, &next) {
                c.remove(*next.last().unwrap() as usize);
            }
        }
        if count == 0 {
            self.undo_discard();
            return 0;
        }
        let (x, y) = (self.anchor.clone(), self.focus.clone());
        self.anchor = self.clamp(x);
        self.focus = self.clamp(y.clone());
        self.anchor = self.focus.clone();
        self.changed();
        count
    }

    /// Inserted and deleted table rows (`w:trPr/w:ins|w:del`).
    fn review_rows(&mut self, accept: bool) -> usize {
        let mut n = 0;
        fn walk(c: &mut Container, accept: bool, n: &mut usize) {
            for b in c.iter_mut() {
                let has = match &**b {
                    Block::Table(t) => t.rows().any(|r| r.items.iter().any(|i| matches!(i, RowItem::Raw(e) if e.is_w("trPr") && (e.wchild("ins").is_some() || e.wchild("del").is_some())))) || t.rows().any(|r| r.cells().count() > 0),
                    Block::Sdt(_) => true,
                    _ => false,
                };
                if !has {
                    continue;
                }
                match Rc::make_mut(b) {
                    Block::Table(t) => {
                        let mut keep = Vec::new();
                        let mut touched = false;
                        for it in t.items.drain(..) {
                            let TableItem::Row(mut row) = it else {
                                keep.push(it);
                                continue;
                            };
                            let mark = row.items.iter().find_map(|i| match i {
                                RowItem::Raw(e) if e.is_w("trPr") => e.wchild("ins").map(|_| "ins").or_else(|| e.wchild("del").map(|_| "del")),
                                _ => None,
                            });
                            match mark {
                                Some(k) if (k == "del") == accept => {
                                    *n += 1;
                                    touched = true;
                                    continue;
                                }
                                Some(_) => {
                                    for i in row.items.iter_mut() {
                                        if let RowItem::Raw(e) = i {
                                            if e.is_w("trPr") {
                                                let x = Rc::make_mut(e);
                                                x.children.retain(|c| !matches!(c, Node::Elem(y) if y.is_w("ins") || y.is_w("del")));
                                            }
                                        }
                                    }
                                    *n += 1;
                                    touched = true;
                                }
                                None => {}
                            }
                            for ci in row.items.iter_mut() {
                                if let RowItem::Cell(cell) = ci {
                                    walk(&mut cell.content, accept, n);
                                }
                            }
                            keep.push(TableItem::Row(row));
                        }
                        t.items = keep;
                        if touched {
                            t.orig = None;
                        }
                    }
                    Block::Sdt(s) => walk(&mut s.content, accept, n),
                    _ => {}
                }
            }
        }
        walk(&mut self.doc.body, accept, &mut n);
        n
    }

    /// Selects the next (dir > 0) or previous tracked change.
    pub fn next_change(&mut self, dir: i32) -> bool {
        let paths = para_paths(&self.doc.body);
        let mut all: Vec<(Vec<u32>, Change)> = Vec::new();
        for p in &paths {
            if let Some(para) = para_at(&self.doc.body, p) {
                for c in changes(para) {
                    all.push((p.clone(), c));
                }
            }
        }
        // from the start of the selection, wrapping around the document
        let (a, _) = self.ordered();
        let here = (a.path.clone(), a.off);
        let pick = if dir > 0 {
            all.iter().find(|(p, c)| (p.clone(), c.off) > here).or(all.first())
        } else {
            all.iter().rev().find(|(p, c)| (p.clone(), c.off) < here).or(all.last())
        };
        let Some((p, c)) = pick.cloned() else { return false };
        self.set_selection(Pos { path: p.clone(), off: c.off }, Pos { path: p, off: c.off + c.len });
        true
    }

    /// The tracked change at the caret, if any.
    pub fn change_at_caret(&self) -> Option<Change> {
        let f = &self.focus;
        let p = para_at(&self.doc.body, &f.path)?;
        changes(p).into_iter().find(|c| f.off >= c.off && f.off <= c.off + c.len)
    }

    pub fn count_changes(&self) -> usize {
        para_paths(&self.doc.body).iter().filter_map(|p| para_at(&self.doc.body, p)).map(|p| changes(p).len()).sum()
    }
}
