//! Browser binding: one `Session` per open document.

use papir_core::editor::{Editor, Pos, Prop};
use papir_core::fonts::Fonts;
use papir_core::layout::paint;
use papir_core::Document;
use serde_json::json;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Session {
    fonts: Option<Fonts>,
    ed: Option<Editor>,
    /// character formatting held by the format painter
    painter: Option<papir_core::props::RunProps>,
}

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn pos_json(p: &Pos) -> serde_json::Value {
    json!({"path": p.path, "off": p.off})
}

#[wasm_bindgen]
impl Session {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Session {
        Session { fonts: Some(Fonts::new()), ed: None, painter: None }
    }

    /// Registers a font face (before or after `open`).
    pub fn add_font(&mut self, family: &str, bold: bool, italic: bool, data: Vec<u8>) -> Result<u16, JsError> {
        match (&mut self.ed, &mut self.fonts) {
            (Some(ed), _) => ed.add_font(family, bold, italic, data).map_err(err),
            (None, Some(f)) => f.add(family, bold, italic, data).map_err(err),
            _ => Err(err("no font registry")),
        }
    }

    pub fn has_family(&self, family: &str) -> bool {
        match (&self.ed, &self.fonts) {
            (Some(ed), _) => ed.fonts.has_family(family),
            (None, Some(f)) => f.has_family(family),
            _ => false,
        }
    }

    /// Opens a document. Layout waits until the first page query, so the
    /// fonts it needs (`needed_families`) can be added in between.
    pub fn open(&mut self, data: &[u8]) -> Result<(), JsError> {
        let doc = Document::open(data).map_err(|e| JsError::new(e.code()))?;
        let fonts = match self.ed.take() {
            Some(ed) => ed.into_fonts().unwrap_or_default(),
            None => self.fonts.take().unwrap_or_default(),
        };
        self.ed = Some(Editor::new(doc, Rc::new(fonts)));
        Ok(())
    }

    /// Families (from `available`, a JSON array) the open document needs.
    pub fn needed_families(&mut self, available: &str) -> String {
        let avail: Vec<String> = serde_json::from_str(available).unwrap_or_default();
        let v = papir_core::fonts::needed_families(&self.ed().doc, &avail);
        serde_json::to_string(&v).unwrap_or_else(|_| "[]".into())
    }

    /// Family that will render `name`, among `available` (JSON array).
    pub fn substitute(&self, name: &str, available: &str) -> Option<String> {
        let avail: Vec<String> = serde_json::from_str(available).unwrap_or_default();
        papir_core::fonts::substitute(name, &avail).map(str::to_string)
    }

    fn ed(&mut self) -> &mut Editor {
        self.ed.as_mut().expect("no document")
    }

    pub fn page_count(&mut self) -> usize {
        self.ed().layout().pages.len()
    }

    /// [w, h, left, right, top, bottom] in points
    pub fn page_size(&mut self, i: usize) -> Vec<f32> {
        let p = &self.ed().layout().pages[i];
        vec![p.w, p.h, p.left, p.right, p.top, p.bottom]
    }

    /// All page sizes as a flat [w0, h0, w1, h1, ...].
    pub fn page_sizes(&mut self) -> Vec<f32> {
        self.ed().layout().pages.iter().flat_map(|p| [p.w, p.h]).collect()
    }

    pub fn page(&mut self, i: usize) -> String {
        let ed = self.ed.as_mut().unwrap();
        let fonts = ed.fonts.clone();
        let l = ed.layout();
        match l.pages.get(i) {
            Some(p) => paint::display_list(p, &fonts),
            None => "{\"w\":0,\"h\":0,\"items\":[]}".into(),
        }
    }

    pub fn media(&mut self, src: &str) -> Option<Vec<u8>> {
        self.ed().doc.media(src).map(|d| d.to_vec())
    }

