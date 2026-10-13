//! Minimal namespace-aware XML DOM.
//!
//! Elements keep their qualified names and attributes exactly as written, so an
//! untouched subtree serializes back to equivalent XML. Matching is done on the
//! resolved namespace (`Ns`), so `w:p` in a document using another prefix for
//! the WordprocessingML namespace still matches.

use quick_xml::events::Event;
use quick_xml::Reader;
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ns {
    None,
    W,
    R,
    Wp,
    A,
    Pic,
    Mc,
    V,
    Rel,
    Other,
}

fn ns_of(uri: &str) -> Ns {
    match uri {
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
        | "http://purl.oclc.org/ooxml/wordprocessingml/main" => Ns::W,
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
        | "http://purl.oclc.org/ooxml/officeDocument/relationships" => Ns::R,
        "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
        | "http://purl.oclc.org/ooxml/drawingml/wordprocessingDrawing" => Ns::Wp,
        "http://schemas.openxmlformats.org/drawingml/2006/main"
        | "http://purl.oclc.org/ooxml/drawingml/main" => Ns::A,
        "http://schemas.openxmlformats.org/drawingml/2006/picture"
        | "http://purl.oclc.org/ooxml/drawingml/picture" => Ns::Pic,
        "http://schemas.openxmlformats.org/markup-compatibility/2006" => Ns::Mc,
        "urn:schemas-microsoft-com:vml" => Ns::V,
        "http://schemas.openxmlformats.org/package/2006/relationships" => Ns::Rel,
        _ => Ns::Other,
    }
}

#[derive(Clone, Debug)]
pub struct Attr {
    pub qname: String,
    pub ns: Ns,
    pub local: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub enum Node {
    Elem(Element),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Element {
    pub qname: String,
    pub ns: Ns,
    pub local: String,
    pub attrs: Vec<Attr>,
    pub children: Vec<Node>,
}

#[derive(Debug)]
pub struct XmlError(pub String);

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "xml: {}", self.0)
    }
}

/// Parses a whole XML document and returns its root element.
/// DTDs are rejected: no entity expansion, no external references.
/// Depth past which containers stop nesting (real documents stay under 20
/// levels; the tree is walked recursively). As an HTML parser does with absurd
/// nesting, a table, cell, content control or text box opened deeper is left
/// out and its paragraphs join the deepest container: the text stays, the
/// structure flattens, nothing overflows the stack.
pub const FLAT_DEPTH: usize = 256;
/// Past this, every element is left out (paragraph content is shallow).
const LAST_DEPTH: usize = FLAT_DEPTH + 64;

/// Containers that may nest without end: flattened past `FLAT_DEPTH`.
fn container(ns: Ns, local: &str) -> bool {
    match ns {
        Ns::W => matches!(
            local,
            "tbl" | "tr" | "tc" | "sdt" | "sdtContent" | "txbxContent" | "customXml" | "smartTag" | "ins" | "del" | "moveFrom" | "moveTo" | "hyperlink" | "fldSimple"
                | "dir" | "bdo" | "drawing" | "pict" | "object"
        ),
        Ns::Wp | Ns::A | Ns::Pic | Ns::Mc | Ns::V | Ns::Other => true,
        _ => false,
    }
}

