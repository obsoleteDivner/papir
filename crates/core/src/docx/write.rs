//! Model → WordprocessingML, package → zip.

use super::{Document, Error};
use crate::model::*;
use crate::xml::{self, Node};
use std::io::Write;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

pub fn save(doc: &Document) -> Result<Vec<u8>, Error> {
    let mut pkg = doc.pkg.clone();
    pkg.set(&doc.part, document_xml(doc).into_bytes());
    if doc.comments.dirty {
        write_comments(doc, &mut pkg);
    }
    if doc.footnotes_dirty {
        write_footnotes(doc, &mut pkg);
    }
    // edited headers and footers
    for st in doc.stories.values().filter(|s| s.dirty) {
        let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n");
        x.push_str(&st.root.open_tag());
        container(&mut x, &st.content);
        x.push_str(&st.root.close_tag());
        pkg.set(&st.part, x.into_bytes());
    }
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for e in &pkg.entries {
        let lower = e.name.to_ascii_lowercase();
        let stored = [".png", ".jpg", ".jpeg", ".gif", ".zip"].iter().any(|x| lower.ends_with(x));
        let opts = zip::write::SimpleFileOptions::default().compression_method(if stored {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        });
        zw.start_file(e.name.as_str(), opts).map_err(|e| Error::Zip(e.to_string()))?;
        zw.write_all(&e.data).map_err(|e| Error::Zip(e.to_string()))?;
    }
    let out = zw.finish().map_err(|e| Error::Zip(e.to_string()))?;
    Ok(out.into_inner())
}

/// The footnotes part: the notes as edited, everything else (separators,
/// continuation notices) as it was read; new notes at the end.
fn write_footnotes(doc: &Document, pkg: &mut super::Package) {
    let (Some(part), Some(root)) = (&doc.footnotes_part, &doc.footnotes_root) else { return };
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n");
    x.push_str(&root.open_tag());
    let mut written = std::collections::HashSet::new();
    for c in &root.children {
        match c {
            Node::Elem(f) if f.is_w("footnote") && f.wattr("type").is_none_or(|t| t == "normal") => {
                let id = f.wattr("id").unwrap_or_default();
                // a note that is no longer there was deleted with its reference
                if let Some(content) = doc.footnotes.get(id) {
                    x.push_str(&f.open_tag());
                    container(&mut x, content);
                    x.push_str(&f.close_tag());
                    written.insert(id.to_string());
                }
            }
            Node::Elem(e) => e.write_to(&mut x),
            Node::Text(t) => xml::escape_into(&mut x, t, false),
        }
    }
    for id in crate::edit::note_ids(doc).into_iter().filter(|id| !written.contains(id)) {
        x.push_str(&format!("<w:footnote w:id=\"{}\">", xml::escape(&id)));
        container(&mut x, &doc.footnotes[&id]);
        x.push_str("</w:footnote>");
    }
    x.push_str(&root.close_tag());
    pkg.set(part, x.into_bytes());
}

pub fn document_xml(doc: &Document) -> String {
    let mut s = String::with_capacity(1 << 16);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n");
    let root = &doc.root;
    let mut open = root.open_tag();
    // generated markup uses the `w:` prefix
    let has_w = root.attrs.iter().any(|a| a.qname == "xmlns:w");
    if !has_w {
        open.insert_str(open.len() - 1, &format!(" xmlns:w=\"{W_NS}\""));
    }
    s.push_str(&open);
    for c in &root.children {
        match c {
            Node::Elem(e) if e.is_w("body") => {
                s.push_str(&e.open_tag());
                container(&mut s, &doc.body);
                if let Some(sp) = &doc.sect {
                    sp.write_to(&mut s);
                }
                s.push_str(&e.close_tag());
            }
            Node::Elem(e) => e.write_to(&mut s),
            Node::Text(t) => xml::escape_into(&mut s, t, false),
        }
    }
    s.push_str(&root.close_tag());
    s
}

pub fn container(s: &mut String, c: &Container) {
    for b in c {
        block(s, b);
    }
}

fn block(s: &mut String, b: &Block) {
    match b {
        Block::Para(p) => para(s, p),
        Block::Table(t) => table(s, t),
        Block::Sdt(x) => {
            s.push_str(&x.open);
            container(s, &x.content);
            s.push_str(&x.close);
        }
        Block::Raw(e) => e.write_to(s),
    }
}

/// `</w:r>` for `<w:r w:rsidR="..">`
fn close_of(open: &str) -> String {
    let name: String = open[1..].chars().take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/').collect();
    format!("</{name}>")
}

