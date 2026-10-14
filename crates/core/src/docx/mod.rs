//! DOCX package: reading with limits, relationships, parts.

mod parse;
mod write;

pub use write::save;

pub fn write_para(out: &mut String, p: &crate::model::Para) {
    write::para(out, p)
}

/// Parses a `w:p` element in the context of `doc` (its relationships).
pub fn parse_para(e: &Element, doc: &Document) -> crate::model::Para {
    let mut ctx = parse::Ctx { part: &doc.part, rels: &doc.rels };
    parse::para(&mut ctx, e)
}

/// Parses a block (`w:p`, `w:tbl`) in the context of `doc`.
pub fn parse_block(e: &Element, doc: &Document) -> crate::model::Block {
    let mut ctx = parse::Ctx { part: &doc.part, rels: &doc.rels };
    let mut v = parse::container(&mut ctx, std::iter::once(&crate::xml::Node::Elem(e.clone())));
    std::rc::Rc::try_unwrap(v.remove(0)).unwrap_or_else(|rc| (*rc).clone())
}

/// XML of one run (`w:r`), as written into a part.
pub fn run_xml(r: &crate::model::Run) -> String {
    let mut s = String::new();
    write::run(&mut s, r);
    s
}

/// XML of blocks, as they are written into a part.
pub fn blocks_xml(c: &Container) -> String {
    let mut s = String::new();
    write::container(&mut s, c);
    s
}

/// Parses blocks from an XML fragment; `ns` holds the `xmlns:*` declarations
/// for its prefixes, `part` and `rels` resolve pictures and links.
pub fn parse_blocks(xml: &str, ns: &str, part: &str, rels: &Rels) -> Result<Container, String> {
    let root = crate::xml::parse(&format!("<w:body{ns}>{xml}</w:body>")).map_err(|e| e.0)?;
    let mut ctx = parse::Ctx { part, rels };
    Ok(parse::container(&mut ctx, root.children.iter()))
}

#[doc(hidden)]
pub fn document_xml_for_test(d: &Document) -> String {
    write::document_xml(d)
}

use crate::model::{Container, Story};
use crate::props::SectProps;
use crate::styles::{Numbering, Settings, Styles, ThemeFonts};
use crate::xml::{self, Element};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::rc::Rc;

/// How untrusted input is read. Sizes are not capped. Once the archive has
/// unpacked `bomb_grace` bytes, it must keep a packed/unpacked ratio above
/// `min_ratio`, counted on the bytes actually consumed (the test Apache POI
/// uses): real documents, even very repetitive tables, stay above 0.03, a
/// decompression bomb is far below and is caught as soon as it passes the grace.
#[derive(Clone, Debug)]
pub struct Limits {
    pub min_ratio: f64,
    pub bomb_grace: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { min_ratio: 0.01, bomb_grace: 64 << 20 }
    }
}

#[derive(Debug)]
pub enum Error {
    /// the main part is a decompression bomb
    Bomb(String),
    NotZip(String),
    Missing(&'static str),
    Xml(String),
    Zip(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Bomb(p) => write!(f, "decompression bomb: {p}"),
            Error::NotZip(e) => write!(f, "not a docx (zip): {e}"),
            Error::Missing(p) => write!(f, "missing part: {p}"),
            Error::Xml(e) => write!(f, "bad xml: {e}"),
            Error::Zip(e) => write!(f, "zip: {e}"),
        }
    }
}

impl Error {
    /// Stable error code for API responses.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Bomb(_) => "zip_bomb",
            Error::NotZip(_) => "not_docx",
            Error::Missing(_) => "missing_part",
            Error::Xml(_) => "bad_xml",
            Error::Zip(_) => "bad_zip",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub data: Rc<Vec<u8>>,
}

/// All parts of the package, in original order.
#[derive(Clone, Debug, Default)]
pub struct Package {
    pub entries: Vec<Entry>,
    /// parts left out while reading, with the reason (a bomb, a broken entry)
    pub damaged: Vec<(String, String)>,
    /// number of this participant when several edit the document: ids it
    /// allocates (relationships, parts, list and comment numbers) are its own
    pub ns: Option<u32>,
}

