//! Several people in one document.
//!
//! The docserver puts the changes of all participants into one order and
//! sends every client the changes of the others. A client whose own edits
//! are not confirmed yet rebases them on top with a three-way merge of
//! states: base (last confirmed), mine (the editor now), theirs (base plus
//! the change that came in). Blocks carry ids that live only in memory, and
//! a changed block travels as its XML. Where both sides changed the same
//! blocks, the region is merged atom by atom (characters, run items,
//! paragraph marks): deletions and insertions of both sides are kept, so two
//! people can type in one paragraph. The server applies changes with the same
//! code, so every copy ends up identical.

use crate::comments::{Comment, Comments};
use crate::docx::{self, Package, Rels};
use crate::model::*;
use crate::props::RunProps;
use crate::styles::Numbering;
use crate::xml::{self, Element};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

pub type Id = Rc<str>;

/// Ids of the blocks of the body and of every header and footer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ids {
    pub body: Vec<Id>,
    pub stories: BTreeMap<String, Vec<Id>>,
}

impl Ids {
    /// Ids of a document as opened: `o0`, `o1`... in every container.
    pub fn initial(st: &State) -> Ids {
        let n = |c: &Container| (0..c.len()).map(|i| Id::from(format!("o{i}"))).collect();
        Ids { body: n(&st.body), stories: st.stories.iter().map(|(k, s)| (k.clone(), n(&s.content))).collect() }
    }
}

/// Fresh ids of one client: unique in the room because the server numbers clients.
#[derive(Clone, Debug)]
pub struct Gen {
    prefix: String,
    n: u64,
}

impl Gen {
    pub fn new(prefix: &str) -> Gen {
        Gen { prefix: prefix.to_string(), n: 0 }
    }

    pub fn next(&mut self) -> Id {
        self.n += 1;
        Id::from(format!("{}{}", self.prefix, self.n))
    }
}

/// What collaborative editing changes in a document.
#[derive(Clone)]
pub struct State {
    pub body: Container,
    pub sect: Option<Rc<Element>>,
    pub comments: Rc<Comments>,
    pub stories: Rc<HashMap<String, Story>>,
    pub pkg: Package,
    pub rels: Rc<Rels>,
    pub numbering: Rc<Numbering>,
}

impl State {
    pub fn of(doc: &docx::Document) -> State {
        State {
            body: doc.body.clone(),
            sect: doc.sect.clone(),
            comments: doc.comments.clone(),
            stories: doc.stories.clone(),
            pkg: doc.pkg.clone(),
            rels: doc.rels.clone(),
            numbering: doc.numbering.clone(),
        }
    }

    pub fn install(self, doc: &mut docx::Document) {
        doc.body = self.body;
        doc.sect = self.sect;
        doc.comments = self.comments;
        doc.stories = self.stories;
        doc.pkg = self.pkg;
        doc.rels = self.rels;
        doc.numbering = self.numbering;
    }

    /// The same state (by pointers: cheap, and exact for copy-on-write data).
    pub fn same(&self, o: &State) -> bool {
        same_blocks(&self.body, &o.body)
            && opt_ptr(&self.sect, &o.sect)
            && Rc::ptr_eq(&self.comments, &o.comments)
            && (Rc::ptr_eq(&self.stories, &o.stories)
                || (self.stories.len() == o.stories.len() && self.stories.iter().all(|(k, s)| o.stories.get(k).is_some_and(|t| same_blocks(&s.content, &t.content)))))
            && same_pkg(&self.pkg, &o.pkg)
    }
}

fn opt_ptr<T>(a: &Option<Rc<T>>, b: &Option<Rc<T>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

fn same_blocks(a: &[Rc<Block>], b: &[Rc<Block>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| Rc::ptr_eq(x, y))
}

fn same_pkg(a: &Package, b: &Package) -> bool {
    a.entries.len() == b.entries.len() && a.entries.iter().zip(&b.entries).all(|(x, y)| x.name == y.name && Rc::ptr_eq(&x.data, &y.data))
}

fn part<'a>(p: &'a Package, name: &str) -> Option<&'a Rc<Vec<u8>>> {
    p.entries.iter().find(|e| e.name == name).map(|e| &e.data)
}

// ---------- ids of a changed container ----------

fn text_of(b: &Block) -> Option<String> {
    match b {
        Block::Para(p) => Some(p.plain_text()),
        _ => None,
    }
}

/// How much two blocks look like the same block after an edit, 0..1.
fn similarity(a: &Block, b: &Block) -> f32 {
    match (a, b) {
        (Block::Para(_), Block::Para(_)) => {
            let (x, y) = (text_of(a).unwrap_or_default(), text_of(b).unwrap_or_default());
            let (x, y): (Vec<char>, Vec<char>) = (x.chars().collect(), y.chars().collect());
            let m = x.len().max(y.len());
            if m == 0 {
                return 1.0;
            }
            let pre = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
            let rest = x.len().min(y.len()) - pre;
            let suf = x.iter().rev().zip(y.iter().rev()).take(rest).take_while(|(p, q)| p == q).count();
            (pre + suf) as f32 / m as f32
        }
        (Block::Table(s), Block::Table(t)) => {
            if s.rows().count() == t.rows().count() {
                1.0
            } else {
                0.6
            }
        }
        (Block::Sdt(_), Block::Sdt(_)) | (Block::Raw(_), Block::Raw(_)) => 0.6,
        _ => 0.0,
    }
}

/// Ids for `cur`, an edited version of `prev`: blocks that were not touched
/// keep their ids, edited ones take the id of the block they most look like,
/// the rest get new ids.
pub fn derive(prev: &[Rc<Block>], prev_ids: &[Id], cur: &[Rc<Block>], fresh: &mut Gen) -> Vec<Id> {
    let (pl, cl) = (prev.len(), cur.len());
    let mut pre = 0;
    while pre < pl.min(cl) && Rc::ptr_eq(&prev[pre], &cur[pre]) {
        pre += 1;
    }
    let mut suf = 0;
    while suf < pl.min(cl) - pre && Rc::ptr_eq(&prev[pl - 1 - suf], &cur[cl - 1 - suf]) {
        suf += 1;
    }
    let mut out: Vec<Option<Id>> = vec![None; cl];
    for i in 0..pre {
        out[i] = Some(prev_ids[i].clone());
    }
    for k in 0..suf {
        out[cl - 1 - k] = Some(prev_ids[pl - 1 - k].clone());
    }
    // untouched blocks in the middle (they keep their order)
    let by_ptr: HashMap<*const Block, usize> = (pre..pl - suf).map(|i| (Rc::as_ptr(&prev[i]), i)).collect();
    let mut anchors = vec![(pre.wrapping_sub(1), pre.wrapping_sub(1))];
    let mut last = pre as isize - 1;
    for j in pre..cl - suf {
        if let Some(&i) = by_ptr.get(&Rc::as_ptr(&cur[j])) {
            if i as isize > last {
                last = i as isize;
                out[j] = Some(prev_ids[i].clone());
                anchors.push((i, j));
            }
        }
    }
    anchors.push((pl - suf, cl - suf));
    // between anchors: pair edited blocks by resemblance, in order
    for w in anchors.windows(2) {
        let (p0, c0) = (w[0].0.wrapping_add(1), w[0].1.wrapping_add(1));
        let (p1, c1) = w[1];
        let mut floor = p0;
        for j in c0..c1 {
            let mut best: Option<(usize, f32)> = None;
            for i in floor..p1 {
                let s = similarity(&prev[i], &cur[j]);
                if s >= 0.34 && best.is_none_or(|(_, b)| s > b) {
                    best = Some((i, s));
                }
            }
            if let Some((i, _)) = best {
                out[j] = Some(prev_ids[i].clone());
                floor = i + 1;
            }
        }
    }
    let mut seen = HashSet::new();
    out.into_iter()
        .map(|o| match o {
            Some(id) if seen.insert(id.clone()) => id,
            _ => fresh.next(),
        })
        .collect()
}

