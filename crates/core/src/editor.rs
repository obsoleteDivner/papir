//! Editing session: document, layout, selection, undo.

use crate::docx::{self, Document};
use crate::edit::{self, dcont_mut, dpara, dpara_mut, dpaths, para_paths};
use crate::fonts::Fonts;
use crate::layout::{self, Cache, Env, Layout};
use crate::model::*;
use crate::props::RunProps;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub path: Vec<u32>,
    pub off: usize,
}

#[derive(Clone)]
struct Snap {
    body: Container,
    sect: Option<Rc<crate::xml::Element>>,
    comments: Rc<crate::comments::Comments>,
    stories: Rc<std::collections::HashMap<String, Story>>,
    anchor: Pos,
    focus: Pos,
    /// the document right after this step, kept when others' changes may
    /// come in: undo then reverts only this step (merged with theirs)
    after: Option<crate::collab::State>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    None,
    Typing,
    Other,
}

pub struct Editor {
    pub doc: Document,
    pub fonts: Rc<Fonts>,
    cache: RefCell<Cache>,
    layout: Option<Layout>,
    pub anchor: Pos,
    pub focus: Pos,
    goal_x: Option<f32>,
    pending: Option<RunProps>,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    last: Kind,
    /// bumped on every change, of the document or of how it is laid out
    pub version: u64,
    /// bumped only when the document itself changes: unsaved when it differs from the saved one
    pub edits: u64,
    /// show tracked changes
    pub markup: bool,
    /// header or footer being edited: (relationship id, page it was opened on,
    /// body selection to return to)
    pub story: Option<(String, usize, Pos, Pos)>,
    /// editing together with others
    pub collab: Option<crate::collab::Client>,
    /// edits are recorded as tracked changes, by `author` at `now`
    pub track: bool,
    pub author: String,
    pub now: String,
    track_id: u32,
}