impl Package {
    /// Prefix for names this participant gives to new parts.
    pub fn name_prefix(&self) -> String {
        self.ns.map(|k| format!("c{k}_")).unwrap_or_default()
    }

    /// A number for a new list or comment: max + 1, or, when several people
    /// edit, the next one in this participant's own range.
    pub fn next_number(&self, used: impl Iterator<Item = u32>) -> u32 {
        match self.ns {
            None => used.max().map(|m| m + 1).unwrap_or(1),
            Some(k) => {
                let lo = 10_000 + k * 1_000;
                used.filter(|v| (lo..lo + 1_000).contains(v)).max().map(|m| m + 1).unwrap_or(lo)
            }
        }
    }

    pub fn read(bytes: &[u8], lim: &Limits) -> Result<Package, Error> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::NotZip(e.to_string()))?;
        let mut entries = Vec::with_capacity(zip.len());
        let mut damaged = Vec::new();
        // packed bytes consumed and bytes unpacked, over the whole archive
        let mut seen = (0u64, 0u64);
        for i in 0..zip.len() {
            // a broken entry is left out; whether the document can do without it is decided later
            let mut f = match zip.by_index_raw(i) {
                Ok(f) => f,
                Err(e) => {
                    damaged.push((format!("#{i}"), e.to_string()));
                    continue;
                }
            };
            if f.is_dir() {
                continue;
            }
            // some writers use Windows separators inside the archive
            let name = match f.name() {
                Ok(n) => n.replace('\\', "/"),
                Err(e) => {
                    damaged.push((format!("#{i}"), e.to_string()));
                    continue;
                }
            };
            let (method, crc) = (f.compression(), f.crc32());
            let mut raw = Vec::new();
            let read = f.read_to_end(&mut raw);
            drop(f);
            let res = match read {
                Ok(_) => unpack(&raw, method, crc, &mut seen, lim),
                Err(e) => Err(e.to_string()),
            };
            match res {
                Ok(data) => entries.push(Entry { name, data: Rc::new(data) }),
                Err(why) => damaged.push((name, why)),
            }
        }
        Ok(Package { entries, damaged, ns: None })
    }

    pub fn get(&self, name: &str) -> Option<&Rc<Vec<u8>>> {
        let name = name.trim_start_matches('/');
        // part names are case-insensitive in OPC
        self.entries.iter().find(|e| e.name == name).or_else(|| self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))).map(|e| &e.data)
    }

    pub fn xml(&self, name: &str) -> Result<Option<Element>, Error> {
        match self.get(name) {
            None => Ok(None),
            Some(d) => {
                let s = decode_text(d);
                xml::parse(&s).map(Some).map_err(|e| Error::Xml(format!("{name}: {}", e.0)))
            }
        }
    }

    /// Adds a relationship from `source` (e.g. "word/document.xml"); returns its id.
    pub fn add_rel(&mut self, source: &str, kind: &str, target: &str, external: bool) -> String {
        let name = rels_name(source);
        let mut xml = self.get(&name).map(|d| decode_text(d)).unwrap_or_else(|| {
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_string()
        });
        let pre = self.ns.map(|k| format!("c{k}_")).unwrap_or_default();
        let mut n = 1;
        while xml.contains(&format!("Id=\"rIdP{pre}{n}\"")) {
            n += 1;
        }
        let id = format!("rIdP{pre}{n}");
        let mode = if external { " TargetMode=\"External\"" } else { "" };
        let rel = format!(
            "<Relationship Id=\"{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}\" Target=\"{}\"{mode}/>",
            xml::escape(target).replace('"', "&quot;")
        );
        if let Some(i) = xml.rfind("</Relationships>") {
            xml.insert_str(i, &rel);
        }
        self.set(&name, xml.into_bytes());
        id
    }

    /// Makes sure files with extension `ext` have a content type.
    pub fn ensure_default_type(&mut self, ext: &str, ct: &str) {
        let mut xml = self.get("[Content_Types].xml").map(|d| decode_text(d)).unwrap_or_default();
        let lower = xml.to_lowercase();
        if lower.contains(&format!("extension=\"{}\"", ext.to_lowercase())) {
            return;
        }
        if let Some(i) = xml.rfind("</Types>") {
            xml.insert_str(i, &format!("<Default Extension=\"{ext}\" ContentType=\"{ct}\"/>"));
            self.set("[Content_Types].xml", xml.into_bytes());
        }
    }

    pub fn ensure_override(&mut self, part: &str, ct: &str) {
        let mut xml = self.get("[Content_Types].xml").map(|d| decode_text(d)).unwrap_or_default();
        if xml.contains(&format!("PartName=\"/{part}\"")) {
            return;
        }
        if let Some(i) = xml.rfind("</Types>") {
            xml.insert_str(i, &format!("<Override PartName=\"/{part}\" ContentType=\"{ct}\"/>"));
            self.set("[Content_Types].xml", xml.into_bytes());
        }
    }

    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name).map(|d| decode_text(d))
    }

    pub fn set(&mut self, name: &str, data: Vec<u8>) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.name == name) {
            e.data = Rc::new(data);
        } else {
            self.entries.push(Entry { name: name.to_string(), data: Rc::new(data) });
        }
    }
}