// ---------- changes ----------

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Del(Id),
    /// after (None: at the start), id, XML
    Ins(Option<Id>, Id, String),
    Set(Id, String),
}

/// One participant's change: everything needed to bring another copy from
/// the same base to the same state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    /// container ("body" or a header/footer relationship id) → operations
    pub blocks: Vec<(String, Vec<Op>)>,
    /// new section properties of the body (None: removed)
    pub sect: Option<Option<String>>,
    /// comment id → its JSON (None: deleted)
    pub comments: Vec<(String, Option<Value>)>,
    /// new or changed package parts (pictures, numbering)
    pub parts: Vec<(String, Vec<u8>)>,
    /// entries added to `.rels` files and `[Content_Types].xml`: file, key, XML
    pub entries: Vec<(String, String, String)>,
    /// new headers and footers: relationship id, part, root element (open tag)
    pub stories: Vec<(String, String, String)>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|(_, o)| o.is_empty()) && self.sect.is_none() && self.comments.is_empty() && self.parts.is_empty() && self.entries.is_empty() && self.stories.is_empty()
    }

    pub fn to_json(&self) -> Value {
        let ops = |v: &[Op]| -> Vec<Value> {
            v.iter()
                .map(|o| match o {
                    Op::Del(id) => json!(["d", &**id]),
                    Op::Ins(a, id, x) => json!(["i", a.as_deref(), &**id, x]),
                    Op::Set(id, x) => json!(["s", &**id, x]),
                })
                .collect()
        };
        let mut o = serde_json::Map::new();
        if !self.blocks.is_empty() {
            o.insert("blocks".into(), Value::Array(self.blocks.iter().map(|(k, v)| json!([k, ops(v)])).collect()));
        }
        if let Some(s) = &self.sect {
            o.insert("sect".into(), json!(s));
        }
        if !self.comments.is_empty() {
            o.insert("comments".into(), Value::Array(self.comments.iter().map(|(k, v)| json!([k, v])).collect()));
        }
        if !self.parts.is_empty() {
            o.insert("parts".into(), Value::Array(self.parts.iter().map(|(n, d)| json!([n, b64_encode(d)])).collect()));
        }
        if !self.entries.is_empty() {
            o.insert("entries".into(), Value::Array(self.entries.iter().map(|(f, k, x)| json!([f, k, x])).collect()));
        }
        if !self.stories.is_empty() {
            o.insert("stories".into(), Value::Array(self.stories.iter().map(|(r, p, x)| json!([r, p, x])).collect()));
        }
        Value::Object(o)
    }

    pub fn from_json(v: &Value) -> Result<Change, String> {
        let bad = || "bad change".to_string();
        let s = |v: &Value| v.as_str().map(str::to_string).ok_or_else(bad);
        let mut c = Change::default();
        for b in v.get("blocks").and_then(Value::as_array).into_iter().flatten() {
            let key = s(&b[0])?;
            let mut ops = Vec::new();
            for o in b[1].as_array().ok_or_else(bad)? {
                ops.push(match o[0].as_str() {
                    Some("d") => Op::Del(Id::from(s(&o[1])?)),
                    Some("i") => Op::Ins(o[1].as_str().map(Id::from), Id::from(s(&o[2])?), s(&o[3])?),
                    Some("s") => Op::Set(Id::from(s(&o[1])?), s(&o[2])?),
                    _ => return Err(bad()),
                });
            }
            c.blocks.push((key, ops));
        }
        if let Some(x) = v.get("sect") {
            c.sect = Some(x.as_str().map(str::to_string));
        }
        for x in v.get("comments").and_then(Value::as_array).into_iter().flatten() {
            c.comments.push((s(&x[0])?, if x[1].is_null() { None } else { Some(x[1].clone()) }));
        }
        for x in v.get("parts").and_then(Value::as_array).into_iter().flatten() {
            c.parts.push((s(&x[0])?, b64_decode(&s(&x[1])?).ok_or_else(bad)?));
        }
        for x in v.get("entries").and_then(Value::as_array).into_iter().flatten() {
            c.entries.push((s(&x[0])?, s(&x[1])?, s(&x[2])?));
        }
        for x in v.get("stories").and_then(Value::as_array).into_iter().flatten() {
            c.stories.push((s(&x[0])?, s(&x[1])?, s(&x[2])?));
        }
        Ok(c)
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(d: &[u8]) -> String {
    let mut s = String::with_capacity(d.len().div_ceil(3) * 4);
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for ch in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if ch == b'=' {
            break;
        }
        let v = B64.iter().position(|&b| b == ch)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn block_xml(b: &Rc<Block>) -> String {
    docx::blocks_xml(&vec![b.clone()])
}

/// `xmlns:*` declarations of an element, for parsing fragments of its part.
pub fn ns_decls(root: &Element) -> String {
    let mut s = String::new();
    for a in root.attrs.iter().filter(|a| a.qname.starts_with("xmlns")) {
        s.push(' ');
        s.push_str(&a.qname);
        s.push_str("=\"");
        xml::escape_into(&mut s, &a.value, true);
        s.push('"');
    }
    if !s.contains("xmlns:w=") {
        s.push_str(" xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"");
    }
    s
}

fn container_ops(a: &[Rc<Block>], ai: &[Id], b: &[Rc<Block>], bi: &[Id]) -> Vec<Op> {
    if same_blocks(a, b) && ai == bi {
        return Vec::new();
    }
    let apos: HashMap<&Id, usize> = ai.iter().enumerate().map(|(i, id)| (id, i)).collect();
    let bset: HashSet<&Id> = bi.iter().collect();
    let mut ops: Vec<Op> = ai.iter().filter(|id| !bset.contains(id)).map(|id| Op::Del(id.clone())).collect();
    let mut last: Option<usize> = None;
    let mut prev: Option<Id> = None;
    for (j, id) in bi.iter().enumerate() {
        match apos.get(id) {
            Some(&i) if last.is_none_or(|l| i > l) => {
                last = Some(i);
                if !Rc::ptr_eq(&a[i], &b[j]) {
                    ops.push(Op::Set(id.clone(), block_xml(&b[j])));
                }
            }
            // moved: out and in again
            Some(_) => {
                ops.push(Op::Del(id.clone()));
                ops.push(Op::Ins(prev.clone(), id.clone(), block_xml(&b[j])));
            }
            None => ops.push(Op::Ins(prev.clone(), id.clone(), block_xml(&b[j]))),
        }
        prev = Some(id.clone());
    }
    ops
}

fn comment_json(c: &Comment) -> Value {
    json!({"author": c.author, "initials": c.initials, "date": c.date, "paras": c.paras.iter().map(|p| p.to_xml()).collect::<Vec<_>>(),
           "paraId": c.para_id, "parent": c.parent, "done": c.done})
}

fn comment_from(id: &str, v: &Value, ns: &str) -> Option<Comment> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let paras = v
        .get("paras")?
        .as_array()?
        .iter()
        .filter_map(|p| {
            let x = p.as_str()?;
            let root = xml::parse(&format!("<w:body{ns}>{x}</w:body>")).ok()?;
            root.elems().next().cloned()
        })
        .collect();
    Some(Comment {
        id: id.to_string(),
        author: s("author"),
        initials: s("initials"),
        date: s("date"),
        paras,
        para_id: v.get("paraId").and_then(Value::as_str).map(str::to_string),
        parent: v.get("parent").and_then(Value::as_str).map(str::to_string),
        done: v.get("done").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn comment_eq(a: &Comment, b: &Comment) -> bool {
    comment_json(a) == comment_json(b)
}

/// Keys of the entries of a `.rels` file or of `[Content_Types].xml`.
fn xml_entries(data: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(data);
    let Ok(root) = xml::parse(text.trim_start_matches('\u{feff}')) else { return Vec::new() };
    root.elems()
        .filter_map(|e| {
            let key = match e.local.as_str() {
                "Relationship" => e.attr(xml::Ns::None, "Id")?.to_string(),
                "Default" => format!("ext:{}", e.attr(xml::Ns::None, "Extension")?.to_lowercase()),
                "Override" => format!("part:{}", e.attr(xml::Ns::None, "PartName")?),
                _ => return None,
            };
            Some((key, e.to_xml()))
        })
        .collect()
}

fn is_entry_file(name: &str) -> bool {
    name == "[Content_Types].xml" || name.ends_with(".rels")
}

/// Parts the model writes on save (their bytes in the package are stale).
fn model_parts(st: &State) -> HashSet<String> {
    let mut s: HashSet<String> = st.stories.values().map(|x| x.part.clone()).collect();
    for r in st.rels.values() {
        if r.kind == "comments" || r.kind == "commentsExtended" {
            s.insert(r.target.clone());
        }
    }
    s.insert("word/document.xml".into());
    s
}

/// The change that turns `a` into `b`.
pub fn diff(a: &State, ai: &Ids, b: &State, bi: &Ids) -> Change {
    let mut c = Change::default();
    let ops = container_ops(&a.body, &ai.body, &b.body, &bi.body);
    if !ops.is_empty() {
        c.blocks.push(("body".into(), ops));
    }
    if !Rc::ptr_eq(&a.stories, &b.stories) {
        let mut keys: Vec<&String> = b.stories.keys().collect();
        keys.sort();
        for k in keys {
            let sb = &b.stories[k];
            let (no_blocks, no_ids): (Container, Vec<Id>) = (Vec::new(), Vec::new());
            let (ca, ia) = match a.stories.get(k) {
                Some(sa) => (&sa.content, ai.stories.get(k).unwrap_or(&no_ids)),
                None => {
                    c.stories.push((k.clone(), sb.part.clone(), sb.root.open_tag()));
                    (&no_blocks, &no_ids)
                }
            };
            let ops = container_ops(ca, ia, &sb.content, bi.stories.get(k).unwrap_or(&no_ids));
            if !ops.is_empty() {
                c.blocks.push((k.clone(), ops));
            }
        }
    }
    if !opt_ptr(&a.sect, &b.sect) {
        c.sect = Some(b.sect.as_ref().map(|e| e.to_xml()));
    }
    if !Rc::ptr_eq(&a.comments, &b.comments) {
        for cb in &b.comments.items {
            if a.comments.get(&cb.id).is_none_or(|ca| !comment_eq(ca, cb)) {
                c.comments.push((cb.id.clone(), Some(comment_json(cb))));
            }
        }
        for ca in &a.comments.items {
            if b.comments.get(&ca.id).is_none() {
                c.comments.push((ca.id.clone(), None));
            }
        }
    }
    if !same_pkg(&a.pkg, &b.pkg) {
        let skip = model_parts(b);
        for e in &b.pkg.entries {
            let old = part(&a.pkg, &e.name);
            if old.is_some_and(|d| Rc::ptr_eq(d, &e.data)) {
                continue;
            }
            if is_entry_file(&e.name) {
                let have: HashMap<String, String> = old.map(|d| xml_entries(d)).unwrap_or_default().into_iter().collect();
                for (k, x) in xml_entries(&e.data) {
                    if have.get(&k) != Some(&x) {
                        c.entries.push((e.name.clone(), k, x));
                    }
                }
            } else if !skip.contains(&e.name) {
                c.parts.push((e.name.clone(), (*e.data).clone()));
            }
        }
    }
    c
}

/// Adds or replaces the entry `key` of a `.rels` file or `[Content_Types].xml`.
fn put_entry(pkg: &mut Package, file: &str, key: &str, xml_: &str) {
    let text = part(pkg, file).map(|d| String::from_utf8_lossy(d).into_owned()).unwrap_or_else(|| {
        if file.ends_with(".rels") {
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".into()
        } else {
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"></Types>".into()
        }
    });
    let have = xml_entries(text.as_bytes());
    let mut t = text.clone();
    if let Some((_, old)) = have.iter().find(|(k, _)| k == key) {
        if old == xml_ {
            return;
        }
        // entries are written back as parsed: find the element by its key attribute
        if let Some(i) = find_entry(&t, key) {
            let end = t[i..].find("/>").map(|e| i + e + 2).or_else(|| t[i..].find('>').map(|e| i + e + 1));
            if let Some(end) = end {
                t.replace_range(i..end, xml_);
            }
        }
    } else {
        let close = if file.ends_with(".rels") { "</Relationships>" } else { "</Types>" };
        match t.rfind(close) {
            Some(i) => t.insert_str(i, xml_),
            None => return,
        }
    }
    pkg.set(file, t.into_bytes());
}

fn find_entry(t: &str, key: &str) -> Option<usize> {
    let (tag, attr, val) = if let Some(e) = key.strip_prefix("ext:") {
        ("<Default", "Extension", e)
    } else if let Some(p) = key.strip_prefix("part:") {
        ("<Override", "PartName", p)
    } else {
        ("<Relationship", "Id", key)
    };
    let mut from = 0;
    while let Some(i) = t[from..].find(tag).map(|i| i + from) {
        let end = t[i..].find('>').map(|e| i + e).unwrap_or(t.len());
        let el = &t[i..end];
        if el.contains(&format!("{attr}=\"{val}\"")) || (attr == "Extension" && el.to_lowercase().contains(&format!("extension=\"{}\"", val.to_lowercase()))) {
            return Some(i);
        }
        from = end;
    }
    None
}

fn numbering_part(st: &State) -> Option<String> {
    st.rels.values().find(|r| r.kind == "numbering").map(|r| r.target.clone())
}

fn reload(st: &mut State, doc_part: &str) {
    if let Ok(r) = docx::read_rels(&st.pkg, doc_part) {
        st.rels = Rc::new(r);
    }
    if let Some(p) = numbering_part(st) {
        if let Ok(Some(root)) = st.pkg.xml(&p) {
            st.numbering = Rc::new(Numbering::parse(&root));
        }
    }
}

/// Applies `c` to `st` (and its ids). `root` is the main part's root element
/// (its namespace declarations parse the blocks).
pub fn apply(st: &mut State, ids: &mut Ids, c: &Change, doc_part: &str, root: &Element) -> Result<(), String> {
    for (name, data) in &c.parts {
        st.pkg.set(name, data.clone());
    }
    for (file, key, x) in &c.entries {
        put_entry(&mut st.pkg, file, key, x);
    }
    if !c.parts.is_empty() || !c.entries.is_empty() {
        reload(st, doc_part);
    }
    let ns = ns_decls(root);
    for (rid, part_, open) in &c.stories {
        let full = format!("{open}{}", open.trim_start_matches('<').split([' ', '>']).next().map(|n| format!("</{n}>")).unwrap_or_default());
        let el = xml::parse(&full).map_err(|e| e.0)?;
        Rc::make_mut(&mut st.stories).insert(rid.clone(), Story { part: part_.clone(), root: Rc::new(el), content: Vec::new(), dirty: true });
        ids.stories.insert(rid.clone(), Vec::new());
    }
    for (key, ops) in &c.blocks {
        if key == "body" {
            let rels = st.rels.clone();
            let parse = |x: &str| docx::parse_blocks(x, &ns, doc_part, &rels).ok().and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)));
            apply_ops(&mut st.body, &mut ids.body, ops, &parse);
        } else {
            let Some(story) = st.stories.get(key) else { continue };
            let sns = ns_decls(&story.root);
            let spart = story.part.clone();
            let rels = docx::read_rels(&st.pkg, &spart).unwrap_or_default();
            let parse = |x: &str| docx::parse_blocks(x, &sns, &spart, &rels).ok().and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)));
            let stories = Rc::make_mut(&mut st.stories);
            let s = stories.get_mut(key).unwrap();
            let sids = ids.stories.entry(key.clone()).or_default();
            apply_ops(&mut s.content, sids, ops, &parse);
            s.dirty = true;
        }
    }
    if let Some(s) = &c.sect {
        st.sect = match s {
            Some(x) => Some(Rc::new(xml::parse(&format!("<w:body{ns}>{x}</w:body>")).map_err(|e| e.0)?.elems().next().cloned().ok_or("bad sectPr")?)),
            None => None,
        };
    }
    if !c.comments.is_empty() {
        let cm = Rc::make_mut(&mut st.comments);
        for (id, v) in &c.comments {
            let at = cm.items.iter().position(|x| &x.id == id);
            match (v.as_ref().and_then(|v| comment_from(id, v, &ns)), at) {
                (Some(new), Some(i)) => cm.items[i] = new,
                (Some(new), None) => cm.items.push(new),
                (None, Some(i)) => {
                    cm.items.remove(i);
                }
                (None, None) => {}
            }
        }
        cm.dirty = true;
    }
    Ok(())
}