    pub fn state(&mut self) -> String {
        let ed = self.ed.as_mut().unwrap();
        let (a, b) = ed.ordered();
        let collapsed = ed.collapsed();
        let (rp, pp) = ed.current();
        let (anchor, focus) = (ed.anchor.clone(), ed.focus.clone());
        let undo = ed.can_undo();
        let in_table = ed.in_table();
        let change = ed.change_at_caret().map(|c| json!({"kind": c.kind, "author": c.author, "date": c.date}));
        let changes = ed.count_changes();
        let markup = ed.markup;
        let track = ed.track;
        // the comment whose range holds the caret
        let comment = {
            let f = &focus;
            ed.comment_anchors().into_iter().filter(|(_, a)| a.start <= *f && *f <= a.end).map(|(id, _)| id).max_by_key(|id| id.parse::<u32>().unwrap_or(0))
        };
        let comment_count = ed.doc.comments.items.len();
        let story = ed.story.as_ref().map(|(rid, page, ..)| {
            let kind = ed.doc.rels.get(rid).map(|r| r.kind.clone()).unwrap_or_default();
            json!({"kind": kind, "page": page})
        });
        let style_name = pp.style.as_deref().or(ed.doc.styles.default_para.as_deref()).and_then(|id| ed.doc.styles.map.get(id)).map(|s| s.name.clone());
        let list_fmt = pp.num_id.filter(|n| *n > 0).and_then(|n| ed.doc.numbering.level(n, pp.ilvl.unwrap_or(0))).map(|l| l.fmt.clone());
        let redo = ed.can_redo();
        let version = ed.version;
        let edits = ed.edits;
        let font = rp
            .font_hansi
            .clone()
            .or(rp.font_ascii.clone())
            .or_else(|| rp.theme_hansi.as_deref().and_then(|t| ed.doc.theme.resolve(t)).map(str::to_string))
            .unwrap_or_else(|| "Times New Roman".into());
        let l = ed.layout();
        let caret = l.caret(&focus.path, focus.off).map(|c| json!({"page": c.page, "x": c.x, "y": c.y, "h": c.h}));
        // a selection that is exactly one picture gets resize handles
        let image = if !collapsed && a.path == b.path && b.off == a.off + 1 {
            l.image_box(&a.path, a.off).map(|(p, x, y, w, h)| json!({"page": p, "x": x, "y": y, "w": w, "h": h, "path": a.path, "off": a.off}))
        } else {
            None
        };
        let sel: Vec<_> = if collapsed {
            Vec::new()
        } else {
            l.sel_rects((&a.path, a.off), (&b.path, b.off)).into_iter().map(|(p, x, y, w, h)| json!([p, x, y, w, h])).collect()
        };
        json!({
            "anchor": pos_json(&anchor),
            "focus": pos_json(&focus),
            "collapsed": collapsed,
            "caret": caret,
            "sel": sel,
            "track": track,
            "undo": undo,
            "redo": redo,
            "version": version,
            "edits": edits,
            "table": in_table,
            "image": image,
            "change": change,
            "changes": changes,
            "markup": markup,
            "comment": comment,
            "comments": comment_count,
            "story": story,
            "fmt": {
                "font": font,
                "theme": rp.theme_hansi,
                "size": rp.sz.map(|s| s as f32 / 2.0),
                "b": rp.b.unwrap_or(false),
                "i": rp.i.unwrap_or(false),
                "u": rp.u.as_deref().is_some_and(|u| u != "none"),
                "s": rp.strike.unwrap_or(false),
                "valign": rp.valign,
                "color": rp.color,
                "highlight": rp.highlight,
                "style": pp.style,
                "jc": pp.jc,
                "line": pp.line,
                "lineRule": pp.line_rule,
                "list": pp.num_id.filter(|n| *n > 0).and_then(|n| list_fmt.clone().map(|f| json!({"id": n, "bullet": f == "bullet"}))),
                "indLeft": pp.ind_left.unwrap_or(0),
                "indRight": pp.ind_right.unwrap_or(0),
                "indFirst": pp.ind_first.unwrap_or(0),
                "styleName": style_name,
            }
        })
        .to_string()
    }

    /// Theme font name resolved by the document.
    pub fn theme_font(&mut self, theme: &str) -> Option<String> {
        self.ed().doc.theme.resolve(theme).map(str::to_string)
    }

    pub fn click(&mut self, page: usize, x: f32, y: f32, extend: bool) {
        self.ed().click(page, x, y, extend);
    }

    pub fn set_selection(&mut self, anchor: JsValue, focus: JsValue) {
        let parse = |v: &JsValue| -> Option<Pos> {
            let s = js_sys_json(v)?;
            let j: serde_json::Value = serde_json::from_str(&s).ok()?;
            Some(Pos {
                path: j["path"].as_array()?.iter().filter_map(|x| x.as_u64().map(|v| v as u32)).collect(),
                off: j["off"].as_u64()? as usize,
            })
        };
        if let (Some(a), Some(f)) = (parse(&anchor), parse(&focus)) {
            self.ed().set_selection(a, f);
        }
    }

