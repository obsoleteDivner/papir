//! Paragraph-level edits and container paths.
//!
//! A path addresses a paragraph: `[i]` is block `i` of the body; a table at
//! `[i]` continues with `row, cell` and an index into the cell; an sdt
//! continues with an index into its content.

use crate::model::*;
use crate::props::RunProps;
use std::rc::Rc;

fn step<'a>(c: &'a Container, path: &[u32]) -> Option<&'a Para> {
    let b = c.get(*path.first()? as usize)?;
    if path.len() == 1 {
        return match &**b {
            Block::Para(p) => Some(p),
            _ => None,
        };
    }
    match &**b {
        Block::Table(t) => {
            let row = t.rows().nth(*path.get(1)? as usize)?;
            let cell = row.cells().nth(*path.get(2)? as usize)?;
            step(&cell.content, &path[3..])
        }
        Block::Sdt(s) => step(&s.content, &path[1..]),
        _ => None,
    }
}

pub fn para_at<'a>(body: &'a Container, path: &[u32]) -> Option<&'a Para> {
    step(body, path)
}

/// Container holding the paragraph at `path` (copy-on-write down the way).
pub fn container_mut<'a>(c: &'a mut Container, path: &[u32]) -> Option<&'a mut Container> {
    if path.len() == 1 {
        return Some(c);
    }
    let i = path[0] as usize;
    let b = Rc::make_mut(c.get_mut(i)?);
    match b {
        Block::Table(t) => {
            t.orig = None;
            let (r, ci) = (*path.get(1)? as usize, *path.get(2)? as usize);
            let cell = t.row_mut(r)?.cell_mut(ci)?;
            container_mut(&mut cell.content, &path[3..])
        }
        Block::Sdt(s) => container_mut(&mut s.content, &path[1..]),
        _ => None,
    }
}

pub fn para_mut<'a>(body: &'a mut Container, path: &[u32]) -> Option<&'a mut Para> {
    let c = container_mut(body, path)?;
    let b = Rc::make_mut(c.get_mut(*path.last()? as usize)?);
    match b {
        Block::Para(p) => {
            p.touch();
            Some(p)
        }
        _ => None,
    }
}