fn apply_ops(c: &mut Container, ids: &mut Vec<Id>, ops: &[Op], parse: &dyn Fn(&str) -> Option<Rc<Block>>) {
    for op in ops {
        match op {
            Op::Del(id) => {
                if let Some(i) = ids.iter().position(|x| x == id) {
                    ids.remove(i);
                    c.remove(i);
                }
            }
            Op::Ins(after, id, x) => {
                let Some(b) = parse(x) else { continue };
                let at = match after {
                    None => 0,
                    Some(a) => ids.iter().position(|x| x == a).map(|i| i + 1).unwrap_or(ids.len()),
                };
                ids.insert(at, id.clone());
                c.insert(at, b);
            }
            Op::Set(id, x) => {
                if let (Some(i), Some(b)) = (ids.iter().position(|x| x == id), parse(x)) {
                    c[i] = b;
                }
            }
        }
    }
}

// ---------- three-way merge ----------

/// A caret to carry through a merge: top-level block index in `mine`, the
/// path below it and the offset; `to` is filled with the new index and offset.
#[derive(Clone, Debug)]
pub struct Caret {
    pub idx: usize,
    pub sub: Vec<u32>,
    pub off: usize,
    pub to: Option<(usize, usize)>,
}

#[derive(Clone)]
enum Kind {
    Ch(char),
    It(Item),
}

