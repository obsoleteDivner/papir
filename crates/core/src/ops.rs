//! Structural edits: lists, pictures, links, tables.

use crate::docx::Rel;
use crate::edit::{dcont_mut, dpara, dpara_mut, droot};
use crate::editor::{remove_numbering, Editor, Kind, Pos};
use crate::model::*;
use crate::props::{CellProps, RunProps, SectProps};
use crate::styles::Numbering;
use crate::xml::{self, Element, Node};
use std::rc::Rc;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn el(xml_src: &str) -> Element {
    xml::parse(xml_src).expect("generated xml")
}

const TCPR_ORDER: &[&str] = &[
    "cnfStyle", "tcW", "gridSpan", "hMerge", "vMerge", "tcBorders", "shd", "noWrap", "tcMar", "textDirection",
    "tcFitText", "vAlign", "hideMark", "headers", "cellIns", "cellDel", "cellMerge", "tcPrChange",
];

/// Sets (or removes with None) a child of a cell's `w:tcPr`, in schema order.
fn tcpr_set(c: &mut TableCell, local: &str, child: Option<Element>) {
    let mut e = c.tcpr.as_deref().cloned().unwrap_or_else(|| Element::new_w("tcPr"));
    e.children.retain(|n| !matches!(n, Node::Elem(x) if x.is_w(local)));
    if let Some(ch) = child {
        e.children.push(Node::Elem(ch));
    }
    let rank = |n: &Node| match n {
        Node::Elem(x) => TCPR_ORDER.iter().position(|o| x.is_w(o)).unwrap_or(TCPR_ORDER.len()),
        _ => TCPR_ORDER.len(),
    };
    e.children.sort_by_key(rank);
    c.props = CellProps::parse(&e);
    c.tcpr = Some(Rc::new(e));
}

fn tcw(w: i32) -> Element {
    Element::new_w("tcW").with_wattr("w", &w.to_string()).with_wattr("type", "dxa")
}

/// An empty paragraph keeping the paragraph and mark properties of `like`.
fn empty_like(like: Option<&Para>) -> Para {
    match like {
        Some(p) => {
            let mut q = p.clone();
            q.touch();
            q.open = "<w:p>".into();
            q.inlines.clear();
            q
        }
        None => Para::new(Default::default(), Default::default()),
    }
}

fn first_para(c: &Container) -> Option<&Para> {
    c.iter().find_map(|b| match &**b {
        Block::Para(p) => Some(p),
        _ => None,
    })
}

/// Grid start column of each cell in a row.
fn grid_pos(r: &Row) -> Vec<(usize, usize)> {
    let mut g = r.props.grid_before as usize;
    r.cells()
        .map(|c| {
            let s = (g, c.props.span.max(1) as usize);
            g += s.1;
            s
        })
        .collect()
}

/// Sets attributes of a `w:sectPr` child (pgSz, pgMar), creating it if needed.
fn sect_child(sect: &mut Element, local: &str, attrs: &[(&str, Option<String>)]) {
    let pos = sect.children.iter().position(|n| matches!(n, Node::Elem(x) if x.is_w(local)));
    let mut el = match pos {
        Some(i) => match sect.children.remove(i) {
            Node::Elem(e) => e,
            _ => unreachable!(),
        },
        None => Element::new_w(local),
    };
    for (k, v) in attrs {
        match v {
            Some(v) => el.set_wattr(k, v),
            None => el.attrs.retain(|a| a.local != *k),
        }
    }
    // pgSz and pgMar come after header/footer references and footnote props
    let at = pos.unwrap_or_else(|| {
        sect.children
            .iter()
            .position(|n| matches!(n, Node::Elem(x) if !(x.is_w("headerReference") || x.is_w("footerReference") || x.is_w("footnotePr") || x.is_w("endnotePr") || x.is_w("type") || (local == "pgMar" && x.is_w("pgSz")))))
            .unwrap_or(sect.children.len())
    });
    sect.children.insert(at.min(sect.children.len()), Node::Elem(el));
}

impl Editor {
    // ---------- headers and footers ----------

    /// Starts editing the header (point above the body) or footer (below it)
    /// of a page; creates it when the page has none.
    pub fn edit_story_at(&mut self, page: usize, x: f32, y: f32) -> bool {
        // a footnote at the bottom of the page
        let note = self.layout().pages[page].notes.iter().find(|(_, t, b)| y >= *t && y <= *b).map(|(id, ..)| id.clone());
        if let Some(id) = note {
            self.edit_note(&id, page, Some((x, y)));
            return true;
        }
        let (top, bottom, header, footer) = {
            let p = &self.layout().pages[page];
            (p.top, p.bottom, p.header.clone(), p.footer.clone())
        };
        let is_header = if y < top {
            true
        } else if y > bottom {
            false
        } else {
            return false;
        };
        let rid = match if is_header { header } else { footer } {
            Some(r) => r,
            None => self.create_story(is_header),
        };
        let (a, f) = (self.anchor.clone(), self.focus.clone());
        self.story = Some((rid, page, a, f));
        self.layout_reset();
        let scope = self.scope();
        let hit = self.layout().hit_in(page, x, y, scope);
        let p = match hit {
            Some((path, off)) => Pos { path: path.to_vec(), off },
            None => Pos { path: self.scope_paths().into_iter().next().unwrap_or_default(), off: 0 },
        };
        self.anchor = p.clone();
        self.focus = p;
        true
    }

    /// Starts editing a footnote; the caret goes where `at` points, else to its end.
    fn edit_note(&mut self, id: &str, page: usize, at: Option<(f32, f32)>) {
        let (a, f) = (self.anchor.clone(), self.focus.clone());
        self.story = Some((format!("{}{id}", crate::edit::NOTE_KEY), page, a, f));
        self.layout_reset();
        let scope = self.scope();
        let hit = at.and_then(|(x, y)| self.layout().hit_in(page, x, y, scope));
        let p = match hit {
            Some((path, off)) => Pos { path: path.to_vec(), off },
            None => {
                let last = self.scope_paths().into_iter().last().unwrap_or_default();
                let off = dpara(&self.doc, &last).map(|p| p.len()).unwrap_or(0);
                Pos { path: last, off }
            }
        };
        self.anchor = p.clone();
        self.focus = p;
    }