/// UTF-8 (with or without BOM) or UTF-16 XML text.
fn decode_text(d: &[u8]) -> String {
    if d.starts_with(&[0xFF, 0xFE]) || d.starts_with(&[0xFE, 0xFF]) {
        let le = d[0] == 0xFF;
        let u: Vec<u16> = d[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&u);
    }
    let d = d.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(d);
    String::from_utf8_lossy(d).into_owned()
}

#[derive(Clone, Debug)]
pub struct Rel {
    pub kind: String,
    pub target: String,
    pub external: bool,
}

pub type Rels = HashMap<String, Rel>;

/// `word/document.xml` -> `word/_rels/document.xml.rels`
pub fn rels_name(part: &str) -> String {
    match part.rfind('/') {
        Some(i) => format!("{}/_rels/{}.rels", &part[..i], &part[i + 1..]),
        None => format!("_rels/{part}.rels"),
    }
}

/// Resolves a relationship target relative to the source part.
pub fn resolve(part: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') {
        return t.to_string();
    }
    let mut segs: Vec<&str> = match part.rfind('/') {
        Some(i) => part[..i].split('/').collect(),
        None => Vec::new(),
    };
    for s in target.split('/') {
        match s {
            ".." => {
                segs.pop();
            }
            "." | "" => {}
            s => segs.push(s),
        }
    }
    segs.join("/")
}

/// One entry's bytes from its packed form, checking the CRC and, past the
/// grace, the ratio of packed bytes consumed to bytes unpacked so far.
fn unpack(raw: &[u8], method: zip::CompressionMethod, crc: u32, seen: &mut (u64, u64), lim: &Limits) -> Result<Vec<u8>, String> {
    let bomb = |packed: u64, unpacked: u64| unpacked > lim.bomb_grace && (packed as f64) <= unpacked as f64 * lim.min_ratio;
    let data = match method {
        zip::CompressionMethod::Stored => raw.to_vec(),
        zip::CompressionMethod::Deflated => {
            let mut dec = flate2::bufread::DeflateDecoder::new(raw);
            let mut data = Vec::new();
            let mut buf = vec![0u8; 64 << 10];
            loop {
                let n = dec.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                let consumed = (raw.len() - dec.get_ref().len()) as u64;
                if bomb(seen.0 + consumed, seen.1 + data.len() as u64) {
                    return Err("zip_bomb".into());
                }
            }
            data
        }
        m => return Err(format!("unsupported compression {m:?}")),
    };
    let mut c = flate2::Crc::new();
    c.update(&data);
    if c.sum() != crc {
        return Err("crc mismatch".into());
    }
    seen.0 += raw.len() as u64;
    seen.1 += data.len() as u64;
    if bomb(seen.0, seen.1) {
        return Err("zip_bomb".into());
    }
    Ok(data)
}

pub fn read_rels(pkg: &Package, part: &str) -> Result<Rels, Error> {
    let mut m = HashMap::new();
    if let Some(root) = pkg.xml(&rels_name(part))? {
        for r in root.elems().filter(|e| e.local == "Relationship") {
            let id = r.attr(xml::Ns::None, "Id").unwrap_or_default().to_string();
            let target = r.attr(xml::Ns::None, "Target").unwrap_or_default();
            let external = r.attr(xml::Ns::None, "TargetMode") == Some("External");
            m.insert(
                id,
                Rel {
                    kind: r.attr(xml::Ns::None, "Type").unwrap_or_default().rsplit('/').next().unwrap_or("").to_string(),
                    target: if external { target.to_string() } else { resolve(part, target) },
                    external,
                },
            );
        }
    }
    Ok(m)
}