#[derive(Clone)]
enum Atom {
    Run { item: Kind, shell: Rc<Run>, props: RunProps },
    Inl(Inline),
    Mark { id: Id, block: Rc<Block> },
    Blk { id: Id, block: Rc<Block> },
    Caret(usize),
}

fn h<T: Hash>(t: T) -> u64 {
    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}

fn key(a: &Atom) -> u64 {
    match a {
        Atom::Run { item: Kind::Ch(c), .. } => h(("c", c)),
        Atom::Run { item: Kind::It(i), .. } => match i {
            Item::Tab => h("tab"),
            Item::Break(k) => h(("br", *k as u8)),
            Item::Image(d) => h(("img", d.el.to_xml())),
            Item::Raw(e) => h(("ri", e.to_xml())),
            Item::Text(t) => h(("t", t)),
        },
        Atom::Inl(Inline::Open(w)) => h(("o", &w.open, &w.close)),
        Atom::Inl(Inline::Close(w)) => h(("x", &w.open, &w.close)),
        Atom::Inl(Inline::Raw(e)) => h(("r", e.to_xml())),
        Atom::Inl(Inline::Run(_)) => h("run"),
        Atom::Mark { id, .. } => h(("m", &**id)),
        Atom::Blk { id, .. } => h(("b", &**id)),
        Atom::Caret(n) => h(("\u{0}caret", n)),
    }
}

