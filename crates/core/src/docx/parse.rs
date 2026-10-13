//! WordprocessingML body → model.

use super::Rels;
use crate::model::*;
use crate::props::{CellProps, ParaProps, RowProps, RunProps, TableProps};
use crate::xml::{Element, Node, Ns};
use std::rc::Rc;

pub struct Ctx<'a> {
    pub part: &'a str,
    pub rels: &'a Rels,
}

fn elems<'a>(nodes: impl Iterator<Item = &'a Node>) -> impl Iterator<Item = &'a Element> {
    nodes.filter_map(|n| match n {
        Node::Elem(e) => Some(e),
        _ => None,
    })
}

pub fn container<'a>(ctx: &mut Ctx, nodes: impl Iterator<Item = &'a Node>) -> Container {
    let mut out = Vec::new();
    for e in elems(nodes) {
        out.push(Rc::new(block(ctx, e)));
    }
    out
}

fn block(ctx: &mut Ctx, e: &Element) -> Block {
    if e.ns != Ns::W {
        return Block::Raw(Rc::new(e.clone()));
    }
    match e.local.as_str() {
        "p" => Block::Para(para(ctx, e)),
        "tbl" => Block::Table(table(ctx, e)),
        "sdt" => match e.wchild("sdtContent") {
            Some(c) => {
                let (open, close) = sdt_tags(e, c);
                Block::Sdt(SdtBlock { open: open.into(), close: close.into(), content: container(ctx, c.children.iter()) })
            }
            None => Block::Raw(Rc::new(e.clone())),
        },
        _ => Block::Raw(Rc::new(e.clone())),
    }
}

/// Opening and closing markup of an `sdt` around its content.
fn sdt_tags(sdt: &Element, content: &Element) -> (String, String) {
    let mut open = sdt.open_tag();
    for c in sdt.elems() {
        if !c.is_w("sdtContent") {
            c.write_to(&mut open);
        }
    }
    open.push_str(&content.open_tag());
    (open, format!("{}{}", content.close_tag(), sdt.close_tag()))
}

pub fn para(ctx: &mut Ctx, e: &Element) -> Para {
    let ppr = e.wchild("pPr");
    let mut p = Para {
        rev: next_rev(),
        open: e.open_tag().into(),
        close: e.close_tag().into(),
        ppr: ppr.map(|x| Rc::new(x.clone())),
        props: ppr.map(ParaProps::parse).unwrap_or_default(),
        mark: ppr.and_then(|x| x.wchild("rPr")).map(RunProps::parse).unwrap_or_default(),
        props_dirty: false,
        inlines: Vec::new(),
        orig: Some(e.to_xml().into()),
    };
    for c in e.elems() {
        if c.is_w("pPr") {
            continue;
        }
        inline(ctx, c, &mut p.inlines);
    }
    p
}

fn inline(ctx: &mut Ctx, c: &Element, out: &mut Vec<Inline>) {
    if c.ns != Ns::W {
        out.push(Inline::Raw(Rc::new(c.clone())));
        return;
    }
    match c.local.as_str() {
        "r" => out.push(Inline::Run(run(ctx, c))),
        "sdt" => match c.wchild("sdtContent") {
            Some(content) => {
                let (open, close) = sdt_tags(c, content);
                wrap(ctx, WrapKind::Sdt, open, close, content, out);
            }
            None => out.push(Inline::Raw(Rc::new(c.clone()))),
        },
        "hyperlink" | "smartTag" | "ins" | "moveTo" | "customXml" | "dir" | "bdo" | "fldSimple" => {
            let kind = match c.local.as_str() {
                "hyperlink" => WrapKind::Hyperlink,
                "fldSimple" => WrapKind::Field(c.wattr("instr").unwrap_or_default().to_string()),
                "ins" | "moveTo" => WrapKind::Ins(c.wattr("author").unwrap_or("").to_string(), c.wattr("date").unwrap_or("").to_string()),
                _ => WrapKind::Other,
            };
            // smartTag/customXml carry their properties as the first child
            let mut open = c.open_tag();
            for p in c.elems().filter(|x| x.is_w("smartTagPr") || x.is_w("customXmlPr")) {
                p.write_to(&mut open);
            }
            wrap(ctx, kind, open, c.close_tag(), c, out);
        }
        _ => out.push(Inline::Raw(Rc::new(c.clone()))),
    }
}