/// All paragraph paths in document order.
pub fn para_paths(body: &Container) -> Vec<Vec<u32>> {
    fn walk(c: &Container, path: &mut Vec<u32>, out: &mut Vec<Vec<u32>>) {
        for (i, b) in c.iter().enumerate() {
            path.push(i as u32);
            match &**b {
                Block::Para(_) => out.push(path.clone()),
                Block::Table(t) => {
                    for (ri, r) in t.rows().enumerate() {
                        for (ci, cell) in r.cells().enumerate() {
                            path.push(ri as u32);
                            path.push(ci as u32);
                            walk(&cell.content, path, out);
                            path.pop();
                            path.pop();
                        }
                    }
                }
                Block::Sdt(s) => walk(&s.content, path, out),
                Block::Raw(_) => {}
            }
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(body, &mut Vec::new(), &mut out);
    out
}

fn split_run(r: &Run, k: usize) -> (Run, Run) {
    let mut a = r.shell();
    let mut b = r.shell();
    let mut pos = 0;
    for it in &r.items {
        let len = it.len();
        if pos + len <= k {
            a.items.push(it.clone());
        } else if pos >= k {
            b.items.push(it.clone());
        } else if let Item::Text(t) = it {
            let cut = t.char_indices().nth(k - pos).map(|(i, _)| i).unwrap_or(t.len());
            a.items.push(Item::Text(t[..cut].to_string()));
            b.items.push(Item::Text(t[cut..].to_string()));
        }
        pos += len;
    }
    (a, b)
}

fn text_items(s: &str) -> Vec<Item> {
    let mut v: Vec<Item> = Vec::new();
    for c in s.chars() {
        match c {
            '\t' => v.push(Item::Tab),
            '\u{b}' => v.push(Item::Break(BreakKind::Line)),
            '\r' | '\n' => {}
            c => match v.last_mut() {
                Some(Item::Text(t)) => t.push(c),
                _ => v.push(Item::Text(c.to_string())),
            },
        }
    }
    v
}

impl Para {
    /// Ensures a run boundary at `off`.
    pub fn split_at(&mut self, off: usize) {
        let mut pos = 0;
        for i in 0..self.inlines.len() {
            if let Inline::Run(r) = &self.inlines[i] {
                let len = r.len();
                if pos < off && off < pos + len {
                    let (a, b) = split_run(r, off - pos);
                    self.inlines[i] = Inline::Run(a);
                    self.inlines.insert(i + 1, Inline::Run(b));
                    return;
                }
                pos += len;
            }
        }
    }

    /// Direct properties of the run holding `off` (left affinity), or the
    /// paragraph mark's.
    pub fn props_at(&self, off: usize) -> RunProps {
        let mut pos = 0;
        let mut last: Option<&RunProps> = None;
        for inl in &self.inlines {
            if let Inline::Run(r) = inl {
                let len = r.len();
                if len > 0 || last.is_none() {
                    if off > pos && off <= pos + len {
                        return r.props.clone();
                    }
                    if pos >= off && last.is_some() {
                        break;
                    }
                    if len > 0 {
                        last = Some(&r.props);
                    }
                }
                if off == 0 && len > 0 {
                    return r.props.clone();
                }
                pos += len;
            }
        }
        last.cloned().unwrap_or_else(|| self.mark.clone())
    }

    pub fn insert_text(&mut self, off: usize, text: &str, props: Option<&RunProps>) {
        let items = text_items(text);
        if items.is_empty() {
            return;
        }
        if let Some(p) = props {
            self.split_at(off);
            let mut r = Run::new(p.clone());
            r.items = items;
            let i = self.insert_index(off);
            self.inlines.insert(i, Inline::Run(r));
            return;
        }
        // into the run holding the position (left affinity)
        let mut pos = 0;
        let mut target: Option<(usize, usize)> = None;
        for (i, inl) in self.inlines.iter().enumerate() {
            if let Inline::Run(r) = inl {
                let len = r.len();
                if (off > pos && off <= pos + len) || (off == 0 && pos == 0 && len > 0) {
                    target = Some((i, off - pos));
                    break;
                }
                pos += len;
            }
        }
        if target.is_none() && off == 0 {
            target = self.inlines.iter().position(|x| matches!(x, Inline::Run(_))).map(|i| (i, 0));
        }
        match target {
            Some((i, k)) => {
                if let Inline::Run(r) = &mut self.inlines[i] {
                    insert_into_run(r, k, items);
                }
            }
            None => {
                let mut r = Run::new(self.mark.clone());
                r.dirty = true;
                r.items = items;
                let i = self.insert_index(off);
                self.inlines.insert(i, Inline::Run(r));
            }
        }
    }

    /// Inline index for a new run at `off` (after `split_at(off)`).
    fn insert_index(&self, off: usize) -> usize {
        let mut pos = 0;
        let mut after_text = 0;
        for (i, inl) in self.inlines.iter().enumerate() {
            if let Inline::Run(r) = inl {
                let len = r.len();
                if pos + len <= off && len > 0 {
                    after_text = i + 1;
                } else if pos >= off && len > 0 {
                    break;
                }
                pos += len;
            }
        }
        if off == 0 {
            // inside leading wrappers (e.g. an empty content control)
            return self.inlines.iter().position(|x| !matches!(x, Inline::Open(_) | Inline::Raw(_))).unwrap_or(self.inlines.len());
        }
        after_text
    }

    /// Inserts whole inlines (a run, a wrapped hyperlink...) at `off`.
    pub fn insert_inlines(&mut self, off: usize, items: Vec<Inline>) {
        self.split_at(off);
        let mut pos = 0;
        let mut at = self.inlines.len();
        for (i, x) in self.inlines.iter().enumerate() {
            if pos >= off && x.len() > 0 {
                at = i;
                break;
            }
            pos += x.len();
        }
        if off == 0 {
            at = self.inlines.iter().position(|x| x.len() > 0).unwrap_or(self.inlines.len());
        }
        for (k, it) in items.into_iter().enumerate() {
            self.inlines.insert(at + k, it);
        }
        self.touch();
    }

    /// Whether `off` sits inside a tracked insertion by `author` (left affinity).
    fn in_own_ins(&self, off: usize, author: &str) -> bool {
        let mut pos = 0;
        let mut stack: Vec<bool> = Vec::new();
        for inl in &self.inlines {
            match inl {
                Inline::Open(w) => stack.push(matches!(&w.kind, crate::model::WrapKind::Ins(a, _) if a == author)),
                Inline::Close(_) => {
                    stack.pop();
                }
                Inline::Run(r) => {
                    let len = r.len();
                    if len > 0 && off > pos && off <= pos + len {
                        return stack.iter().any(|x| *x);
                    }
                    pos += len;
                }
                Inline::Raw(_) => {}
            }
        }
        false
    }

    /// Records `[from, to)` (just inserted) as a tracked insertion by `author`.
    pub fn track_insert(&mut self, from: usize, to: usize, author: &str, date: &str, id: u32) {
        if from >= to || self.in_own_ins(to, author) && self.in_own_ins(from + 1, author) {
            return;
        }
        self.split_at(from);
        self.split_at(to);
        let mut pos = 0;
        let (mut first, mut last) = (None, None);
        for (i, inl) in self.inlines.iter().enumerate() {
            if let Inline::Run(r) = inl {
                let len = r.len();
                if len > 0 && pos >= from && pos + len <= to {
                    first.get_or_insert(i);
                    last = Some(i);
                }
                pos += len;
            }
        }
        let (Some(a), Some(b)) = (first, last) else { return };
        let esc = |v: &str| crate::xml::escape(v).replace('"', "&quot;");
        let w = Rc::new(crate::model::Wrapper {
            kind: crate::model::WrapKind::Ins(author.to_string(), date.to_string()),
            open: format!("<w:ins w:id=\"{id}\" w:author=\"{}\" w:date=\"{}\">", esc(author), esc(date)),
            close: "</w:ins>".into(),
        });
        self.inlines.insert(b + 1, Inline::Close(w.clone()));
        self.inlines.insert(a, Inline::Open(w));
        self.touch();
    }

    /// Tracked deletion of `[from, to)`: what `author` inserted goes for good,
    /// the rest stays as deleted text (w:del) until accepted.
    pub fn track_delete(&mut self, from: usize, to: usize, author: &str, date: &str, id: u32) {
        if from >= to {
            return;
        }
        self.split_at(from);
        self.split_at(to);
        let esc = |v: &str| crate::xml::escape(v).replace('"', "&quot;");
        let mut pos = 0;
        let mut stack: Vec<bool> = Vec::new();
        let mut out: Vec<Inline> = Vec::with_capacity(self.inlines.len());
        for inl in std::mem::take(&mut self.inlines) {
            match &inl {
                Inline::Open(w) => stack.push(matches!(&w.kind, crate::model::WrapKind::Ins(a, _) if a == author)),
                Inline::Close(_) => {
                    stack.pop();
                }
                Inline::Run(r) => {
                    let len = r.len();
                    if len > 0 && pos >= from && pos + len <= to {
                        pos += len;
                        let raws: Vec<Item> = r.items.iter().filter(|it| matches!(it, Item::Raw(_))).cloned().collect();
                        if !stack.iter().any(|x| *x) {
                            // kept as deleted text
                            let xml = crate::docx::run_xml(r).replace("<w:t>", "<w:delText>").replace("<w:t ", "<w:delText ").replace("</w:t>", "</w:delText>");
                            let src = format!("<w:body xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><w:del w:id=\"{id}\" w:author=\"{}\" w:date=\"{}\">{xml}</w:del></w:body>", esc(author), esc(date));
                            if let Some(el) = crate::xml::parse(&src).ok().and_then(|b| b.elems().next().cloned()) {
                                out.push(Inline::Raw(Rc::new(el)));
                            }
                        }
                        if !raws.is_empty() {
                            let mut keep = r.shell();
                            keep.items = raws;
                            out.push(Inline::Run(keep));
                        }
                        continue;
                    }
                    pos += len;
                }
                Inline::Raw(_) => {}
            }
            out.push(inl);
        }
        // insertions left empty go too
        let mut i = 0;
        while i + 1 < out.len() {
            if matches!((&out[i], &out[i + 1]), (Inline::Open(w), Inline::Close(_)) if matches!(w.kind, crate::model::WrapKind::Ins(..))) {
                out.drain(i..i + 2);
                continue;
            }
            i += 1;
        }
        self.inlines = out;
        self.touch();
    }

    /// Deletes the content in `[from, to)`. Zero-width markup stays.
    pub fn delete(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        self.split_at(from);
        self.split_at(to);
        let mut pos = 0;
        for inl in self.inlines.iter_mut() {
            if let Inline::Run(r) = inl {
                let len = r.len();
                if pos >= from && pos + len <= to && len > 0 {
                    r.items.retain(|it| matches!(it, Item::Raw(_)));
                }
                pos += len;
            }
        }
        self.inlines.retain(|x| !matches!(x, Inline::Run(r) if r.items.is_empty()));
    }

    /// Splits at `off`; `self` keeps the head, the tail is returned.
    /// Wrappers open at the split are closed in the head and reopened.
    pub fn split_off(&mut self, off: usize) -> Para {
        self.split_at(off);
        let mut pos = 0;
        let mut cut = self.inlines.len();
        for (i, inl) in self.inlines.iter().enumerate() {
            let len = inl.len();
            match inl {
                Inline::Run(_) if len > 0 && pos >= off => {
                    cut = i;
                    break;
                }
                Inline::Open(_) if pos >= off && off > 0 => {
                    cut = i;
                    break;
                }
                _ => {}
            }
            pos += len;
        }
        if off == 0 {
            // keep leading markup with the head only if nothing follows
            cut = self.inlines.iter().position(|x| x.len() > 0 || matches!(x, Inline::Open(_))).unwrap_or(self.inlines.len());
        }
        let tail_inl: Vec<Inline> = self.inlines.drain(cut..).collect();
        // balance wrappers
        let mut open: Vec<Rc<Wrapper>> = Vec::new();
        for inl in &self.inlines {
            match inl {
                Inline::Open(w) => open.push(w.clone()),
                Inline::Close(_) => {
                    open.pop();
                }
                _ => {}
            }
        }
        for w in open.iter().rev() {
            self.inlines.push(Inline::Close(w.clone()));
        }
        let mut tail = Para {
            rev: next_rev(),
            open: "<w:p>".into(),
            close: self.close.clone(),
            ppr: self.ppr.clone(),
            props: self.props.clone(),
            mark: self.mark.clone(),
            props_dirty: self.props_dirty,
            inlines: open.iter().map(|w| Inline::Open(Rc::new(reopened(w)))).collect(),
            orig: None,
        };
        tail.inlines.extend(tail_inl);
        // the section break stays with the paragraph mark, i.e. the tail
        if let Some(ppr) = &self.ppr {
            if ppr.wchild("sectPr").is_some() {
                let mut e = (**ppr).clone();
                e.children.retain(|n| !matches!(n, crate::xml::Node::Elem(x) if x.is_w("sectPr")));
                self.ppr = Some(Rc::new(e));
            }
        }
        self.touch();
        tail
    }

    /// Appends `other`'s content (merging two paragraphs).
    pub fn append(&mut self, other: &Para) {
        self.inlines.extend(other.inlines.iter().cloned());
        if let Some(s) = other.sect() {
            let mut e = self.ppr.as_deref().cloned().unwrap_or_else(|| crate::xml::Element::new_w("pPr"));
            e.children.retain(|n| !matches!(n, crate::xml::Node::Elem(x) if x.is_w("sectPr")));
            e.children.push(crate::xml::Node::Elem(s.clone()));
            self.ppr = Some(Rc::new(e));
        }
        self.touch();
    }

    /// Applies `f` to the direct properties of runs in `[from, to)`.
    pub fn format(&mut self, from: usize, to: usize, f: &dyn Fn(&mut RunProps)) {
        if from >= to {
            return;
        }
        self.split_at(from);
        self.split_at(to);
        let mut pos = 0;
        for inl in self.inlines.iter_mut() {
            if let Inline::Run(r) = inl {
                let len = r.len();
                if pos >= from && pos + len <= to && len > 0 {
                    f(&mut r.props);
                    r.dirty = true;
                }
                pos += len;
            }
        }
        self.touch();
    }

    /// Direct run properties over `[from, to)`, one entry per run.
    pub fn props_in(&self, from: usize, to: usize) -> Vec<RunProps> {
        let mut v = Vec::new();
        let mut pos = 0;
        for inl in &self.inlines {
            if let Inline::Run(r) = inl {
                let len = r.len();
                if len > 0 && pos < to && pos + len > from {
                    v.push(r.props.clone());
                }
                pos += len;
            }
        }
        v
    }

    /// Word boundaries around `off`.
    pub fn word_at(&self, off: usize) -> (usize, usize) {
        let chars: Vec<char> = self.plain_text().chars().collect();
        let class = |c: char| if c.is_alphanumeric() || c == '\'' || c == '’' || c == '_' { 1 } else if c.is_whitespace() { 0 } else { 2 };
        if chars.is_empty() {
            return (0, 0);
        }
        let i = off.min(chars.len() - 1);
        let k = class(chars[i]);
        let mut a = i;
        while a > 0 && class(chars[a - 1]) == k {
            a -= 1;
        }
        let mut b = i + 1;
        while b < chars.len() && class(chars[b]) == k {
            b += 1;
        }
        // a word takes its trailing spaces, like Word
        if k == 1 {
            while b < chars.len() && chars[b] == ' ' {
                b += 1;
            }
        }
        (a, b)
    }

    /// Next/previous word start for Ctrl+arrows.
    pub fn word_step(&self, off: usize, dir: i32) -> usize {
        let chars: Vec<char> = self.plain_text().chars().collect();
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let mut i = off.min(chars.len());
        if dir > 0 {
            while i < chars.len() && word(chars[i]) {
                i += 1;
            }
            while i < chars.len() && !word(chars[i]) {
                i += 1;
            }
        } else {
            while i > 0 && !word(chars[i - 1]) {
                i -= 1;
            }
            while i > 0 && word(chars[i - 1]) {
                i -= 1;
            }
        }
        i
    }
}

/// A wrapper continued in the next paragraph: an sdt copy without its id
/// (ids must be unique; Word assigns one).
fn reopened(w: &Wrapper) -> Wrapper {
    let mut open = w.open.clone();
    if w.kind == WrapKind::Sdt {
        if let Some(i) = open.find("<w:id ") {
            if let Some(j) = open[i..].find("/>") {
                open.replace_range(i..i + j + 2, "");
            }
        }
    }
    Wrapper { kind: w.kind.clone(), open, close: w.close.clone() }
}

fn insert_into_run(r: &mut Run, mut k: usize, items: Vec<Item>) {
    let mut idx = 0;
    while idx < r.items.len() {
        let len = r.items[idx].len();
        if let Item::Text(t) = &mut r.items[idx] {
            if k <= len {
                if let [Item::Text(s)] = items.as_slice() {
                    let at = t.char_indices().nth(k).map(|(i, _)| i).unwrap_or(t.len());
                    t.insert_str(at, s);
                    return;
                }
                // split the text item around the insertion
                let at = t.char_indices().nth(k).map(|(i, _)| i).unwrap_or(t.len());
                let rest = t.split_off(at);
                let mut tail = items;
                if !rest.is_empty() {
                    tail.push(Item::Text(rest));
                }
                for (j, it) in tail.into_iter().enumerate() {
                    r.items.insert(idx + 1 + j, it);
                }
                return;
            }
        } else if k == 0 {
            for (j, it) in items.into_iter().enumerate() {
                r.items.insert(idx + j, it);
            }
            return;
        }
        k -= len.min(k);
        idx += 1;
    }
    r.items.extend(items);
}

// ---------- headers and footers ----------

/// First path element of a paragraph in a header or footer: this bit plus
/// the story's index in `story_ids`.
pub const STORY: u32 = 0x4000_0000;

/// Header and footer relationship ids in a stable order.
pub fn story_ids(doc: &crate::Document) -> Vec<String> {
    let mut v: Vec<String> = doc.stories.keys().cloned().collect();
    v.sort();
    v
}

pub fn story_index(doc: &crate::Document, rid: &str) -> Option<u32> {
    story_ids(doc).iter().position(|r| r == rid).map(|i| i as u32)
}

pub fn story_rid(doc: &crate::Document, path: &[u32]) -> Option<String> {
    let first = *path.first()?;
    if first & STORY == 0 {
        return None;
    }
    story_ids(doc).get((first & !STORY) as usize).cloned()
}

/// Paragraph at a path of the body or of a header/footer.
pub fn dpara<'a>(doc: &'a crate::Document, path: &[u32]) -> Option<&'a Para> {
    match story_rid(doc, path) {
        Some(rid) => para_at(&doc.stories.get(&rid)?.content, &path[1..]),
        None if path.first().is_some_and(|f| f & STORY != 0) => None,
        None => para_at(&doc.body, path),
    }
}