    /// Inserts a footnote at the caret and starts editing its text.
    pub fn insert_footnote(&mut self) -> bool {
        if self.story.is_some() {
            return false;
        }
        let f = self.focus.clone();
        let Some(props) = dpara(&self.doc, &f.path).map(|p| p.props_at(f.off)) else { return false };
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let f = self.focus.clone();
        self.ensure_footnotes();
        // a number not used by any note, separators included
        let used = self.doc.footnotes.keys().cloned().chain(
            self.doc.footnotes_root.iter().flat_map(|r| r.wchildren("footnote").filter_map(|n| n.wattr("id")).map(String::from).collect::<Vec<_>>()),
        );
        let id = (used.filter_map(|i| i.parse::<i64>().ok()).max().unwrap_or(0) + 1).max(1).to_string();
        let style_by = |name: &str| self.doc.styles.map.values().find(|s| s.name.eq_ignore_ascii_case(name)).map(|s| s.id.clone());
        let mark = match style_by("footnote reference") {
            Some(st) => format!("<w:rStyle w:val=\"{}\"/>", xml::escape(&st)),
            None => "<w:vertAlign w:val=\"superscript\"/>".into(),
        };
        let text = style_by("footnote text").map(|st| format!("<w:pStyle w:val=\"{}\"/>", xml::escape(&st))).unwrap_or_default();
        // the mark in the text
        let mut rp = props.clone();
        match style_by("footnote reference") {
            Some(st) => rp.style = Some(st),
            None => rp.valign = Some("superscript".into()),
        }
        let mut r = Run::new(rp);
        r.items.push(Item::Raw(Rc::new(el(&format!("<w:x xmlns:w=\"{W}\"><w:footnoteReference w:id=\"{id}\"/></w:x>")).elems().next().cloned().unwrap())));
        if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
            p.insert_inlines(f.off, vec![Inline::Run(r)]);
        }
        // the note: its number, a space, the text to come
        let src = format!("<w:x xmlns:w=\"{W}\"><w:p><w:pPr>{text}</w:pPr><w:r><w:rPr>{mark}</w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space=\"preserve\"> </w:t></w:r></w:p></w:x>");
        let block = crate::docx::parse_block(&el(&src).elems().next().cloned().unwrap(), &self.doc);
        Rc::make_mut(&mut self.doc.footnotes).insert(id.clone(), vec![Rc::new(block)]);
        self.doc.footnotes_dirty = true;
        // after the mark when the note is closed
        self.anchor = Pos { off: f.off, ..f.clone() };
        self.focus = self.anchor.clone();
        self.changed();
        let page = self.layout().pages.iter().position(|p| p.notes.iter().any(|(n, ..)| *n == id)).unwrap_or(0);
        self.edit_note(&id, page, None);
        true
    }

    /// The footnotes part, created (with its separators) when the document has none.
    fn ensure_footnotes(&mut self) {
        if self.doc.footnotes_root.is_some() {
            return;
        }
        let pre = self.doc.pkg.name_prefix();
        let mut file = "footnotes.xml".to_string();
        let mut n = 1;
        while self.doc.pkg.get(&format!("word/{file}")).is_some() {
            file = format!("footnotes{pre}{n}.xml");
            n += 1;
        }
        let part = format!("word/{file}");
        let sep = |kind: &str, id: i32, mark: &str| {
            format!("<w:footnote w:type=\"{kind}\" w:id=\"{id}\"><w:p><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:r><w:{mark}/></w:r></w:p></w:footnote>")
        };
        let src = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w:footnotes xmlns:w=\"{W}\" xmlns:r=\"{R}\">{}{}</w:footnotes>",
            sep("separator", -1, "separator"),
            sep("continuationSeparator", 0, "continuationSeparator")
        );
        self.doc.pkg.set(&part, src.clone().into_bytes());
        self.doc.pkg.ensure_override(&part, "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml");
        let doc_part = self.doc.part.clone();
        let rid = self.doc.pkg.add_rel(&doc_part, "footnotes", &file, false);
        Rc::make_mut(&mut self.doc.rels).insert(rid, Rel { kind: "footnotes".into(), target: part.clone(), external: false });
        self.doc.footnotes_part = Some(part);
        self.doc.footnotes_root = Some(Rc::new(el(&src)));
    }

    /// Back to the body, where the caret was before.
    pub fn exit_story(&mut self) {
        if let Some((_, _, a, f)) = self.story.take() {
            self.layout_reset();
            self.anchor = self.clamp(a);
            self.focus = self.clamp(f);
        }
    }

    /// A new empty header or footer for the whole document.
    fn create_story(&mut self, header: bool) -> String {
        self.checkpoint(Kind::Other);
        let kind = if header { "header" } else { "footer" };
        let tag = if header { "hdr" } else { "ftr" };
        let pre = self.doc.pkg.name_prefix();
        let mut n = 1;
        while self.doc.pkg.get(&format!("word/{kind}{pre}{n}.xml")).is_some() {
            n += 1;
        }
        let file = format!("{kind}{pre}{n}.xml");
        let part = format!("word/{file}");
        let style = self.doc.styles.map.values().find(|s| s.kind == "paragraph" && s.name.eq_ignore_ascii_case(kind)).map(|s| s.id.clone());
        let ppr = style.map(|s| format!("<w:pPr><w:pStyle w:val=\"{}\"/></w:pPr>", xml::escape(&s))).unwrap_or_default();
        let src = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w:{tag} xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:p>{ppr}</w:p></w:{tag}>");
        self.doc.pkg.set(&part, src.clone().into_bytes());
        self.doc.pkg.ensure_override(&part, &format!("application/vnd.openxmlformats-officedocument.wordprocessingml.{kind}+xml"));
        let doc_part = self.doc.part.clone();
        let rid = self.doc.pkg.add_rel(&doc_part, kind, &file, false);
        Rc::make_mut(&mut self.doc.rels).insert(rid.clone(), Rel { kind: kind.into(), target: part.clone(), external: false });
        let root = el(&src);
        let p_el = root.elems().next().cloned().unwrap();
        let content = vec![Rc::new(crate::docx::parse_block(&p_el, &self.doc))];
        Rc::make_mut(&mut self.doc.stories).insert(rid.clone(), Story { part, root: Rc::new(root), content, dirty: true });
        // referenced from the last section; earlier sections inherit it
        let mut sect = self.doc.sect.as_deref().cloned().unwrap_or_else(|| Element::new_w("sectPr"));
        let mut r = Element::new_w(&format!("{kind}Reference")).with_wattr("type", "default");
        r.attrs.push(xml::Attr { qname: "r:id".into(), ns: xml::Ns::R, local: "id".into(), value: rid.clone() });
        if !self.doc.root.attrs.iter().any(|a| a.qname == "xmlns:r") {
            r.attrs.push(xml::Attr { qname: "xmlns:r".into(), ns: xml::Ns::None, local: "xmlns:r".into(), value: R.into() });
        }
        sect.children.retain(|c| !matches!(c, Node::Elem(x) if x.is_w(&format!("{kind}Reference")) && x.wattr("type") == Some("default")));
        sect.children.insert(0, Node::Elem(r));
        self.doc.sect = Some(Rc::new(sect));
        self.changed();
        rid
    }

    /// Inserts a field (PAGE, NUMPAGES) at the caret.
    pub fn insert_field(&mut self, code: &str, shown: &str) {
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let f = self.focus.clone();
        let Some(props) = dpara(&self.doc, &f.path).map(|p| p.props_at(f.off)) else { return };
        let raw = |x: &str| {
            let mut r = Run::new(props.clone());
            r.items.push(Item::Raw(Rc::new(el(&format!("<w:x xmlns:w=\"{W}\">{x}</w:x>")).elems().next().cloned().unwrap())));
            Inline::Run(r)
        };
        let mut text = Run::new(props.clone());
        text.items.push(Item::Text(shown.to_string()));
        let runs = vec![
            raw("<w:fldChar w:fldCharType=\"begin\"/>"),
            raw(&format!("<w:instrText xml:space=\"preserve\"> {code} </w:instrText>")),
            raw("<w:fldChar w:fldCharType=\"separate\"/>"),
            Inline::Run(text),
            raw("<w:fldChar w:fldCharType=\"end\"/>"),
        ];
        let n = shown.chars().count();
        if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
            p.insert_inlines(f.off, runs);
        }
        let np = Pos { off: f.off + n, ..f };
        self.anchor = np.clone();
        self.focus = np;
        self.changed();
    }

    // ---------- pictures ----------

    /// Resizes the inline picture at `off` of a paragraph (points).
    pub fn resize_image(&mut self, path: &[u32], off: usize, w: f32, h: f32) -> bool {
        let (w, h) = (w.clamp(4.0, 2000.0), h.clamp(4.0, 2000.0));
        let found = dpara(&self.doc, path).is_some_and(|p| image_index(p, off).is_some());
        if !found {
            return false;
        }
        self.checkpoint(Kind::Other);
        if let Some(p) = dpara_mut(&mut self.doc, path) {
            if let Some((i, j)) = image_index(p, off) {
                if let Inline::Run(r) = &mut p.inlines[i] {
                    if let Item::Image(d) = &r.items[j] {
                        r.items[j] = Item::Image(Rc::new(d.resized(w, h)));
                    }
                }
            }
        }
        self.changed();
        true
    }

    // ---------- page setup ----------

    /// kind: "margins" (top, bottom, left, right), "orient" (a = 1 for
    /// landscape), "size" (a = width, b = height, portrait). Twips. Applies
    /// to every section.
    pub fn set_page(&mut self, kind: &str, a: i32, b: i32, c: i32, d: i32) {
        self.checkpoint(Kind::Other);
        let apply = |sect: &mut Element| {
            let sp = SectProps::parse(sect);
            match kind {
                "margins" => sect_child(sect, "pgMar", &[("top", Some(a.to_string())), ("bottom", Some(b.to_string())), ("left", Some(c.to_string())), ("right", Some(d.to_string()))]),
                "orient" => {
                    let (lo, hi) = (sp.w.min(sp.h), sp.w.max(sp.h));
                    let (w, h, o) = if a == 1 { (hi, lo, Some("landscape".to_string())) } else { (lo, hi, None) };
                    sect_child(sect, "pgSz", &[("w", Some(w.to_string())), ("h", Some(h.to_string())), ("orient", o)]);
                }
                "size" => {
                    let landscape = sp.w > sp.h;
                    let (w, h) = if landscape { (b.max(a), a.min(b)) } else { (a.min(b), b.max(a)) };
                    sect_child(sect, "pgSz", &[("w", Some(w.to_string())), ("h", Some(h.to_string()))]);
                }
                _ => {}
            }
        };
        let mut body_sect = self.doc.sect.as_deref().cloned().unwrap_or_else(|| Element::new_w("sectPr"));
        apply(&mut body_sect);
        self.doc.sect = Some(Rc::new(body_sect));
        for path in crate::edit::para_paths(&self.doc.body) {
            if dpara(&self.doc, &path).is_some_and(|p| p.sect().is_some()) {
                if let Some(p) = dpara_mut(&mut self.doc, &path) {
                    if let Some(ppr) = p.ppr.as_mut() {
                        let e = Rc::make_mut(ppr);
                        for n in e.children.iter_mut() {
                            if let Node::Elem(x) = n {
                                if x.is_w("sectPr") {
                                    apply(x);
                                }
                            }
                        }
                    }
                }
            }
        }
        self.changed();
    }

    /// Moves boundary `i` of a table's grid by `delta` twips (0 < i < n moves
    /// a border between columns, i = n the right edge).
    pub fn resize_column(&mut self, tpath: &[u32], i: usize, delta: i32) -> bool {
        self.checkpoint(Kind::Other);
        let Some(t) = self.table_mut(tpath) else {
            self.undo_discard();
            return false;
        };
        let n = t.grid.len();
        if i == 0 || i > n || delta == 0 {
            self.undo_discard();
            return false;
        }
        let min = 300;
        if i < n {
            let d = delta.clamp(min - t.grid[i - 1], t.grid[i] - min);
            t.grid[i - 1] += d;
            t.grid[i] -= d;
        } else {
            t.grid[n - 1] = (t.grid[n - 1] + delta).max(min);
        }
        let grid = t.grid.clone();
        for it in t.items.iter_mut() {
            if let TableItem::Row(row) = it {
                let pos = grid_pos(row);
                for (ci, (gx, span)) in pos.into_iter().enumerate() {
                    let w: i32 = grid.iter().skip(gx).take(span).sum();
                    if let Some(cell) = row.cell_mut(ci) {
                        tcpr_set(cell, "tcW", Some(tcw(w)));
                    }
                }
            }
        }
        // a fixed table width follows the grid
        let total: i32 = grid.iter().sum();
        for it in t.items.iter_mut() {
            if let TableItem::Raw(e) = it {
                if e.is_w("tblPr") {
                    let e = Rc::make_mut(e);
                    for n in e.children.iter_mut() {
                        if let Node::Elem(x) = n {
                            if x.is_w("tblW") && x.wattr("type") == Some("dxa") {
                                x.set_wattr("w", &total.to_string());
                            }
                        }
                    }
                }
            }
        }
        set_grid(t);
        self.changed();
        true
    }

    // ---------- lists ----------

    fn list_kind(&self, p: &Para) -> Option<bool> {
        let eff = self.doc.styles.para(&p.props, None);
        let n = eff.num_id.filter(|n| *n > 0)?;
        let l = self.doc.numbering.level(n, eff.ilvl.unwrap_or(0))?;
        Some(l.fmt == "bullet")
    }

    /// Bullets (`bullet`) or numbering on the selected paragraphs; off when
    /// they all have it already.
    pub fn toggle_list(&mut self, bullet: bool) {
        let paths: Vec<Vec<u32>> = self.ranges().into_iter().map(|r| r.0).collect();
        let all_on = paths.iter().all(|p| dpara(&self.doc, p).and_then(|p| self.list_kind(p)) == Some(bullet));
        self.checkpoint(Kind::Other);
        let styles = self.doc.styles.clone();
        if all_on {
            for path in &paths {
                if let Some(p) = dpara_mut(&mut self.doc, path) {
                    remove_numbering(p, &styles);
                }
            }
        } else {
            let id = self.list_num(bullet);
            for path in &paths {
                let keep = dpara(&self.doc, path).filter(|p| self.list_kind(p).is_some()).and_then(|p| styles.para(&p.props, None).ilvl);
                if let Some(p) = dpara_mut(&mut self.doc, path) {
                    p.props.num_id = Some(id);
                    p.props.ilvl = Some(keep.unwrap_or(0));
                    // the level's indents apply, not leftovers
                    p.props.ind_left = None;
                    p.props.ind_first = None;
                    p.props_dirty = true;
                }
            }
        }
        self.changed();
    }

    /// Tab / Shift+Tab in a list item; false when not in a list.
    pub fn list_level(&mut self, delta: i32) -> bool {
        let paths: Vec<Vec<u32>> = self.ranges().into_iter().map(|r| r.0).collect();
        if !paths.iter().all(|p| dpara(&self.doc, p).is_some_and(|p| self.list_kind(p).is_some())) {
            return false;
        }
        self.checkpoint(Kind::Other);
        let styles = self.doc.styles.clone();
        for path in &paths {
            let cur = dpara(&self.doc, path).map(|p| styles.para(&p.props, None)).unwrap_or_default();
            if let Some(p) = dpara_mut(&mut self.doc, path) {
                p.props.num_id = cur.num_id;
                p.props.ilvl = Some((cur.ilvl.unwrap_or(0) + delta).clamp(0, 8));
                p.props_dirty = true;
            }
        }
        self.changed();
        true
    }

    /// A list definition of the kind, reusing one the document has.
    fn list_num(&mut self, bullet: bool) -> i32 {
        let nb = self.doc.numbering.clone();
        let mut ids: Vec<&i32> = nb.nums.keys().collect();
        ids.sort();
        for id in ids {
            let (abs, starts, over) = &nb.nums[id];
            if !starts.is_empty() || !over.is_empty() {
                continue;
            }
            if let Some(Some(l)) = nb.abs.get(abs).and_then(|v| v.first()) {
                let ok = if bullet { l.fmt == "bullet" } else { l.fmt == "decimal" && l.text == "%1." };
                if ok {
                    return *id;
                }
            }
        }
        self.add_numbering(bullet)
    }

    fn add_numbering(&mut self, bullet: bool) -> i32 {
        let part = self
            .doc
            .rels
            .values()
            .find(|r| r.kind == "numbering")
            .map(|r| r.target.clone());
        let part = match part {
            Some(p) => p,
            None => {
                let p = "word/numbering.xml".to_string();
                let doc_part = self.doc.part.clone();
                let id = self.doc.pkg.add_rel(&doc_part, "numbering", "numbering.xml", false);
                self.doc.pkg.ensure_override(&p, "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml");
                self.doc.pkg.set(&p, format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w:numbering xmlns:w=\"{W}\"></w:numbering>").into_bytes());
                Rc::make_mut(&mut self.doc.rels).insert(id, Rel { kind: "numbering".into(), target: p.clone(), external: false });
                p
            }
        };
        let mut text = self.doc.pkg.text(&part).unwrap_or_default();
        let abs = self.doc.pkg.next_number(self.doc.numbering.abs.keys().map(|&k| k.max(0) as u32)) as i32;
        let num = self.doc.pkg.next_number(self.doc.numbering.nums.keys().map(|&k| k.max(0) as u32)) as i32;
        let levels: String = (0..9)
            .map(|i| {
                let left = 720 * (i + 1);
                if bullet {
                    let ch = ["•", "◦", "▪"][i % 3];
                    format!("<w:lvl w:ilvl=\"{i}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"{ch}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{left}\" w:hanging=\"360\"/></w:pPr></w:lvl>")
                } else {
                    let fmt = ["decimal", "lowerLetter", "lowerRoman"][i % 3];
                    format!("<w:lvl w:ilvl=\"{i}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"%{}.\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{left}\" w:hanging=\"360\"/></w:pPr></w:lvl>", i + 1)
                }
            })
            .collect();
        let abs_xml = format!("<w:abstractNum w:abstractNumId=\"{abs}\"><w:multiLevelType w:val=\"hybridMultilevel\"/>{levels}</w:abstractNum>");
        let num_xml = format!("<w:num w:numId=\"{num}\"><w:abstractNumId w:val=\"{abs}\"/></w:num>");
        // schema: all abstractNum before any num
        let first_num = text.find("<w:num ").or_else(|| text.find("<w:num>"));
        let close = text.rfind("</w:numbering>");
        match (first_num, close) {
            (Some(i), Some(_)) => {
                text.insert_str(i, &abs_xml);
                let c = text.rfind("</w:numbering>").unwrap();
                text.insert_str(c, &num_xml);
            }
            (None, Some(c)) => text.insert_str(c, &format!("{abs_xml}{num_xml}")),
            _ => return 0,
        }
        if let Ok(root) = xml::parse(&text) {
            self.doc.numbering = Rc::new(Numbering::parse(&root));
        }
        self.doc.pkg.set(&part, text.into_bytes());
        num
    }

    // ---------- pictures and links ----------

    fn content_width(&self, path: &[u32]) -> i32 {
        if let Some((tp, r, c)) = self.cell_at(path) {
            if let Some(Block::Table(t)) = self.block(&tp) {
                if let Some(cell) = t.rows().nth(r).and_then(|row| row.cells().nth(c)) {
                    if let Some(w) = cell.props.width {
                        return (w - 216).max(720);
                    }
                }
            }
        }
        let s: SectProps = self.doc.last_sect();
        s.w - s.left - s.right
    }

    /// Inserts a picture at the caret; `w`, `h` are its pixel size.
    pub fn insert_image(&mut self, data: Vec<u8>, ext: &str, w: f32, h: f32) -> Result<(), String> {
        let ext = ext.to_lowercase();
        let ct = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            _ => return Err("unsupported image type".into()),
        };
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let pre = self.doc.pkg.name_prefix();
        let mut n = 1;
        while self.doc.pkg.get(&format!("word/media/papir{pre}{n}.{ext}")).is_some() {
            n += 1;
        }
        let name = format!("word/media/papir{pre}{n}.{ext}");
        self.doc.pkg.set(&name, data);
        self.doc.pkg.ensure_default_type(&ext, ct);
        let doc_part = self.doc.part.clone();
        let rid = self.doc.pkg.add_rel(&doc_part, "image", &format!("media/papir{pre}{n}.{ext}"), false);
        Rc::make_mut(&mut self.doc.rels).insert(rid.clone(), Rel { kind: "image".into(), target: name, external: false });

        // pixels at 96 dpi → points, fit into the column
        let max_w = self.content_width(&self.focus.path.clone()) as f32 / 20.0;
        let (mut wp, mut hp) = (w * 0.75, h * 0.75);
        if wp > max_w {
            hp *= max_w / wp;
            wp = max_w;
        }
        let (cx, cy) = ((wp * 12700.0) as i64, (hp * 12700.0) as i64);
        let id = 0x5000 + n;
        let run_xml = format!(
            "<w:r xmlns:w=\"{W}\"><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/><wp:docPr id=\"{id}\" name=\"Зображення {n}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" noChangeAspect=\"1\"/></wp:cNvGraphicFramePr><a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"papir{n}.{ext}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{rid}\" xmlns:r=\"{R}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
        );
        let p_el = el(&format!("<w:p xmlns:w=\"{W}\">{run_xml}</w:p>"));
        let parsed = crate::docx::parse_para(&p_el, &self.doc);
        let f = self.focus.clone();
        if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
            let props = p.props_at(f.off);
            let mut inl = parsed.inlines;
            for i in inl.iter_mut() {
                if let Inline::Run(r) = i {
                    r.open = "<w:r>".into();
                    r.props = props.clone();
                    r.dirty = true;
                }
            }
            p.insert_inlines(f.off, inl);
        }
        let np = Pos { off: f.off + 1, ..f };
        self.anchor = np.clone();
        self.focus = np;
        self.changed();
        Ok(())
    }

    /// Inserts a hyperlink (or links the selected text).
    pub fn insert_link(&mut self, url: &str, text: &str) -> Result<(), String> {
        let u = url.trim();
        let lower = u.to_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")) {
            return Err("bad url".into());
        }
        let text = if text.is_empty() {
            if self.collapsed() { u.to_string() } else { self.selected_text() }
        } else {
            text.to_string()
        };
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let doc_part = self.doc.part.clone();
        let rid = self.doc.pkg.add_rel(&doc_part, "hyperlink", u, true);
        Rc::make_mut(&mut self.doc.rels).insert(rid.clone(), Rel { kind: "hyperlink".into(), target: u.into(), external: true });
        let f = self.focus.clone();
        let style = self.doc.styles.map.values().find(|s| s.kind == "character" && s.name.eq_ignore_ascii_case("hyperlink")).map(|s| s.id.clone());
        let n = text.chars().count();
        if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
            let mut props: RunProps = p.props_at(f.off);
            match style {
                Some(s) => props.style = Some(s),
                None => {
                    props.color = Some("0563C1".into());
                    props.u = Some("single".into());
                }
            }
            let mut r = Run::new(props);
            r.items.push(Item::Text(text));
            let w = Rc::new(Wrapper {
                kind: WrapKind::Hyperlink,
                open: format!("<w:hyperlink r:id=\"{rid}\" xmlns:r=\"{R}\" w:history=\"1\">"),
                close: "</w:hyperlink>".into(),
            });
            p.insert_inlines(f.off, vec![Inline::Open(w.clone()), Inline::Run(r), Inline::Close(w)]);
        }
        let np = Pos { off: f.off + n, ..f };
        self.anchor = np.clone();
        self.focus = np;
        self.changed();
        Ok(())
    }

    // ---------- tables ----------

    fn block(&self, path: &[u32]) -> Option<&Block> {
        let prefix = path.len();
        let (mut c, path) = droot(&self.doc, path)?;
        let _ = prefix;
        let mut k = 0;
        loop {
            let b = c.get(*path.get(k)? as usize)?;
            if k + 1 == path.len() {
                return Some(b);
            }
            match &**b {
                Block::Table(t) => {
                    let cell = t.rows().nth(*path.get(k + 1)? as usize)?.cells().nth(*path.get(k + 2)? as usize)?;
                    c = &cell.content;
                    k += 3;
                }
                Block::Sdt(s) => {
                    c = &s.content;
                    k += 1;
                }
                _ => return None,
            }
        }
    }

    /// Innermost table cell holding `path`: (table path, row, cell).
    pub fn cell_at(&self, full: &[u32]) -> Option<(Vec<u32>, usize, usize)> {
        // table paths keep the header/footer prefix
        let (mut c, path) = droot(&self.doc, full)?;
        let skip = full.len() - path.len();
        let mut k = 0;
        let mut found = None;
        while k < path.len() {
            let b = c.get(path[k] as usize)?;
            match &**b {
                Block::Table(t) if k + 2 < path.len() => {
                    let (r, ci) = (path[k + 1] as usize, path[k + 2] as usize);
                    found = Some((full[..=k + skip].to_vec(), r, ci));
                    c = &t.rows().nth(r)?.cells().nth(ci)?.content;
                    k += 3;
                }
                Block::Sdt(s) if k + 1 < path.len() => {
                    c = &s.content;
                    k += 1;
                }
                _ => break,
            }
        }
        found
    }

    fn table_mut(&mut self, tpath: &[u32]) -> Option<&mut Table> {
        let c = dcont_mut(&mut self.doc, tpath)?;
        match Rc::make_mut(c.get_mut(*tpath.last()? as usize)?) {
            Block::Table(t) => {
                t.orig = None;
                Some(t)
            }
            _ => None,
        }
    }

    /// Tab in a table: caret to the start of the next (previous) cell;
    /// after the last cell a new row is added.
    pub fn next_cell(&mut self, dir: i32) -> bool {
        let Some((tp, r, c)) = self.cell_at(&self.focus.path) else { return false };
        let Some(Block::Table(t)) = self.block(&tp) else { return false };
        let counts: Vec<usize> = t.rows().map(|row| row.cells().count()).collect();
        let (mut r, mut c) = (r as i64, c as i64);
        c += dir as i64;
        if c < 0 {
            r -= 1;
            if r < 0 {
                return true;
            }
            c = counts[r as usize] as i64 - 1;
        } else if c >= counts[r as usize] as i64 {
            r += 1;
            c = 0;
            if r as usize >= counts.len() {
                self.checkpoint(Kind::Other);
                let cur = (r - 1) as usize;
                self.add_row(&tp, cur, true);
                self.changed();
            }
        }
        let path = [tp.as_slice(), &[r as u32, c as u32, 0]].concat();
        let len = dpara(&self.doc, &path).map(|p| p.len()).unwrap_or(0);
        // like Word: the cell's content gets selected
        self.set_selection(Pos { path: path.clone(), off: 0 }, Pos { path, off: len });
        true
    }

    pub fn in_table(&self) -> bool {
        self.cell_at(&self.focus.path).is_some()
    }

    pub fn insert_table(&mut self, rows: usize, cols: usize) {
        let (rows, cols) = (rows.clamp(1, 200), cols.clamp(1, 63));
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let f = self.focus.clone();
        let width = self.content_width(&f.path);
        let cw = width / cols as i32;
        let style = self
            .doc
            .styles
            .map
            .values()
            .find(|s| s.kind == "table" && (s.name.eq_ignore_ascii_case("table grid") || s.id == "TableGrid"))
            .map(|s| s.id.clone());
        let look = "<w:tblLook w:val=\"04A0\" w:firstRow=\"1\" w:lastRow=\"0\" w:firstColumn=\"1\" w:lastColumn=\"0\" w:noHBand=\"0\" w:noVBand=\"1\"/>";
        let tblpr = match &style {
            Some(s) => format!("<w:tblPr><w:tblStyle w:val=\"{}\"/><w:tblW w:w=\"0\" w:type=\"auto\"/>{look}</w:tblPr>", xml::escape(s)),
            None => {
                let b = |n: &str| format!("<w:{n} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>");
                format!(
                    "<w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>{}{}{}{}{}{}</w:tblBorders><w:tblCellMar><w:left w:w=\"108\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar>{look}</w:tblPr>",
                    b("top"), b("left"), b("bottom"), b("right"), b("insideH"), b("insideV")
                )
            }
        };
        let para = if style.is_some() { "<w:p/>".to_string() } else { "<w:p><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr></w:p>".to_string() };
        let grid: String = (0..cols).map(|_| format!("<w:gridCol w:w=\"{cw}\"/>")).collect();
        let row: String = format!("<w:tr>{}</w:tr>", (0..cols).map(|_| format!("<w:tc><w:tcPr><w:tcW w:w=\"{cw}\" w:type=\"dxa\"/></w:tcPr>{para}</w:tc>")).collect::<String>());
        let xml_src = format!("<w:tbl xmlns:w=\"{W}\">{tblpr}<w:tblGrid>{grid}</w:tblGrid>{}</w:tbl>", row.repeat(rows));
        let table = crate::docx::parse_block(&el(&xml_src), &self.doc);
        let mut table = match table {
            Block::Table(t) => t,
            _ => return,
        };
        table.orig = None;
        // cells start with the formatting of the paragraph the caret was in
        if let Some(cur) = dpara(&self.doc, &f.path) {
            let mut like = empty_like(Some(cur));
            like.props.num_id = None;
            like.props.ilvl = None;
            like.props.ind_left = None;
            like.props.ind_first = None;
            like.props.ind_right = None;
            like.props.jc = None;
            like.props.page_break_before = None;
            like.props_dirty = true;
            like.ppr = like.ppr.as_ref().map(|e| {
                let mut e = (**e).clone();
                e.children.retain(|n| !matches!(n, Node::Elem(x) if x.is_w("sectPr") || x.is_w("numPr")));
                Rc::new(e)
            });
            if style.is_none() {
                like.props.after = Some(0);
                like.props.before = Some(0);
                like.props.line = Some(240);
                like.props.line_rule = Some("auto".into());
            }
            for it in table.items.iter_mut() {
                if let TableItem::Row(r) = it {
                    for ci in r.items.iter_mut() {
                        if let RowItem::Cell(c) = ci {
                            let mut p = like.clone();
                            p.rev = next_rev();
                            c.content = vec![Rc::new(Block::Para(p))];
                        }
                    }
                }
            }
        }

        let idx = *f.path.last().unwrap() as usize;
        let (len, empty) = dpara(&self.doc, &f.path).map(|p| (p.len(), p.len() == 0)).unwrap_or((0, true));
        let at = if empty || f.off == 0 {
            idx
        } else {
            if f.off < len {
                // the table goes between the two halves
                if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
                    let tail = p.split_off(f.off);
                    if let Some(c) = dcont_mut(&mut self.doc, &f.path) {
                        c.insert(idx + 1, Rc::new(Block::Para(tail)));
                    }
                }
            } else {
                let like = dpara(&self.doc, &f.path).cloned();
                if let Some(c) = dcont_mut(&mut self.doc, &f.path) {
                    let next_is_para = matches!(c.get(idx + 1).map(|b| &**b), Some(Block::Para(_)));
                    if !next_is_para {
                        c.insert(idx + 1, Rc::new(Block::Para(empty_like(like.as_ref()))));
                    }
                }
            }
            idx + 1
        };
        if let Some(c) = dcont_mut(&mut self.doc, &f.path) {
            c.insert(at, Rc::new(Block::Table(table)));
        }
        let mut np = f.path[..f.path.len() - 1].to_vec();
        np.extend([at as u32, 0, 0, 0]);
        let p = Pos { path: np, off: 0 };
        self.anchor = p.clone();
        self.focus = p;
        self.changed();
    }

    /// row_above, row_below, col_left, col_right, del_row, del_col,
    /// del_table, merge, distribute
    pub fn table_op(&mut self, op: &str) -> bool {
        let Some((tp, r, c)) = self.cell_at(&self.focus.path) else { return false };
        self.checkpoint(Kind::Other);
        let ok = match op {
            "row_above" | "row_below" => self.add_row(&tp, r, op == "row_below"),
            "col_left" | "col_right" => self.add_col(&tp, r, c, op == "col_right"),
            "del_row" => self.del_row(&tp, r),
            "del_col" => self.del_col(&tp, r, c),
            "del_table" => self.del_table(&tp),
            "merge" => self.merge(&tp),
            "distribute" => self.distribute(&tp),
            _ => false,
        };
        if ok {
            let (a, f) = (self.anchor.clone(), self.focus.clone());
            self.anchor = self.clamp(a);
            self.focus = self.clamp(f);
            self.changed();
        } else {
            self.undo_pop();
        }
        ok
    }

    fn undo_pop(&mut self) {
        // nothing changed: drop the checkpoint
        let _ = self.undo_discard();
    }

    fn add_row(&mut self, tp: &[u32], r: usize, below: bool) -> bool {
        let Some(t) = self.table_mut(tp) else { return false };
        let Some(row) = t.rows().nth(r).cloned() else { return false };
        let mut new = row.clone();
        new.wrap = None;
        new.open = "<w:tr>".into();
        for it in new.items.iter_mut() {
            if let RowItem::Cell(cell) = it {
                let like = first_para(&cell.content).cloned();
                cell.content = vec![Rc::new(Block::Para(empty_like(like.as_ref())))];
                cell.wrap = None;
                cell.open = "<w:tc>".into();
                if cell.props.vmerge.is_some() {
                    tcpr_set(cell, "vMerge", None);
                }
            }
        }
        let idx = item_index(t, r);
        t.items.insert(if below { idx + 1 } else { idx }, TableItem::Row(new));
        if !below {
            // the caret stays in its row, which moved down
            let k = tp.len();
            for p in [&mut self.anchor, &mut self.focus] {
                if p.path.len() > k + 1 && p.path[..k] == *tp {
                    p.path[k] += 1;
                }
            }
        }
        true
    }

    fn add_col(&mut self, tp: &[u32], r: usize, c: usize, right: bool) -> bool {
        let Some(t) = self.table_mut(tp) else { return false };
        let Some(row) = t.rows().nth(r) else { return false };
        let pos = grid_pos(row);
        let Some(&(gx, span)) = pos.get(c) else { return false };
        // split the current column (or the last of a merged cell) in two
        let g = if right { gx + span - 1 } else { gx };
        if g >= t.grid.len() {
            return false;
        }
        let half = (t.grid[g] / 2).max(360);
        t.grid[g] = half;
        let new_g = if right { g + 1 } else { g };
        t.grid.insert(new_g, half);
        for it in t.items.iter_mut() {
            let TableItem::Row(row) = it else { continue };
            let pos = grid_pos(row);
            let Some(ci) = pos.iter().position(|(x, s)| *x <= g && g < x + s) else { continue };
            let (_, s) = pos[ci];
            let cell = row.cell_mut(ci).unwrap();
            if s > 1 {
                cell.props.span += 1;
                let span = cell.props.span;
                tcpr_set(cell, "gridSpan", Some(Element::new_w("gridSpan").with_wattr("val", &span.to_string())));
                continue;
            }
            tcpr_set(cell, "tcW", Some(tcw(half)));
            let mut nc = cell.clone();
            nc.wrap = None;
            nc.open = "<w:tc>".into();
            let like = first_para(&nc.content).cloned();
            nc.content = vec![Rc::new(Block::Para(empty_like(like.as_ref())))];
            let at = row.items.iter().enumerate().filter(|(_, x)| matches!(x, RowItem::Cell(_))).nth(ci).map(|(i, _)| i).unwrap();
            row.items.insert(if right { at + 1 } else { at }, RowItem::Cell(nc));
        }
        set_grid(t);
        if !right {
            let k = tp.len();
            for p in [&mut self.anchor, &mut self.focus] {
                if p.path.len() > k + 2 && p.path[..k] == *tp && p.path[k] as usize == r {
                    p.path[k + 1] += 1;
                }
            }
        }
        true
    }

    fn del_row(&mut self, tp: &[u32], r: usize) -> bool {
        let rows = match self.block(tp) {
            Some(Block::Table(t)) => t.rows().count(),
            _ => return false,
        };
        if rows <= 1 {
            return self.del_table(tp);
        }
        let Some(t) = self.table_mut(tp) else { return false };
        let idx = item_index(t, r);
        // a merged area starting here continues from the next row
        let starts: Vec<usize> = t.rows().nth(r).map(|row| grid_pos(row).into_iter().zip(row.cells()).filter(|(_, c)| c.props.vmerge == Some(crate::props::VMerge::Restart)).map(|((g, _), _)| g).collect()).unwrap_or_default();
        let removed = t.items.remove(idx);
        if let Some(next) = t.row_mut(r) {
            let pos = grid_pos(next);
            for (ci, (g, _)) in pos.iter().enumerate() {
                if starts.contains(g) {
                    let cell = next.cell_mut(ci).unwrap();
                    if cell.props.vmerge == Some(crate::props::VMerge::Continue) {
                        tcpr_set(cell, "vMerge", Some(Element::new_w("vMerge").with_wattr("val", "restart")));
                    }
                }
            }
        }
        // a row that carried the wrapper's start or end hands it on
        if let TableItem::Row(old) = removed {
            if let Some((o, cl)) = old.wrap {
                if let Some(TableItem::Row(n)) = t.items.get_mut(idx).filter(|x| matches!(x, TableItem::Row(_))) {
                    let (a, b) = n.wrap.take().unwrap_or(("".into(), "".into()));
                    n.wrap = Some((format!("{o}{a}").into(), format!("{b}{cl}").into()));
                }
            }
        }
        let k = tp.len();
        let new_rows = rows - 1;
        for p in [&mut self.anchor, &mut self.focus] {
            if p.path.len() > k + 2 && p.path[..k] == *tp {
                p.path[k] = (p.path[k]).min(new_rows as u32 - 1);
                p.path[k + 1] = 0;
                p.path.truncate(k + 3);
                p.path.push(0);
                p.off = 0;
            }
        }
        true
    }

    fn del_col(&mut self, tp: &[u32], r: usize, c: usize) -> bool {
        let Some(t) = self.table_mut(tp) else { return false };
        let Some(row) = t.rows().nth(r) else { return false };
        let Some(&(g, _)) = grid_pos(row).get(c) else { return false };
        if t.grid.len() <= 1 {
            return self.del_table(tp);
        }
        let Some(t) = self.table_mut(tp) else { return false };
        let w = t.grid.remove(g.min(t.grid.len() - 1));
        for it in t.items.iter_mut() {
            let TableItem::Row(row) = it else { continue };
            let pos = grid_pos(row);
            let Some(ci) = pos.iter().position(|(x, s)| *x <= g && g < x + s) else { continue };
            if pos[ci].1 > 1 {
                let cell = row.cell_mut(ci).unwrap();
                cell.props.span -= 1;
                let span = cell.props.span;
                tcpr_set(cell, "gridSpan", if span > 1 { Some(Element::new_w("gridSpan").with_wattr("val", &span.to_string())) } else { None });
                let cw = cell.props.width.unwrap_or(w * 2) - w;
                tcpr_set(cell, "tcW", Some(tcw(cw.max(360))));
            } else {
                let at = row.items.iter().enumerate().filter(|(_, x)| matches!(x, RowItem::Cell(_))).nth(ci).map(|(i, _)| i).unwrap();
                row.items.remove(at);
            }
        }
        // rows left without cells go
        t.items.retain(|i| !matches!(i, TableItem::Row(r) if r.cells().count() == 0));
        set_grid(t);
        let k = tp.len();
        for p in [&mut self.anchor, &mut self.focus] {
            if p.path.len() > k + 2 && p.path[..k] == *tp {
                p.path[k + 1] = p.path[k + 1].saturating_sub(1);
                p.path.truncate(k + 3);
                p.path.push(0);
                p.off = 0;
            }
        }
        true
    }

    fn del_table(&mut self, tp: &[u32]) -> bool {
        let idx = *tp.last().unwrap() as usize;
        let Some(c) = dcont_mut(&mut self.doc, tp) else { return false };
        c.remove(idx);
        // keep a paragraph where the table was
        let has_para = matches!(c.get(idx).map(|b| &**b), Some(Block::Para(_)));
        if !has_para {
            c.insert(idx, Rc::new(Block::Para(Para::new(Default::default(), Default::default()))));
        }
        let p = Pos { path: tp.to_vec(), off: 0 };
        self.anchor = p.clone();
        self.focus = p;
        true
    }

    fn distribute(&mut self, tp: &[u32]) -> bool {
        let Some(t) = self.table_mut(tp) else { return false };
        let n = t.grid.len().max(1) as i32;
        let each = t.grid.iter().sum::<i32>() / n;
        for g in t.grid.iter_mut() {
            *g = each;
        }
        for it in t.items.iter_mut() {
            if let TableItem::Row(row) = it {
                for ci in 0..row.cells().count() {
                    let cell = row.cell_mut(ci).unwrap();
                    let span = cell.props.span.max(1) as i32;
                    tcpr_set(cell, "tcW", Some(tcw(each * span)));
                }
            }
        }
        set_grid(t);
        true
    }

    /// Merges the selected cells (one row, one column, or a rectangle).
    fn merge(&mut self, tp: &[u32]) -> bool {
        let (a, b) = self.ordered();
        let (Some((ta, ra, ca)), Some((tb, rb, cb))) = (self.cell_at(&a.path), self.cell_at(&b.path)) else { return false };
        if ta != tp || tb != tp {
            return false;
        }
        let Some(Block::Table(t0)) = self.block(tp) else { return false };
        let ga = t0.rows().nth(ra).map(|r| grid_pos(r)[ca]).unwrap();
        let gb = t0.rows().nth(rb).map(|r| grid_pos(r)[cb]).unwrap();
        let (r1, r2) = (ra.min(rb), ra.max(rb));
        let g1 = ga.0.min(gb.0);
        let g2 = (ga.0 + ga.1).max(gb.0 + gb.1);
        if r1 == r2 && ga == gb {
            return false;
        }
        let Some(t) = self.table_mut(tp) else { return false };
        // horizontal: cells of each row within [g1, g2) become one
        for r in r1..=r2 {
            let Some(row) = t.row_mut(r) else { continue };
            let pos = grid_pos(row);
            let idx: Vec<usize> = pos.iter().enumerate().filter(|(_, (g, s))| *g >= g1 && g + s <= g2).map(|(i, _)| i).collect();
            if idx.len() < 2 {
                continue;
            }
            let span: usize = idx.iter().map(|&i| pos[i].1).sum();
            let mut moved: Vec<Rc<Block>> = Vec::new();
            for &i in &idx[1..] {
                let c = row.cell(i);
                moved.extend(c.content.iter().filter(|b| !is_empty_para(b)).cloned());
            }
            let width: i32 = idx.iter().map(|&i| row.cell(i).props.width.unwrap_or(0)).sum();
            let first = row.cell_mut(idx[0]).unwrap();
            if !moved.is_empty() {
                first.content.retain(|b| !is_empty_para(b));
                first.content.extend(moved);
                if first.content.is_empty() {
                    first.content.push(Rc::new(Block::Para(Para::new(Default::default(), Default::default()))));
                }
            }
            first.props.span = span as u32;
            tcpr_set(first, "gridSpan", Some(Element::new_w("gridSpan").with_wattr("val", &span.to_string())));
            if width > 0 {
                tcpr_set(first, "tcW", Some(tcw(width)));
            }
            // drop the merged-away cells, last first
            let item_idx: Vec<usize> = row.items.iter().enumerate().filter(|(_, x)| matches!(x, RowItem::Cell(_))).map(|(i, _)| i).collect();
            for &i in idx[1..].iter().rev() {
                row.items.remove(item_idx[i]);
            }
        }
        // vertical: the cell at g1 of the first row restarts, the rest continue
        if r2 > r1 {
            let mut moved: Vec<Rc<Block>> = Vec::new();
            for r in r1 + 1..=r2 {
                let Some(row) = t.row_mut(r) else { continue };
                let pos = grid_pos(row);
                if let Some(ci) = pos.iter().position(|(g, _)| *g == g1) {
                    let cell = row.cell_mut(ci).unwrap();
                    moved.extend(cell.content.iter().filter(|b| !is_empty_para(b)).cloned());
                    let like = first_para(&cell.content).cloned();
                    cell.content = vec![Rc::new(Block::Para(empty_like(like.as_ref())))];
                    tcpr_set(cell, "vMerge", Some(Element::new_w("vMerge")));
                }
            }
            if let Some(row) = t.row_mut(r1) {
                let pos = grid_pos(row);
                if let Some(ci) = pos.iter().position(|(g, _)| *g == g1) {
                    let cell = row.cell_mut(ci).unwrap();
                    tcpr_set(cell, "vMerge", Some(Element::new_w("vMerge").with_wattr("val", "restart")));
                    if !moved.is_empty() {
                        cell.content.retain(|b| !is_empty_para(b));
                        cell.content.extend(moved);
                        if cell.content.is_empty() {
                            cell.content.push(Rc::new(Block::Para(Para::new(Default::default(), Default::default()))));
                        }
                    }
                }
            }
        }
        let p = Pos { path: [tp, &[r1 as u32, 0, 0]].concat(), off: 0 };
        // the first cell of the merged area
        let first_ci = t.rows().nth(r1).map(|row| grid_pos(row).iter().position(|(g, _)| *g == g1).unwrap_or(0)).unwrap_or(0);
        let mut path = p.path;
        let k = tp.len();
        path[k + 1] = first_ci as u32;
        path.push(0);
        let p = Pos { path, off: 0 };
        self.anchor = p.clone();
        self.focus = p;
        true
    }
}

