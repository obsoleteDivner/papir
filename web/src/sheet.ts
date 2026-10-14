import init, { Book } from "../pkg/papir_wasm.js";
import { initials } from "./ribbon.js";
import { t, translatePage } from "./i18n.js";

/** Settings from the server (absent when opened locally). */
interface Config { session?: string; api?: string; fileName?: string; canWrite?: boolean; user?: string; fontsUrl?: string; editUrl?: string }

interface Layout {
  dw: number; dh: number; cols: [number, number, number, boolean][]; rows: [number, number, boolean][];
  merges: [number, number, number, number][]; frozen: [number, number]; filter: [number, number, number, number] | null;
  grid: boolean; nrows: number; ncols: number;
}
interface Style {
  font: [string, number, boolean, boolean, boolean, boolean, number | null];
  fill: number | null; border: ([string, number] | null)[]; h: string | null; v: string | null; wrap: boolean; indent: number; rot: number;
}
/** row, col, text, style, kind (0 text, 1 number, 2 logical, 3 error, 4 empty), colour */
type CellData = [number, number, string, number, number, number | null];

const cfg: Config = (() => {
  try {
    return JSON.parse(document.getElementById("papir-config")?.textContent || "{}");
  } catch {
    return {};
  }
})();
const $ = <T extends HTMLElement>(s: string) => document.querySelector(s) as T;
const hex = (c: number) => "#" + c.toString(16).padStart(6, "0");

function toast(msg: string, ms = 3500): void {
  const t = $("#toast");
  t.textContent = msg;
  t.hidden = false;
  clearTimeout((toast as any).t);
  (toast as any).t = setTimeout(() => (t.hidden = true), ms);
}

/** Sizes along one axis: a default with exceptions (hidden = 0). */
class Axis {
  private idx: number[] = [];
  private delta: number[] = [];
  constructor(readonly def: number, private custom: Map<number, number>, public count: number) {
    this.idx = [...custom.keys()].sort((a, b) => a - b);
    let acc = 0;
    for (const i of this.idx) {
      this.delta.push(acc);
      acc += custom.get(i)! - def;
    }
    this.delta.push(acc);
  }
  size(i: number): number {
    return this.custom.get(i) ?? this.def;
  }
  /** Position of the start of i. */
  pos(i: number): number {
    let lo = 0, hi = this.idx.length;
    while (lo < hi) {
      const m = (lo + hi) >> 1;
      if (this.idx[m] < i) lo = m + 1;
      else hi = m;
    }
    return i * this.def + this.delta[lo];
  }
  total(): number {
    return this.pos(this.count);
  }
  /** Index at position p. */
  at(p: number): number {
    let lo = 0, hi = this.count;
    while (lo < hi) {
      const m = (lo + hi + 1) >> 1;
      if (this.pos(m) <= p) lo = m;
      else hi = m - 1;
    }
    return lo;
  }
}

const FONT_FOR: Record<string, string> = {
  calibri: "Carlito", "calibri light": "Carlito", cambria: "Caladea", arial: "Liberation Sans", "arial cyr": "Liberation Sans", helvetica: "Liberation Sans",
  "times new roman": "Liberation Serif", "times new roman cyr": "Liberation Serif", "courier new": "Liberation Mono", consolas: "Liberation Mono",
  verdana: "DejaVu Sans", tahoma: "DejaVu Sans", "segoe ui": "Liberation Sans", georgia: "DejaVu Serif",
};

class Fonts {
  private manifest: { family: string; bold: boolean; italic: boolean; file: string }[] = [];
  private loaded = new Map<string, Promise<void>>();
  constructor(private base: string) {}
  async init(): Promise<void> {
    try {
      this.manifest = await (await fetch(this.base + "fonts.json")).json();
    } catch {
      this.manifest = [];
    }
  }
  family(name: string): string {
    const f = FONT_FOR[name.toLowerCase()] ?? (this.manifest.some((m) => m.family.toLowerCase() === name.toLowerCase()) ? name : "Liberation Sans");
    return f;
  }
  css(name: string): string {
    return `"papir ${this.family(name)}", "${name}", sans-serif`;
  }
  load(names: string[]): Promise<void> {
    const fams = [...new Set(names.map((n) => this.family(n)))];
    return Promise.all(
      fams.map((fam) => {
        let p = this.loaded.get(fam);
        if (!p) {
          const faces = this.manifest.filter((m) => m.family === fam);
          p = Promise.all(faces.map(async (f) => {
            const ff = new FontFace(`papir ${fam}`, `url(${this.base}${f.file})`, { weight: f.bold ? "700" : "400", style: f.italic ? "italic" : "normal" });
            document.fonts.add(ff);
            await ff.load().catch(() => ff);
          })).then(() => undefined);
          this.loaded.set(fam, p);
        }
        return p;
      }),
    ).then(() => undefined);
  }
}

const BORDER_W: Record<string, number> = { hair: 0.5, thin: 1, dotted: 1, dashed: 1, dashDot: 1, dashDotDot: 1, medium: 2, mediumDashed: 2, mediumDashDot: 2, mediumDashDotDot: 2, slantDashDot: 2, thick: 3, double: 3 };
const BORDER_DASH: Record<string, number[]> = { dotted: [1, 1], hair: [1, 1], dashed: [3, 1], mediumDashed: [6, 2], dashDot: [5, 2, 1, 2], mediumDashDot: [6, 2, 2, 2], dashDotDot: [5, 2, 1, 2, 1, 2], slantDashDot: [6, 2, 2, 2] };

/** The grid: draws only what is visible, frozen panes stay. */
class Grid {
  private canvas = $<HTMLCanvasElement>("#xl-canvas");
  private ctx = this.canvas.getContext("2d", { alpha: false })!;
  private scroller = $("#xl-scroll");
  private spacer = $("#xl-spacer");
  sheet = 0;
  zoom = 1;
  lay!: Layout;
  styles: Style[] = [];
  rows!: Axis;
  cols!: Axis;
  /** rows hidden by the filter */
  filtered = new Set<number>();
  filterOn = false;
  private filterSel = new Map<number, Set<string>>();
  sel = { r: 0, c: 0, r2: 0, c2: 0 };
  private mergeAt = new Map<string, [number, number, number, number]>();
  private queued = false;
  onSelect: () => void = () => {};