fn wrap(ctx: &mut Ctx, kind: WrapKind, open: String, close: String, content: &Element, out: &mut Vec<Inline>) {
    let w = Rc::new(Wrapper { kind, open, close });
    out.push(Inline::Open(w.clone()));
    for x in content.elems() {
        if x.is_w("smartTagPr") || x.is_w("customXmlPr") {
            continue;
        }
        inline(ctx, x, out);
    }
    out.push(Inline::Close(w));
}

fn run(ctx: &mut Ctx, e: &Element) -> Run {
    let rpr = e.wchild("rPr");
    let mut r = Run {
        open: e.open_tag().into(),
        rpr: rpr.map(|x| Rc::new(x.clone())),
        props: rpr.map(RunProps::parse).unwrap_or_default(),
        dirty: false,
        items: Vec::new(),
    };
    for c in e.elems() {
        if c.is_w("rPr") {
            continue;
        }
        let item = if c.ns == Ns::W {
            match c.local.as_str() {
                "t" => Item::Text(c.text()),
                "tab" | "ptab" => Item::Tab,
                "br" => Item::Break(match c.wattr("type") {
                    Some("page") => BreakKind::Page,
                    Some("column") => BreakKind::Column,
                    _ => BreakKind::Line,
                }),
                "cr" => Item::Break(BreakKind::Line),
                "noBreakHyphen" => Item::Text("\u{2011}".into()),
                "drawing" | "pict" | "object" => drawing(ctx, c, c),
                _ => Item::Raw(Rc::new(c.clone())),
            }
        } else if c.is(Ns::Mc, "AlternateContent") {
            // prefer the first choice holding a picture, else the fallback
            match c.find(Ns::W, "drawing").or_else(|| c.find(Ns::W, "pict")) {
                Some(d) => drawing(ctx, c, d),
                None => Item::Raw(Rc::new(c.clone())),
            }
        } else {
            Item::Raw(Rc::new(c.clone()))
        };
        match (&item, r.items.last_mut()) {
            (Item::Text(t), Some(Item::Text(prev))) => prev.push_str(t),
            _ => r.items.push(item),
        }
    }
    r
}

const EMU_PER_PT: f32 = 12700.0;

/// First descendant with this local name, in any namespace.
fn find_local<'a>(e: &'a Element, local: &str) -> Option<&'a Element> {
    for c in e.elems() {
        if c.local == local {
            return Some(c);
        }
        if let Some(f) = find_local(c, local) {
            return Some(f);
        }
    }
    None
}