/// (inline, item) of the picture occupying caret position `off`.
pub(crate) fn image_index(p: &Para, off: usize) -> Option<(usize, usize)> {
    let mut pos = 0;
    for (i, inl) in p.inlines.iter().enumerate() {
        if let Inline::Run(r) = inl {
            for (j, it) in r.items.iter().enumerate() {
                if pos == off && matches!(it, Item::Image(d) if !d.anchored) {
                    return Some((i, j));
                }
                pos += it.len();
            }
        }
    }
    None
}

fn is_empty_para(b: &Rc<Block>) -> bool {
    matches!(&**b, Block::Para(p) if p.len() == 0)
}

fn item_index(t: &Table, r: usize) -> usize {
    t.items
        .iter()
        .enumerate()
        .filter(|(_, x)| matches!(x, TableItem::Row(_)))
        .nth(r)
        .map(|(i, _)| i)
        .unwrap_or(t.items.len())
}

/// Rewrites `w:tblGrid` from `t.grid`.
fn set_grid(t: &mut Table) {
    let mut g = Element::new_w("tblGrid");
    for w in &t.grid {
        g.children.push(Node::Elem(Element::new_w("gridCol").with_wattr("w", &w.to_string())));
    }
    match t.items.iter().position(|i| matches!(i, TableItem::Raw(e) if e.is_w("tblGrid"))) {
        Some(i) => t.items[i] = TableItem::Raw(Rc::new(g)),
        None => {
            let at = t.items.iter().position(|i| matches!(i, TableItem::Row(_))).unwrap_or(0);
            t.items.insert(at, TableItem::Raw(Rc::new(g)));
        }
    }
}

impl Row {
    fn cell(&self, i: usize) -> &TableCell {
        self.cells().nth(i).unwrap()
    }
}