/// Run property names the UI can toggle or set.
pub enum Prop<'a> {
    Bold,
    Italic,
    Underline,
    Strike,
    Super,
    Sub,
    Font(&'a str),
    Size(f32),
    Color(Option<&'a str>),
    Highlight(Option<&'a str>),
}

impl Editor {
    pub fn new(doc: Document, fonts: Rc<Fonts>) -> Editor {
        let first = para_paths(&doc.body).into_iter().next().unwrap_or_else(|| vec![0]);
        let p = Pos { path: first, off: 0 };
        Editor {
            doc,
            fonts,
            cache: RefCell::new(Cache::default()),
            layout: None,
            anchor: p.clone(),
            focus: p,
            goal_x: None,
            pending: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last: Kind::None,
            version: 0,
            edits: 0,
            markup: true,
            story: None,
            collab: None,
            track: false,
            author: "Користувач".into(),
            now: "2026-01-01T00:00:00Z".into(),
            track_id: 0,
        }
        .with_settings()
    }

    fn with_settings(mut self) -> Editor {
        self.track = self.doc.settings.track;
        self
    }

    /// Shows or hides comments (the shading of commented text).
    pub fn set_comments(&mut self, show: bool) {
        let mut c = self.cache.borrow_mut();
        if c.hide_comments != !show {
            c.hide_comments = !show;
            c.clear();
        }
        drop(c);
        self.layout_reset();
    }

    /// An id for a new tracked change (apart from other people's when editing together).
    fn next_track_id(&mut self) -> u32 {
        self.track_id += 1;
        2_000_000 + self.doc.pkg.ns.unwrap_or(0) * 100_000 + self.track_id
    }

    /// Paragraphs of what is being edited: the body, or a header/footer.
    pub fn scope_paths(&self) -> Vec<Vec<u32>> {
        let story = self.story.as_ref().and_then(|(rid, ..)| edit::story_index(&self.doc, rid));
        dpaths(&self.doc, story)
    }

    pub fn layout(&mut self) -> &Layout {
        if self.layout.is_none() {
            let story = self.story.as_ref().map(|(rid, page, ..)| (rid.clone(), *page));
            let env = Env { doc: &self.doc, fonts: &self.fonts, cache: &self.cache, markup: self.markup, story };
            self.layout = Some(layout::layout_document(&env));
        }
        self.layout.as_ref().unwrap()
    }

    /// Registers a font after the document was opened.
    pub fn add_font(&mut self, family: &str, bold: bool, italic: bool, data: Vec<u8>) -> Result<u16, String> {
        let fonts = Rc::get_mut(&mut self.fonts).ok_or("fonts are shared")?;
        let id = fonts.add(family, bold, italic, data)?;
        // measurements change for text in this family
        self.cache.borrow_mut().clear();
        self.layout = None;
        self.version += 1;
        Ok(id)
    }

    /// Shows tracked changes or the document as if they were accepted.
    pub fn set_markup(&mut self, on: bool) {
        self.markup = on;
        self.cache.borrow_mut().clear();
        self.layout = None;
        self.version += 1;
    }

    pub fn into_fonts(self) -> Option<Fonts> {
        Rc::try_unwrap(self.fonts).ok()
    }

    /// The document as PDF, laid out exactly as on screen.
    pub fn export_pdf(&mut self) -> Vec<u8> {
        let fonts = self.fonts.clone();
        self.layout();
        crate::pdf::write(&self.doc, self.layout.as_ref().unwrap(), &fonts)
    }

    pub fn save(&self) -> Result<Vec<u8>, docx::Error> {
        docx::save(&self.doc)
    }

    /// Layout again (scope changed, nothing in the document did).
    pub(crate) fn layout_reset(&mut self) {
        self.layout = None;
        self.version += 1;
    }

    pub(crate) fn changed(&mut self) {
        self.layout = None;
        self.version += 1;
        self.edits += 1;
    }

    // ---------- selection ----------

    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.focus {
            (self.anchor.clone(), self.focus.clone())
        } else {
            (self.focus.clone(), self.anchor.clone())
        }
    }

    pub fn collapsed(&self) -> bool {
        self.anchor == self.focus
    }

    fn para_len(&self, path: &[u32]) -> usize {
        dpara(&self.doc, path).map(|p| p.len()).unwrap_or(0)
    }

    /// `p` moved onto a valid position of the document as it is now.
    pub fn clamp_pos(&self, p: Pos) -> Pos {
        self.clamp(p)
    }

    pub(crate) fn clamp(&self, p: Pos) -> Pos {
        if dpara(&self.doc, &p.path).is_some() {
            let len = self.para_len(&p.path);
            return Pos { off: p.off.min(len), path: p.path };
        }
        // nearest existing paragraph before it
        let paths = self.scope_paths();
        let path = paths.iter().rev().find(|x| **x <= p.path).or(paths.first()).cloned().unwrap_or_default();
        let off = self.para_len(&path);
        Pos { path, off }
    }

    pub fn set_selection(&mut self, anchor: Pos, focus: Pos) {
        self.anchor = self.clamp(anchor);
        self.focus = self.clamp(focus);
        self.goal_x = None;
        self.pending = None;
        self.last = Kind::None;
    }

    pub fn select_all(&mut self) {
        let paths = self.scope_paths();
        if let (Some(a), Some(b)) = (paths.first(), paths.last()) {
            let end = self.para_len(b);
            self.set_selection(Pos { path: a.clone(), off: 0 }, Pos { path: b.clone(), off: end });
        }
    }

    pub fn select_word(&mut self) {
        if let Some(p) = dpara(&self.doc, &self.focus.path) {
            let (a, b) = p.word_at(self.focus.off);
            let path = self.focus.path.clone();
            self.set_selection(Pos { path: path.clone(), off: a }, Pos { path, off: b });
        }
    }

    pub fn select_para(&mut self) {
        let path = self.focus.path.clone();
        let len = self.para_len(&path);
        self.set_selection(Pos { path: path.clone(), off: 0 }, Pos { path, off: len });
    }

    /// Moves the focus; `extend` keeps the anchor.
    /// unit: char, word, line, para, doc, vline (up/down), page handled by host.
    pub fn go(&mut self, unit: &str, dir: i32, extend: bool) {
        let f = self.focus.clone();
        if !extend && !self.collapsed() && (unit == "char") {
            let (a, b) = self.ordered();
            let p = if dir < 0 { a } else { b };
            self.set_selection(p.clone(), p);
            return;
        }
        let paths = self.scope_paths();
        let idx = paths.iter().position(|p| *p == f.path).unwrap_or(0);
        let len = self.para_len(&f.path);
        let mut goal = None;
        let np = match unit {
            "char" | "word" => {
                if dir < 0 {
                    if f.off > 0 {
                        let off = if unit == "word" { dpara(&self.doc, &f.path).map(|p| p.word_step(f.off, -1)).unwrap_or(0) } else { f.off - 1 };
                        Pos { off, ..f.clone() }
                    } else if idx > 0 {
                        let path = paths[idx - 1].clone();
                        let off = self.para_len(&path);
                        Pos { path, off }
                    } else {
                        f.clone()
                    }
                } else if f.off < len {
                    let off = if unit == "word" { dpara(&self.doc, &f.path).map(|p| p.word_step(f.off, 1)).unwrap_or(len) } else { f.off + 1 };
                    Pos { off, ..f.clone() }
                } else if idx + 1 < paths.len() {
                    Pos { path: paths[idx + 1].clone(), off: 0 }
                } else {
                    f.clone()
                }
            }
            "line" => {
                let b = self.layout().line_bounds(&f.path, f.off);
                match b {
                    Some((s, e)) => Pos { off: if dir < 0 { s } else { e }, ..f.clone() },
                    None => f.clone(),
                }
            }
            "para" => {
                if dir < 0 {
                    if f.off > 0 || idx == 0 {
                        Pos { off: 0, ..f.clone() }
                    } else {
                        Pos { path: paths[idx - 1].clone(), off: 0 }
                    }
                } else if idx + 1 < paths.len() {
                    Pos { path: paths[idx + 1].clone(), off: 0 }
                } else {
                    Pos { off: len, ..f.clone() }
                }
            }
            "doc" => {
                if dir < 0 {
                    Pos { path: paths.first().cloned().unwrap_or_default(), off: 0 }
                } else {
                    let path = paths.last().cloned().unwrap_or_default();
                    let off = self.para_len(&path);
                    Pos { path, off }
                }
            }
            "vline" => {
                let gx = match self.goal_x {
                    Some(x) => x,
                    None => self.layout().caret(&f.path, f.off).map(|c| c.x).unwrap_or(0.0),
                };
                goal = Some(gx);
                match self.layout().vertical(&f.path, f.off, gx, dir) {
                    Some((path, off)) => Pos { path: path.to_vec(), off },
                    None => {
                        if dir < 0 {
                            Pos { off: 0, ..f.clone() }
                        } else {
                            Pos { off: len, ..f.clone() }
                        }
                    }
                }
            }
            _ => f.clone(),
        };
        let anchor = if extend { self.anchor.clone() } else { np.clone() };
        self.set_selection(anchor, np);
        self.goal_x = goal;
    }

    /// First path element of the header/footer being edited.
    pub fn scope(&self) -> Option<u32> {
        self.story.as_ref().and_then(|(rid, ..)| edit::story_index(&self.doc, rid)).map(|i| edit::STORY | i)
    }

    pub fn click(&mut self, page: usize, x: f32, y: f32, extend: bool) {
        let scope = self.scope();
        if let Some((path, off)) = self.layout().hit_in(page, x, y, scope) {
            let p = Pos { path: path.to_vec(), off };
            let a = if extend { self.anchor.clone() } else { p.clone() };
            self.set_selection(a, p);
        }
    }

    // ---------- undo ----------

    pub(crate) fn checkpoint(&mut self, kind: Kind) {
        let coalesce = kind == Kind::Typing && self.last == Kind::Typing;
        if !coalesce {
            if self.collab.is_some() {
                let now = crate::collab::State::of(&self.doc);
                if let Some(top) = self.undo.last_mut() {
                    top.after.get_or_insert(now);
                }
            }
            self.undo.push(Snap { body: self.doc.body.clone(), sect: self.doc.sect.clone(), comments: self.doc.comments.clone(), stories: self.doc.stories.clone(), anchor: self.anchor.clone(), focus: self.focus.clone(), after: None });
            if self.undo.len() > 500 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = kind;
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        let back = self.step_back(s);
        self.redo.push(back);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        let back = self.step_back(s);
        self.undo.push(back);
        true
    }

    /// Goes back to the state of `s`; returns the step that comes back here.
    /// When others changed the document since, only this step is reverted:
    /// the way from `after` to `s` is merged into the document as it is now.
    fn step_back(&mut self, s: Snap) -> Snap {
        use crate::collab::{derive_ids, merge, Caret, Gen, Ids, State};
        let cur = State::of(&self.doc);
        let here = Snap { body: cur.body.clone(), sect: cur.sect.clone(), comments: cur.comments.clone(), stories: cur.stories.clone(), anchor: self.anchor.clone(), focus: self.focus.clone(), after: None };
        let merged = match &s.after {
            Some(after) if self.collab.is_some() && !after.same(&cur) => {
                let before = State { body: s.body.clone(), sect: s.sect.clone(), comments: s.comments.clone(), stories: s.stories.clone(), pkg: cur.pkg.clone(), rels: cur.rels.clone(), numbering: cur.numbering.clone() };
                let mut fresh = Gen::new("u.");
                let ai = Ids::initial(after);
                let bi = derive_ids(after, &ai, &before, &mut fresh);
                let ci = derive_ids(after, &ai, &cur, &mut fresh);
                let mut carets: Vec<Caret> = [&s.anchor, &s.focus]
                    .iter()
                    .filter(|p| !p.path.is_empty())
                    .map(|p| Caret { idx: p.path[0] as usize, sub: p.path[1..].to_vec(), off: p.off, to: None })
                    .collect();
                let part = self.doc.part.clone();
                let (m, _) = merge(after, &ai, &before, &bi, &cur, &ci, &part, &mut fresh, &mut carets);
                let mapped: Vec<Pos> = carets.iter().filter_map(|c| c.to.map(|(i, off)| Pos { path: std::iter::once(i as u32).chain(c.sub.iter().copied()).collect(), off })).collect();
                Some((m, mapped))
            }
            _ => None,
        };
        match merged {
            Some((m, mapped)) => {
                m.install(&mut self.doc);
                let a = mapped.first().cloned().unwrap_or(s.anchor);
                let f = mapped.get(1).cloned().unwrap_or_else(|| a.clone());
                self.anchor = self.clamp(a);
                self.focus = self.clamp(f);
            }
            None => {
                self.doc.body = s.body;
                self.doc.sect = s.sect;
                self.doc.comments = s.comments;
                self.doc.stories = s.stories;
                self.anchor = self.clamp(s.anchor);
                self.focus = self.clamp(s.focus);
            }
        }
        self.last = Kind::None;
        self.changed();
        let mut here = here;
        if self.collab.is_some() {
            here.after = Some(State::of(&self.doc));
        }
        here
    }

    // ---------- editing together ----------

    /// Starts editing together: the document as it is now is `version` of
    /// the room; `prefix` makes this participant's block ids unique, `ns`
    /// its numbers for lists, comments and parts.
    pub fn collab_start(&mut self, prefix: &str, ns: u32, version: u64) {
        self.doc.pkg.ns = Some(ns);
        let st = crate::collab::State::of(&self.doc);
        let ids = crate::collab::Ids::initial(&st);
        self.collab = Some(crate::collab::Client::new(st, ids, prefix, version));
    }

    /// My change to send (JSON), if any and nothing is on its way.
    pub fn collab_take(&mut self) -> Option<String> {
        let now = crate::collab::State::of(&self.doc);
        let c = self.collab.as_mut()?;
        c.track(now);
        c.take().map(|ch| ch.to_json().to_string())
    }

    pub fn collab_ack(&mut self, version: u64) {
        if let Some(c) = self.collab.as_mut() {
            c.ack(version);
        }
    }

    pub fn collab_nack(&mut self) {
        if let Some(c) = self.collab.as_mut() {
            c.nack();
        }
    }

    pub fn collab_version(&self) -> Option<u64> {
        self.collab.as_ref().map(|c| c.version)
    }

    /// Unconfirmed edits (shown as "saving").
    pub fn collab_pending(&mut self) -> bool {
        let now = crate::collab::State::of(&self.doc);
        match self.collab.as_mut() {
            Some(c) => {
                c.track(now);
                c.pending()
            }
            None => false,
        }
    }

    /// Someone else's change, in the server's order: my edits are rebased on it.
    pub fn collab_remote(&mut self, change: &str, version: u64) -> Result<(), String> {
        use crate::collab::{Caret, Change, State};
        let v: serde_json::Value = serde_json::from_str(change).map_err(|e| e.to_string())?;
        let ch = Change::from_json(&v)?;
        let now = State::of(&self.doc);
        let in_story = self.story.is_some();
        let mut carets: Vec<Caret> = if in_story {
            Vec::new()
        } else {
            [&self.anchor, &self.focus].iter().filter(|p| !p.path.is_empty()).map(|p| Caret { idx: p.path[0] as usize, sub: p.path[1..].to_vec(), off: p.off, to: None }).collect()
        };
        let part = self.doc.part.clone();
        let root = self.doc.root.clone();
        let Some(c) = self.collab.as_mut() else { return Err("not editing together".into()) };
        c.track(now);
        // the step before is complete: undo will revert just it
        if let Some(top) = self.undo.last_mut() {
            top.after.get_or_insert_with(|| c.local().clone());
        }
        let merged = c.remote(&ch, version, &part, &root, &mut carets)?;
        merged.install(&mut self.doc);
        if !in_story {
            let pos = |c: &Caret| c.to.map(|(i, off)| Pos { path: std::iter::once(i as u32).chain(c.sub.iter().copied()).collect(), off });
            let a = carets.first().and_then(pos);
            let f = carets.get(1).and_then(pos).or_else(|| a.clone());
            if let (Some(a), Some(f)) = (a, f) {
                self.anchor = self.clamp(a);
                self.focus = self.clamp(f);
            } else {
                self.anchor = self.clamp(self.anchor.clone());
                self.focus = self.clamp(self.focus.clone());
            }
        } else {
            self.anchor = self.clamp(self.anchor.clone());
            self.focus = self.clamp(self.focus.clone());
        }
        // typing after this goes into a new undo step
        self.last = Kind::None;
        self.changed();
        Ok(())
    }

    /// Drops the last checkpoint (an operation that changed nothing).
    pub(crate) fn undo_discard(&mut self) -> bool {
        self.undo.pop().is_some()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // ---------- text ----------

    /// Deletes the selection; returns false when it was collapsed.
    pub(crate) fn delete_selection(&mut self) -> bool {
        if self.collapsed() {
            return false;
        }
        if self.track {
            let (a, _) = self.ordered();
            let (author, date) = (self.author.clone(), self.now.clone());
            for (path, from, to) in self.ranges() {
                let id = self.next_track_id();
                if let Some(p) = dpara_mut(&mut self.doc, &path) {
                    p.track_delete(from, to, &author, &date, id);
                }
            }
            let a = self.clamp(a);
            self.anchor = a.clone();
            self.focus = a;
            return true;
        }
        let (a, b) = self.ordered();
        let paths = self.scope_paths();
        let doc = &mut self.doc;
        if a.path == b.path {
            if let Some(p) = dpara_mut(doc, &a.path) {
                p.delete(a.off, b.off);
            }
        } else if a.path[..a.path.len() - 1] == b.path[..b.path.len() - 1] {
            // same container: cut the ends, drop what is between, merge
            let ai = *a.path.last().unwrap() as usize;
            let bi = *b.path.last().unwrap() as usize;
            let tail = dpara(doc, &b.path).map(|p| {
                let mut t = p.clone();
                t.touch();
                t.delete(0, b.off);
                t
            });
            if let Some(c) = dcont_mut(doc, &a.path) {
                c.drain(ai + 1..=bi);
            }
            if let Some(p) = dpara_mut(doc, &a.path) {
                let len = p.len();
                p.delete(a.off, len);
                if let Some(t) = tail {
                    p.append(&t);
                }
            }
        } else {
            // across containers: clear text, keep structure
            for path in paths {
                if path < a.path || path > b.path {
                    continue;
                }
                if let Some(p) = dpara_mut(doc, &path) {
                    let len = p.len();
                    let from = if path == a.path { a.off } else { 0 };
                    let to = if path == b.path { b.off } else { len };
                    p.delete(from, to);
                }
            }
        }
        self.anchor = a.clone();
        self.focus = a;
        true
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if text.contains('\n') || text.contains('\r') {
            return self.paste_text(text);
        }
        // typing is undone word by word: a space starts a new step
        let kind = if self.collapsed() && !text.starts_with(' ') && text.chars().count() < 4 { Kind::Typing } else { Kind::Other };
        self.checkpoint(kind);
        self.delete_selection();
        let f = self.focus.clone();
        let pending = self.pending.clone();
        let n = text.chars().filter(|c| *c != '\r' && *c != '\n').count();
        let tid = if self.track { self.next_track_id() } else { 0 };
        let (track, author, date) = (self.track, self.author.clone(), self.now.clone());
        if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
            p.insert_text(f.off, text, pending.as_ref());
            if track {
                p.track_insert(f.off, f.off + n, &author, &date, tid);
            }
        }
        let np = Pos { off: f.off + n, ..f };
        self.anchor = np.clone();
        self.focus = np;
        self.pending = None;
        self.goal_x = None;
        self.changed();
        if text.ends_with(' ') {
            self.last = Kind::Other;
        } else if kind == Kind::Other && self.collapsed() {
            self.last = Kind::Typing;
        }
    }

    /// Enter: splits the paragraph at the caret.
    pub fn split(&mut self) {
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let f = self.focus.clone();
        let styles = self.doc.styles.clone();
        let numbering_off = dpara(&self.doc, &f.path).is_some_and(|p| p.len() == 0 && self.has_numbering(p));
        if numbering_off {
            // Enter on an empty list item ends the list
            if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
                remove_numbering(p, &styles);
            }
            self.changed();
            return;
        }
        let Some(p) = dpara_mut(&mut self.doc, &f.path) else { return };
        let at_end = f.off >= p.len();
        let mut tail = p.split_off(f.off);
        if at_end {
            // the style's "next" style, without direct character formatting
            if let Some(next) = p.props.style.as_deref().and_then(|s| styles.map.get(s)).and_then(|s| s.next.clone()) {
                if Some(&next) != p.props.style.as_ref() {
                    tail.props.style = Some(next);
                    tail.props_dirty = true;
                }
            }
        }
        let idx = *f.path.last().unwrap() as usize;
        if let Some(c) = dcont_mut(&mut self.doc, &f.path) {
            c.insert(idx + 1, Rc::new(Block::Para(tail)));
        }
        let mut np = f.path.clone();
        *np.last_mut().unwrap() += 1;
        let p = Pos { path: np, off: 0 };
        self.anchor = p.clone();
        self.focus = p;
        self.changed();
    }

    pub(crate) fn has_numbering(&self, p: &Para) -> bool {
        self.doc.styles.para(&p.props, None).num_id.is_some_and(|n| n > 0)
    }

    /// Shift+Enter
    pub fn line_break(&mut self) {
        self.insert_text("\u{b}");
    }

    pub fn page_break(&mut self) {
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let f = self.focus.clone();
        if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
            p.split_at(f.off);
            let mut r = Run::new(p.props_at(f.off));
            r.items.push(Item::Break(BreakKind::Page));
            let mut pos = 0;
            let mut at = p.inlines.len();
            for (i, x) in p.inlines.iter().enumerate() {
                if pos >= f.off && x.len() > 0 {
                    at = i;
                    break;
                }
                pos += x.len();
            }
            p.inlines.insert(at, Inline::Run(r));
        }
        let np = Pos { off: f.off + 1, ..f };
        self.anchor = np.clone();
        self.focus = np;
        self.changed();
    }

    pub fn backspace(&mut self, word: bool) {
        if !self.collapsed() {
            self.checkpoint(Kind::Other);
            self.delete_selection();
            self.changed();
            return;
        }
        let f = self.focus.clone();
        if f.off > 0 {
            self.checkpoint(Kind::Other);
            let from = if word { dpara(&self.doc, &f.path).map(|p| p.word_step(f.off, -1)).unwrap_or(f.off - 1) } else { f.off - 1 };
            let tid = if self.track { self.next_track_id() } else { 0 };
            let (track, author, date) = (self.track, self.author.clone(), self.now.clone());
            if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
                if track {
                    p.track_delete(from, f.off, &author, &date, tid);
                } else {
                    p.delete(from, f.off);
                }
            }
            let np = Pos { off: from, ..f };
            self.anchor = np.clone();
            self.focus = np;
            self.changed();
            return;
        }
        if self.track {
            // paragraph marks are not tracked: step into the previous paragraph
            let paths = self.scope_paths();
            if let Some(i) = paths.iter().position(|p| *p == f.path).filter(|i| *i > 0) {
                let prev = paths[i - 1].clone();
                let np = Pos { off: self.para_len(&prev), path: prev };
                self.anchor = np.clone();
                self.focus = np;
            }
            return;
        }
        let styles = self.doc.styles.clone();
        if dpara(&self.doc, &f.path).is_some_and(|p| self.has_numbering(p)) {
            self.checkpoint(Kind::Other);
            if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
                remove_numbering(p, &styles);
            }
            self.changed();
            return;
        }
        // merge into the previous paragraph of the same container
        let idx = *f.path.last().unwrap();
        if idx == 0 {
            return self.go("char", -1, false);
        }
        let mut prev = f.path.clone();
        *prev.last_mut().unwrap() -= 1;
        let Some(prev_len) = dpara(&self.doc, &prev).map(|p| p.len()) else {
            return self.go("char", -1, false);
        };
        self.checkpoint(Kind::Other);
        let cur = dpara(&self.doc, &f.path).cloned();
        if let (Some(cur), Some(p)) = (cur, dpara_mut(&mut self.doc, &prev)) {
            p.append(&cur);
        }
        if let Some(c) = dcont_mut(&mut self.doc, &f.path) {
            c.remove(idx as usize);
        }
        let np = Pos { path: prev, off: prev_len };
        self.anchor = np.clone();
        self.focus = np;
        self.changed();
    }

    pub fn delete(&mut self, word: bool) {
        if !self.collapsed() {
            self.checkpoint(Kind::Other);
            self.delete_selection();
            self.changed();
            return;
        }
        let f = self.focus.clone();
        let len = self.para_len(&f.path);
        if f.off < len {
            self.checkpoint(Kind::Other);
            let to = if word { dpara(&self.doc, &f.path).map(|p| p.word_step(f.off, 1)).unwrap_or(f.off + 1) } else { f.off + 1 };
            let tid = if self.track { self.next_track_id() } else { 0 };
            let (track, author, date) = (self.track, self.author.clone(), self.now.clone());
            if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
                if track {
                    p.track_delete(f.off, to, &author, &date, tid);
                } else {
                    p.delete(f.off, to);
                }
            }
            self.changed();
            return;
        }
        if self.track {
            return;
        }
        let mut next = f.path.clone();
        *next.last_mut().unwrap() += 1;
        let Some(np) = dpara(&self.doc, &next).cloned() else { return };
        self.checkpoint(Kind::Other);
        if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
            p.append(&np);
        }
        if let Some(c) = dcont_mut(&mut self.doc, &next) {
            c.remove(*next.last().unwrap() as usize);
        }
        self.changed();
    }

    // ---------- formatting ----------

    /// Paragraphs touched by the selection with their ranges.
    pub(crate) fn ranges(&self) -> Vec<(Vec<u32>, usize, usize)> {
        let (a, b) = self.ordered();
        self.scope_paths()
            .into_iter()
            .filter(|p| *p >= a.path && *p <= b.path)
            .map(|p| {
                let len = self.para_len(&p);
                let from = if p == a.path { a.off } else { 0 };
                let to = if p == b.path { b.off } else { len };
                (p, from, to)
            })
            .collect()
    }

    /// Effective properties at the caret or over the selection's start.
    pub fn current(&self) -> (RunProps, crate::props::ParaProps) {
        let (a, _) = self.ordered();
        let Some(p) = dpara(&self.doc, &a.path) else { return Default::default() };
        let direct = if self.collapsed() {
            self.pending.clone().unwrap_or_else(|| p.props_at(a.off))
        } else {
            p.props_in(a.off, a.off + 1).into_iter().next().unwrap_or_else(|| p.props_at(a.off + 1))
        };
        let pp = crate::layout::para::effective(&self.doc, p, None);
        let rp = self.doc.styles.run(pp.style.as_deref(), None, &direct);
        (rp, pp)
    }

    pub fn set_prop(&mut self, prop: Prop) {
        let (cur, _) = self.current();
        let on = |v: Option<bool>| !v.unwrap_or(false);
        let f: Box<dyn Fn(&mut RunProps)> = match prop {
            Prop::Bold => {
                let v = on(cur.b);
                Box::new(move |r| r.b = Some(v))
            }
            Prop::Italic => {
                let v = on(cur.i);
                Box::new(move |r| r.i = Some(v))
            }
            Prop::Underline => {
                let v = cur.u.as_deref().is_none_or(|u| u == "none");
                Box::new(move |r| r.u = Some(if v { "single".into() } else { "none".into() }))
            }
            Prop::Strike => {
                let v = on(cur.strike);
                Box::new(move |r| r.strike = Some(v))
            }
            Prop::Super | Prop::Sub => {
                let want = if matches!(prop, Prop::Super) { "superscript" } else { "subscript" };
                let v = if cur.valign.as_deref() == Some(want) { "baseline" } else { want }.to_string();
                Box::new(move |r| r.valign = Some(v.clone()))
            }
            Prop::Font(name) => {
                let n = name.to_string();
                Box::new(move |r| {
                    r.font_ascii = Some(n.clone());
                    r.font_hansi = Some(n.clone());
                    r.font_cs = Some(n.clone());
                    r.theme_ascii = None;
                    r.theme_hansi = None;
                })
            }
            Prop::Size(pt) => {
                let hp = (pt * 2.0).round().max(2.0) as u32;
                Box::new(move |r| r.sz = Some(hp))
            }
            Prop::Color(c) => {
                let c = c.map(str::to_string).unwrap_or_else(|| "auto".into());
                Box::new(move |r| r.color = Some(c.clone()))
            }
            Prop::Highlight(h) => {
                let h = h.map(str::to_string).unwrap_or_else(|| "none".into());
                Box::new(move |r| r.highlight = Some(h.clone()))
            }
        };
        if self.collapsed() {
            let p = dpara(&self.doc, &self.focus.path).map(|p| p.props_at(self.focus.off)).unwrap_or_default();
            let mut pend = self.pending.clone().unwrap_or(p);
            f(&mut pend);
            self.pending = Some(pend);
            return;
        }
        self.checkpoint(Kind::Other);
        for (path, from, to) in self.ranges() {
            if let Some(p) = dpara_mut(&mut self.doc, &path) {
                p.format(from, to, &*f);
            }
        }
        self.changed();
    }

    /// Character formatting of the first selected character, or at the caret
    /// (the sample for "Формат за зразком").
    pub fn format_sample(&self) -> RunProps {
        let (a, b) = self.ordered();
        let off = if a == b { a.off } else { a.off + 1 };
        dpara(&self.doc, &a.path).map(|p| p.props_at(off)).unwrap_or_default()
    }

    /// Gives the selection the character formatting `props` (format painter).
    pub fn paint_format(&mut self, props: &RunProps) {
        if self.collapsed() {
            self.pending = Some(props.clone());
            return;
        }
        self.checkpoint(Kind::Other);
        let src = props.clone();
        for (path, from, to) in self.ranges() {
            if let Some(p) = dpara_mut(&mut self.doc, &path) {
                p.format(from, to, &|r: &mut RunProps| *r = src.clone());
            }
        }
        self.changed();
    }

    /// Removes direct character formatting (Ctrl+Space).
    pub fn clear_format(&mut self) {
        if self.collapsed() {
            self.pending = Some(RunProps::default());
            return;
        }
        self.checkpoint(Kind::Other);
        for (path, from, to) in self.ranges() {
            if let Some(p) = dpara_mut(&mut self.doc, &path) {
                p.format(from, to, &|r: &mut RunProps| *r = RunProps::default());
            }
        }
        self.changed();
    }

    /// Paragraph indents from the ruler, twips.
    pub fn set_indents(&mut self, left: i32, first: i32, right: i32) {
        self.set_para(&move |p| {
            p.ind_left = Some(left);
            p.ind_first = Some(first);
            p.ind_right = Some(right);
        });
    }

    /// Target of the hyperlink at a position (external links only).
    pub fn link_at(&self, path: &[u32], off: usize) -> Option<String> {
        let p = dpara(&self.doc, path)?;
        let mut pos = 0;
        let mut open: Vec<&Wrapper> = Vec::new();
        for i in &p.inlines {
            match i {
                Inline::Open(w) => open.push(w),
                Inline::Close(_) => {
                    open.pop();
                }
                Inline::Run(r) => {
                    let len = r.len();
                    if off >= pos && off < pos + len.max(1) && len > 0 {
                        let w = open.iter().rev().find(|w| w.kind == WrapKind::Hyperlink)?;
                        let i = w.open.find("r:id=\"")? + 6;
                        let id = &w.open[i..i + w.open[i..].find('"')?];
                        return self.doc.rels.get(id).filter(|r| r.external).map(|r| r.target.clone());
                    }
                    pos += len;
                }
                Inline::Raw(_) => {}
            }
        }
        None
    }

    pub fn set_para(&mut self, f: &dyn Fn(&mut crate::props::ParaProps)) {
        self.checkpoint(Kind::Other);
        for (path, _, _) in self.ranges() {
            if let Some(p) = dpara_mut(&mut self.doc, &path) {
                f(&mut p.props);
                p.props_dirty = true;
            }
        }
        self.changed();
    }

    pub fn set_style(&mut self, id: &str) {
        let is_char = self.doc.styles.map.get(id).is_some_and(|s| s.kind == "character");
        if is_char {
            if self.collapsed() {
                return;
            }
            self.checkpoint(Kind::Other);
            let id = id.to_string();
            for (path, from, to) in self.ranges() {
                if let Some(p) = dpara_mut(&mut self.doc, &path) {
                    p.format(from, to, &|r: &mut RunProps| r.style = Some(id.clone()));
                }
            }
            self.changed();
        } else {
            let id = id.to_string();
            self.set_para(&move |p| p.style = Some(id.clone()));
        }
    }

    // ---------- clipboard ----------

    pub fn selected_text(&self) -> String {
        let mut out = Vec::new();
        for (path, from, to) in self.ranges() {
            if let Some(p) = dpara(&self.doc, &path) {
                let s: String = p.plain_text().chars().skip(from).take(to - from).collect();
                out.push(s.replace('\u{FFFC}', ""));
            }
        }
        out.join("\n")
    }

    /// Selected paragraphs as WordprocessingML, for pasting with formatting.
    pub fn selected_xml(&self) -> String {
        let mut s = String::from("<papir xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">");
        for (path, from, to) in self.ranges() {
            if let Some(p) = dpara(&self.doc, &path) {
                let mut q = p.clone();
                q.touch();
                let len = q.len();
                q.delete(to, len);
                q.delete(0, from);
                // drop pictures and markup that refer to this package
                for inl in q.inlines.iter_mut() {
                    if let Inline::Run(r) = inl {
                        r.items.retain(|it| matches!(it, Item::Text(_) | Item::Tab | Item::Break(_)));
                    }
                }
                q.inlines.retain(|i| matches!(i, Inline::Run(r) if !r.items.is_empty()));
                q.ppr = q.ppr.as_ref().map(|e| {
                    let mut e = (**e).clone();
                    e.children.retain(|n| !matches!(n, crate::xml::Node::Elem(x) if x.is_w("sectPr")));
                    Rc::new(e)
                });
                crate::docx::write_para(&mut s, &q);
            }
        }
        s.push_str("</papir>");
        s
    }

    pub fn cut(&mut self) {
        if self.collapsed() {
            return;
        }
        self.checkpoint(Kind::Other);
        self.delete_selection();
        self.changed();
    }

    pub fn paste_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = text.split('\n').collect();
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let f = self.focus.clone();
        if lines.len() == 1 {
            if let Some(p) = dpara_mut(&mut self.doc, &f.path) {
                p.insert_text(f.off, lines[0], None);
            }
            let np = Pos { off: f.off + lines[0].chars().count(), ..f };
            self.anchor = np.clone();
            self.focus = np;
            self.changed();
            return;
        }
        let Some(p) = dpara_mut(&mut self.doc, &f.path) else { return };
        let mut tail = p.split_off(f.off);
        p.insert_text(f.off, lines[0], None);
        let template = tail.clone();
        let mut new_paras = Vec::new();
        for l in &lines[1..lines.len() - 1] {
            let mut q = template.clone();
            q.touch();
            q.inlines.clear();
            q.insert_text(0, l, None);
            new_paras.push(q);
        }
        let last = lines[lines.len() - 1];
        tail.insert_text(0, last, None);
        new_paras.push(tail);
        let idx = *f.path.last().unwrap() as usize;
        let count = new_paras.len();
        if let Some(c) = dcont_mut(&mut self.doc, &f.path) {
            for (k, q) in new_paras.into_iter().enumerate() {
                c.insert(idx + 1 + k, Rc::new(Block::Para(q)));
            }
        }
        let mut np = f.path.clone();
        *np.last_mut().unwrap() += count as u32;
        let p = Pos { path: np, off: last.chars().count() };
        self.anchor = p.clone();
        self.focus = p;
        self.changed();
    }

    /// Pastes paragraphs produced by `selected_xml`.
    pub fn paste_xml(&mut self, xml: &str) -> bool {
        let Ok(root) = crate::xml::parse(xml) else { return false };
        let paras: Vec<Para> = root
            .elems()
            .filter(|e| e.is_w("p"))
            .map(|e| {
                let mut p = crate::docx::parse_para(e, &self.doc);
                // copies must not repeat the source's w14:paraId
                p.open = "<w:p>".into();
                p.touch();
                p
            })
            .collect();
        if paras.is_empty() {
            return false;
        }
        self.checkpoint(Kind::Other);
        self.delete_selection();
        let f = self.focus.clone();
        let Some(p) = dpara_mut(&mut self.doc, &f.path) else { return false };
        if paras.len() == 1 {
            let src = &paras[0];
            p.split_at(f.off);
            let mut tail = p.split_off(f.off);
            let n = src.len();
            let mut add = src.inlines.clone();
            for i in add.iter_mut() {
                if let Inline::Run(r) = i {
                    r.dirty = true;
                }
            }
            p.inlines.extend(add);
            tail.props = p.props.clone();
            p.append(&tail);
            let np = Pos { off: f.off + n, ..f };
            self.anchor = np.clone();
            self.focus = np;
            self.changed();
            return true;
        }
        let mut tail = p.split_off(f.off);
        p.inlines.extend(paras[0].inlines.iter().cloned());
        // the last pasted paragraph joins the tail, which keeps its own properties
        let last = paras.last().unwrap();
        let last_len = last.len();
        let mut joined = last.inlines.clone();
        joined.extend(tail.inlines.drain(..));
        tail.inlines = joined;
        tail.touch();
        let mut new_paras: Vec<Para> = paras[1..paras.len() - 1].iter().cloned().collect();
        new_paras.push(tail);
        let idx = *f.path.last().unwrap() as usize;
        let count = new_paras.len();
        if let Some(c) = dcont_mut(&mut self.doc, &f.path) {
            for (k, q) in new_paras.into_iter().enumerate() {
                c.insert(idx + 1 + k, Rc::new(Block::Para(q)));
            }
        }
        let mut np = f.path.clone();
        *np.last_mut().unwrap() += count as u32;
        let p = Pos { path: np, off: last_len };
        self.anchor = p.clone();
        self.focus = p;
        self.changed();
        true
    }

    /// Plain-text search from the caret; selects the match.
    pub fn find(&mut self, needle: &str, case: bool, back: bool) -> bool {
        if needle.is_empty() {
            return false;
        }
        let norm = |s: &str| if case { s.to_string() } else { s.to_lowercase() };
        let n = norm(needle);
        let nlen = needle.chars().count();
        let paths = self.scope_paths();
        let (a, b) = self.ordered();
        let start_idx = paths.iter().position(|p| *p == if back { a.path.clone() } else { b.path.clone() }).unwrap_or(0);
        let total = paths.len();
        for k in 0..=total {
            let i = if back { (start_idx + total * 2 - k) % total } else { (start_idx + k) % total };
            let Some(p) = dpara(&self.doc, &paths[i]) else { continue };
            let text: Vec<char> = norm(&p.plain_text()).chars().collect();
            let hay: String = text.iter().collect();
            let mut hits: Vec<usize> = Vec::new();
            let mut from = 0;
            while let Some(bi) = hay[from..].find(&n) {
                let ci = hay[..from + bi].chars().count();
                hits.push(ci);
                from += bi + n.len().max(1);
            }
            let pick = if back {
                hits.into_iter().rev().find(|&h| !(k == 0) || h + nlen <= a.off)
            } else {
                hits.into_iter().find(|&h| !(k == 0) || h >= b.off)
            };
            if let Some(h) = pick {
                let path = paths[i].clone();
                self.set_selection(Pos { path: path.clone(), off: h }, Pos { path, off: h + nlen });
                return true;
            }
        }
        false
    }

    /// Replaces the selection if it matches, then finds the next match.
    pub fn replace(&mut self, needle: &str, with: &str, case: bool) -> bool {
        let sel = self.selected_text();
        let eq = if case { sel == needle } else { sel.to_lowercase() == needle.to_lowercase() };
        if eq && !self.collapsed() {
            self.insert_text(with);
        }
        self.find(needle, case, false)
    }

    pub fn replace_all(&mut self, needle: &str, with: &str, case: bool) -> usize {
        if needle.is_empty() {
            return 0;
        }
        let norm = |s: &str| if case { s.to_string() } else { s.to_lowercase() };
        let n = norm(needle);
        let nlen = needle.chars().count();
        self.checkpoint(Kind::Other);
        let mut count = 0;
        for path in self.scope_paths() {
            let Some(p) = dpara(&self.doc, &path) else { continue };
            let hay = norm(&p.plain_text());
            let mut hits = Vec::new();
            let mut from = 0;
            while let Some(bi) = hay[from..].find(&n) {
                hits.push(hay[..from + bi].chars().count());
                from += bi + n.len();
            }
            if hits.is_empty() {
                continue;
            }
            let p = dpara_mut(&mut self.doc, &path).unwrap();
            for &h in hits.iter().rev() {
                // the replacement takes the formatting of the first replaced char
                let props = p.props_at(h + 1);
                p.delete(h, h + nlen);
                p.insert_text(h, with, Some(&props));
            }
            count += hits.len();
        }
        if count == 0 {
            self.undo.pop();
            return 0;
        }
        let (a, b) = (self.anchor.clone(), self.focus.clone());
        self.anchor = self.clamp(a);
        self.focus = self.clamp(b);
        self.changed();
        count
    }

    pub fn word_count(&self) -> usize {
        self.scope_paths()
            .iter()
            .filter_map(|p| dpara(&self.doc, p))
            .map(|p| p.plain_text().split_whitespace().count())
            .sum()
    }
}

pub(crate) fn remove_numbering(p: &mut Para, styles: &crate::styles::Styles) {
    let from_style = styles.para(&crate::props::ParaProps { style: p.props.style.clone(), ..Default::default() }, None).num_id.is_some_and(|n| n > 0);
    if from_style {
        p.props.num_id = Some(0);
        p.props.ilvl = Some(0);
    } else {
        p.props.num_id = None;
        p.props.ilvl = None;
    }
    p.props_dirty = true;
}

pub use edit::para_paths as paragraphs;
