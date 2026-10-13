//! Comments: word/comments.xml (+ commentsExtended.xml for replies and
//! resolved state). In the body a comment is `w:commentRangeStart`,
//! `w:commentRangeEnd` (paragraph-level raw markup) and a run holding
//! `w:commentReference`.

use crate::xml::{self, Element, Node, Ns};
use std::rc::Rc;

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub initials: String,
    pub date: String,
    /// paragraphs of the comment (`w:p` elements)
    pub paras: Vec<Element>,
    /// w14:paraId of the last paragraph (links replies and done state)
    pub para_id: Option<String>,
    /// para_id of the comment this one answers
    pub parent: Option<String>,
    pub done: bool,
}

impl Comment {
    pub fn text(&self) -> String {
        self.paras.iter().map(|p| p.text()).collect::<Vec<_>>().join("\n")
    }
}

#[derive(Clone, Debug, Default)]
pub struct Comments {
    pub items: Vec<Comment>,
    /// changed since loading: the parts are written again on save
    pub dirty: bool,
}

const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
const W15: &str = "http://schemas.microsoft.com/office/word/2012/wordml";

fn para_id(p: &Element) -> Option<String> {
    p.attrs.iter().find(|a| a.local == "paraId").map(|a| a.value.clone())
}

impl Comments {
    pub fn parse(comments: Option<&Element>, ext: Option<&Element>) -> Comments {
        let mut items: Vec<Comment> = Vec::new();
        if let Some(root) = comments {
            for c in root.wchildren("comment") {
                let paras: Vec<Element> = c.elems().filter(|e| e.is_w("p")).cloned().collect();
                items.push(Comment {
                    id: c.wattr("id").unwrap_or("").to_string(),
                    author: c.wattr("author").unwrap_or("").to_string(),
                    initials: c.wattr("initials").unwrap_or("").to_string(),
                    date: c.wattr("date").unwrap_or("").to_string(),
                    para_id: paras.last().and_then(para_id),
                    paras,
                    parent: None,
                    done: false,
                });
            }
        }
        if let Some(root) = ext {
            for e in root.elems().filter(|e| e.local == "commentEx") {
                let get = |n: &str| e.attrs.iter().find(|a| a.local == n).map(|a| a.value.clone());
                let Some(pid) = get("paraId") else { continue };
                if let Some(c) = items.iter_mut().find(|c| c.para_id.as_deref() == Some(&pid)) {
                    c.parent = get("paraIdParent");
                    c.done = get("done").as_deref() == Some("1");
                }
            }
        }
        Comments { items, dirty: false }
    }

    pub fn get(&self, id: &str) -> Option<&Comment> {
        self.items.iter().find(|c| c.id == id)
    }

    pub fn next_id(&self, body_ids: &[u32]) -> String {
        let m = self.items.iter().filter_map(|c| c.id.parse::<u32>().ok()).chain(body_ids.iter().copied()).max();
        m.map(|v| v + 1).unwrap_or(0).to_string()
    }

    /// `next_id`, or the next number in this participant's range when several edit.
    pub fn next_id_in(&self, body_ids: &[u32], pkg: &crate::docx::Package) -> String {
        match pkg.ns {
            None => self.next_id(body_ids),
            Some(_) => pkg.next_number(self.items.iter().filter_map(|c| c.id.parse::<u32>().ok()).chain(body_ids.iter().copied())).to_string(),
        }
    }

    /// A new w14:paraId that is not used yet.
    pub fn new_para_id(&self, seed: u64) -> String {
        let mut v = (seed.wrapping_mul(2654435761) & 0x7FFF_FFFF) as u32;
        while self.items.iter().any(|c| c.para_id.as_deref() == Some(&format!("{v:08X}"))) {
            v = (v + 7919) & 0x7FFF_FFFF;
        }
        format!("{v:08X}")
    }

    pub fn comments_xml(&self) -> String {
        let mut s = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:w14=\"{W14}\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">");
        for c in &self.items {
            s.push_str("<w:comment w:id=\"");
            xml::escape_into(&mut s, &c.id, true);
            s.push_str("\" w:author=\"");
            xml::escape_into(&mut s, &c.author, true);
            s.push_str("\" w:date=\"");
            xml::escape_into(&mut s, &c.date, true);
            s.push_str("\" w:initials=\"");
            xml::escape_into(&mut s, &c.initials, true);
            s.push_str("\">");
            for p in &c.paras {
                p.write_to(&mut s);
            }
            s.push_str("</w:comment>");
        }
        s.push_str("</w:comments>");
        s
    }

    pub fn ext_xml(&self) -> String {
        let mut s = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w15:commentsEx xmlns:w15=\"{W15}\">");
        for c in &self.items {
            if let Some(pid) = &c.para_id {
                s.push_str(&format!("<w15:commentEx w15:paraId=\"{pid}\""));
                if let Some(p) = &c.parent {
                    s.push_str(&format!(" w15:paraIdParent=\"{p}\""));
                }
                s.push_str(&format!(" w15:done=\"{}\"/>", if c.done { 1 } else { 0 }));
            }
        }
        s.push_str("</w15:commentsEx>");
        s
    }
}