/// Atoms of blocks; carets (index in `blocks`, offset, number) go in as atoms.
fn atomize(blocks: &[Rc<Block>], ids: &[Id], carets: &[(usize, usize, usize)]) -> Vec<Atom> {
    let mut out = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let here: Vec<&(usize, usize, usize)> = carets.iter().filter(|c| c.0 == i).collect();
        match &**b {
            Block::Para(p) => {
                let mut off = 0;
                let put = |out: &mut Vec<Atom>, off: usize, last: bool| {
                    for c in &here {
                        if c.1 == off || (last && c.1 > off) {
                            out.push(Atom::Caret(c.2));
                        }
                    }
                };
                for inl in &p.inlines {
                    match inl {
                        Inline::Run(r) => {
                            let shell = Rc::new(r.shell());
                            for it in &r.items {
                                match it {
                                    Item::Text(t) => {
                                        for ch in t.chars() {
                                            put(&mut out, off, false);
                                            out.push(Atom::Run { item: Kind::Ch(ch), shell: shell.clone(), props: r.props.clone() });
                                            off += 1;
                                        }
                                    }
                                    other => {
                                        if other.len() > 0 {
                                            put(&mut out, off, false);
                                        }
                                        out.push(Atom::Run { item: Kind::It(other.clone()), shell: shell.clone(), props: r.props.clone() });
                                        off += other.len();
                                    }
                                }
                            }
                        }
                        other => out.push(Atom::Inl(other.clone())),
                    }
                }
                put(&mut out, off, true);
                out.push(Atom::Mark { id: ids[i].clone(), block: b.clone() });
            }
            _ => out.push(Atom::Blk { id: ids[i].clone(), block: b.clone() }),
        }
    }
    out
}

/// Matching pairs of a longest common subsequence (Myers); None when the
/// sequences differ in too many places.
fn lcs(a: &[u64], b: &[u64]) -> Option<Vec<(usize, usize)>> {
    let mut pre = 0;
    while pre < a.len().min(b.len()) && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len().min(b.len()) - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let (x, y) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut pairs: Vec<(usize, usize)> = (0..pre).map(|i| (i, i)).collect();
    let (n, m) = (x.len() as isize, y.len() as isize);
    if n > 0 && m > 0 {
        let cap = (n + m).min(2000);
        let off = cap + 1;
        let mut v = vec![0isize; (2 * cap + 3) as usize];
        let mut trace: Vec<Vec<isize>> = Vec::new();
        let mut found = None;
        'outer: for d in 0..=cap {
            trace.push(v.clone());
            let mut k = -d;
            while k <= d {
                let mut px = if k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]) { v[(off + k + 1) as usize] } else { v[(off + k - 1) as usize] + 1 };
                let mut py = px - k;
                while px < n && py < m && x[px as usize] == y[py as usize] {
                    px += 1;
                    py += 1;
                }
                v[(off + k) as usize] = px;
                if px >= n && py >= m {
                    found = Some(d);
                    break 'outer;
                }
                k += 2;
            }
        }
        let d_end = found?;
        let (mut px, mut py) = (n, m);
        let mut mid = Vec::new();
        for d in (1..=d_end).rev() {
            let v = &trace[d as usize];
            let k = px - py;
            let pk = if k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]) { k + 1 } else { k - 1 };
            let sx = v[(off + pk) as usize];
            let sy = sx - pk;
            while px > sx && py > sy {
                px -= 1;
                py -= 1;
                mid.push((px as usize, py as usize));
            }
            px = sx;
            py = sy;
        }
        while px > 0 && py > 0 {
            px -= 1;
            py -= 1;
            mid.push((px as usize, py as usize));
        }
        mid.reverse();
        pairs.extend(mid.into_iter().map(|(i, j)| (i + pre, j + pre)));
    }
    pairs.extend((0..suf).map(|k| (a.len() - suf + k, b.len() - suf + k)));
    Some(pairs)
}

/// For every atom of `a`: its match in the other sequence; for every gap of
/// `a` (before atom g, g = len at the end): the atoms inserted there.
fn script(alen: usize, blen: usize, pairs: &[(usize, usize)]) -> (Vec<Option<usize>>, Vec<Vec<usize>>) {
    let mut keep = vec![None; alen];
    let mut ins = vec![Vec::new(); alen + 1];
    let mut j = 0;
    for &(pa, pb) in pairs.iter().chain(std::iter::once(&(alen, blen))) {
        while j < pb {
            ins[pa].push(j);
            j += 1;
        }
        if pa < alen {
            keep[pa] = Some(pb);
        }
        j = pb + 1;
    }
    (keep, ins)
}

fn para_shell_changed(a: &Block, b: &Block) -> bool {
    match (a, b) {
        (Block::Para(x), Block::Para(y)) => !(opt_ptr(&x.ppr, &y.ppr) && x.props == y.props && x.mark == y.mark && x.open == y.open),
        _ => true,
    }
}

fn merge_kept(a: &Atom, b: &Atom, c: &Atom, fresh: &mut Gen) -> Atom {
    match (a, b, c) {
        (Atom::Run { props: pa, .. }, Atom::Run { props: pb, .. }, Atom::Run { .. }) => {
            if pb != pa {
                b.clone()
            } else {
                c.clone()
            }
        }
        (Atom::Mark { block: ba, .. }, Atom::Mark { block: bb, .. }, Atom::Mark { .. }) => {
            if para_shell_changed(ba, bb) {
                b.clone()
            } else {
                c.clone()
            }
        }
        (Atom::Blk { block: ba, .. }, Atom::Blk { id, block: bb }, Atom::Blk { block: bc, .. }) => {
            let block = if Rc::ptr_eq(ba, bb) {
                bc.clone()
            } else if Rc::ptr_eq(ba, bc) {
                bb.clone()
            } else {
                merge_block(ba, bb, bc, fresh)
            };
            Atom::Blk { id: id.clone(), block }
        }
        _ => b.clone(),
    }
}

/// Atoms of `a` merged with the edits of `b` (mine) and `c` (theirs).
fn merge_atoms(a: &[Atom], b: &[Atom], c: &[Atom], fresh: &mut Gen) -> Option<Vec<Atom>> {
    let (ka, kb, kc): (Vec<u64>, Vec<u64>, Vec<u64>) = (a.iter().map(key).collect(), b.iter().map(key).collect(), c.iter().map(key).collect());
    let (keep_b, ins_b) = script(a.len(), b.len(), &lcs(&ka, &kb)?);
    let (keep_c, ins_c) = script(a.len(), c.len(), &lcs(&ka, &kc)?);
    let mut out = Vec::with_capacity(a.len().max(b.len()).max(c.len()));
    for g in 0..=a.len() {
        // a caret with nothing of mine before it stays left of their text
        let lead = ins_b[g].iter().take_while(|&&j| matches!(b[j], Atom::Caret(_))).count();
        out.extend(ins_b[g][..lead].iter().map(|&j| b[j].clone()));
        out.extend(ins_c[g].iter().map(|&k| c[k].clone()));
        out.extend(ins_b[g][lead..].iter().map(|&j| b[j].clone()));
        if g < a.len() {
            if let (Some(j), Some(k)) = (keep_b[g], keep_c[g]) {
                out.push(merge_kept(&a[g], &b[j], &c[k], fresh));
            }
        }
    }
    Some(out)
}