#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub created: Option<String>,
}

/// A loaded document.
#[derive(Clone, Debug)]
pub struct Document {
    pub pkg: Package,
    pub part: String,
    /// `w:document` without the body's children
    pub root: Rc<Element>,
    pub body: Container,
    /// body-level `w:sectPr`
    pub sect: Option<Rc<Element>>,
    pub styles: Rc<Styles>,
    pub numbering: Rc<Numbering>,
    pub theme: Rc<ThemeFonts>,
    pub settings: Rc<Settings>,
    pub rels: Rc<Rels>,
    pub stories: Rc<HashMap<String, Story>>,
    pub meta: Meta,
    pub comments: Rc<crate::comments::Comments>,
    /// parsed charts by part name
    pub charts: Rc<HashMap<String, crate::chart::Chart>>,
    /// footnotes by id (separators left out)
    pub footnotes: Rc<HashMap<String, Container>>,
    /// the footnotes part and its root as read (separators and the like are kept on save)
    pub footnotes_part: Option<String>,
    pub footnotes_root: Option<Rc<Element>>,
    /// a footnote was edited or added: the part is written again on save
    pub footnotes_dirty: bool,
    /// parts that could not be read and were left out (styles, a header...)
    pub damaged: Vec<String>,
}

impl Document {
    pub fn open(bytes: &[u8]) -> Result<Document, Error> {
        Document::open_with(bytes, &Limits::default())
    }