pub fn parse(src: &str) -> Result<Element, XmlError> {
    let mut reader = Reader::from_str(src);
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements = false;
    // namespace prefixes in scope: (prefix, namespace, level that declared it);
    // resolved here, as a namespace-aware reader would limit the depth
    let mut binds: Vec<(String, Ns, usize)> = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    // for every open tag: whether it is on `stack` (false: flattened away)
    let mut open: Vec<bool> = Vec::new();
    let mut root: Option<Element> = None;
    loop {
        let ev = reader.read_event().map_err(|e| XmlError(e.to_string()))?;
        match ev {
            Event::Start(ref s) | Event::Empty(ref s) => {
                let level = open.len();
                let qname = s.name().as_ref().to_string();
                let mut raw = Vec::new();
                for a in s.attributes().with_checks(false) {
                    let a = a.map_err(|e| XmlError(e.to_string()))?;
                    let key = a.key.as_ref().to_string();
                    #[allow(deprecated)]
                    let value = a
                        .unescape_value()
                        .map(|v| v.into_owned())
                        .unwrap_or_else(|_| a.value.to_string());
                    if key == "xmlns" {
                        binds.push((String::new(), ns_of(&value), level));
                    } else if let Some(p) = key.strip_prefix("xmlns:") {
                        binds.push((p.to_string(), ns_of(&value), level));
                    }
                    raw.push((key, value));
                }
                let (ns, local) = resolve(&binds, &qname, true);
                let attrs = raw
                    .into_iter()
                    .map(|(qname, value)| {
                        let (ns, local) = resolve(&binds, &qname, false);
                        Attr { qname, ns, local, value }
                    })
                    .collect();
                let keep = stack.len() < FLAT_DEPTH || (stack.len() < LAST_DEPTH && !container(ns, &local));
                let el = Element { qname, ns, local, attrs, children: Vec::new() };
                if matches!(ev, Event::Empty(_)) {
                    if keep {
                        push_child(&mut stack, &mut root, el);
                    }
                    while binds.last().is_some_and(|b| b.2 >= level) {
                        binds.pop();
                    }
                } else {
                    open.push(keep);
                    if keep {
                        stack.push(el);
                    }
                }
            }
            Event::End(_) => {
                let kept = open.pop().ok_or_else(|| XmlError("unbalanced end tag".into()))?;
                while binds.last().is_some_and(|b| b.2 >= open.len()) {
                    binds.pop();
                }
                if kept {
                    let el = stack.pop().ok_or_else(|| XmlError("unbalanced end tag".into()))?;
                    push_child(&mut stack, &mut root, el);
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    let raw = t.to_string();
                    let s = unescape_text(&raw)?;
                    if !s.is_empty() {
                        match top.children.last_mut() {
                            Some(Node::Text(t)) => t.push_str(&s),
                            _ => top.children.push(Node::Text(s)),
                        }
                    }
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.children.push(Node::Text(t.to_string()));
                }
            }
            Event::GeneralRef(r) => {
                if let Some(top) = stack.last_mut() {
                    let name = r.as_ref().to_string();
                    let s = resolve_entity(&name)?;
                    match top.children.last_mut() {
                        Some(Node::Text(t)) => t.push_str(&s),
                        _ => top.children.push(Node::Text(s)),
                    }
                }
            }
            Event::DocType(_) => return Err(XmlError("DTD is not allowed".into())),
            Event::Eof => break,
            _ => {}
        }
    }
    if !open.is_empty() {
        return Err(XmlError("unexpected end of document".into()));
    }
    root.ok_or_else(|| XmlError("empty document".into()))
}

/// Namespace and local name of `qname`; an unprefixed attribute has no namespace.
fn resolve(binds: &[(String, Ns, usize)], qname: &str, element: bool) -> (Ns, String) {
    let (prefix, local) = match qname.split_once(':') {
        Some((p, l)) => (p, l),
        None => ("", qname),
    };
    if prefix.is_empty() && !element {
        return (Ns::None, local.to_string());
    }
    let ns = match prefix {
        "xml" | "xmlns" => Ns::Other,
        p => binds.iter().rev().find(|b| b.0 == p).map(|b| b.1).unwrap_or(Ns::None),
    };
    (ns, local.to_string())
}

fn push_child(stack: &mut [Element], root: &mut Option<Element>, el: Element) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(Node::Elem(el)),
        None => *root = Some(el),
    }
}

pub(crate) fn resolve_entity(name: &str) -> Result<String, XmlError> {
    Ok(match name {
        "lt" => "<".into(),
        "gt" => ">".into(),
        "amp" => "&".into(),
        "apos" => "'".into(),
        "quot" => "\"".into(),
        n if n.starts_with("#x") => char_ref(u32::from_str_radix(&n[2..], 16).ok())?,
        n if n.starts_with('#') => char_ref(n[1..].parse().ok())?,
        n => return Err(XmlError(format!("unknown entity &{n};"))),
    })
}