fn new_para(shell: &Block, inlines: Vec<Inline>) -> Para {
    match shell {
        Block::Para(p) => Para {
            rev: next_rev(),
            open: p.open.clone(),
            close: p.close.clone(),
            ppr: p.ppr.clone(),
            props: p.props.clone(),
            mark: p.mark.clone(),
            props_dirty: p.props_dirty,
            inlines,
            orig: None,
        },
        _ => {
            let mut p = Para::new(Default::default(), Default::default());
            p.inlines = inlines;
            p
        }
    }
}

/// Blocks (and ids) back from atoms; carets found among them are reported
/// as (number, block index, offset).
fn rebuild(atoms: Vec<Atom>, fresh: &mut Gen) -> (Container, Vec<Id>, Vec<(usize, usize, usize)>) {
    let mut out: Container = Vec::new();
    let mut ids = Vec::new();
    let mut carets = Vec::new();
    let mut inl: Vec<Inline> = Vec::new();
    let mut run: Option<(Rc<Run>, Run)> = None;
    let mut off = 0;
    let mut last_shell: Option<Rc<Block>> = None;
    fn flush(run: &mut Option<(Rc<Run>, Run)>, inl: &mut Vec<Inline>) {
        if let Some((_, r)) = run.take() {
            inl.push(Inline::Run(r));
        }
    }
    for a in atoms {
        match a {
            Atom::Run { item, shell, props } => {
                let same = run.as_ref().is_some_and(|(s, r)| Rc::ptr_eq(s, &shell) && r.props == props);
                if !same {
                    flush(&mut run, &mut inl);
                    let dirty = shell.dirty || props != shell.props;
                    let r = Run { open: shell.open.clone(), rpr: shell.rpr.clone(), props, dirty, items: Vec::new() };
                    run = Some((shell, r));
                }
                let r = &mut run.as_mut().unwrap().1;
                match item {
                    Kind::Ch(ch) => {
                        if let Some(Item::Text(t)) = r.items.last_mut() {
                            t.push(ch);
                        } else {
                            r.items.push(Item::Text(ch.to_string()));
                        }
                        off += 1;
                    }
                    Kind::It(it) => {
                        off += it.len();
                        r.items.push(it);
                    }
                }
            }
            Atom::Inl(i) => {
                flush(&mut run, &mut inl);
                inl.push(i);
            }
            Atom::Caret(n) => carets.push((n, out.len(), off)),
            Atom::Mark { id, block } => {
                flush(&mut run, &mut inl);
                out.push(Rc::new(Block::Para(new_para(&block, std::mem::take(&mut inl)))));
                ids.push(id);
                last_shell = Some(block);
                off = 0;
            }
            Atom::Blk { id, block } => {
                flush(&mut run, &mut inl);
                if !inl.is_empty() {
                    let shell = last_shell.clone().unwrap_or_else(|| Rc::new(Block::Para(Para::new(Default::default(), Default::default()))));
                    out.push(Rc::new(Block::Para(new_para(&shell, std::mem::take(&mut inl)))));
                    ids.push(fresh.next());
                }
                out.push(block);
                ids.push(id);
                off = 0;
            }
        }
    }
    flush(&mut run, &mut inl);
    if !inl.is_empty() {
        let shell = last_shell.unwrap_or_else(|| Rc::new(Block::Para(Para::new(Default::default(), Default::default()))));
        out.push(Rc::new(Block::Para(new_para(&shell, inl))));
        ids.push(fresh.next());
    }
    (out, ids, carets)
}

fn shape(t: &Table) -> Vec<usize> {
    t.rows().map(|r| r.cells().count()).collect()
}

fn merge_nested(a: &Container, b: &Container, c: &Container, fresh: &mut Gen) -> Container {
    let ai: Vec<Id> = (0..a.len()).map(|i| Id::from(format!("n{i}"))).collect();
    let bi = derive(a, &ai, b, fresh);
    let ci = derive(a, &ai, c, fresh);
    merge_container(a, &ai, b, &bi, c, &ci, fresh, &mut []).0
}

/// A block both sides changed: tables cell by cell when their shape stayed.
fn merge_block(a: &Rc<Block>, b: &Rc<Block>, c: &Rc<Block>, fresh: &mut Gen) -> Rc<Block> {
    match (&**a, &**b, &**c) {
        (Block::Table(ta), Block::Table(tb), Block::Table(tc)) => {
            let (sa, sb, sc) = (shape(ta), shape(tb), shape(tc));
            if sa == sb && sa == sc {
                let mut t = tb.clone();
                t.orig = None;
                if tb.grid == ta.grid {
                    t.grid = tc.grid.clone();
                }
                let rows_a: Vec<&Row> = ta.rows().collect();
                let rows_c: Vec<&Row> = tc.rows().collect();
                for (ri, sh) in sa.iter().enumerate() {
                    for ci in 0..*sh {
                        let ca = rows_a[ri].cells().nth(ci).unwrap();
                        let cc = rows_c[ri].cells().nth(ci).unwrap();
                        let cb = t.row_mut(ri).unwrap().cell_mut(ci).unwrap();
                        cb.content = merge_nested(&ca.content, &cb.content, &cc.content, fresh);
                    }
                }
                Rc::new(Block::Table(t))
            } else if sb != sa {
                b.clone()
            } else {
                c.clone()
            }
        }
        (Block::Sdt(xa), Block::Sdt(xb), Block::Sdt(xc)) if xb.open == xa.open && xc.open == xa.open => {
            let mut s = xb.clone();
            s.content = merge_nested(&xa.content, &xb.content, &xc.content, fresh);
            Rc::new(Block::Sdt(s))
        }
        _ => b.clone(),
    }
}