    /// Comments shown or not (off when the host turns review off).
    pub fn set_comments(&mut self, show: bool) {
        self.ed().set_comments(show);
    }

    // ---------- tracked changes ----------

    /// Record edits as tracked changes (insertions, deletions).
    pub fn set_track(&mut self, on: bool) {
        self.ed().track = on;
    }

    /// Who edits and when (ISO time), for tracked changes and comments.
    pub fn set_author(&mut self, name: &str, now: &str) {
        let ed = self.ed();
        ed.author = name.to_string();
        ed.now = now.to_string();
    }

    // ---------- editing together ----------

    /// Joins a room: this copy is `version`; `prefix` and `ns` keep my ids apart.
    pub fn collab_start(&mut self, prefix: &str, ns: u32, version: f64) {
        self.ed().collab_start(prefix, ns, version as u64);
    }

    /// My change to send (JSON), or nothing.
    pub fn collab_take(&mut self) -> Option<String> {
        self.ed().collab_take()
    }

    pub fn collab_ack(&mut self, version: f64) {
        self.ed().collab_ack(version as u64);
    }

    pub fn collab_nack(&mut self) {
        self.ed().collab_nack();
    }

    /// Someone else's change; mine are rebased on it.
    pub fn collab_remote(&mut self, change: &str, version: f64) -> Result<(), JsError> {
        self.ed().collab_remote(change, version as u64).map_err(|e| JsError::new(&e))
    }

    pub fn collab_version(&mut self) -> Option<f64> {
        self.ed().collab_version().map(|v| v as f64)
    }

    pub fn collab_pending(&mut self) -> bool {
        self.ed().collab_pending()
    }

    pub fn collab_waiting(&mut self) -> bool {
        self.ed().collab.as_ref().is_some_and(|c| c.waiting())
    }

    /// Caret and selection rectangles of another person's selection (positions as in `set_selection`).
    pub fn rects_for(&mut self, anchor: &str, focus: &str) -> String {
        let parse = |s: &str| -> Option<Pos> {
            let j: serde_json::Value = serde_json::from_str(s).ok()?;
            Some(Pos { path: j["path"].as_array()?.iter().filter_map(|x| x.as_u64().map(|v| v as u32)).collect(), off: j["off"].as_u64()? as usize })
        };
        let (Some(a), Some(f)) = (parse(anchor), parse(focus)) else { return "null".into() };
        let ed = self.ed();
        if a.path.is_empty() || f.path.is_empty() {
            return "null".into();
        }
        let (a, f) = (ed.clamp_pos(a), ed.clamp_pos(f));
        let l = ed.layout();
        let caret = l.caret(&f.path, f.off).map(|c| json!({"page": c.page, "x": c.x, "y": c.y, "h": c.h}));
        let (x, y) = if (&a.path, a.off) <= (&f.path, f.off) { (&a, &f) } else { (&f, &a) };
        let sel: Vec<_> = if a == f { Vec::new() } else { l.sel_rects((&x.path, x.off), (&y.path, y.off)).into_iter().map(|(p, x, y, w, h)| json!([p, x, y, w, h])).collect() };
        json!({"caret": caret, "sel": sel}).to_string()
    }

    /// Text of every paragraph on a page, with its path (spelling).
    pub fn page_texts(&mut self, page: usize) -> String {
        let ed = self.ed();
        let paths: Vec<Vec<u32>> = {
            let l = ed.layout();
            let Some(p) = l.pages.get(page) else { return "[]".into() };
            let mut v: Vec<Vec<u32>> = Vec::new();
            for it in &p.items {
                if let papir_core::layout::Placed::Line { path: Some(path), .. } = it {
                    if v.last() != Some(&path.to_vec()) && !v.contains(&path.to_vec()) {
                        v.push(path.to_vec());
                    }
                }
            }
            v
        };
        let out: Vec<_> = paths.into_iter().filter_map(|path| papir_core::edit::dpara(&ed.doc, &path).map(|p| json!([path, p.plain_text()]))).collect();
        serde_json::Value::Array(out).to_string()
    }

    pub fn select_word(&mut self) {
        self.ed().select_word();
    }

    pub fn select_para(&mut self) {
        self.ed().select_para();
    }

    pub fn select_all(&mut self) {
        self.ed().select_all();
    }

    pub fn go(&mut self, unit: &str, dir: i32, extend: bool) {
        self.ed().go(unit, dir, extend);
    }

    pub fn insert(&mut self, text: &str) {
        self.ed().insert_text(text);
    }