  constructor(private book: Book, private fonts: Fonts) {
    this.styles = JSON.parse(book.styles());
    new ResizeObserver(() => this.resize()).observe(this.scroller);
    this.scroller.addEventListener("scroll", () => this.draw());
    this.bind();
  }

  private get hh(): number {
    return Math.round(20 * this.zoom);
  }
  private get hw(): number {
    const digits = String(Math.max(this.rows.count, 100)).length;
    return Math.round((14 + digits * 8) * this.zoom);
  }

  open(i: number): void {
    this.sheet = i;
    this.lay = JSON.parse(this.book.layout(i));
    this.filtered.clear();
    this.filterSel.clear();
    this.filterOn = !!this.lay.filter;
    this.mergeAt.clear();
    for (const m of this.lay.merges) {
      for (let r = m[0]; r <= m[2] && r - m[0] < 500; r++) for (let c = m[1]; c <= m[3] && c - m[1] < 200; c++) this.mergeAt.set(`${r},${c}`, m);
    }
    this.sel = { r: this.lay.frozen[0], c: this.lay.frozen[1], r2: this.lay.frozen[0], c2: this.lay.frozen[1] };
    this.sel = { r: 0, c: 0, r2: 0, c2: 0 };
    this.axes();
    this.scroller.scrollTop = 0;
    this.scroller.scrollLeft = 0;
    // fonts used by this sheet's styles
    void this.fonts.load([...new Set(this.styles.map((s) => s.font[0]))].slice(0, 12)).then(() => this.draw());
    this.onSelect();
    this.draw();
  }

  axes(): void {
    const l = this.lay;
    const rows = new Map<number, number>();
    for (const [r, h, hidden] of l.rows) rows.set(r, hidden ? 0 : h);
    for (const r of this.filtered) rows.set(r, 0);
    const cols = new Map<number, number>();
    for (const [a, b, w, hidden] of l.cols) for (let c = a; c <= Math.min(b, 16383); c++) cols.set(c, hidden ? 0 : w);
    const vh = this.scroller.clientHeight / this.zoom;
    const vw = this.scroller.clientWidth / this.zoom;
    this.rows = new Axis(l.dh, rows, Math.min(1048576, Math.max(l.nrows + 40, Math.ceil(vh / l.dh) + 5)));
    this.cols = new Axis(l.dw, cols, Math.min(16384, Math.max(l.ncols + 8, Math.ceil(vw / l.dw) + 3)));
    this.resize();
  }

  setZoom(z: number): void {
    const fx = this.scroller.scrollLeft / Math.max(1, this.scroller.scrollWidth);
    const fy = this.scroller.scrollTop / Math.max(1, this.scroller.scrollHeight);
    this.zoom = z;
    this.axes();
    this.scroller.scrollLeft = fx * this.scroller.scrollWidth;
    this.scroller.scrollTop = fy * this.scroller.scrollHeight;
    this.draw();
  }

  private resize(): void {
    if (!this.rows) return;
    const z = this.zoom;
    this.spacer.style.width = `${this.hw + this.cols.total() * z}px`;
    this.spacer.style.height = `${this.hh + this.rows.total() * z}px`;
    this.draw();
  }

  draw(): void {
    if (this.queued) return;
    this.queued = true;
    requestAnimationFrame(() => {
      this.queued = false;
      this.paint();
    });
  }

  // ---------- geometry (screen pixels) ----------

  private x(c: number): number {
    const fc = this.lay.frozen[1];
    return this.hw + this.cols.pos(c) * this.zoom - (c >= fc ? this.scroller.scrollLeft : 0);
  }
  private y(r: number): number {
    const fr = this.lay.frozen[0];
    return this.hh + this.rows.pos(r) * this.zoom - (r >= fr ? this.scroller.scrollTop : 0);
  }
  private fw(): number {
    return this.cols.pos(this.lay.frozen[1]) * this.zoom;
  }
  private fh(): number {
    return this.rows.pos(this.lay.frozen[0]) * this.zoom;
  }

  /** Cell under a point of the view. */
  hit(px: number, py: number): { r: number; c: number } {
    const z = this.zoom;
    const fx = this.hw + this.fw();
    const fy = this.hh + this.fh();
    const cx = px < fx ? (px - this.hw) / z : (px - this.hw + this.scroller.scrollLeft) / z;
    const cy = py < fy ? (py - this.hh) / z : (py - this.hh + this.scroller.scrollTop) / z;
    return { r: Math.max(0, Math.min(this.rows.count - 1, this.rows.at(Math.max(0, cy)))), c: Math.max(0, Math.min(this.cols.count - 1, this.cols.at(Math.max(0, cx)))) };
  }

  /** Visible indices of the scrolled part. */
  private visible(): { r0: number; r1: number; c0: number; c1: number } {
    const z = this.zoom;
    const s = this.scroller;
    const fr = this.lay.frozen[0], fc = this.lay.frozen[1];
    const r0 = Math.max(fr, this.rows.at((s.scrollTop + this.fh()) / z));
    const r1 = Math.min(this.rows.count - 1, this.rows.at((s.scrollTop + s.clientHeight - this.hh) / z) + 1);
    const c0 = Math.max(fc, this.cols.at((s.scrollLeft + this.fw()) / z));
    const c1 = Math.min(this.cols.count - 1, this.cols.at((s.scrollLeft + s.clientWidth - this.hw) / z) + 1);
    return { r0, r1, c0, c1 };
  }

  // ---------- painting ----------

  private fetch(r0: number, r1: number, c0: number, c1: number, into: Map<string, CellData>): void {
    if (r1 < r0 || c1 < c0) return;
    for (const d of JSON.parse(this.book.range(this.sheet, r0, r1, c0, c1)) as CellData[]) into.set(`${d[0]},${d[1]}`, d);
  }