pub fn para(s: &mut String, p: &Para) {
    if let Some(o) = &p.orig {
        s.push_str(o);
        return;
    }
    s.push_str(&p.open);
    if p.props_dirty {
        let mark = p
            .ppr
            .as_ref()
            .and_then(|x| x.wchild("rPr"))
            .cloned()
            .or_else(|| p.mark.write(None));
        if let Some(ppr) = p.props.write(p.ppr.as_deref(), mark) {
            ppr.write_to(s);
        }
    } else if let Some(ppr) = &p.ppr {
        ppr.write_to(s);
    }
    for i in &p.inlines {
        match i {
            Inline::Run(r) => run(s, r),
            Inline::Open(w) => s.push_str(&w.open),
            Inline::Close(w) => s.push_str(&w.close),
            Inline::Raw(e) => e.write_to(s),
        }
    }
    s.push_str(&p.close);
}

pub(crate) fn run(s: &mut String, r: &Run) {
    if r.items.is_empty() {
        return;
    }
    s.push_str(&r.open);
    if r.dirty {
        if let Some(rpr) = r.props.write(r.rpr.as_deref()) {
            rpr.write_to(s);
        }
    } else if let Some(rpr) = &r.rpr {
        rpr.write_to(s);
    }
    for it in &r.items {
        match it {
            Item::Text(t) => {
                if !t.is_empty() {
                    s.push_str("<w:t xml:space=\"preserve\">");
                    xml::escape_into(s, t, false);
                    s.push_str("</w:t>");
                }
            }
            Item::Tab => s.push_str("<w:tab/>"),
            Item::Break(BreakKind::Line) => s.push_str("<w:br/>"),
            Item::Break(BreakKind::Page) => s.push_str("<w:br w:type=\"page\"/>"),
            Item::Break(BreakKind::Column) => s.push_str("<w:br w:type=\"column\"/>"),
            Item::Image(d) => d.el.write_to(s),
            Item::Raw(e) => e.write_to(s),
        }
    }
    s.push_str(&close_of(&r.open));
}

fn table(s: &mut String, t: &Table) {
    if let Some(o) = &t.orig {
        s.push_str(o);
        return;
    }
    s.push_str(&t.open);
    for it in &t.items {
        match it {
            TableItem::Raw(e) => e.write_to(s),
            TableItem::Row(r) => {
                if let Some((o, _)) = &r.wrap {
                    s.push_str(o);
                }
                s.push_str(&r.open);
                for ci in &r.items {
                    match ci {
                        RowItem::Raw(e) => e.write_to(s),
                        RowItem::Cell(c) => {
                            if let Some((o, _)) = &c.wrap {
                                s.push_str(o);
                            }
                            s.push_str(&c.open);
                            if let Some(p) = &c.tcpr {
                                p.write_to(s);
                            }
                            container(s, &c.content);
                            s.push_str(&c.close);
                            if let Some((_, cl)) = &c.wrap {
                                s.push_str(cl);
                            }
                        }
                    }
                }
                s.push_str(&r.close);
                if let Some((_, cl)) = &r.wrap {
                    s.push_str(cl);
                }
            }
        }
    }
    s.push_str(&t.close);
}

/// comments.xml and commentsExtended.xml from the model; parts, relationships
/// and content types are added when the document had no comments.
fn write_comments(doc: &Document, pkg: &mut super::Package) {
    let find = |k: &str| doc.rels.values().find(|r| r.kind == k).map(|r| r.target.clone());
    let dir = doc.part.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let ensure = |pkg: &mut super::Package, kind: &str, file: &str, ct: &str| -> String {
        match find(kind) {
            Some(p) => p,
            None => {
                let part = if dir.is_empty() { file.to_string() } else { format!("{dir}/{file}") };
                // the extended part's relationship type lives in another namespace
                if kind == "commentsExtended" {
                    let name = super::rels_name(&doc.part);
                    let mut x = pkg.text(&name).unwrap_or_default();
                    if let Some(i) = x.rfind("</Relationships>") {
                        x.insert_str(i, &format!("<Relationship Id=\"rIdPcx\" Type=\"http://schemas.microsoft.com/office/2011/relationships/commentsExtended\" Target=\"{file}\"/>"));
                        pkg.set(&name, x.into_bytes());
                    }
                } else {
                    pkg.add_rel(&doc.part, kind, file, false);
                }
                pkg.ensure_override(&part, ct);
                part
            }
        }
    };
    let c = ensure(pkg, "comments", "comments.xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml");
    pkg.set(&c, doc.comments.comments_xml().into_bytes());
    let e = ensure(pkg, "commentsExtended", "commentsExtended.xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml");
    pkg.set(&e, doc.comments.ext_xml().into_bytes());
}