    pub fn enter(&mut self) {
        self.ed().split();
    }

    pub fn line_break(&mut self) {
        self.ed().line_break();
    }

    pub fn page_break(&mut self) {
        self.ed().page_break();
    }

    pub fn backspace(&mut self, word: bool) {
        self.ed().backspace(word);
    }

    pub fn del(&mut self, word: bool) {
        self.ed().delete(word);
    }

    /// b, i, u, s, sup, sub
    pub fn toggle(&mut self, name: &str) {
        let p = match name {
            "b" => Prop::Bold,
            "i" => Prop::Italic,
            "u" => Prop::Underline,
            "s" => Prop::Strike,
            "sup" => Prop::Super,
            "sub" => Prop::Sub,
            _ => return,
        };
        self.ed().set_prop(p);
    }

    /// Remembers the character formatting at the selection (format painter).
    pub fn copy_format(&mut self) {
        let f = self.ed().format_sample();
        self.painter = Some(f);
    }

    /// Applies the remembered formatting to the selection; false when nothing was copied.
    pub fn paste_format(&mut self) -> bool {
        let Some(f) = self.painter.clone() else { return false };
        self.ed().paint_format(&f);
        true
    }

    pub fn set_font(&mut self, name: &str) {
        self.ed().set_prop(Prop::Font(name));
    }

    pub fn set_size(&mut self, pt: f32) {
        self.ed().set_prop(Prop::Size(pt));
    }

    /// `RRGGBB` or empty for automatic
    pub fn set_color(&mut self, c: &str) {
        self.ed().set_prop(Prop::Color(if c.is_empty() { None } else { Some(c) }));
    }

    pub fn set_highlight(&mut self, c: &str) {
        self.ed().set_prop(Prop::Highlight(if c.is_empty() { None } else { Some(c) }));
    }

    /// left, center, right, both
    pub fn align(&mut self, jc: &str) {
        let jc = jc.to_string();
        self.ed().set_para(&move |p| p.jc = Some(jc.clone()));
    }

    /// Moves the left indent by `delta` twips (to the next default tab stop).
    pub fn indent(&mut self, delta: i32) {
        self.ed().set_para(&move |p| {
            let cur = p.ind_left.unwrap_or(0);
            let next = if delta > 0 { (cur / delta.abs() + 1) * delta.abs() } else { ((cur - 1).max(0) / delta.abs()) * delta.abs() };
            p.ind_left = Some(next.max(0));
        });
    }

    /// Line spacing as a multiple (1.0, 1.15, 1.5, 2.0).
    pub fn line_spacing(&mut self, mult: f32) {
        let v = (mult * 240.0).round() as i32;
        self.ed().set_para(&move |p| {
            p.line = Some(v);
            p.line_rule = Some("auto".into());
        });
    }

    /// Space after paragraphs, points.
    pub fn space_after(&mut self, pt: f32) {
        let v = (pt * 20.0).round() as i32;
        self.ed().set_para(&move |p| p.after = Some(v));
    }

    pub fn clear_format(&mut self) {
        self.ed().clear_format();
    }

    /// kind: margins (top, bottom, left, right), orient (a = 1 landscape),
    /// size (width, height); twips
    pub fn set_page(&mut self, kind: &str, a: i32, b: i32, c: i32, d: i32) {
        self.ed().set_page(kind, a, b, c, d);
    }

    /// Indents of the selected paragraphs, twips.
    pub fn set_indents(&mut self, left: i32, first: i32, right: i32) {
        self.ed().set_indents(left, first, right);
    }

    /// Table column boundary near a point: {"path":[..],"i":n,"x":pt} or "".
    pub fn column_border(&mut self, page: usize, x: f32, y: f32, tol: f32) -> String {
        match self.ed().layout().column_border(page, x, y, tol) {
            Some((p, i, bx)) => json!({"path": &*p, "i": i, "x": bx}).to_string(),
            None => String::new(),
        }
    }

    /// Moves a column boundary by `delta` points.
    pub fn resize_column(&mut self, path: &str, i: usize, delta: f32) -> bool {
        let Ok(p) = serde_json::from_str::<Vec<u32>>(path) else { return false };
        self.ed().resize_column(&p, i, (delta * 20.0).round() as i32)
    }

    /// External link at a point of a page.
    pub fn link_at(&mut self, page: usize, x: f32, y: f32) -> Option<String> {
        let ed = self.ed();
        let (path, off) = ed.layout().hit(page, x, y)?;
        let path = path.to_vec();
        ed.link_at(&path, off).or_else(|| if off > 0 { ed.link_at(&path, off - 1) } else { None })
    }