/// Three-way merge of a container: regions where only one side changed take
/// that side; regions both changed are merged atom by atom.
pub fn merge_container(a: &[Rc<Block>], ai: &[Id], b: &[Rc<Block>], bi: &[Id], c: &[Rc<Block>], ci: &[Id], fresh: &mut Gen, carets: &mut [Caret]) -> (Container, Vec<Id>) {
    let bpos: HashMap<&Id, usize> = bi.iter().enumerate().map(|(i, x)| (x, i)).collect();
    let cpos: HashMap<&Id, usize> = ci.iter().enumerate().map(|(i, x)| (x, i)).collect();
    // blocks nobody touched, in order on all sides
    let mut anchors: Vec<(usize, usize, usize)> = Vec::new();
    let (mut lj, mut lk) = (-1isize, -1isize);
    for (i, id) in ai.iter().enumerate() {
        if let (Some(&j), Some(&k)) = (bpos.get(id), cpos.get(id)) {
            if Rc::ptr_eq(&a[i], &b[j]) && Rc::ptr_eq(&a[i], &c[k]) && j as isize > lj && k as isize > lk && !carets.iter().any(|x| x.idx == j) {
                anchors.push((i, j, k));
                lj = j as isize;
                lk = k as isize;
            }
        }
    }
    anchors.push((a.len(), b.len(), c.len()));
    let mut out: Container = Vec::with_capacity(c.len().max(b.len()));
    let mut ids: Vec<Id> = Vec::with_capacity(out.capacity());
    let (mut i0, mut j0, mut k0) = (0, 0, 0);
    for &(i1, j1, k1) in &anchors {
        let (ra, rb, rc) = (&a[i0..i1], &b[j0..j1], &c[k0..k1]);
        let b_same = same_blocks(ra, rb);
        let c_same = same_blocks(ra, rc);
        let here: Vec<usize> = (0..carets.len()).filter(|&n| carets[n].idx >= j0 && carets[n].idx < j1).collect();
        let base = out.len();
        if c_same || b_same {
            // one side only: take it as is (same blocks, same ids)
            let (blocks, bid) = if c_same { (rb, &bi[j0..j1]) } else { (rc, &ci[k0..k1]) };
            out.extend(blocks.iter().cloned());
            ids.extend(bid.iter().cloned());
            if c_same {
                for &n in &here {
                    carets[n].to = Some((base + carets[n].idx - j0, carets[n].off));
                }
            } else if !here.is_empty() {
                // my caret where they edited: the atoms only tell where it goes now
                let cs: Vec<(usize, usize, usize)> = here.iter().filter(|&&n| carets[n].sub.is_empty()).map(|&n| (carets[n].idx - j0, carets[n].off, n)).collect();
                let (aa, ab, ac) = (atomize(ra, &ai[i0..i1], &[]), atomize(rb, &bi[j0..j1], &cs), atomize(rc, &ci[k0..k1], &[]));
                if let Some(m) = merge_atoms(&aa, &ab, &ac, fresh) {
                    let (_, _, found) = rebuild(m, fresh);
                    for (n, bix, off) in found {
                        carets[n].to = Some((base + bix.min(rc.len().saturating_sub(1)), off));
                    }
                }
                for &n in &here {
                    if carets[n].to.is_none() {
                        let id = &bi[carets[n].idx];
                        let at = ids[base..].iter().position(|x| x == id).map(|p| base + p).unwrap_or(out.len().saturating_sub(1));
                        carets[n].to = Some((at, carets[n].off));
                    }
                }
            }
        } else {
            // both changed: atom by atom
            let cs: Vec<(usize, usize, usize)> = here.iter().filter(|&&n| carets[n].sub.is_empty()).map(|&n| (carets[n].idx - j0, carets[n].off, n)).collect();
            let (aa, ab, ac) = (atomize(ra, &ai[i0..i1], &[]), atomize(rb, &bi[j0..j1], &cs), atomize(rc, &ci[k0..k1], &[]));
            match merge_atoms(&aa, &ab, &ac, fresh) {
                Some(m) => {
                    let (blocks, bid, found) = rebuild(m, fresh);
                    for (n, bix, off) in found {
                        carets[n].to = Some((base + bix.min(blocks.len().saturating_sub(1)), off));
                    }
                    out.extend(blocks);
                    ids.extend(bid);
                }
                // too different: mine wins in this region
                None => {
                    out.extend(rb.iter().cloned());
                    ids.extend(bi[j0..j1].iter().cloned());
                }
            }
            // carets inside tables: same block id, clamped later
            for &n in &here {
                if carets[n].to.is_none() {
                    let id = &bi[carets[n].idx];
                    let at = ids[base..].iter().position(|x| x == id).map(|p| base + p).unwrap_or(out.len().saturating_sub(1));
                    carets[n].to = Some((at, carets[n].off));
                }
            }
        }
        if i1 < a.len() {
            out.push(b[j1].clone());
            ids.push(bi[j1].clone());
            for x in carets.iter_mut().filter(|x| x.idx == j1) {
                x.to = Some((out.len() - 1, x.off));
            }
        }
        (i0, j0, k0) = (i1 + 1, j1 + 1, k1 + 1);
    }
    // ids must stay unique
    let mut seen = HashSet::new();
    for id in ids.iter_mut() {
        if !seen.insert(id.clone()) {
            *id = fresh.next();
            seen.insert(id.clone());
        }
    }
    (out, ids)
}

fn merge_entry_file(a: Option<&Rc<Vec<u8>>>, b: &Rc<Vec<u8>>, c: Option<&Rc<Vec<u8>>>, name: &str) -> Vec<u8> {
    // start from theirs, add what mine added or changed
    let mut pkg = Package::default();
    let Some(c) = c else { return (**b).clone() };
    pkg.set(name, (**c).clone());
    let had: HashMap<String, String> = a.map(|d| xml_entries(d)).unwrap_or_default().into_iter().collect();
    for (k, x) in xml_entries(b) {
        if had.get(&k) != Some(&x) {
            put_entry(&mut pkg, name, &k, &x);
        }
    }
    (**part(&pkg, name).unwrap()).clone()
}

/// Numbering of both sides: definitions by id, mine winning where I changed one.
fn merge_numbering(a: Option<&Rc<Vec<u8>>>, b: &Rc<Vec<u8>>, c: &Rc<Vec<u8>>) -> Vec<u8> {
    let parse = |d: &[u8]| xml::parse(String::from_utf8_lossy(d).trim_start_matches('\u{feff}')).ok();
    let (Some(eb), Some(mut ec)) = (parse(b), parse(c)) else { return (**b).clone() };
    let id = |e: &Element| e.wattr("abstractNumId").or_else(|| e.wattr("numId")).map(|v| format!("{}:{v}", e.local));
    let ea = a.and_then(|d| parse(d));
    let base: HashMap<String, String> = ea.iter().flat_map(|e| e.elems()).filter_map(|e| Some((id(e)?, e.to_xml()))).collect();
    for e in eb.elems() {
        let Some(k) = id(e) else { continue };
        let x = e.to_xml();
        if base.get(&k) == Some(&x) {
            continue;
        }
        let pos = ec.children.iter().position(|n| matches!(n, xml::Node::Elem(t) if id(t).as_deref() == Some(&k)));
        match pos {
            Some(p) => ec.children[p] = xml::Node::Elem(e.clone()),
            None => {
                // abstract definitions come before the instances
                let at = if e.local == "abstractNum" {
                    ec.children.iter().position(|n| matches!(n, xml::Node::Elem(t) if t.local == "num")).unwrap_or(ec.children.len())
                } else {
                    ec.children.len()
                };
                ec.children.insert(at, xml::Node::Elem(e.clone()));
            }
        }
    }
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n{}", ec.to_xml()).into_bytes()
}

