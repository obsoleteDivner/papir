/**
 * Clipboard HTML (Word, browsers) → WordprocessingML paragraphs for
 * `Session.paste_xml`. Keeps paragraphs, line breaks, bold, italic,
 * underline, strike, sub/superscript, colour, size and font.
 */

interface Fmt {
  b?: boolean;
  i?: boolean;
  u?: boolean;
  s?: boolean;
  va?: "superscript" | "subscript";
  color?: string;
  sz?: number;
  font?: string;
}

const BLOCK = new Set(["P", "DIV", "LI", "H1", "H2", "H3", "H4", "H5", "H6", "TR", "BLOCKQUOTE", "PRE", "SECTION", "ARTICLE", "UL", "OL", "TABLE"]);
const HEAD_SIZE: Record<string, number> = { H1: 20, H2: 16, H3: 14, H4: 12, H5: 11, H6: 11 };

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]!));

function hex(c: string): string | undefined {
  const m = c.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  if (m) return [m[1], m[2], m[3]].map((x) => Number(x).toString(16).padStart(2, "0")).join("").toUpperCase();
  const h = c.match(/^#([0-9a-f]{6}|[0-9a-f]{3})$/i);
  if (h) return (h[1].length === 3 ? h[1].replace(/./g, "$&$&") : h[1]).toUpperCase();
  return undefined;
}

function size(v: string): number | undefined {
  const m = v.match(/([\d.]+)\s*(pt|px)/);
  if (!m) return undefined;
  const pt = m[2] === "px" ? Number(m[1]) * 0.75 : Number(m[1]);
  return pt > 0 && pt < 400 ? pt : undefined;
}

function styleOf(el: HTMLElement, f: Fmt): Fmt {
  const n: Fmt = { ...f };
  const t = el.tagName;
  if (t === "B" || t === "STRONG") n.b = true;
  if (t === "I" || t === "EM") n.i = true;
  if (t === "U" || t === "INS") n.u = true;
  if (t === "S" || t === "STRIKE" || t === "DEL") n.s = true;
  if (t === "SUP") n.va = "superscript";
  if (t === "SUB") n.va = "subscript";
  if (HEAD_SIZE[t]) {
    n.b = true;
    n.sz = HEAD_SIZE[t];
  }
  if (t === "FONT") {
    const c = el.getAttribute("color");
    if (c) n.color = hex(c) ?? n.color;
    const face = el.getAttribute("face");
    if (face) n.font = face.split(",")[0].replace(/["']/g, "").trim();
  }
  const st = el.style;
  if (st) {
    const w = st.fontWeight;
    if (w === "bold" || Number(w) >= 600) n.b = true;
    else if (w === "normal" || (Number(w) > 0 && Number(w) < 600)) n.b = false;
    if (st.fontStyle === "italic") n.i = true;
    else if (st.fontStyle === "normal") n.i = false;
    const dec = st.textDecorationLine || st.textDecoration;
    if (dec.includes("underline")) n.u = true;
    if (dec.includes("line-through")) n.s = true;
    if (st.color) n.color = hex(st.color) ?? n.color;
    if (st.fontSize) n.sz = size(st.fontSize) ?? n.sz;
    if (st.fontFamily) n.font = st.fontFamily.split(",")[0].replace(/["']/g, "").trim() || n.font;
    if (st.verticalAlign === "super") n.va = "superscript";
    if (st.verticalAlign === "sub") n.va = "subscript";
  }
  return n;
}

function rpr(f: Fmt): string {
  let s = "";
  if (f.font) s += `<w:rFonts w:ascii="${esc(f.font)}" w:hAnsi="${esc(f.font)}" w:cs="${esc(f.font)}"/>`;
  if (f.b) s += "<w:b/><w:bCs/>";
  if (f.i) s += "<w:i/><w:iCs/>";
  if (f.s) s += "<w:strike/>";
  if (f.color && f.color !== "000000") s += `<w:color w:val="${f.color}"/>`;
  if (f.sz) s += `<w:sz w:val="${Math.round(f.sz * 2)}"/><w:szCs w:val="${Math.round(f.sz * 2)}"/>`;
  if (f.u) s += `<w:u w:val="single"/>`;
  if (f.va) s += `<w:vertAlign w:val="${f.va}"/>`;
  return s ? `<w:rPr>${s}</w:rPr>` : "";
}

export function htmlToXml(html: string): string | null {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const paras: string[] = [];
  let cur = "";
  let open = false;
  const flush = (force = false) => {
    if (open || force) paras.push(`<w:p>${cur}</w:p>`);
    cur = "";
    open = false;
  };
  const text = (t: string, f: Fmt) => {
    t = t.replace(/[\r\n\t ]+/g, " ");
    if (!t) return;
    if (!open && t.trim() === "") return;
    open = true;
    cur += `<w:r>${rpr(f)}<w:t xml:space="preserve">${esc(t)}</w:t></w:r>`;
  };
  const walk = (n: Node, f: Fmt) => {
    if (n.nodeType === Node.TEXT_NODE) return text(n.textContent ?? "", f);
    if (n.nodeType !== Node.ELEMENT_NODE) return;
    const el = n as HTMLElement;
    const t = el.tagName;
    if (t === "STYLE" || t === "SCRIPT" || t === "HEAD" || t === "TITLE" || t === "META") return;
    if (t === "BR") {
      open = true;
      cur += "<w:r><w:br/></w:r>";
      return;
    }
    if (t === "TD" || t === "TH") {
      el.childNodes.forEach((c) => walk(c, styleOf(el, f)));
      text("\t", f);
      return;
    }
    const block = BLOCK.has(t);
    if (block) flush();
    const g = styleOf(el, f);
    el.childNodes.forEach((c) => walk(c, g));
    if (block) flush();
  };
  walk(doc.body, {});
  flush();
  if (!paras.length) return null;
  return `<papir xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">${paras.join("")}</papir>`;
}