    /// Selects the picture under a point; true when there is one.
    pub fn select_image_at(&mut self, page: usize, x: f32, y: f32) -> bool {
        let ed = self.ed();
        let Some((path, off)) = ed.layout().image_at(page, x, y) else { return false };
        let path = path.to_vec();
        ed.set_selection(Pos { path: path.clone(), off }, Pos { path, off: off + 1 });
        true
    }

    pub fn resize_image(&mut self, path: &str, off: usize, w: f32, h: f32) -> bool {
        let Ok(p) = serde_json::from_str::<Vec<u32>>(path) else { return false };
        self.ed().resize_image(&p, off, w, h)
    }

    /// Accepts or rejects tracked changes at the selection, or all.
    pub fn review(&mut self, accept: bool, all: bool) -> usize {
        self.ed().review(accept, all)
    }

    pub fn next_change(&mut self, dir: i32) -> bool {
        self.ed().next_change(dir)
    }

    pub fn set_markup(&mut self, on: bool) {
        self.ed().set_markup(on);
    }

    /// Comments in document order: [{id, author, initials, date, text, done,
    /// paraId, parent, anchor}]
    pub fn comments(&mut self) -> String {
        let ed = self.ed();
        let anchors = ed.comment_anchors();
        let mut list: Vec<_> = ed
            .doc
            .comments
            .items
            .iter()
            .map(|c| {
                let a = anchors.get(&c.id);
                (a.map(|a| (a.start.path.clone(), a.start.off)), c)
            })
            .collect();
        list.sort_by(|x, y| x.0.cmp(&y.0));
        let v: Vec<_> = list
            .into_iter()
            .map(|(a, c)| {
                json!({"id": c.id, "author": c.author, "initials": c.initials, "date": c.date, "text": c.text(),
                       "done": c.done, "paraId": c.para_id, "parent": c.parent, "anchored": a.is_some()})
            })
            .collect();
        serde_json::Value::Array(v).to_string()
    }

    pub fn add_comment(&mut self, text: &str, author: &str, date: &str) -> Option<String> {
        self.ed().add_comment(text, author, date)
    }

    pub fn reply_comment(&mut self, id: &str, text: &str, author: &str, date: &str) -> Option<String> {
        self.ed().reply_comment(id, text, author, date)
    }

    pub fn resolve_comment(&mut self, id: &str, done: bool) -> bool {
        self.ed().resolve_comment(id, done)
    }

    pub fn delete_comment(&mut self, id: &str) -> bool {
        self.ed().delete_comment(id)
    }

    pub fn select_comment(&mut self, id: &str) -> bool {
        self.ed().select_comment(id)
    }

    /// Starts editing the header/footer at a point outside the body area.
    pub fn edit_story_at(&mut self, page: usize, x: f32, y: f32) -> bool {
        self.ed().edit_story_at(page, x, y)
    }

    pub fn exit_story(&mut self) {
        self.ed().exit_story();
    }

    /// PAGE, NUMPAGES
    pub fn insert_field(&mut self, code: &str, shown: &str) {
        self.ed().insert_field(code, shown);
    }

    pub fn toggle_list(&mut self, bullet: bool) {
        self.ed().toggle_list(bullet);
    }

    /// Changes the list level of the selection; false outside lists.
    pub fn list_level(&mut self, delta: i32) -> bool {
        self.ed().list_level(delta)
    }

    pub fn next_cell(&mut self, dir: i32) -> bool {
        self.ed().next_cell(dir)
    }

    pub fn insert_image(&mut self, data: Vec<u8>, ext: &str, w: f32, h: f32) -> Result<(), JsError> {
        self.ed().insert_image(data, ext, w, h).map_err(err)
    }

    pub fn insert_link(&mut self, url: &str, text: &str) -> Result<(), JsError> {
        self.ed().insert_link(url, text).map_err(err)
    }

    pub fn insert_table(&mut self, rows: usize, cols: usize) {
        self.ed().insert_table(rows, cols);
    }

    /// row_above, row_below, col_left, col_right, del_row, del_col,
    /// del_table, merge, distribute
    pub fn table_op(&mut self, op: &str) -> bool {
        self.ed().table_op(op)
    }

    pub fn set_style(&mut self, id: &str) {
        self.ed().set_style(id);
    }