fn char_ref(c: Option<u32>) -> Result<String, XmlError> {
    c.and_then(char::from_u32)
        .map(|c| c.to_string())
        .ok_or_else(|| XmlError("bad character reference".into()))
}

pub(crate) fn unescape_text(raw: &str) -> Result<String, XmlError> {
    if !raw.contains('&') {
        return Ok(raw.to_string());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let j = rest[i..].find(';').ok_or_else(|| XmlError("bad entity".into()))?;
        out.push_str(&resolve_entity(&rest[i + 1..i + j])?);
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Element {
    pub fn new_w(local: &str) -> Element {
        Element {
            qname: format!("w:{local}"),
            ns: Ns::W,
            local: local.to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn is(&self, ns: Ns, local: &str) -> bool {
        self.ns == ns && self.local == local
    }

    pub fn is_w(&self, local: &str) -> bool {
        self.is(Ns::W, local)
    }

    pub fn attr(&self, ns: Ns, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.ns == ns && a.local == local)
            .map(|a| a.value.as_str())
    }

    /// `w:` attribute, falling back to an unprefixed one (some writers omit it).
    pub fn wattr(&self, local: &str) -> Option<&str> {
        self.attr(Ns::W, local).or_else(|| self.attr(Ns::None, local))
    }

    pub fn set_wattr(&mut self, local: &str, value: &str) {
        if let Some(a) = self.attrs.iter_mut().find(|a| a.ns == Ns::W && a.local == local) {
            a.value = value.to_string();
        } else {
            self.attrs.push(Attr {
                qname: format!("w:{local}"),
                ns: Ns::W,
                local: local.to_string(),
                value: value.to_string(),
            });
        }
    }

    pub fn with_wattr(mut self, local: &str, value: &str) -> Element {
        self.set_wattr(local, value);
        self
    }

    pub fn elems(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Elem(e) => Some(e),
            _ => None,
        })
    }

    pub fn child(&self, ns: Ns, local: &str) -> Option<&Element> {
        self.elems().find(|e| e.is(ns, local))
    }

    pub fn wchild(&self, local: &str) -> Option<&Element> {
        self.child(Ns::W, local)
    }

    pub fn wchildren<'a>(&'a self, local: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.elems().filter(move |e| e.is_w(local))
    }

    /// First descendant (depth-first) matching.
    pub fn find(&self, ns: Ns, local: &str) -> Option<&Element> {
        for e in self.elems() {
            if e.is(ns, local) {
                return Some(e);
            }
            if let Some(f) = e.find(ns, local) {
                return Some(f);
            }
        }
        None
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for c in &self.children {
            match c {
                Node::Text(t) => s.push_str(t),
                Node::Elem(e) => s.push_str(&e.text()),
            }
        }
        s
    }

    pub fn write_to(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.qname);
        for a in &self.attrs {
            out.push(' ');
            out.push_str(&a.qname);
            out.push_str("=\"");
            escape_into(out, &a.value, true);
            out.push('"');
        }
        if self.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for c in &self.children {
            match c {
                Node::Text(t) => escape_into(out, t, false),
                Node::Elem(e) => e.write_to(out),
            }
        }
        out.push_str("</");
        out.push_str(&self.qname);
        out.push('>');
    }

    pub fn to_xml(&self) -> String {
        let mut s = String::new();
        self.write_to(&mut s);
        s
    }

    /// Opening tag only, e.g. `<w:p w:rsidR="..">`.
    pub fn open_tag(&self) -> String {
        let mut out = String::new();
        out.push('<');
        out.push_str(&self.qname);
        for a in &self.attrs {
            let _ = write!(out, " {}=\"", a.qname);
            escape_into(&mut out, &a.value, true);
            out.push('"');
        }
        out.push('>');
        out
    }

    pub fn close_tag(&self) -> String {
        format!("</{}>", self.qname)
    }
}

pub fn escape_into(out: &mut String, s: &str, attr: bool) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' if attr => out.push_str("&quot;"),
            '\n' if attr => out.push_str("&#10;"),
            '\t' if attr => out.push_str("&#9;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' && c != '\r' => {}
            c => out.push(c),
        }
    }
}

pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    escape_into(&mut o, s, false);
    o
}