/// Picture or shape. `whole` is kept for saving, `d` is what we read.
fn drawing(ctx: &mut Ctx, whole: &Element, d: &Element) -> Item {
    let rid = d
        .find(Ns::A, "blip")
        .and_then(|b| b.attr(Ns::R, "embed").or_else(|| b.attr(Ns::R, "link")))
        .or_else(|| d.find(Ns::V, "imagedata").and_then(|i| i.attr(Ns::R, "id")))
        .map(str::to_string);
    let (mut w, mut h, mut anchor) = (0.0, 0.0, None);
    if let Some(ext) = d.find(Ns::Wp, "extent") {
        let emu = |a: &str| ext.attr(Ns::None, a).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) / EMU_PER_PT;
        w = emu("cx");
        h = emu("cy");
    } else if let Some(shape) = d.find(Ns::V, "shape").or_else(|| d.find(Ns::V, "rect")) {
        // VML: style="width:120pt;height:40pt"
        let style = shape.attr(Ns::None, "style").unwrap_or("");
        for kv in style.split(';') {
            let mut it = kv.splitn(2, ':');
            let (k, v) = (it.next().unwrap_or("").trim(), it.next().unwrap_or("").trim());
            let pt = crate::props::twips(v).map(|t| t as f32 / 20.0).unwrap_or(0.0);
            match k {
                "width" => w = pt,
                "height" => h = pt,
                _ => {}
            }
        }
    }
    if let Some(a) = d.find(Ns::Wp, "anchor") {
        let emu = |v: Option<&str>| v.and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) / EMU_PER_PT;
        let pos = |axis: &str| -> (f32, String, Option<String>) {
            match a.child(Ns::Wp, axis) {
                Some(p) => {
                    let rel = p.attr(Ns::None, "relativeFrom").unwrap_or("column").to_string();
                    let off = p.child(Ns::Wp, "posOffset").and_then(|o| o.text().trim().parse::<f32>().ok()).map(|v| v / EMU_PER_PT).unwrap_or(0.0);
                    let align = p.child(Ns::Wp, "align").map(|al| al.text().trim().to_string());
                    (off, rel, align)
                }
                None => (0.0, if axis == "positionH" { "column".into() } else { "paragraph".into() }, None),
            }
        };
        let (x, rel_h, align_h) = pos("positionH");
        let (y, rel_v, align_v) = pos("positionV");
        let behind = matches!(a.attr(Ns::None, "behindDoc"), Some("1") | Some("true"));
        let wrap = if a.child(Ns::Wp, "wrapSquare").is_some() || a.child(Ns::Wp, "wrapTight").is_some() || a.child(Ns::Wp, "wrapThrough").is_some() {
            Wrap::Square
        } else if a.child(Ns::Wp, "wrapTopAndBottom").is_some() {
            Wrap::TopBottom
        } else if behind {
            Wrap::Behind
        } else {
            Wrap::Front
        };
        anchor = Some(Float {
            x,
            y,
            rel_h,
            rel_v,
            align_h,
            align_v,
            wrap,
            dist: [emu(a.attr(Ns::None, "distT")), emu(a.attr(Ns::None, "distB")), emu(a.attr(Ns::None, "distL")), emu(a.attr(Ns::None, "distR"))],
        });
    }
    // a chart: c:chart r:id → its part
    let chart = find_local(d, "chart")
        .and_then(|c| c.attr(Ns::R, "id"))
        .and_then(|id| ctx.rels.get(id))
        .filter(|r| !r.external)
        .map(|r| r.target.clone());
    // text box, fill and outline of a shape
    let txbx = d.find(Ns::W, "txbxContent").map(|c| container(ctx, c.children.iter()));
    let sppr = find_local(d, "spPr");
    let srgb = |e: &Element| e.elems().find(|x| x.local == "srgbClr").and_then(|c| c.attr(Ns::None, "val")).and_then(crate::layout::color);
    let fill = sppr.and_then(|sp| sp.elems().find(|x| x.local == "solidFill")).and_then(srgb);
    let line = sppr.and_then(|sp| sp.elems().find(|x| x.local == "ln")).filter(|ln| ln.elems().all(|x| x.local != "noFill")).map(|ln| {
        let c = ln.elems().find(|x| x.local == "solidFill").and_then(srgb).unwrap_or(0);
        let w = ln.attr(Ns::None, "w").and_then(|v| v.parse::<f32>().ok()).map(|v| v / EMU_PER_PT).unwrap_or(0.75);
        (c, w)
    });
    let insets = match find_local(d, "bodyPr") {
        Some(b) => {
            let ins = |n: &str, dflt: f32| b.attr(Ns::None, n).and_then(|v| v.parse::<f32>().ok()).map(|v| v / EMU_PER_PT).unwrap_or(dflt);
            [ins("tIns", 3.6), ins("bIns", 3.6), ins("lIns", 7.2), ins("rIns", 7.2)]
        }
        None => [3.6, 3.6, 7.2, 7.2],
    };
    let src = rid.as_ref().and_then(|id| ctx.rels.get(id)).filter(|r| !r.external).map(|r| r.target.clone());
    let _ = ctx.part;
    Item::Image(Rc::new(Drawing {
        el: whole.clone(),
        rid,
        src,
        width: w,
        height: h,
        anchored: anchor.is_some(),
        anchor,
        txbx,
        chart,
        fill,
        line,
        insets,
    }))
}

fn table(ctx: &mut Ctx, e: &Element) -> Table {
    let mut t = Table {
        open: e.open_tag().into(),
        close: e.close_tag().into(),
        props: e.wchild("tblPr").map(TableProps::parse).unwrap_or_default(),
        grid: e
            .wchild("tblGrid")
            .map(|g| g.wchildren("gridCol").map(|c| c.wattr("w").and_then(crate::props::twips).unwrap_or(0)).collect())
            .unwrap_or_default(),
        items: Vec::new(),
        orig: Some(e.to_xml().into()),
    };
    table_children(ctx, e.elems().collect(), &mut t.items);
    t
}