    /// [{id, name, kind}] of paragraph styles for the style gallery.
    pub fn styles(&mut self) -> String {
        let doc = &self.ed().doc;
        let st = doc.styles.clone();
        let theme = doc.theme.clone();
        let v: Vec<_> = st
            .quick_styles()
            .into_iter()
            .map(|s| {
                // what the style looks like, for the gallery
                let r = st.run(Some(&s.id), None, &Default::default());
                let font = r.font_ascii.clone().or_else(|| r.theme_ascii.as_deref().and_then(|t| theme.resolve(t)).map(str::to_string));
                json!({"id": s.id, "name": s.name, "q": s.q_format,
                       "font": font, "size": r.sz.map(|h| h as f32 / 2.0), "color": r.color,
                       "b": r.b.unwrap_or(false), "i": r.i.unwrap_or(false), "caps": r.caps.unwrap_or(false)})
            })
            .collect();
        serde_json::Value::Array(v).to_string()
    }

    /// Language of the document's text (`w:docDefaults` or the Normal style), e.g. "uk-UA".
    pub fn doc_lang(&mut self) -> Option<String> {
        let doc = &self.ed().doc;
        let part = doc.rels.values().find(|r| r.kind == "styles").map(|r| r.target.clone())?;
        let text = doc.pkg.text(&part)?;
        let at = text.find("w:docDefaults").or_else(|| text.find("w:styleId=\"Normal\""))?;
        let rest = &text[at..];
        let l = rest.find("<w:lang ")?;
        let tag = &rest[l..l + rest[l..].find('>')?];
        let v = tag.find("w:val=\"")? + 7;
        Some(tag[v..v + tag[v..].find('"')?].to_string())
    }

    pub fn copy_text(&mut self) -> String {
        self.ed().selected_text()
    }

    pub fn copy_xml(&mut self) -> String {
        self.ed().selected_xml()
    }

    pub fn cut(&mut self) {
        self.ed().cut();
    }

    pub fn paste_text(&mut self, s: &str) {
        self.ed().paste_text(s);
    }

    pub fn paste_xml(&mut self, s: &str) -> bool {
        self.ed().paste_xml(s)
    }

    pub fn undo(&mut self) -> bool {
        self.ed().undo()
    }

    pub fn redo(&mut self) -> bool {
        self.ed().redo()
    }

    pub fn find(&mut self, needle: &str, case: bool, back: bool) -> bool {
        self.ed().find(needle, case, back)
    }

    pub fn replace(&mut self, needle: &str, with: &str, case: bool) -> bool {
        self.ed().replace(needle, with, case)
    }

    pub fn replace_all(&mut self, needle: &str, with: &str, case: bool) -> usize {
        self.ed().replace_all(needle, with, case)
    }

    pub fn save(&mut self) -> Result<Vec<u8>, JsError> {
        self.ed().save().map_err(|e| JsError::new(e.code()))
    }

    pub fn export_pdf(&mut self) -> Vec<u8> {
        self.ed().export_pdf()
    }

    pub fn version(&mut self) -> f64 {
        self.ed().version as f64
    }

    pub fn word_count(&mut self) -> usize {
        self.ed().word_count()
    }

    pub fn meta(&mut self) -> String {
        let m = &self.ed().doc.meta;
        json!({"title": m.title, "author": m.author}).to_string()
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

fn js_sys_json(v: &JsValue) -> Option<String> {
    v.as_string()
}

/// A workbook for the XLSX viewer.
#[wasm_bindgen]
pub struct Book {
    b: papir_core::xlsx::Book,
}

/// Excel's column width (characters of the default font) in pixels at 100%.
fn col_px(w: f32) -> f32 {
    let mdw = 7.0;
    ((256.0 * w + (128.0f32 / mdw).trunc()) / 256.0 * mdw).trunc().max(0.0)
}

#[wasm_bindgen]
impl Book {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8]) -> Result<Book, JsError> {
        papir_core::xlsx::Book::open(bytes).map(|b| Book { b }).map_err(|e| JsError::new(&e.to_string()))
    }

    pub fn sheets(&self) -> String {
        let v: Vec<_> = self.b.sheets.iter().map(|s| json!({"name": s.name, "hidden": s.hidden, "rows": s.nrows, "cols": s.ncols, "color": s.tab_color})).collect();
        serde_json::Value::Array(v).to_string()
    }