/// `mine` rebased on `theirs` (both descend from `base`). Carets are positions in `mine`'s body.
#[allow(clippy::too_many_arguments)]
pub fn merge(base: &State, bids: &Ids, mine: &State, mids: &Ids, theirs: &State, tids: &Ids, doc_part: &str, fresh: &mut Gen, carets: &mut [Caret]) -> (State, Ids) {
    let mut st = theirs.clone();
    let mut ids = tids.clone();
    let (body, body_ids) = merge_container(&base.body, &bids.body, &mine.body, &mids.body, &theirs.body, &tids.body, fresh, carets);
    st.body = body;
    ids.body = body_ids;
    if !Rc::ptr_eq(&mine.stories, &base.stories) {
        let stories = Rc::make_mut(&mut st.stories);
        let empty = Vec::new();
        for (k, sm) in mine.stories.iter() {
            let sb = base.stories.get(k);
            if sb.is_some_and(|sb| same_blocks(&sb.content, &sm.content)) {
                continue;
            }
            match (sb, theirs.stories.get(k)) {
                (Some(sb), Some(stt)) => {
                    let (c, i) = merge_container(&sb.content, bids.stories.get(k).unwrap_or(&empty), &sm.content, mids.stories.get(k).unwrap_or(&empty), &stt.content, tids.stories.get(k).unwrap_or(&empty), fresh, &mut []);
                    let s = stories.get_mut(k).unwrap();
                    s.content = c;
                    s.dirty = true;
                    ids.stories.insert(k.clone(), i);
                }
                _ => {
                    stories.insert(k.clone(), sm.clone());
                    ids.stories.insert(k.clone(), mids.stories.get(k).cloned().unwrap_or_default());
                }
            }
        }
    }
    if !opt_ptr(&mine.sect, &base.sect) {
        st.sect = mine.sect.clone();
    }
    if !Rc::ptr_eq(&mine.comments, &base.comments) {
        let cm = Rc::make_mut(&mut st.comments);
        for c in &mine.comments.items {
            let was = base.comments.get(&c.id);
            if was.is_some_and(|w| comment_eq(w, c)) {
                continue;
            }
            match cm.items.iter().position(|x| x.id == c.id) {
                Some(i) => cm.items[i] = c.clone(),
                None => cm.items.push(c.clone()),
            }
        }
        for w in &base.comments.items {
            if mine.comments.get(&w.id).is_none() {
                cm.items.retain(|x| x.id != w.id);
            }
        }
        cm.dirty = true;
    }
    if !same_pkg(&mine.pkg, &base.pkg) {
        let num = numbering_part(mine);
        for e in &mine.pkg.entries {
            let old = part(&base.pkg, &e.name);
            if old.is_some_and(|d| Rc::ptr_eq(d, &e.data)) {
                continue;
            }
            let theirs_part = part(&theirs.pkg, &e.name);
            let data = match theirs_part {
                Some(t) if old.is_some_and(|o| Rc::ptr_eq(o, t)) || old.is_none() && Rc::ptr_eq(t, &e.data) => (*e.data).clone(),
                None => (*e.data).clone(),
                Some(t) => {
                    if is_entry_file(&e.name) {
                        merge_entry_file(old, &e.data, Some(t), &e.name)
                    } else if num.as_deref() == Some(e.name.as_str()) {
                        merge_numbering(old, &e.data, t)
                    } else {
                        (*e.data).clone()
                    }
                }
            };
            st.pkg.set(&e.name, data);
        }
        reload(&mut st, doc_part);
    }
    (st, ids)
}

/// Ids for `cur`, an edited version of `prev` (every container).
pub fn derive_ids(prev: &State, pids: &Ids, cur: &State, fresh: &mut Gen) -> Ids {
    let body = if same_blocks(&prev.body, &cur.body) && pids.body.len() == cur.body.len() { pids.body.clone() } else { derive(&prev.body, &pids.body, &cur.body, fresh) };
    let mut stories = BTreeMap::new();
    for (k, s) in cur.stories.iter() {
        let ids = match (prev.stories.get(k), pids.stories.get(k)) {
            (Some(p), Some(i)) if same_blocks(&p.content, &s.content) && i.len() == s.content.len() => i.clone(),
            (Some(p), Some(i)) => derive(&p.content, i, &s.content, fresh),
            _ => (0..s.content.len()).map(|_| fresh.next()).collect(),
        };
        stories.insert(k.clone(), ids);
    }
    Ids { body, stories }
}

/// One participant's side: the confirmed state, the change on its way to
/// the server, and what the editor holds now.
pub struct Client {
    pub version: u64,
    pub fresh: Gen,
    base: State,
    base_ids: Ids,
    local: State,
    local_ids: Ids,
    /// the change sent and not answered yet: one at a time, every send gets
    /// exactly one answer, in order
    inflight: Option<(State, Ids)>,
    /// a change of someone else came first: the server will refuse it
    doomed: bool,
}

impl Client {
    pub fn new(st: State, ids: Ids, prefix: &str, version: u64) -> Client {
        Client { version, fresh: Gen::new(prefix), base: st.clone(), base_ids: ids.clone(), local: st, local_ids: ids, inflight: None, doomed: false }
    }

    /// Follows the editor: ids for its state now.
    pub fn track(&mut self, cur: State) {
        if cur.same(&self.local) {
            return;
        }
        self.local_ids = derive_ids(&self.local, &self.local_ids, &cur, &mut self.fresh);
        self.local = cur;
    }

    pub fn local(&self) -> &State {
        &self.local
    }

    /// Ids of the editor's state as last tracked (diagnostics, tests).
    pub fn local_ids(&self) -> &Ids {
        &self.local_ids
    }

    pub fn base_ids(&self) -> &Ids {
        &self.base_ids
    }

    /// Edits not confirmed by the server yet.
    pub fn pending(&self) -> bool {
        self.inflight.is_some() || !self.local.same(&self.base)
    }

    pub fn waiting(&self) -> bool {
        self.inflight.is_some()
    }

    /// The change to send, when there is one and nothing is on its way.
    pub fn take(&mut self) -> Option<Change> {
        if self.inflight.is_some() {
            return None;
        }
        let c = diff(&self.base, &self.base_ids, &self.local, &self.local_ids);
        if c.is_empty() {
            return None;
        }
        self.inflight = Some((self.local.clone(), self.local_ids.clone()));
        Some(c)
    }

    pub fn ack(&mut self, version: u64) {
        let doomed = std::mem::take(&mut self.doomed);
        if let Some((s, i)) = self.inflight.take() {
            // (a doomed change cannot be accepted: someone else's came first)
            if !doomed {
                self.base = s;
                self.base_ids = i;
                self.version = version;
            }
        }
    }

    pub fn nack(&mut self) {
        self.inflight = None;
        self.doomed = false;
    }

    /// Someone else's change: the new local state (mine rebased on it).
    pub fn remote(&mut self, c: &Change, version: u64, doc_part: &str, root: &Element, carets: &mut [Caret]) -> Result<State, String> {
        let mut theirs = self.base.clone();
        let mut tids = self.base_ids.clone();
        apply(&mut theirs, &mut tids, c, doc_part, root)?;
        // what is on its way was made on the old base: the server refuses it,
        // its answer is still to come
        if self.inflight.is_some() {
            self.doomed = true;
        }
        let (m, mids) = if self.local.same(&self.base) && carets.is_empty() {
            (theirs.clone(), tids.clone())
        } else {
            merge(&self.base, &self.base_ids, &self.local, &self.local_ids, &theirs, &tids, doc_part, &mut self.fresh, carets)
        };
        self.base = theirs;
        self.base_ids = tids;
        self.local = m.clone();
        self.local_ids = mids;
        self.version = version;
        Ok(m)
    }
}