/// Paragraph of a comment's text, with its paraId.
pub fn comment_para(text: &str, pid: &str) -> Element {
    let mut s = format!("<w:p xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:w14=\"{W14}\" w14:paraId=\"{pid}\" w14:textId=\"77777777\">");
    s.push_str("<w:r><w:t xml:space=\"preserve\">");
    xml::escape_into(&mut s, text, false);
    s.push_str("</w:t></w:r></w:p>");
    let mut e = xml::parse(&s).expect("generated");
    // namespace declarations live on the comments root
    e.attrs.retain(|a| !(a.qname == "xmlns:w" || a.qname == "xmlns:w14"));
    e
}

/// Element of a comment marker: commentRangeStart / commentRangeEnd / commentReference.
pub fn marker(local: &str, id: &str) -> Rc<Element> {
    Rc::new(Element::new_w(local).with_wattr("id", id))
}

pub fn is_marker(e: &Element, local: &str) -> Option<String> {
    (e.ns == Ns::W && e.local == local).then(|| e.wattr("id").unwrap_or("").to_string())
}

pub fn strip_ns(e: &mut Element) {
    e.children.retain(|n| !matches!(n, Node::Text(t) if t.trim().is_empty()));
}

use crate::edit::{para_at, para_mut, para_paths};
use crate::editor::{Editor, Kind, Pos};
use crate::model::{Inline, Item, Run};
use std::collections::HashMap;

/// Where a comment sits in the body.
#[derive(Clone, Debug)]
pub struct Anchor {
    pub start: Pos,
    pub end: Pos,
}

fn initials(name: &str) -> String {
    name.split_whitespace().take(2).filter_map(|w| w.chars().next()).flat_map(|c| c.to_uppercase()).collect()
}

impl Editor {
    /// Anchors of all comments found in the body.
    pub fn comment_anchors(&self) -> HashMap<String, Anchor> {
        let mut starts: HashMap<String, Pos> = HashMap::new();
        let mut out: HashMap<String, Anchor> = HashMap::new();
        for path in para_paths(&self.doc.body) {
            let Some(p) = para_at(&self.doc.body, &path) else { continue };
            let mut pos = 0;
            for inl in &p.inlines {
                match inl {
                    Inline::Raw(e) => {
                        let here = Pos { path: path.clone(), off: pos };
                        if let Some(id) = is_marker(e, "commentRangeStart") {
                            starts.insert(id, here);
                        } else if let Some(id) = is_marker(e, "commentRangeEnd") {
                            let start = starts.get(&id).cloned().unwrap_or_else(|| here.clone());
                            out.insert(id, Anchor { start, end: here });
                        }
                    }
                    Inline::Run(r) => {
                        for it in &r.items {
                            if let Item::Raw(e) = it {
                                if let Some(id) = is_marker(e, "commentReference") {
                                    let here = Pos { path: path.clone(), off: pos };
                                    out.entry(id).or_insert(Anchor { start: here.clone(), end: here });
                                }
                            }
                        }
                        pos += r.len();
                    }
                    _ => {}
                }
            }
        }
        out
    }

    fn body_comment_ids(&self) -> Vec<u32> {
        self.comment_anchors().keys().filter_map(|k| k.parse().ok()).collect()
    }

    fn reference_run(&self, id: &str) -> Inline {
        let mut props = crate::props::RunProps::default();
        if let Some(s) = self.doc.styles.map.values().find(|s| s.kind == "character" && s.name.eq_ignore_ascii_case("annotation reference")) {
            props.style = Some(s.id.clone());
        }
        let mut r = Run::new(props);
        r.items.push(Item::Raw(marker("commentReference", id)));
        Inline::Run(r)
    }

    /// Marks the range [a, b] with comment `id`.
    fn put_markers(&mut self, id: &str, a: &Pos, b: &Pos) {
        let reference = self.reference_run(id);
        if let Some(p) = para_mut(&mut self.doc.body, &b.path) {
            p.insert_inlines(b.off, vec![Inline::Raw(marker("commentRangeEnd", id)), reference]);
        }
        if let Some(p) = para_mut(&mut self.doc.body, &a.path) {
            p.insert_inlines(a.off, vec![Inline::Raw(marker("commentRangeStart", id))]);
        }
    }