    /// Sizes in pixels at 100%, merges, frozen panes.
    pub fn layout(&self, i: usize) -> String {
        let Some(s) = self.b.sheets.get(i) else { return "null".into() };
        let cols: Vec<_> = s.cols.iter().map(|c| json!([c.min, c.max, col_px(c.width.unwrap_or(s.default_w)), c.hidden])).collect();
        let rows: Vec<_> = s.rows.iter().filter(|r| r.ht.is_some() || r.hidden).map(|r| json!([r.r, (r.ht.unwrap_or(s.default_h) * 96.0 / 72.0).round(), r.hidden])).collect();
        json!({
            "dw": col_px(s.default_w), "dh": (s.default_h * 96.0 / 72.0).round(), "cols": cols, "rows": rows,
            "merges": s.merges, "frozen": [s.frozen.0, s.frozen.1], "filter": s.filter, "grid": s.grid,
            "nrows": s.nrows, "ncols": s.ncols,
        })
        .to_string()
    }

    pub fn styles(&self) -> String {
        let b = &self.b;
        let v: Vec<_> = b
            .xfs
            .iter()
            .map(|x| {
                let f = b.fonts.get(x.font as usize).cloned().unwrap_or_default();
                let br = b.borders.get(x.border as usize).cloned().unwrap_or_default();
                json!({
                    "font": [f.name, f.size, f.b, f.i, f.u, f.strike, f.color],
                    "fill": x.fill, "border": br.sides.iter().map(|s| s.as_ref().map(|(st, c)| json!([st, c]))).collect::<Vec<_>>(),
                    "h": x.h, "v": x.v, "wrap": x.wrap, "indent": x.indent, "rot": x.rot,
                })
            })
            .collect();
        serde_json::Value::Array(v).to_string()
    }

    /// Cells of rows r0..=r1, columns c0..=c1: [row, col, text, style, kind, colour];
    /// kind: 0 text, 1 number, 2 logical, 3 error, 4 empty but styled.
    pub fn range(&self, i: usize, r0: u32, r1: u32, c0: u32, c1: u32) -> String {
        let Some(s) = self.b.sheets.get(i) else { return "[]".into() };
        let mut out = Vec::new();
        for row in s.rows_in(r0, r1) {
            let a = row.cells.partition_point(|c| c.c < c0);
            for c in row.cells[a..].iter().take_while(|c| c.c <= c1) {
                use papir_core::xlsx::V;
                let kind = match c.v {
                    V::None => 4,
                    V::N(_) => 1,
                    V::B(_) => 2,
                    V::E(_) => 3,
                    V::S(_) => 0,
                };
                if kind == 4 {
                    let x = self.b.xfs.get(c.s as usize);
                    let styled = x.is_some_and(|x| x.fill.is_some() || self.b.borders.get(x.border as usize).is_some_and(|b| b.sides.iter().any(Option::is_some)));
                    if !styled {
                        continue;
                    }
                }
                let (text, color, _) = self.b.display(c);
                out.push(json!([row.r, c.c, text, c.s, kind, color]));
            }
        }
        serde_json::Value::Array(out).to_string()
    }

    /// Address, shown text and formula of one cell (the formula bar).
    pub fn cell(&self, i: usize, r: u32, c: u32) -> String {
        use papir_core::xlsx::{col_name, V};
        let addr = format!("{}{}", col_name(c), r + 1);
        let Some(cell) = self.b.sheets.get(i).and_then(|s| s.cell(r, c)) else { return json!({"addr": addr, "text": "", "formula": null}).to_string() };
        // the bar shows the value as entered: plain numbers, dates as dates
        let text = match &cell.v {
            V::N(n) => {
                let code = self.b.format_code(cell.s);
                if papir_core::numfmt::is_date(&code) {
                    papir_core::numfmt::format(*n, if n.fract() == 0.0 { "dd.mm.yyyy" } else { "dd.mm.yyyy h:mm:ss" }, self.b.date1904).0
                } else if code.contains('%') {
                    format!("{}%", papir_core::numfmt::general(n * 100.0))
                } else {
                    papir_core::numfmt::general(*n)
                }
            }
            _ => self.b.display(cell).0,
        };
        json!({"addr": addr, "text": text, "formula": cell.f.as_ref().map(|f| format!("={f}"))}).to_string()
    }