/// Content of an sdt/customXml that may wrap rows or cells.
fn wrapper_content(c: &Element) -> Option<&Element> {
    if c.is_w("sdt") {
        c.wchild("sdtContent")
    } else if c.is_w("customXml") {
        Some(c)
    } else {
        None
    }
}

fn contains(c: &Element, local: &str) -> bool {
    wrapper_content(c).is_some_and(|w| w.elems().any(|x| x.is_w(local) || (wrapper_content(x).is_some() && contains(x, local))))
}

fn wrap_first_last<T>(items: &mut [T], start: usize, open: &str, close: &str, get: impl Fn(&mut T) -> Option<&mut Option<(Rc<str>, Rc<str>)>>) {
    let idx: Vec<usize> = (start..items.len()).filter(|&i| get(&mut items[i]).is_some()).collect();
    let (Some(&first), Some(&last)) = (idx.first(), idx.last()) else { return };
    if let Some(w) = get(&mut items[first]) {
        let (pre, post) = w.take().unwrap_or_else(|| ("".into(), "".into()));
        *w = Some((format!("{open}{pre}").into(), post));
    }
    if let Some(w) = get(&mut items[last]) {
        let (pre, post) = w.take().unwrap_or_else(|| ("".into(), "".into()));
        *w = Some((pre, format!("{post}{close}").into()));
    }
}

fn table_children(ctx: &mut Ctx, children: Vec<&Element>, items: &mut Vec<TableItem>) {
    for c in children {
        if c.is_w("tr") {
            items.push(TableItem::Row(row(ctx, c, None)));
        } else if contains(c, "tr") {
            let (open, close) = wrap_tags(c);
            let start = items.len();
            let inner = wrapper_content(c).unwrap();
            table_children(ctx, inner.elems().filter(|x| !x.is_w("customXmlPr")).collect(), items);
            wrap_first_last(items, start, &open, &close, |it| match it {
                TableItem::Row(r) => Some(&mut r.wrap),
                _ => None,
            });
        } else {
            items.push(TableItem::Raw(Rc::new(c.clone())));
        }
    }
}

fn row_children(ctx: &mut Ctx, children: Vec<&Element>, items: &mut Vec<RowItem>) {
    for c in children {
        if c.is_w("tc") {
            items.push(RowItem::Cell(cell(ctx, c, None)));
        } else if contains(c, "tc") {
            let (open, close) = wrap_tags(c);
            let start = items.len();
            let inner = wrapper_content(c).unwrap();
            row_children(ctx, inner.elems().filter(|x| !x.is_w("customXmlPr")).collect(), items);
            wrap_first_last(items, start, &open, &close, |it| match it {
                RowItem::Cell(c) => Some(&mut c.wrap),
                _ => None,
            });
        } else {
            items.push(RowItem::Raw(Rc::new(c.clone())));
        }
    }
}

fn wrap_tags(c: &Element) -> (String, String) {
    if c.is_w("sdt") {
        let content = c.wchild("sdtContent").expect("checked by contains()");
        sdt_tags(c, content)
    } else {
        let mut open = c.open_tag();
        for p in c.elems().filter(|x| x.is_w("customXmlPr")) {
            p.write_to(&mut open);
        }
        (open, c.close_tag())
    }
}

fn row(ctx: &mut Ctx, e: &Element, wrap: Option<(Rc<str>, Rc<str>)>) -> Row {
    let mut r = Row {
        open: e.open_tag().into(),
        close: e.close_tag().into(),
        wrap,
        props: e.wchild("trPr").map(RowProps::parse).unwrap_or_default(),
        items: Vec::new(),
    };
    row_children(ctx, e.elems().collect(), &mut r.items);
    r
}

fn cell(ctx: &mut Ctx, e: &Element, wrap: Option<(Rc<str>, Rc<str>)>) -> TableCell {
    let tcpr = e.wchild("tcPr");
    TableCell {
        open: e.open_tag().into(),
        close: e.close_tag().into(),
        wrap,
        tcpr: tcpr.map(|x| Rc::new(x.clone())),
        props: tcpr.map(CellProps::parse).unwrap_or(CellProps { span: 1, ..Default::default() }),
        content: container(ctx, e.children.iter().filter(|n| !matches!(n, Node::Elem(x) if x.is_w("tcPr")))),
    }
}