    /// Comments the selection (the word at the caret when nothing is
    /// selected). `date` is an ISO time from the host.
    pub fn add_comment(&mut self, text: &str, author: &str, date: &str) -> Option<String> {
        if text.trim().is_empty() {
            return None;
        }
        if self.collapsed() {
            self.select_word();
        }
        let (a, b) = self.ordered();
        self.checkpoint(Kind::Other);
        let ids = self.body_comment_ids();
        let c = Rc::make_mut(&mut self.doc.comments);
        let id = c.next_id_in(&ids, &self.doc.pkg);
        let pid = c.new_para_id(self.version + c.items.len() as u64 + 1);
        c.items.push(Comment {
            id: id.clone(),
            author: author.to_string(),
            initials: initials(author),
            date: date.to_string(),
            paras: vec![comment_para(text, &pid)],
            para_id: Some(pid),
            parent: None,
            done: false,
        });
        c.dirty = true;
        self.put_markers(&id, &a, &b);
        self.changed();
        Some(id)
    }

    pub fn reply_comment(&mut self, parent: &str, text: &str, author: &str, date: &str) -> Option<String> {
        if text.trim().is_empty() {
            return None;
        }
        let anchor = self.comment_anchors().get(parent).cloned()?;
        self.checkpoint(Kind::Other);
        let ids = self.body_comment_ids();
        let c = Rc::make_mut(&mut self.doc.comments);
        // the parent needs a paraId to be pointed at
        let seed = self.version + c.items.len() as u64 + 11;
        let ppid = match c.items.iter().find(|x| x.id == parent).and_then(|x| x.para_id.clone()) {
            Some(p) => p,
            None => {
                let pid = c.new_para_id(seed);
                if let Some(x) = c.items.iter_mut().find(|x| x.id == parent) {
                    if let Some(last) = x.paras.last_mut() {
                        last.attrs.retain(|a| a.local != "paraId");
                        last.attrs.push(xml::Attr { qname: "w14:paraId".into(), ns: Ns::Other, local: "paraId".into(), value: pid.clone() });
                    }
                    x.para_id = Some(pid.clone());
                }
                pid
            }
        };
        let id = c.next_id_in(&ids, &self.doc.pkg);
        let pid = c.new_para_id(seed + 1);
        c.items.push(Comment {
            id: id.clone(),
            author: author.to_string(),
            initials: initials(author),
            date: date.to_string(),
            paras: vec![comment_para(text, &pid)],
            para_id: Some(pid),
            parent: Some(ppid),
            done: false,
        });
        c.dirty = true;
        self.put_markers(&id, &anchor.start, &anchor.end);
        self.changed();
        Some(id)
    }

    /// Marks a thread resolved (or open again).
    pub fn resolve_comment(&mut self, id: &str, done: bool) -> bool {
        let Some(pid) = self.doc.comments.get(id).map(|c| c.para_id.clone()) else { return false };
        self.checkpoint(Kind::Other);
        let c = Rc::make_mut(&mut self.doc.comments);
        for x in c.items.iter_mut() {
            if x.id == id || (pid.is_some() && x.parent == pid) {
                x.done = done;
            }
        }
        c.dirty = true;
        self.changed();
        true
    }

    /// Deletes a comment with its replies and their marks in the text.
    pub fn delete_comment(&mut self, id: &str) -> bool {
        let Some(pid) = self.doc.comments.get(id).map(|c| c.para_id.clone()) else { return false };
        self.checkpoint(Kind::Other);
        let gone: Vec<String> = self
            .doc
            .comments
            .items
            .iter()
            .filter(|x| x.id == id || (pid.is_some() && x.parent == pid))
            .map(|x| x.id.clone())
            .collect();
        let c = Rc::make_mut(&mut self.doc.comments);
        c.items.retain(|x| !gone.contains(&x.id));
        c.dirty = true;
        let is_gone = |e: &Element| ["commentRangeStart", "commentRangeEnd", "commentReference"].iter().any(|l| is_marker(e, l).is_some_and(|i| gone.contains(&i)));
        for path in para_paths(&self.doc.body) {
            let has = para_at(&self.doc.body, &path).is_some_and(|p| {
                p.inlines.iter().any(|i| match i {
                    Inline::Raw(e) => is_gone(e),
                    Inline::Run(r) => r.items.iter().any(|it| matches!(it, Item::Raw(e) if is_gone(e))),
                    _ => false,
                })
            });
            if !has {
                continue;
            }
            if let Some(p) = para_mut(&mut self.doc.body, &path) {
                p.inlines.retain(|i| !matches!(i, Inline::Raw(e) if is_gone(e)));
                for i in p.inlines.iter_mut() {
                    if let Inline::Run(r) = i {
                        r.items.retain(|it| !matches!(it, Item::Raw(e) if is_gone(e)));
                    }
                }
                p.inlines.retain(|i| !matches!(i, Inline::Run(r) if r.items.is_empty()));
            }
        }
        self.changed();
        true
    }

    pub fn select_comment(&mut self, id: &str) -> bool {
        let Some(a) = self.comment_anchors().get(id).cloned() else { return false };
        self.set_selection(a.start, a.end);
        true
    }
}