    /// Next cell (row-major from r, c, exclusive) whose text contains `needle`, any case.
    pub fn find(&self, i: usize, needle: &str, r: u32, c: u32, back: bool) -> String {
        let Some(s) = self.b.sheets.get(i) else { return "null".into() };
        let n = needle.to_lowercase();
        if n.is_empty() {
            return "null".into();
        }
        let hit = |cell: &papir_core::xlsx::Cell| self.b.display(cell).0.to_lowercase().contains(&n);
        let all: Box<dyn Iterator<Item = (u32, &papir_core::xlsx::Cell)>> = if back {
            Box::new(s.rows.iter().rev().flat_map(|row| row.cells.iter().rev().map(move |c| (row.r, c))))
        } else {
            Box::new(s.rows.iter().flat_map(|row| row.cells.iter().map(move |c| (row.r, c))))
        };
        let after = |rr: u32, cc: u32| if back { (rr, cc) < (r, c) } else { (rr, cc) > (r, c) };
        let mut first = None;
        for (rr, cell) in all {
            if hit(cell) {
                if after(rr, cell.c) {
                    return json!([rr, cell.c]).to_string();
                }
                first.get_or_insert((rr, cell.c));
            }
        }
        // wrap around
        first.map(|(a, b)| json!([a, b]).to_string()).unwrap_or_else(|| "null".into())
    }

    /// Count, sum and average of the numbers in a range (status bar).
    pub fn stats(&self, i: usize, r0: u32, c0: u32, r1: u32, c1: u32) -> String {
        let Some(s) = self.b.sheets.get(i) else { return "null".into() };
        let (mut count, mut nums, mut sum) = (0u32, 0u32, 0f64);
        for row in s.rows_in(r0, r1) {
            for c in row.cells.iter().filter(|c| c.c >= c0 && c.c <= c1) {
                match c.v {
                    papir_core::xlsx::V::None => {}
                    papir_core::xlsx::V::N(n) => {
                        count += 1;
                        nums += 1;
                        sum += n;
                    }
                    _ => count += 1,
                }
            }
        }
        json!({"count": count, "nums": nums, "sum": papir_core::numfmt::general(sum), "avg": if nums > 0 { papir_core::numfmt::general(sum / nums as f64) } else { String::new() }}).to_string()
    }
}

/// Spelling (Hunspell dictionaries), used from a worker.
#[wasm_bindgen]
pub struct Speller {
    dicts: Vec<(String, spellbook::Dictionary)>,
}

impl Default for Speller {
    fn default() -> Self {
        Speller::new()
    }
}

/// Dictionary language of a word: Cyrillic is Ukrainian, Latin is English.
fn lang_of(w: &str) -> &'static str {
    if w.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)) {
        "uk"
    } else {
        "en"
    }
}

fn normalize(w: &str) -> String {
    w.replace(['’', 'ʼ', '`'], "'")
}

#[wasm_bindgen]
impl Speller {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Speller {
        Speller { dicts: Vec::new() }
    }

    /// Adds a dictionary for `lang` ("uk", "en").
    pub fn load(&mut self, lang: &str, aff: &str, dic: &str) -> Result<(), JsError> {
        let d = spellbook::Dictionary::new(aff, dic).map_err(|e| JsError::new(&e.to_string()))?;
        self.dicts.retain(|(l, _)| l != lang);
        self.dicts.push((lang.to_string(), d));
        Ok(())
    }

    fn dict(&self, w: &str) -> Option<&spellbook::Dictionary> {
        let l = lang_of(w);
        self.dicts.iter().find(|(x, _)| x == l).map(|(_, d)| d)
    }

    /// Indexes of the misspelled words of a JSON list.
    pub fn check(&self, words: &str) -> String {
        let list: Vec<String> = serde_json::from_str(words).unwrap_or_default();
        let bad: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                let w = normalize(w);
                // numbers, single letters, words with digits are not checked
                if w.chars().count() < 2 || w.chars().any(|c| c.is_ascii_digit()) {
                    return false;
                }
                match self.dict(&w) {
                    Some(d) => !d.check(&w),
                    None => false,
                }
            })
            .map(|(i, _)| i)
            .collect();
        serde_json::to_string(&bad).unwrap()
    }

    pub fn suggest(&self, word: &str) -> String {
        let w = normalize(word);
        let mut out = Vec::new();
        if let Some(d) = self.dict(&w) {
            d.suggest(&w, &mut out);
        }
        out.truncate(8);
        serde_json::to_string(&out).unwrap()
    }

    /// A word the person accepts (personal dictionary).
    pub fn add(&mut self, word: &str) {
        let w = normalize(word);
        let l = lang_of(&w);
        if let Some((_, d)) = self.dicts.iter_mut().find(|(x, _)| x == l) {
            let _ = d.add(&w);
        }
    }
}