pub fn dpara_mut<'a>(doc: &'a mut crate::Document, path: &[u32]) -> Option<&'a mut Para> {
    match story_rid(doc, path) {
        Some(rid) => {
            let st = Rc::make_mut(&mut doc.stories).get_mut(&rid)?;
            st.dirty = true;
            para_mut(&mut st.content, &path[1..])
        }
        None if path.first().is_some_and(|f| f & STORY != 0) => None,
        None => para_mut(&mut doc.body, path),
    }
}

pub fn dcont_mut<'a>(doc: &'a mut crate::Document, path: &[u32]) -> Option<&'a mut Container> {
    match story_rid(doc, path) {
        Some(rid) => {
            let st = Rc::make_mut(&mut doc.stories).get_mut(&rid)?;
            st.dirty = true;
            container_mut(&mut st.content, &path[1..])
        }
        None if path.first().is_some_and(|f| f & STORY != 0) => None,
        None => container_mut(&mut doc.body, path),
    }
}

/// Root container of a path and the path inside it.
pub fn droot<'a, 'p>(doc: &'a crate::Document, path: &'p [u32]) -> Option<(&'a Container, &'p [u32])> {
    match story_rid(doc, path) {
        Some(rid) => Some((&doc.stories.get(&rid)?.content, &path[1..])),
        None if path.first().is_some_and(|f| f & STORY != 0) => None,
        None => Some((&doc.body, path)),
    }
}

/// Paragraph paths of the body, or of one header/footer (with its prefix).
pub fn dpaths(doc: &crate::Document, story: Option<u32>) -> Vec<Vec<u32>> {
    match story {
        None => para_paths(&doc.body),
        Some(i) => {
            let Some(rid) = story_ids(doc).get(i as usize).cloned() else { return Vec::new() };
            let Some(st) = doc.stories.get(&rid) else { return Vec::new() };
            para_paths(&st.content)
                .into_iter()
                .map(|p| {
                    let mut v = vec![STORY | i];
                    v.extend(p);
                    v
                })
                .collect()
        }
    }
}