    pub fn open_with(bytes: &[u8], lim: &Limits) -> Result<Document, Error> {
        let pkg = Package::read(bytes, lim)?;
        // only the main part must be readable: a broken style sheet, header or
        // relationship list is left out, as Word recovers such files
        let mut damaged = Vec::new();
        let soft = |name: &str, damaged: &mut Vec<String>| match pkg.xml(name) {
            Ok(x) => x,
            Err(_) => {
                damaged.push(name.to_string());
                None
            }
        };
        let soft_rels = |part: &str, damaged: &mut Vec<String>| {
            read_rels(&pkg, part).unwrap_or_else(|_| {
                damaged.push(rels_name(part));
                Rels::new()
            })
        };
        let root_rels = soft_rels("", &mut damaged);
        let part = root_rels
            .values()
            .find(|r| r.kind == "officeDocument")
            .map(|r| r.target.clone())
            .unwrap_or_else(|| "word/document.xml".into());
        if let Some((_, why)) = pkg.damaged.iter().find(|(n, _)| n.eq_ignore_ascii_case(&part)) {
            return Err(if why == "zip_bomb" { Error::Bomb(part.clone()) } else { Error::Zip(format!("{part}: {why}")) });
        }
        damaged.extend(pkg.damaged.iter().map(|(n, _)| n.clone()));
        let mut root = pkg.xml(&part)?.ok_or(Error::Missing("word/document.xml"))?;
        let rels = soft_rels(&part, &mut damaged);
        let by_kind = |k: &str| rels.values().find(|r| r.kind == k).map(|r| r.target.clone());

        let styles = match by_kind("styles") {
            Some(p) => soft(&p, &mut damaged).map(|e| Styles::parse(&e)).unwrap_or_default(),
            None => Styles::default(),
        };
        let numbering = match by_kind("numbering") {
            Some(p) => soft(&p, &mut damaged).map(|e| Numbering::parse(&e)).unwrap_or_default(),
            None => Numbering::default(),
        };
        let theme = match by_kind("theme") {
            Some(p) => soft(&p, &mut damaged).map(|e| ThemeFonts::parse(&e)).unwrap_or_default(),
            None => ThemeFonts::default(),
        };
        let settings = match by_kind("settings") {
            Some(p) => soft(&p, &mut damaged).map(|e| Settings::parse(&e)).unwrap_or_default(),
            None => Settings::default(),
        };

        let body_idx = root
            .children
            .iter()
            .position(|n| matches!(n, xml::Node::Elem(e) if e.is_w("body")))
            .ok_or(Error::Missing("w:body"))?;
        let body_el = match &mut root.children[body_idx] {
            xml::Node::Elem(e) => std::mem::replace(&mut e.children, Vec::new()),
            _ => unreachable!(),
        };
        let mut ctx = parse::Ctx { part: &part, rels: &rels };
        let mut sect = None;
        // block by block: the markup of each is freed once it is read, so a
        // large document is not held twice (tree and model) at the same time
        let mut body = Vec::with_capacity(body_el.len());
        for n in body_el {
            match n {
                xml::Node::Elem(e) if e.is_w("sectPr") => sect = Some(Rc::new(e)),
                n => body.extend(parse::container(&mut ctx, std::iter::once(&n))),
            }
        }

        let mut stories = HashMap::new();
        for (id, r) in rels.iter() {
            if r.kind == "header" || r.kind == "footer" {
                if let Some(h) = soft(&r.target, &mut damaged) {
                    let hrels = soft_rels(&r.target, &mut damaged);
                    let mut hctx = parse::Ctx { part: &r.target, rels: &hrels };
                    let content = parse::container(&mut hctx, h.children.iter());
                    stories.insert(id.clone(), Story { part: r.target.clone(), root: Rc::new(h), content, dirty: false });
                }
            }
        }

        let comments = {
            let part = |k: &str| rels.values().find(|r| r.kind == k).map(|r| r.target.clone());
            let c = match part("comments") {
                Some(p) => soft(&p, &mut damaged),
                None => None,
            };
            let e = match part("commentsExtended") {
                Some(p) => soft(&p, &mut damaged),
                None => None,
            };
            crate::comments::Comments::parse(c.as_ref(), e.as_ref())
        };

        let mut charts = HashMap::new();
        for r in rels.values().filter(|r| r.kind == "chart" && !r.external) {
            if let Ok(Some(root)) = pkg.xml(&r.target) {
                if let Some(c) = crate::chart::parse(&root) {
                    charts.insert(r.target.clone(), c);
                }
            }
        }

        let mut footnotes = HashMap::new();
        let mut footnotes_part = None;
        let mut footnotes_root = None;
        if let Some(fpart) = rels.values().find(|r| r.kind == "footnotes").map(|r| r.target.clone()) {
            footnotes_part = Some(fpart.clone());
            if let Some(root) = soft(&fpart, &mut damaged) {
                let frels = soft_rels(&fpart, &mut damaged);
                let mut fctx = parse::Ctx { part: &fpart, rels: &frels };
                for f in root.wchildren("footnote") {
                    if f.wattr("type").is_some_and(|t| t != "normal") {
                        continue;
                    }
                    if let Some(id) = f.wattr("id") {
                        footnotes.insert(id.to_string(), parse::container(&mut fctx, f.children.iter()));
                    }
                }
                footnotes_root = Some(Rc::new(root));
            }
        }

        let meta = match soft("docProps/core.xml", &mut damaged) {
            Some(c) => {
                let t = |n: &str| c.elems().find(|e| e.local == n).map(|e| e.text()).filter(|s| !s.is_empty());
                Meta { title: t("title"), author: t("creator"), created: t("created") }
            }
            None => Meta::default(),
        };

        Ok(Document {
            pkg,
            part,
            root: Rc::new(root),
            body,
            sect,
            styles: Rc::new(styles),
            numbering: Rc::new(numbering),
            theme: Rc::new(theme),
            settings: Rc::new(settings),
            rels: Rc::new(rels),
            stories: Rc::new(stories),
            meta,
            comments: Rc::new(comments),
            charts: Rc::new(charts),
            footnotes: Rc::new(footnotes),
            footnotes_part,
            footnotes_root,
            footnotes_dirty: false,
            damaged,
        })
    }

    /// Final section properties (body-level sectPr).
    pub fn last_sect(&self) -> SectProps {
        self.sect.as_ref().map(|s| SectProps::parse(s)).unwrap_or_default()
    }

    pub fn media(&self, part: &str) -> Option<&Rc<Vec<u8>>> {
        self.pkg.get(part)
    }
}