  private font(st: Style | undefined): string {
    const f = st?.font ?? ["Calibri", 11, false, false, false, false, null];
    return `${f[3] ? "italic " : ""}${f[2] ? "700 " : ""}${(f[1] * 96 / 72) * this.zoom}px ${this.fonts.css(f[0])}`;
  }

  /** colours of the theme (CSS variables), read once per frame */
  private theme = { brand: "#0d63b3", active: "#dde9f5", range: "rgba(13,99,179,0.12)" };

  private paint(): void {
    const css = getComputedStyle(document.documentElement);
    const v = (n: string, d: string) => css.getPropertyValue(n).trim() || d;
    this.theme = { brand: v("--accent", "#0d63b3"), active: v("--active", "#dde9f5"), range: v("--range", "rgba(13,99,179,0.12)") };
    const dpr = window.devicePixelRatio || 1;
    const cw = this.canvas.clientWidth, ch = this.canvas.clientHeight;
    if (this.canvas.width !== Math.round(cw * dpr) || this.canvas.height !== Math.round(ch * dpr)) {
      this.canvas.width = Math.round(cw * dpr);
      this.canvas.height = Math.round(ch * dpr);
    }
    const ctx = this.ctx;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, cw, ch);
    if (!this.lay) return;
    const { r0, r1, c0, c1 } = this.visible();
    const fr = this.lay.frozen[0], fc = this.lay.frozen[1];
    // data: the visible part, frozen panes, and some columns around for overflowing text
    const cells = new Map<string, CellData>();
    const cm0 = Math.max(fc, c0 - 10), cm1 = Math.min(this.cols.count - 1, c1 + 10);
    this.fetch(r0, r1, cm0, cm1, cells);
    if (fr) this.fetch(0, fr - 1, cm0, cm1, cells);
    if (fc) this.fetch(r0, r1, 0, fc - 1, cells);
    if (fr && fc) this.fetch(0, fr - 1, 0, fc - 1, cells);
    const vw = this.scroller.clientWidth, vh = this.scroller.clientHeight;
    const fx = this.hw + this.fw(), fy = this.hh + this.fh();
    const quads: [number, number, number, number, number, number, number, number][] = [
      // rows a..b, cols c..d, clip x, y, w, h
      [r0, r1, c0, c1, fx, fy, vw - fx, vh - fy],
    ];
    if (fr) quads.push([0, fr - 1, c0, c1, fx, this.hh, vw - fx, fy - this.hh]);
    if (fc) quads.push([r0, r1, 0, fc - 1, this.hw, fy, fx - this.hw, vh - fy]);
    if (fr && fc) quads.push([0, fr - 1, 0, fc - 1, this.hw, this.hh, fx - this.hw, fy - this.hh]);
    for (const [a, b, c, d, x, y, w, h] of quads) {
      if (w <= 0 || h <= 0) continue;
      ctx.save();
      ctx.beginPath();
      ctx.rect(x, y, w, h);
      ctx.clip();
      this.region(a, b, c, d, cells);
      ctx.restore();
    }
    this.selection(fx, fy, vw, vh);
    this.headers(r0, r1, c0, c1, fx, fy, vw, vh);
    // the lines of frozen panes
    ctx.fillStyle = "#b6bcc4";
    if (fr) ctx.fillRect(0, fy - 0.5, vw, 1.5);
    if (fc) ctx.fillRect(fx - 0.5, 0, 1.5, vh);
  }

  private merged(r: number, c: number): [number, number, number, number] | undefined {
    return this.mergeAt.get(`${r},${c}`);
  }

  private region(r0: number, r1: number, c0: number, c1: number, cells: Map<string, CellData>): void {
    const ctx = this.ctx;
    const z = this.zoom;
    // gridlines
    if (this.lay.grid) {
      ctx.fillStyle = "#e2e3e5";
      for (let c = c0; c <= c1 + 1; c++) ctx.fillRect(Math.round(this.x(c)) - 0.5, this.y(r0), 1, this.y(r1 + 1) - this.y(r0));
      for (let r = r0; r <= r1 + 1; r++) ctx.fillRect(this.x(c0), Math.round(this.y(r)) - 0.5, this.x(c1 + 1) - this.x(c0), 1);
    }
    // merged areas: one cell, no lines inside
    const merges = this.lay.merges.filter((m) => m[2] >= r0 && m[0] <= r1 && m[3] >= c0 && m[1] <= c1);
    const rect = (r: number, c: number): [number, number, number, number] => {
      const m = this.merged(r, c);
      const [ra, ca, rb, cb] = m ?? [r, c, r, c];
      return [this.x(ca), this.y(ra), this.x(cb + 1) - this.x(ca), this.y(rb + 1) - this.y(ra)];
    };
    for (const m of merges) {
      const [x, y, w, h] = rect(m[0], m[1]);
      const d = cells.get(`${m[0]},${m[1]}`);
      ctx.fillStyle = d && this.styles[d[3]]?.fill != null ? hex(this.styles[d[3]].fill!) : "#fff";
      ctx.fillRect(x + 0.5, y + 0.5, w - 1, h - 1);
    }
    // fills
    for (const d of cells.values()) {
      const [r, c, , s] = d;
      if (r < r0 || r > r1 || c < c0 || c > c1 || this.rows.size(r) === 0) continue;
      const st = this.styles[s];
      if (st?.fill == null || this.merged(r, c)) continue;
      const [x, y, w, h] = rect(r, c);
      ctx.fillStyle = hex(st.fill);
      ctx.fillRect(x - 0.5, y - 0.5, w + 1, h + 1);
    }
    // text
    const occupied = (r: number, c: number) => {
      const d = cells.get(`${r},${c}`);
      return (d && d[2] !== "") || !!this.merged(r, c);
    };
    const drawText = (d: CellData) => {
      const [r, c, text, s, kind, color] = d;
      if (!text || this.rows.size(r) === 0) return;
      const m = this.merged(r, c);
      if (m && (m[0] !== r || m[1] !== c)) return;
      const st = this.styles[s];
      let [x, y, w, h] = rect(r, c);
      if (w <= 0 || h <= 0) return;
      ctx.font = this.font(st);
      const fs = ((st?.font[1] ?? 11) * 96 / 72) * z;
      let align = st?.h ?? "general";
      if (align === "general") align = kind === 1 ? "right" : kind === 2 || kind === 3 ? "center" : "left";
      if (align === "centerContinuous" || align === "distributed") align = "center";
      if (align === "fill" || align === "justify") align = "left";
      const pad = 2 * z + (st?.indent ?? 0) * 9 * z;
      const fg = color != null ? hex(color) : st?.font[6] != null ? hex(st.font[6]!) : "#000";
      ctx.fillStyle = fg;
      ctx.textBaseline = "alphabetic";
      const lines: string[] = [];
      if (st?.wrap) {
        for (const para of text.split("\n")) {
          let line = "";
          for (const word of para.split(/(\s+)/)) {
            const t = line + word;
            if (line && ctx.measureText(t).width > w - 2 * pad) {
              lines.push(line.trimEnd());
              line = word.trimStart();
            } else line = t;
          }
          lines.push(line);
        }
      } else lines.push(text.replace(/\n/g, " "));
      let tw = Math.max(...lines.map((l) => ctx.measureText(l).width));
      let clipX = x, clipW = w;
      if (!st?.wrap && !m && tw > w - 2 * pad) {
        if (kind === 1) {
          // a number that does not fit: ####
          const n = Math.max(1, Math.floor((w - 2 * pad) / ctx.measureText("#").width));
          lines[0] = "#".repeat(n);
          tw = ctx.measureText(lines[0]).width;
        } else if (kind === 0) {
          // text runs over empty neighbours
          let cr = c + 1;
          if (align !== "right") while (cr < this.cols.count && cr <= c + 60 && !occupied(r, cr) && this.x(cr) - x < tw + 2 * pad) cr++;
          let cl = c - 1;
          if (align !== "left") while (cl >= 0 && cl >= c - 60 && !occupied(r, cl) && x - this.x(cl) < tw) cl--;
          clipX = this.x(cl + 1);
          clipW = this.x(cr) - clipX;
        }
      }
      const lh = fs * 1.22;
      const total = lines.length * lh;
      const va = st?.v ?? "bottom";
      let ty = va === "top" ? y + lh * 0.82 + 1 : va === "center" || va === "distributed" ? y + (h - total) / 2 + lh * 0.82 : y + h - total + lh * 0.82 - 2 * z;
      ctx.save();
      ctx.beginPath();
      ctx.rect(clipX, y, clipW, h);
      ctx.clip();
      for (const l of lines) {
        const lw = ctx.measureText(l).width;
        const tx = align === "right" ? x + w - pad - lw : align === "center" ? x + (w - lw) / 2 : x + pad;
        ctx.fillText(l, tx, ty);
        if (st?.font[4] || st?.font[5]) {
          const yy = st.font[4] ? ty + 2 * z : ty - fs * 0.3;
          ctx.fillRect(tx, yy, lw, Math.max(1, z));
        }
        ty += lh;
      }
      ctx.restore();
    };
    for (const d of cells.values()) if (d[0] >= r0 && d[0] <= r1 && d[1] >= Math.max(0, c0 - 10) && d[1] <= c1 + 10) drawText(d);
    // merges whose first cell is outside the window still show their text
    for (const m of merges) if (m[0] < r0 || m[1] < c0) {
      const d = cells.get(`${m[0]},${m[1]}`) ?? (JSON.parse(this.book.range(this.sheet, m[0], m[0], m[1], m[1])) as CellData[])[0];
      if (d) drawText(d);
    }
    // borders
    for (const d of cells.values()) {
      const [r, c, , s] = d;
      if (r < r0 || r > r1 || c < c0 || c > c1) continue;
      const b = this.styles[s]?.border;
      if (!b || !b.some((x) => x)) continue;
      const [x, y, w, h] = rect(r, c);
      const edges: [number, number, number, number][] = [[x, y, x, y + h], [x + w, y, x + w, y + h], [x, y, x + w, y], [x, y + h, x + w, y + h]];
      b.forEach((side, i) => {
        if (!side) return;
        const [style, color] = side;
        const lw = (BORDER_W[style] ?? 1) * Math.max(1, z * 0.9);
        ctx.strokeStyle = hex(color);
        ctx.lineWidth = style === "double" ? 1 : lw;
        ctx.setLineDash((BORDER_DASH[style] ?? []).map((v) => v * lw * 2));
        const [ax, ay, bx, by] = edges[i];
        const off = (lw % 2) / 2;
        if (style === "double") {
          for (const k of [-1, 1]) {
            ctx.beginPath();
            const dx = ax === bx ? k : 0, dy = ay === by ? k : 0;
            ctx.moveTo(Math.round(ax) + dx + 0.5, Math.round(ay) + dy + 0.5);
            ctx.lineTo(Math.round(bx) + dx + 0.5, Math.round(by) + dy + 0.5);
            ctx.stroke();
          }
        } else {
          ctx.beginPath();
          ctx.moveTo(Math.round(ax) + off, Math.round(ay) + off);
          ctx.lineTo(Math.round(bx) + off, Math.round(by) + off);
          ctx.stroke();
        }
      });
      ctx.setLineDash([]);
    }
    // filter buttons in the header row
    const f = this.lay.filter;
    if (f && this.filterOn && f[0] >= r0 && f[0] <= r1) {
      for (let c = Math.max(f[1], c0); c <= Math.min(f[3], c1); c++) {
        const bx = this.x(c + 1) - 17 * z, by = this.y(f[0] + 1) - 17 * z, s = 15 * z;
        ctx.fillStyle = this.filterSel.has(c) ? this.theme.active : "#f3f4f6";
        ctx.strokeStyle = "#9aa1aa";
        ctx.lineWidth = 1;
        ctx.fillRect(bx, by, s, s);
        ctx.strokeRect(Math.round(bx) + 0.5, Math.round(by) + 0.5, s - 1, s - 1);
        ctx.fillStyle = "#3a3f45";
        ctx.beginPath();
        ctx.moveTo(bx + s * 0.28, by + s * 0.4);
        ctx.lineTo(bx + s * 0.72, by + s * 0.4);
        ctx.lineTo(bx + s * 0.5, by + s * 0.66);
        ctx.fill();
      }
    }
  }

  private selection(fx: number, fy: number, vw: number, vh: number): void {
    const ctx = this.ctx;
    const s = this.sel;
    const [ra, rb] = [Math.min(s.r, s.r2), Math.max(s.r, s.r2)];
    const [ca, cb] = [Math.min(s.c, s.c2), Math.max(s.c, s.c2)];
    ctx.save();
    ctx.beginPath();
    ctx.rect(this.hw, this.hh, vw - this.hw, vh - this.hh);
    ctx.clip();
    const x = this.x(ca), y = this.y(ra), w = this.x(cb + 1) - x, h = this.y(rb + 1) - y;
    if (ra !== rb || ca !== cb) {
      ctx.fillStyle = this.theme.range;
      ctx.fillRect(x, y, w, h);
    }
    ctx.strokeStyle = this.theme.brand;
    ctx.lineWidth = 2;
    ctx.strokeRect(Math.round(x), Math.round(y), Math.round(w), Math.round(h));
    ctx.restore();
    void fx;
    void fy;
  }

  private headers(r0: number, r1: number, c0: number, c1: number, fx: number, fy: number, vw: number, vh: number): void {
    const ctx = this.ctx;
    const z = this.zoom;
    const s = this.sel;
    const [ra, rb] = [Math.min(s.r, s.r2), Math.max(s.r, s.r2)];
    const [ca, cb] = [Math.min(s.c, s.c2), Math.max(s.c, s.c2)];
    ctx.fillStyle = "#f7f8fa";
    ctx.fillRect(0, 0, vw, this.hh);
    ctx.fillRect(0, 0, this.hw, vh);
    ctx.font = `${12 * z}px "Papir UI", "Segoe UI", system-ui, sans-serif`;
    ctx.textBaseline = "middle";
    ctx.textAlign = "center";
    const fr = this.lay.frozen[0], fc = this.lay.frozen[1];
    const colIdx = [...Array(fc).keys(), ...Array.from({ length: Math.max(0, c1 - c0 + 1) }, (_, k) => c0 + k)];
    const rowIdx = [...Array(fr).keys(), ...Array.from({ length: Math.max(0, r1 - r0 + 1) }, (_, k) => r0 + k)];
    for (const c of colIdx) {
      const x = this.x(c), w = this.x(c + 1) - x;
      if (w <= 0 || (c >= fc && x + w < fx)) continue;
      const on = c >= ca && c <= cb;
      if (on) {
        ctx.fillStyle = this.theme.active;
        ctx.fillRect(x, 0, w, this.hh);
      }
      ctx.fillStyle = "#d6d9de";
      ctx.fillRect(Math.round(x + w) - 0.5, 0, 1, this.hh);
      ctx.fillStyle = on ? this.theme.brand : "#5d6066";
      if (w > 10 * z) ctx.fillText(colName(c), x + w / 2, this.hh / 2 + 0.5);
    }
    for (const r of rowIdx) {
      const y = this.y(r), h = this.y(r + 1) - y;
      if (h <= 0 || (r >= fr && y + h < fy)) continue;
      const on = r >= ra && r <= rb;
      if (on) {
        ctx.fillStyle = this.theme.active;
        ctx.fillRect(0, y, this.hw, h);
      }
      ctx.fillStyle = "#d6d9de";
      ctx.fillRect(0, Math.round(y + h) - 0.5, this.hw, 1);
      ctx.fillStyle = on || (this.filtered.size && this.lay.filter && r > this.lay.filter[0]) ? this.theme.brand : "#5d6066";
      if (h > 8 * z) ctx.fillText(String(r + 1), this.hw / 2, y + h / 2 + 0.5);
    }
    ctx.fillStyle = "#f7f8fa";
    ctx.fillRect(0, 0, this.hw, this.hh);
    ctx.fillStyle = "#d6d9de";
    ctx.fillRect(0, Math.round(this.hh) - 0.5, vw, 1);
    ctx.fillRect(Math.round(this.hw) - 0.5, 0, 1, vh);
    ctx.textAlign = "start";
    ctx.textBaseline = "alphabetic";
  }

  // ---------- input ----------

  select(r: number, c: number, extend = false): void {
    r = Math.max(0, Math.min(r, this.rows.count - 1));
    c = Math.max(0, Math.min(c, this.cols.count - 1));
    if (extend) {
      this.sel.r2 = r;
      this.sel.c2 = c;
    } else {
      const m = this.merged(r, c);
      this.sel = m ? { r: m[0], c: m[1], r2: m[2], c2: m[3] } : { r, c, r2: r, c2: c };
    }
    this.reveal(extend ? r : this.sel.r, extend ? c : this.sel.c);
    this.onSelect();
    this.draw();
  }

  /** Scrolls so that the cell is visible. */
  reveal(r: number, c: number): void {
    const s = this.scroller, z = this.zoom;
    const fr = this.lay.frozen[0], fc = this.lay.frozen[1];
    if (r >= fr) {
      const top = this.rows.pos(r) * z - this.fh(), bot = this.rows.pos(r + 1) * z - this.fh();
      if (top < s.scrollTop) s.scrollTop = top;
      else if (bot > s.scrollTop + s.clientHeight - this.hh - this.fh()) s.scrollTop = bot - (s.clientHeight - this.hh - this.fh());
    }
    if (c >= fc) {
      const l = this.cols.pos(c) * z - this.fw(), rr = this.cols.pos(c + 1) * z - this.fw();
      if (l < s.scrollLeft) s.scrollLeft = l;
      else if (rr > s.scrollLeft + s.clientWidth - this.hw - this.fw()) s.scrollLeft = rr - (s.clientWidth - this.hw - this.fw());
    }
  }

  private bind(): void {
    const s = this.scroller;
    let dragging = false;
    const local = (e: MouseEvent) => {
      const r = s.getBoundingClientRect();
      return { x: e.clientX - r.left, y: e.clientY - r.top };
    };
    s.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      const p = local(e);
      if (p.x > s.clientWidth || p.y > s.clientHeight) return;
      s.focus();
      if (p.y < this.hh && p.x > this.hw) {
        // a column header: the whole column
        const { c } = this.hit(p.x, this.hh + 1);
        this.sel = { r: 0, c, r2: Math.max(0, this.lay.nrows - 1), c2: c };
        this.onSelect();
        this.draw();
        return;
      }
      if (p.x < this.hw && p.y > this.hh) {
        const { r } = this.hit(this.hw + 1, p.y);
        this.sel = { r, c: 0, r2: r, c2: Math.max(0, this.lay.ncols - 1) };
        this.onSelect();
        this.draw();
        return;
      }
      const { r, c } = this.hit(p.x, p.y);
      // a filter button?
      const f = this.lay.filter;
      if (f && this.filterOn && r === f[0] && c >= f[1] && c <= f[3] && p.x > this.x(c + 1) - 18 * this.zoom && p.y > this.y(r + 1) - 18 * this.zoom) {
        // the document's "click elsewhere closes menus" must not see this click
        e.stopPropagation();
        this.openFilter(c, e.clientX, e.clientY);
        return;
      }
      this.select(r, c, e.shiftKey);
      dragging = true;
    });
    window.addEventListener("mousemove", (e) => {
      if (!dragging) return;
      const p = local(e);
      const { r, c } = this.hit(Math.max(this.hw + 1, p.x), Math.max(this.hh + 1, p.y));
      if (r !== this.sel.r2 || c !== this.sel.c2) this.select(r, c, true);
    });
    window.addEventListener("mouseup", () => (dragging = false));
    s.addEventListener("wheel", (e) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      const z = Math.min(2, Math.max(0.5, Math.round((this.zoom - Math.sign(e.deltaY) * 0.1) * 10) / 10));
      this.setZoom(z);
      ($("#xl-zoom") as HTMLSelectElement).value = String(Math.round(z * 100));
    }, { passive: false });
    s.addEventListener("keydown", (e) => {
      const page = Math.max(1, Math.floor((s.clientHeight - this.hh) / (this.lay.dh * this.zoom)) - 1);
      const sel = this.sel;
      const r = e.shiftKey ? sel.r2 : sel.r, c = e.shiftKey ? sel.c2 : sel.c;
      const go = (nr: number, nc: number) => {
        e.preventDefault();
        // skip hidden rows and columns
        const dr = Math.sign(nr - r), dc = Math.sign(nc - c);
        while (dr && nr > 0 && nr < this.rows.count - 1 && this.rows.size(nr) === 0) nr += dr;
        while (dc && nc > 0 && nc < this.cols.count - 1 && this.cols.size(nc) === 0) nc += dc;
        this.select(nr, nc, e.shiftKey);
      };
      const ctrl = e.ctrlKey || e.metaKey;
      switch (e.key) {
        case "ArrowDown": return go(ctrl ? Math.max(0, this.lay.nrows - 1) : r + 1, c);
        case "ArrowUp": return go(ctrl ? 0 : r - 1, c);
        case "ArrowRight": return go(r, ctrl ? Math.max(0, this.lay.ncols - 1) : c + 1);
        case "ArrowLeft": return go(r, ctrl ? 0 : c - 1);
        case "PageDown": return go(r + page, c);
        case "PageUp": return go(r - page, c);
        case "Home": return go(ctrl ? 0 : r, 0);
        case "End": if (ctrl) return go(Math.max(0, this.lay.nrows - 1), Math.max(0, this.lay.ncols - 1)); break;
        case "Enter": return go(r + (e.shiftKey ? -1 : 1), c);
        case "Tab": return go(r, c + (e.shiftKey ? -1 : 1));
      }
      if (ctrl && e.code === "KeyA") {
        e.preventDefault();
        this.sel = { r: 0, c: 0, r2: Math.max(0, this.lay.nrows - 1), c2: Math.max(0, this.lay.ncols - 1) };
        this.onSelect();
        this.draw();
      } else if (ctrl && e.code === "KeyC") {
        e.preventDefault();
        void navigator.clipboard.writeText(this.tsv()).then(() => toast(t("Скопійовано")), () => toast(t("Браузер не дав доступу до буфера")));
      }
    });
  }

  /** The selection as tab-separated text (what Excel puts on the clipboard). */
  tsv(): string {
    const s = this.sel;
    const [ra, rb] = [Math.min(s.r, s.r2), Math.min(Math.max(s.r, s.r2), Math.min(s.r, s.r2) + 20000)];
    const [ca, cb] = [Math.min(s.c, s.c2), Math.max(s.c, s.c2)];
    const m = new Map<string, string>();
    for (const d of JSON.parse(this.book.range(this.sheet, ra, rb, ca, cb)) as CellData[]) m.set(`${d[0]},${d[1]}`, d[2]);
    const out: string[] = [];
    for (let r = ra; r <= rb; r++) {
      if (this.rows.size(r) === 0) continue;
      const row: string[] = [];
      for (let c = ca; c <= cb; c++) row.push((m.get(`${r},${c}`) ?? "").replace(/\t|\n/g, " "));
      out.push(row.join("\t"));
    }
    return out.join("\n");
  }

  // ---------- filter ----------

  private openFilter(c: number, x: number, y: number): void {
    const f = this.lay.filter!;
    const menu = $("#xl-filter-menu");
    const vals = new Map<string, number>();
    for (const d of JSON.parse(this.book.range(this.sheet, f[0] + 1, Math.max(f[2], this.lay.nrows - 1), c, c)) as CellData[]) vals.set(d[2], (vals.get(d[2]) ?? 0) + 1);
    const empty = Math.max(0, Math.max(f[2], this.lay.nrows - 1) - f[0] - [...vals.values()].reduce((a, b) => a + b, 0));
    if (empty) vals.set("", empty);
    const keys = [...vals.keys()].sort((a, b) => a.localeCompare(b, "uk", { numeric: true })).slice(0, 1000);
    const cur = this.filterSel.get(c);
    const esc = (t: string) => t.replace(/[&<>"]/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[ch]!);
    menu.innerHTML = `<label><input type="checkbox" data-all checked> <b>${t("Виділити все")}</b></label><div class="xf-list">${keys
      .map((k, i) => `<label><input type="checkbox" data-i="${i}" ${!cur || cur.has(k) ? "checked" : ""}> ${k === "" ? t("(Пусті)") : esc(k)}</label>`)
      .join("")}</div><div class="xf-acts"><button data-cancel>${t("Скасувати")}</button><button class="primary" data-ok>${t("Застосувати")}</button></div>`;
    const boxes = [...menu.querySelectorAll<HTMLInputElement>("[data-i]")];
    const all = menu.querySelector<HTMLInputElement>("[data-all]")!;
    all.checked = boxes.every((b) => b.checked);
    all.onchange = () => boxes.forEach((b) => (b.checked = all.checked));
    menu.querySelector<HTMLButtonElement>("[data-cancel]")!.onclick = () => (menu.hidden = true);
    menu.querySelector<HTMLButtonElement>("[data-ok]")!.onclick = () => {
      menu.hidden = true;
      const keep = new Set(keys.filter((_, i) => boxes[i].checked));
      if (keep.size === keys.length) this.filterSel.delete(c);
      else this.filterSel.set(c, keep);
      this.applyFilter();
    };
    menu.hidden = false;
    menu.style.left = `${Math.min(x, innerWidth - menu.offsetWidth - 8)}px`;
    menu.style.top = `${Math.min(y + 4, innerHeight - menu.offsetHeight - 8)}px`;
  }

  applyFilter(): void {
    const f = this.lay.filter;
    this.filtered.clear();
    if (f && this.filterSel.size) {
      const last = Math.max(f[2], this.lay.nrows - 1);
      const data = new Map<string, string>();
      for (const d of JSON.parse(this.book.range(this.sheet, f[0] + 1, last, f[1], f[3])) as CellData[]) data.set(`${d[0]},${d[1]}`, d[2]);
      for (let r = f[0] + 1; r <= last; r++) {
        for (const [c, keep] of this.filterSel) if (!keep.has(data.get(`${r},${c}`) ?? "")) {
          this.filtered.add(r);
          break;
        }
      }
    }
    this.axes();
    const shown = f ? Math.max(f[2], this.lay.nrows - 1) - f[0] - this.filtered.size : 0;
    $("#xl-status").textContent = this.filterSel.size ? t("Фільтр: показано {0} рядків", shown) : t("Готово");
  }

  toggleFilter(): void {
    if (!this.lay.filter) {
      // no filter in the file: the used range, headers in the selected row
      if (!this.lay.ncols) return toast(t("Аркуш порожній"));
      const r = Math.min(this.sel.r, this.sel.r2);
      this.lay.filter = [r, 0, Math.max(r, this.lay.nrows - 1), this.lay.ncols - 1];
      this.filterOn = true;
    } else {
      this.filterOn = !this.filterOn;
      if (!this.filterOn) {
        this.filterSel.clear();
        this.applyFilter();
      }
    }
    this.draw();
  }

  /** The used range as an HTML table (printing). */
  html(maxRows = 5000): string {
    const nr = Math.min(this.lay.nrows, maxRows), nc = this.lay.ncols;
    if (!nr || !nc) return "";
    const m = new Map<string, CellData>();
    for (const d of JSON.parse(this.book.range(this.sheet, 0, nr - 1, 0, nc - 1)) as CellData[]) m.set(`${d[0]},${d[1]}`, d);
    const skip = new Set<string>();
    const esc = (t: string) => t.replace(/[&<>]/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[ch]!);
    let out = "<table>";
    for (let r = 0; r < nr; r++) {
      if (this.rows.size(r) === 0) continue;
      out += "<tr>";
      for (let c = 0; c < nc; c++) {
        if (skip.has(`${r},${c}`) || this.cols.size(c) === 0) continue;
        const mg = this.merged(r, c);
        let span = "";
        if (mg) {
          for (let a = mg[0]; a <= mg[2]; a++) for (let b = mg[1]; b <= mg[3]; b++) if (a !== r || b !== c) skip.add(`${a},${b}`);
          span = ` rowspan="${mg[2] - mg[0] + 1}" colspan="${mg[3] - mg[1] + 1}"`;
        }
        const d = m.get(`${r},${c}`);
        const st = d ? this.styles[d[3]] : undefined;
        const css = [
          st?.fill != null ? `background:${hex(st.fill)}` : "",
          st?.font[2] ? "font-weight:700" : "",
          st?.font[3] ? "font-style:italic" : "",
          d && d[5] != null ? `color:${hex(d[5])}` : st?.font[6] != null ? `color:${hex(st.font[6]!)}` : "",
          d && (st?.h === "right" || (!st?.h && d[4] === 1)) ? "text-align:right" : st?.h === "center" ? "text-align:center" : "",
        ].filter(Boolean).join(";");
        out += `<td${span}${css ? ` style="${css}"` : ""}>${d ? esc(d[2]) : ""}</td>`;
      }
      out += "</tr>";
    }
    return out + "</table>";
  }
}

function colName(c: number): string {
  let s = "";
  c += 1;
  while (c > 0) {
    const m = (c - 1) % 26;
    s = String.fromCharCode(65 + m) + s;
    c = Math.floor((c - 1) / 26);
  }
  return s;
}

async function main(): Promise<void> {
  translatePage();
  await init();
  const local = !cfg.session;
  let name = cfg.fileName ?? t("таблиця.xlsx");
  let bytes: Uint8Array;
  if (local) {
    const src = new URLSearchParams(location.search).get("open");
    if (!src) {
      toast(t("Вкажіть файл: ?open=шлях.xlsx"), 10000);
      return;
    }
    const r = await fetch(src);
    if (!r.ok) {
      toast(t("Не вдалося отримати файл ({0})", r.status), 10000);
      return;
    }
    bytes = new Uint8Array(await r.arrayBuffer());
    name = decodeURIComponent(src.split("/").pop() || name);
  } else {
    const r = await fetch(`${cfg.api}/session/${cfg.session}/file`);
    if (!r.ok) {
      toast(r.status === 401 ? t("Сесію завершено. Оновіть сторінку в системі.") : t("Не вдалося отримати файл"), 10000);
      return;
    }
    bytes = new Uint8Array(await r.arrayBuffer());
  }
  document.title = t("{0} — Папір", name);
  $("#doc-title").textContent = name;
  const me = cfg.user || t("Користувач");
  $("#avatar").textContent = initials(me);
  $("#avatar").title = me;
  $("#share-list").innerHTML = `<div class="sm-user"><span class="avatar">${initials(me)}</span><span></span><small>${t("ви")}</small></div>`;
  ($("#share-list").querySelector("span:not(.avatar)") as HTMLElement).textContent = me;
  let book: Book;
  try {
    book = new Book(bytes);
  } catch (e) {
    toast(t("Не вдалося відкрити таблицю ({0})", (e as Error).message ?? e), 10000);
    return;
  }
  const fonts = new Fonts(cfg.fontsUrl ?? new URL("../fonts/", location.href).href);
  await fonts.init();
  const grid = new Grid(book, fonts);
  const sheets: { name: string; hidden: boolean; color: number | null }[] = JSON.parse(book.sheets());
  const tabs = $("#xl-tabs");
  const visible = sheets.map((s, i) => ({ ...s, i })).filter((s) => !s.hidden);
  for (const s of visible) {
    const b = document.createElement("button");
    b.textContent = s.name;
    if (s.color != null) b.style.boxShadow = `inset 0 -3px 0 ${hex(s.color)}`;
    b.addEventListener("click", () => {
      tabs.querySelectorAll("button").forEach((x) => x.classList.toggle("active", x === b));
      grid.open(s.i);
      $("#xl-scroll").focus();
    });
    tabs.appendChild(b);
  }
  if (visible.length < 2) tabs.innerHTML = "";
  // formula bar and status line follow the selection
  grid.onSelect = () => {
    const s = grid.sel;
    const [ra, rb] = [Math.min(s.r, s.r2), Math.max(s.r, s.r2)];
    const [ca, cb] = [Math.min(s.c, s.c2), Math.max(s.c, s.c2)];
    const cell = JSON.parse(book.cell(grid.sheet, s.r, s.c));
    $("#xl-addr").textContent = ra === rb && ca === cb ? cell.addr : `${colName(ca)}${ra + 1}:${colName(cb)}${rb + 1}`;
    $("#xl-content").textContent = cell.formula ?? cell.text;
    $("#xl-content").title = cell.formula ?? cell.text;
    const st = ra === rb && ca === cb ? null : JSON.parse(book.stats(grid.sheet, ra, ca, rb, cb));
    $("#xl-stats").textContent = st && st.count > 1 ? (st.nums ? t("Середнє: {0}    Кількість: {1}    Сума: {2}", st.avg, st.count, st.sum) : t("Кількість: {0}", st.count)) : "";
  };
  const first = visible[0]?.i ?? 0;
  (tabs.querySelector("button") as HTMLElement | null)?.classList.add("active");
  grid.open(first);
  $("#xl-scroll").focus();
  (window as any).papir = { book, grid };

  // toolbar
  const zoom = $("#xl-zoom") as HTMLSelectElement;
  zoom.addEventListener("change", () => grid.setZoom(Number(zoom.value) / 100));
  const edit = $("#xl-edit") as HTMLButtonElement;
  if (!cfg.editUrl || cfg.canWrite === false) {
    edit.disabled = true;
    edit.title = cfg.editUrl ? t("Відкрийте документ для редагування в системі") : t("Редагування таблиць — у Collabora; її адресу не налаштовано");
  }
  const run: Record<string, () => void> = {
    download: () => {
      const a = document.createElement("a");
      a.href = URL.createObjectURL(new Blob([bytes], { type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" }));
      a.download = name;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 10000);
    },
    print: () => {
      const root = $("#print-root");
      root.innerHTML = grid.html();
      if (!root.innerHTML) return toast(t("Аркуш порожній"));
      window.print();
      root.innerHTML = "";
    },
    filter: () => {
      grid.toggleFilter();
      $('[data-x="filter"]').classList.toggle("active", grid.filterOn);
    },
    edit: () => {
      if (cfg.editUrl) location.href = cfg.editUrl;
    },
  };
  document.querySelectorAll<HTMLButtonElement>("[data-x]").forEach((b) => b.addEventListener("click", () => run[b.dataset.x!]?.()));
  $('[data-x="filter"]').classList.toggle("active", grid.filterOn);
  // sharing popover
  const share = $("#share-menu");
  $(".people").addEventListener("click", (e) => {
    e.stopPropagation();
    share.hidden = !share.hidden;
    const r = $(".people").getBoundingClientRect();
    share.style.left = `${Math.min(r.left, innerWidth - share.offsetWidth - 8)}px`;
    share.style.top = `${r.bottom + 4}px`;
  });
  document.addEventListener("mousedown", (e) => {
    if (!(e.target as HTMLElement).closest(".menu, .people")) document.querySelectorAll<HTMLElement>(".menu").forEach((m) => (m.hidden = true));
  });
  // search: Enter — next, Shift+Enter — previous
  const search = $("#xl-search") as HTMLInputElement;
  search.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      search.value = "";
      $("#xl-scroll").focus();
      return;
    }
    if (e.key !== "Enter" || !search.value.trim()) return;
    e.preventDefault();
    const hit = JSON.parse(book.find(grid.sheet, search.value.trim(), grid.sel.r, grid.sel.c, e.shiftKey));
    if (!hit) return toast(t("Нічого не знайдено"));
    grid.select(hit[0], hit[1]);
  });
  document.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.code === "KeyF") {
      e.preventDefault();
      search.focus();
      search.select();
    } else if ((e.ctrlKey || e.metaKey) && e.code === "KeyP") {
      e.preventDefault();
      run.print();
    }
  });
}

main().catch((e) => {
  console.error(e);
  toast(t("Помилка запуску: {0}", e.message || e), 10000);
});
