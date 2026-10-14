import type { Session } from "../pkg/papir_wasm.js";
import type { DisplayList, Face, State } from "./types.js";
import { htmlToXml } from "./html.js";
import { t } from "./i18n.js";

const PT = 96 / 72;
const GAP = 14;
const TOP = 12;

interface PagePos { x: number; y: number; w: number; h: number }

export interface Remote {
  name: string;
  color: string;
  /** show the name (just moved) or only the caret */
  label: boolean;
  caret: { page: number; x: number; y: number; h: number } | null;
  sel: [number, number, number, number, number][];
}

/** Canvas view of the document with caret, selection and input. */
export class DocView {
  readonly scroller: HTMLElement;
  private spacer: HTMLElement;
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private caretEl: HTMLElement;
  readonly input: HTMLTextAreaElement;
  private fontCss: string[] = [];
  private pages: PagePos[] = [];
  private dls = new Map<number, DisplayList>();
  private images = new Map<string, ImageBitmap | "loading" | "error">();
  private drawQueued = false;
  private seenVersion = -1;
  private composing = false;
  private dragging = false;
  private dragTimer = 0;
  private lastMouse = { x: 0, y: 0 };
  zoom = 1;
  readOnly = false;
  /** space above the first page (room for the ruler) */
  top = TOP;
  /** other people's carets and selections */
  remote: Remote[] = [];
  /** zoom follows the width of the window until it is set by hand */
  autoFit = false;
  /** largest zoom the automatic fit goes to */
  fitMax = 1;
  /** misspelled words: page, x, y, w, h */
  spell: [number, number, number, number, number][] = [];
  state!: State;
  onState: (s: State) => void = () => {};
  onEdit: () => void = () => {};
  onShortcut: (e: KeyboardEvent) => boolean = () => false;
  onDraw: () => void = () => {};
  onImageError: (msg: string) => void = () => {};
  onContextMenu: (x: number, y: number, inTable: boolean, link: string | null) => void = () => {};
  private colDrag: { path: string; i: number; x0: number; page: number; cur: number } | null = null;
  /** corner being dragged on a selected picture: dx/dy direction of growth */
  private imgDrag: { sx: number; sy: number; x0: number; y0: number; w: number; h: number; nw: number; nh: number } | null = null;

  constructor(private s: Session, private faces: Face[], root: HTMLElement) {
    this.scroller = root.querySelector("#scroller")!;
    this.spacer = root.querySelector("#spacer")!;
    this.canvas = root.querySelector("#canvas")!;
    this.ctx = this.canvas.getContext("2d", { alpha: false })!;
    this.caretEl = root.querySelector("#caret")!;
    this.input = root.querySelector("#input")!;
    this.bind();
    new ResizeObserver(() => {
      if (this.autoFit) this.fit();
      this.layoutPages();
    }).observe(this.scroller);
    this.refresh(false);
  }

  get scale(): number {
    return this.zoom * PT;
  }

  // ---------- geometry ----------

  private layoutPages(): void {
    const sizes = this.s.page_sizes();
    const sc = this.scale;
    const vw = this.scroller.clientWidth;
    let maxW = 0;
    for (let i = 0; i < sizes.length; i += 2) maxW = Math.max(maxW, sizes[i] * sc);
    const contentW = Math.max(vw, maxW + 2 * GAP);
    let y = this.top;
    this.pages = [];
    for (let i = 0; i < sizes.length; i += 2) {
      const w = sizes[i] * sc;
      const h = sizes[i + 1] * sc;
      this.pages.push({ x: Math.round((contentW - w) / 2), y: Math.round(y), w, h });
      y += h + GAP;
    }
    this.spacer.style.width = `${contentW}px`;
    this.spacer.style.height = `${y + TOP}px`;
    this.placeCaret();
    this.requestDraw();
  }

  setTop(px: number): void {
    if (this.top === px) return;
    this.top = px;
    this.layoutPages();
  }

  pageCount(): number {
    return this.pages.length;
  }

  /** Index of the page at the viewport's vertical center. */
  currentPage(): number {
    const mid = this.scroller.scrollTop + this.scroller.clientHeight / 3;
    for (let i = 0; i < this.pages.length; i++) {
      if (this.pages[i].y + this.pages[i].h + GAP / 2 >= mid) return i;
    }
    return Math.max(0, this.pages.length - 1);
  }

  /** Page and point coordinates under a client position. */
  private hit(clientX: number, clientY: number): { page: number; x: number; y: number } | null {
    if (!this.pages.length) return null;
    const r = this.scroller.getBoundingClientRect();
    const cx = clientX - r.left + this.scroller.scrollLeft;
    const cy = clientY - r.top + this.scroller.scrollTop;
    let page = this.pages.length - 1;
    for (let i = 0; i < this.pages.length; i++) {
      if (cy < this.pages[i].y + this.pages[i].h + GAP / 2) {
        page = i;
        break;
      }
    }
    const p = this.pages[page];
    return { page, x: (cx - p.x) / this.scale, y: (cy - p.y) / this.scale };
  }

  setZoom(z: number): void {
    const before = this.scroller.scrollTop / Math.max(1, this.scroller.scrollHeight);
    this.zoom = Math.min(2, Math.max(0.5, z));
    this.layoutPages();
    this.scroller.scrollTop = before * this.scroller.scrollHeight;
    this.onState(this.state);
  }

  fitWidth(): void {
    const sizes = this.s.page_sizes();
    if (!sizes.length) return;
    this.setZoom((this.scroller.clientWidth - 2 * GAP - 20) / (sizes[0] * PT));
  }

  /** The page as wide as the window allows, up to `fitMax`; follows resizing. */
  fit(): void {
    const sizes = this.s.page_sizes();
    if (!sizes.length || !this.scroller.clientWidth) return;
    const z = Math.min(this.fitMax, (this.scroller.clientWidth - 2 * GAP - 20) / (sizes[0] * PT));
    const zz = Math.max(0.5, Math.round(z * 100) / 100);
    if (Math.abs(zz - this.zoom) > 0.005) {
      this.zoom = zz;
      this.layoutPages();
      this.onState(this.state);
    }
  }

  // ---------- state ----------

  refresh(scroll = true): void {
    this.state = JSON.parse(this.s.state());
    if (this.state.version !== this.seenVersion) {
      this.seenVersion = this.state.version;
      this.dls.clear();
      this.layoutPages();
    }
    this.placeCaret();
    if (scroll) this.revealCaret();
    this.requestDraw();
    this.onState(this.state);
  }

  /** Runs an editing command and refreshes. */
  exec(f: () => unknown, edit = true): void {
    if (edit && this.readOnly) return;
    const v = this.state?.version;
    f();
    this.refresh();
    if (edit && this.state.version !== v) this.onEdit();
  }

  private placeCaret(): void {
    const c = this.state?.caret;
    if (!c || !this.pages[c.page]) {
      this.caretEl.classList.remove("on");
      return;
    }
    const p = this.pages[c.page];
    const sc = this.scale;
    const x = p.x + c.x * sc;
    const y = p.y + c.y * sc;
    const h = Math.max(8, c.h * sc);
    Object.assign(this.caretEl.style, { left: `${x}px`, top: `${y}px`, height: `${h}px` });
    // restart blinking at every move
    this.caretEl.classList.remove("on");
    if (this.state.collapsed && !this.readOnly && document.activeElement === this.input) {
      void this.caretEl.offsetWidth;
      this.caretEl.classList.add("on");
    }
    Object.assign(this.input.style, { left: `${x}px`, top: `${y}px`, height: `${h}px`, fontSize: `${Math.max(10, (this.state.fmt.size ?? 11) * sc)}px` });
  }

  private revealCaret(): void {
    const c = this.state.caret;
    if (!c || !this.pages[c.page]) return;
    const p = this.pages[c.page];
    const sc = this.scale;
    const y = p.y + c.y * sc;
    const h = c.h * sc;
    const x = p.x + c.x * sc;
    const el = this.scroller;
    const m = 30;
    if (y < el.scrollTop + m) el.scrollTop = y - m;
    else if (y + h > el.scrollTop + el.clientHeight - m) el.scrollTop = y + h - el.clientHeight + m;
    if (x < el.scrollLeft + m) el.scrollLeft = x - m;
    else if (x > el.scrollLeft + el.clientWidth - m) el.scrollLeft = x - el.clientWidth + m;
  }

  focus(): void {
    this.input.focus({ preventScroll: true });
    this.placeCaret();
  }

  // ---------- drawing ----------

  requestDraw(): void {
    if (this.drawQueued) return;
    this.drawQueued = true;
    requestAnimationFrame(() => {
      this.drawQueued = false;
      this.draw();
    });
  }

  private dl(i: number): DisplayList {
    let d = this.dls.get(i);
    if (!d) {
      d = JSON.parse(this.s.page(i)) as DisplayList;
      this.dls.set(i, d);
    }
    return d;
  }

  private image(src: string): ImageBitmap | null {
    const im = this.images.get(src);
    if (im === undefined) {
      this.images.set(src, "loading");
      const bytes = this.s.media(src);
      if (!bytes) {
        this.images.set(src, "error");
        return null;
      }
      createImageBitmap(new Blob([bytes]))
        .then((b) => {
          this.images.set(src, b);
          this.requestDraw();
        })
        .catch(() => this.images.set(src, "error"));
      return null;
    }
    return im === "loading" || im === "error" ? null : im;
  }

  /** Page rectangle in viewport pixels and the points-to-pixels scale. */
  pageBox(i: number): { x: number; y: number; w: number; h: number; scale: number } | null {
    const p = this.pages[i];
    if (!p) return null;
    return { x: p.x - this.scroller.scrollLeft, y: p.y - this.scroller.scrollTop, w: p.w, h: p.h, scale: this.scale };
  }

  private draw(): void {
    this.onDraw();
    const dpr = window.devicePixelRatio || 1;
    const cw = this.canvas.clientWidth;
    const ch = this.canvas.clientHeight;
    if (this.canvas.width !== Math.round(cw * dpr) || this.canvas.height !== Math.round(ch * dpr)) {
      this.canvas.width = Math.round(cw * dpr);
      this.canvas.height = Math.round(ch * dpr);
    }
    const ctx = this.ctx;
    const css = getComputedStyle(document.documentElement);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = css.getPropertyValue("--canvas").trim() || "#e9e9e9";
    ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);
    const sx = this.scroller.scrollLeft;
    const sy = this.scroller.scrollTop;
    const sc = this.scale;
    const brand = css.getPropertyValue("--accent").trim() || "#0d63b3";
    const k = sc * dpr;
    for (let i = 0; i < this.pages.length; i++) {
      const p = this.pages[i];
      const top = p.y - sy;
      if (top > ch || top + p.h < 0) continue;
      const ox = (p.x - sx) * dpr;
      const oy = top * dpr;
      ctx.save();
      ctx.shadowColor = css.getPropertyValue("--page-shadow").trim() || "rgba(0,0,0,.2)";
      ctx.shadowBlur = 6 * dpr;
      ctx.shadowOffsetY = 1 * dpr;
      ctx.fillStyle = "#fff";
      ctx.fillRect(ox, oy, p.w * dpr, p.h * dpr);
      ctx.restore();
      ctx.save();
      ctx.beginPath();
      ctx.rect(ox, oy, p.w * dpr, p.h * dpr);
      ctx.clip();
      this.paint(this.dl(i), ox, oy, k);
      const st = this.state.story;
      if (st && st.kind === "footnote") {
        // the footnote stays clear, the rest of the page fades
        const [, , , , top, bottom] = this.s.page_size(i);
        const nt = st.page === i ? st.top ?? bottom : bottom;
        const nb = st.page === i ? st.bottom ?? bottom : bottom;
        ctx.fillStyle = "rgba(255,255,255,0.55)";
        ctx.fillRect(ox, oy + top * k, p.w * dpr, (nt - top) * k);
        ctx.fillRect(ox, oy + nb * k, p.w * dpr, (bottom - nb) * k);
        if (st.page === i) {
          ctx.strokeStyle = brand;
          ctx.lineWidth = dpr;
          ctx.setLineDash([4 * dpr, 3 * dpr]);
          ctx.strokeRect(ox + 4 * dpr, oy + nt * k - 2 * dpr, p.w * dpr - 8 * dpr, (nb - nt) * k + 4 * dpr);
          ctx.setLineDash([]);
          ctx.font = `${11 * dpr}px system-ui, sans-serif`;
          ctx.fillStyle = brand;
          ctx.fillText(t("Виноска"), ox + 8 * dpr, oy + nt * k - 6 * dpr);
        }
      } else if (st) {
        const [, , , , top, bottom] = this.s.page_size(i);
        ctx.fillStyle = "rgba(255,255,255,0.55)";
        ctx.fillRect(ox, oy + top * k, p.w * dpr, (bottom - top) * k);
        ctx.strokeStyle = brand;
        ctx.lineWidth = dpr;
        ctx.setLineDash([4 * dpr, 3 * dpr]);
        for (const yy of [top, bottom]) {
          ctx.beginPath();
          ctx.moveTo(ox, oy + yy * k + 0.5);
          ctx.lineTo(ox + p.w * dpr, oy + yy * k + 0.5);
          ctx.stroke();
        }
        ctx.setLineDash([]);
        ctx.font = `${11 * dpr}px system-ui, sans-serif`;
        ctx.fillStyle = brand;
        const label = st.kind === "footer" ? t("Нижній колонтитул") : t("Верхній колонтитул");
        const ly = st.kind === "footer" ? bottom * k + 14 * dpr : top * k - 5 * dpr;
        ctx.fillText(label, ox + 8 * dpr, oy + ly);
      }
      const im = this.state.image;
      if (im && im.page === i) {
        const w = this.imgDrag ? this.imgDrag.nw : im.w;
        const hh = this.imgDrag ? this.imgDrag.nh : im.h;
        // grow from the corner opposite to the dragged one
        const x = this.imgDrag && this.imgDrag.sx < 0 ? im.x + im.w - w : im.x;
        const y = this.imgDrag && this.imgDrag.sy < 0 ? im.y + im.h - hh : im.y;
        ctx.strokeStyle = brand;
        ctx.lineWidth = dpr;
        ctx.strokeRect(ox + x * k, oy + y * k, w * k, hh * k);
        ctx.fillStyle = "#fff";
        const hs = 4 * dpr;
        for (const [cx, cy] of [[x, y], [x + w, y], [x, y + hh], [x + w, y + hh]]) {
          ctx.fillRect(ox + cx * k - hs, oy + cy * k - hs, hs * 2, hs * 2);
          ctx.strokeRect(ox + cx * k - hs, oy + cy * k - hs, hs * 2, hs * 2);
        }
      }
      if (this.colDrag && this.colDrag.page === i) {
        ctx.fillStyle = brand;
        ctx.fillRect(ox + this.colDrag.cur * k - dpr / 2, oy, dpr, p.h * dpr);
      }
      // selection
      ctx.fillStyle = "rgba(51,132,255,0.28)";
      for (const [pg, x, y, w, h] of this.state.image ? [] : this.state.sel) {
        if (pg === i) ctx.fillRect(ox + x * k, oy + y * k, w * k, h * k);
      }
      this.drawSpell(i, ox, oy, k, dpr);
      this.drawRemote(i, ox, oy, k, dpr);
      ctx.restore();
    }
  }

  /** Red wavy lines under misspelled words. */
  private drawSpell(page: number, ox: number, oy: number, k: number, dpr: number): void {
    const ctx = this.ctx;
    ctx.strokeStyle = "#d93025";
    ctx.lineWidth = dpr;
    for (const [pg, x, y, w, h] of this.spell) {
      if (pg !== page) continue;
      const x0 = ox + x * k, x1 = ox + (x + w) * k, yb = oy + (y + h) * k - 1.5 * dpr;
      const step = 2 * dpr;
      ctx.beginPath();
      ctx.moveTo(x0, yb);
      for (let xx = x0, up = true; xx < x1; xx += step, up = !up) ctx.lineTo(Math.min(xx + step, x1), yb + (up ? -step / 1.5 : 0));
      ctx.stroke();
    }
  }

  /** Carets and selections of the others, with their names. */
  private drawRemote(page: number, ox: number, oy: number, k: number, dpr: number): void {
    const ctx = this.ctx;
    for (const r of this.remote) {
      ctx.globalAlpha = 0.2;
      ctx.fillStyle = r.color;
      for (const [pg, x, y, w, h] of r.sel) if (pg === page) ctx.fillRect(ox + x * k, oy + y * k, w * k, h * k);
      ctx.globalAlpha = 1;
      const c = r.caret;
      if (!c || c.page !== page) continue;
      const x = Math.round(ox + c.x * k);
      const y = oy + c.y * k;
      ctx.fillRect(x - dpr, y, 2 * dpr, c.h * k);
      if (!r.label) {
        // a small mark on top of the caret; the name only after a move
        ctx.fillRect(x - 2.5 * dpr, y - 2 * dpr, 5 * dpr, 5 * dpr);
        continue;
      }
      // name flag above the caret
      ctx.font = `600 ${11 * dpr}px "Papir UI", "Segoe UI", system-ui, sans-serif`;
      const tw = ctx.measureText(r.name).width;
      const fh = 16 * dpr;
      const fy = Math.max(oy, y - fh);
      ctx.fillRect(x - dpr, fy, tw + 10 * dpr, fh);
      ctx.fillStyle = "#fff";
      ctx.textBaseline = "middle";
      ctx.fillText(r.name, x + 4 * dpr, fy + fh / 2 + dpr / 2);
      ctx.textBaseline = "alphabetic";
    }
  }

  private paint(d: DisplayList, ox: number, oy: number, k: number): void {
    const ctx = this.ctx;
    (ctx as any).fontKerning = "none";
    ctx.textBaseline = "alphabetic";
    let curFont = "";
    for (const it of d.items) {
      switch (it[0]) {
        case "r": {
          ctx.fillStyle = it[5];
          ctx.fillRect(ox + it[1] * k, oy + it[2] * k, it[3] * k, it[4] * k);
          break;
        }
        case "p": {
          ctx.fillStyle = it[1];
          ctx.beginPath();
          for (let i = 2; i + 1 < it.length; i += 2) {
            const x = ox + it[i] * k, y = oy + it[i + 1] * k;
            if (i === 2) ctx.moveTo(x, y); else ctx.lineTo(x, y);
          }
          ctx.closePath();
          ctx.fill();
          break;
        }
        case "l": {
          const [, x1, y1, x2, y2, w, color, style] = it;
          ctx.strokeStyle = color;
          ctx.lineWidth = Math.max(1, w * k);
          ctx.setLineDash(style === 2 ? [w * k, w * k * 2] : style === 3 ? [w * k * 4, w * k * 3] : []);
          const snap = (v: number) => (ctx.lineWidth % 2 >= 1 ? Math.floor(v) + 0.5 : Math.round(v));
          if (style === 1) {
            const o = Math.max(1, w * k);
            const dx = y1 === y2 ? 0 : o;
            const dy = y1 === y2 ? o : 0;
            ctx.lineWidth = Math.max(1, w * k / 3);
            for (const s of [-1, 1]) {
              ctx.beginPath();
              ctx.moveTo(snap(ox + x1 * k + s * dx), snap(oy + y1 * k + s * dy));
              ctx.lineTo(snap(ox + x2 * k + s * dx), snap(oy + y2 * k + s * dy));
              ctx.stroke();
            }
          } else {
            ctx.beginPath();
            ctx.moveTo(snap(ox + x1 * k), snap(oy + y1 * k));
            ctx.lineTo(snap(ox + x2 * k), snap(oy + y2 * k));
            ctx.stroke();
          }
          ctx.setLineDash([]);
          break;
        }
        case "t": {
          const [, x, y, text, face, size, color] = it;
          const f = this.font(face).replace("{size}", (size * k).toFixed(2));
          if (f !== curFont) {
            ctx.font = f;
            curFont = f;
          }
          ctx.fillStyle = color;
          ctx.fillText(text, ox + x * k, oy + y * k);
          break;
        }
        case "i": {
          const [, x, y, w, h, src] = it;
          const im = src ? this.image(src) : null;
          if (im) ctx.drawImage(im, ox + x * k, oy + y * k, w * k, h * k);
          else {
            ctx.strokeStyle = "#bbb";
            ctx.lineWidth = 1;
            ctx.strokeRect(ox + x * k, oy + y * k, w * k, h * k);
          }
          break;
        }
      }
    }
  }

  private font(id: number): string {
    let css = this.fontCss[id];
    if (!css) {
      const f = this.faces[id] ?? this.faces[0];
      css = `${f.italic ? "italic " : ""}${f.bold ? "700" : "400"} {size}px "papir ${f.family}"`;
      this.fontCss[id] = css;
    }
    return css;
  }

  // ---------- input ----------

  private bind(): void {
    const sc = this.scroller;
    sc.addEventListener("scroll", () => this.requestDraw(), { passive: true });
    sc.addEventListener("mousedown", (e) => this.mouseDown(e));
    sc.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      const h = this.hit(e.clientX, e.clientY);
      if (!h) return;
      const link = this.s.link_at(h.page, h.x, h.y) ?? null;
      this.onContextMenu(e.clientX, e.clientY, this.state.table, link);
    });
    sc.addEventListener("dragover", (e) => {
      if (e.dataTransfer?.types.includes("Files")) e.preventDefault();
    });
    sc.addEventListener("drop", (e) => {
      const f = e.dataTransfer?.files[0];
      if (f && f.type.startsWith("image/")) {
        e.preventDefault();
        e.stopPropagation();
        const h = this.hit(e.clientX, e.clientY);
        if (h) this.s.click(h.page, h.x, h.y, false);
        void this.insertImageFile(f, (m) => this.onImageError(m));
      }
    });
    window.addEventListener("mousemove", (e) => this.mouseMove(e));
    window.addEventListener("mouseup", () => this.mouseUp());
    sc.addEventListener(
      "wheel",
      (e) => {
        if (e.ctrlKey) {
          e.preventDefault();
          this.setZoom(this.zoom * (e.deltaY < 0 ? 1.1 : 1 / 1.1));
        }
      },
      { passive: false },
    );
    const inp = this.input;
    inp.addEventListener("keydown", (e) => this.keyDown(e));
    inp.addEventListener("beforeinput", (e) => this.beforeInput(e as InputEvent));
    inp.addEventListener("input", () => {
      if (!this.composing && inp.value) {
        const v = inp.value;
        inp.value = "";
        this.exec(() => this.s.insert(v));
      }
    });
    inp.addEventListener("compositionstart", () => {
      this.composing = true;
      inp.classList.add("composing");
    });
    inp.addEventListener("compositionend", (e) => {
      this.composing = false;
      inp.classList.remove("composing");
      inp.value = "";
      if (e.data) this.exec(() => this.s.insert(e.data));
    });
    inp.addEventListener("focus", () => this.placeCaret());
    inp.addEventListener("blur", () => this.caretEl.classList.remove("on"));
    inp.addEventListener("copy", (e) => this.copy(e, false));
    inp.addEventListener("cut", (e) => this.copy(e, true));
    inp.addEventListener("paste", (e) => this.paste(e));
  }

  private mouseDown(e: MouseEvent): void {
    const h = this.hit(e.clientX, e.clientY);
    if (!h) return;
    if (e.button === 2) {
      // right click: keep a selection under the pointer, else move the caret;
      // the menu opens on the contextmenu event that follows
      e.preventDefault();
      this.focus();
      if (this.state.collapsed || !this.inSelection(h)) this.exec(() => this.s.click(h.page, h.x, h.y, false), false);
      return;
    }
    if (e.button !== 0) return;
    e.preventDefault();
    this.focus();
    if (e.ctrlKey || e.metaKey) {
      const link = this.s.link_at(h.page, h.x, h.y);
      if (link) {
        window.open(link, "_blank", "noopener,noreferrer");
        return;
      }
    }
    if (!this.readOnly) {
      const corner = this.handleAt(h);
      if (corner) {
        const im = this.state.image!;
        this.imgDrag = { sx: corner[0], sy: corner[1], x0: h.x, y0: h.y, w: im.w, h: im.h, nw: im.w, nh: im.h };
        return;
      }
      if (e.detail === 1 && this.s.select_image_at(h.page, h.x, h.y)) {
        this.refresh(false);
        return;
      }
      const b = this.s.column_border(h.page, h.x, h.y, 3 / this.scale);
      if (b) {
        const j = JSON.parse(b);
        this.colDrag = { path: JSON.stringify(j.path), i: j.i, x0: h.x, page: h.page, cur: h.x };
        return;
      }
    }
    if (e.detail === 2 && !this.readOnly) {
      // headers and footers open with a double click outside the body area,
      // a footnote with a double click on it; a double click elsewhere goes back
      const [, , , , top, bottom] = this.s.page_size(h.page);
      const st = this.state.story;
      const inside = st
        ? st.kind === "footnote"
          ? h.page === st.page && h.y >= (st.top ?? 0) && h.y <= (st.bottom ?? 0)
          : h.y < top || h.y > bottom
        : false;
      if (st && !inside) {
        this.exec(() => {
          this.s.exit_story();
          this.s.click(h.page, h.x, h.y, false);
        }, false);
        return;
      }
      if (!st && this.s.edit_story_at(h.page, h.x, h.y)) {
        this.refresh();
        return;
      }
    }
    if (e.detail === 2) {
      this.exec(() => this.s.select_word(), false);
    } else if (e.detail >= 3) {
      this.exec(() => this.s.select_para(), false);
    } else {
      this.exec(() => this.s.click(h.page, h.x, h.y, e.shiftKey), false);
      this.dragging = true;
      this.lastMouse = { x: e.clientX, y: e.clientY };
    }
  }

  /** Corner handle of the selected picture under a point: growth direction. */
  private handleAt(h: { page: number; x: number; y: number }): [number, number] | null {
    const im = this.state?.image;
    if (!im || im.page !== h.page) return null;
    const tol = 6 / this.scale;
    for (const [cx, cy, sx, sy] of [
      [im.x, im.y, -1, -1],
      [im.x + im.w, im.y, 1, -1],
      [im.x, im.y + im.h, -1, 1],
      [im.x + im.w, im.y + im.h, 1, 1],
    ]) {
      if (Math.abs(h.x - cx) <= tol && Math.abs(h.y - cy) <= tol) return [sx, sy];
    }
    return null;
  }

  private inSelection(h: { page: number; x: number; y: number }): boolean {
    return this.state.sel.some(([p, x, y, w, hh]) => p === h.page && h.x >= x && h.x <= x + w && h.y >= y && h.y <= y + hh);
  }

  private mouseMove(e: MouseEvent): void {
    if (this.imgDrag) {
      const h = this.hit(e.clientX, e.clientY);
      const d = this.imgDrag;
      if (h) {
        // keep the proportions: follow the larger relative change
        const fx = (d.w + d.sx * (h.x - d.x0)) / d.w;
        const fy = (d.h + d.sy * (h.y - d.y0)) / d.h;
        const f = Math.max(0.05, Math.abs(fx - 1) > Math.abs(fy - 1) ? fx : fy);
        d.nw = d.w * f;
        d.nh = d.h * f;
        this.requestDraw();
      }
      return;
    }
    if (this.colDrag) {
      const h = this.hit(e.clientX, e.clientY);
      if (h && h.page === this.colDrag.page) {
        this.colDrag.cur = h.x;
        this.requestDraw();
      }
      return;
    }
    if (!this.dragging) {
      // column borders show a resize cursor
      if (this.scroller.contains(e.target as Node) && !this.readOnly) {
        const h = this.hit(e.clientX, e.clientY);
        const near = !!h && !!this.s.column_border(h.page, h.x, h.y, 3 / this.scale);
        this.scroller.classList.toggle("col-resize", near);
        const corner = h ? this.handleAt(h) : null;
        this.scroller.style.cursor = corner ? (corner[0] * corner[1] > 0 ? "nwse-resize" : "nesw-resize") : "";
      }
      return;
    }
    this.lastMouse = { x: e.clientX, y: e.clientY };
    this.dragTo();
    // keep scrolling while the pointer is outside
    const r = this.scroller.getBoundingClientRect();
    const out = e.clientY < r.top || e.clientY > r.bottom;
    if (out && !this.dragTimer) {
      this.dragTimer = window.setInterval(() => {
        const r2 = this.scroller.getBoundingClientRect();
        const dy = this.lastMouse.y < r2.top ? this.lastMouse.y - r2.top : this.lastMouse.y > r2.bottom ? this.lastMouse.y - r2.bottom : 0;
        if (!dy) return;
        this.scroller.scrollTop += dy / 2;
        this.dragTo();
      }, 40);
    } else if (!out && this.dragTimer) {
      clearInterval(this.dragTimer);
      this.dragTimer = 0;
    }
  }

  private dragTo(): void {
    const r = this.scroller.getBoundingClientRect();
    const y = Math.min(Math.max(this.lastMouse.y, r.top + 1), r.bottom - 1);
    const h = this.hit(this.lastMouse.x, y);
    if (h) {
      this.s.click(h.page, h.x, h.y, true);
      this.refresh(false);
    }
  }

  private mouseUp(): void {
    if (this.imgDrag) {
      const d = this.imgDrag;
      const im = this.state.image;
      this.imgDrag = null;
      if (im && (Math.abs(d.nw - d.w) > 0.5 || Math.abs(d.nh - d.h) > 0.5)) {
        this.exec(() => {
          this.s.resize_image(JSON.stringify(im.path), im.off, d.nw, d.nh);
          // keep the picture selected
          this.s.set_selection(JSON.stringify({ path: im.path, off: im.off }), JSON.stringify({ path: im.path, off: im.off + 1 }));
        });
      } else this.requestDraw();
      return;
    }
    if (this.colDrag) {
      const d = this.colDrag;
      this.colDrag = null;
      if (Math.abs(d.cur - d.x0) > 0.5) this.exec(() => this.s.resize_column(d.path, d.i, d.cur - d.x0));
      else this.requestDraw();
      return;
    }
    this.dragging = false;
    if (this.dragTimer) {
      clearInterval(this.dragTimer);
      this.dragTimer = 0;
    }
  }

  private beforeInput(e: InputEvent): void {
    if (this.composing || e.isComposing) return;
    if (e.inputType === "insertText" && e.data) {
      e.preventDefault();
      this.exec(() => this.s.insert(e.data!));
    } else if (e.inputType === "insertLineBreak" || e.inputType === "insertParagraph") {
      e.preventDefault();
      this.exec(() => (e.inputType === "insertLineBreak" ? this.s.line_break() : this.s.enter()));
    } else if (e.inputType === "deleteContentBackward") {
      e.preventDefault();
      this.exec(() => this.s.backspace(false));
    } else if (e.inputType === "deleteContentForward") {
      e.preventDefault();
      this.exec(() => this.s.del(false));
    }
  }

  private keyDown(e: KeyboardEvent): void {
    if (this.composing || e.isComposing || e.keyCode === 229) return;
    const ctrl = e.ctrlKey || e.metaKey;
    const shift = e.shiftKey;
    if (this.onShortcut(e)) {
      e.preventDefault();
      return;
    }
    const nav = (unit: string, dir: number) => {
      e.preventDefault();
      this.exec(() => this.s.go(unit, dir, shift), false);
    };
    switch (e.key) {
      case "ArrowLeft":
        return nav(ctrl ? "word" : "char", -1);
      case "ArrowRight":
        return nav(ctrl ? "word" : "char", 1);
      case "ArrowUp":
        return nav(ctrl ? "para" : "vline", -1);
      case "ArrowDown":
        return nav(ctrl ? "para" : "vline", 1);
      case "Home":
        return nav(ctrl ? "doc" : "line", -1);
      case "End":
        return nav(ctrl ? "doc" : "line", 1);
      case "PageUp":
      case "PageDown": {
        e.preventDefault();
        const c = this.state.caret;
        if (!c) return;
        const p = this.pages[c.page];
        const dy = (e.key === "PageUp" ? -1 : 1) * this.scroller.clientHeight * 0.9;
        const r = this.scroller.getBoundingClientRect();
        const clientX = r.left + p.x + c.x * this.scale - this.scroller.scrollLeft;
        const clientY = r.top + p.y + c.y * this.scale - this.scroller.scrollTop + dy;
        this.scroller.scrollTop += dy;
        const h = this.hit(clientX, clientY);
        if (h) this.exec(() => this.s.click(h.page, h.x, h.y, shift), false);
        return;
      }
      case "Backspace":
        e.preventDefault();
        return this.exec(() => this.s.backspace(ctrl));
      case "Delete":
        e.preventDefault();
        return this.exec(() => this.s.del(ctrl));
      case "Enter":
        e.preventDefault();
        return this.exec(() => (ctrl ? this.s.page_break() : shift ? this.s.line_break() : this.s.enter()));
      case "Escape":
        if (this.state.story) {
          e.preventDefault();
          return this.exec(() => this.s.exit_story(), false);
        }
        return;
      case "Tab":
        e.preventDefault();
        // list level, next table cell, or a tab character
        return this.exec(() => {
          if (this.s.list_level(shift ? -1 : 1)) return;
          if (this.state.table && this.s.next_cell(shift ? -1 : 1)) return;
          if (!shift) this.s.insert("\t");
        });
    }
  }

  private copy(e: ClipboardEvent, cut: boolean): void {
    e.preventDefault();
    if (this.state.collapsed || !e.clipboardData) return;
    const text = this.s.copy_text();
    e.clipboardData.setData("text/plain", text);
    e.clipboardData.setData("text/html", `<meta charset="utf-8"><div>${escapeHtml(text).replace(/\n/g, "<br>")}</div>`);
    try {
      e.clipboardData.setData("application/x-papir", this.s.copy_xml());
    } catch {
      /* custom types unsupported */
    }
    if (cut) this.exec(() => this.s.cut());
  }

  private paste(e: ClipboardEvent): void {
    e.preventDefault();
    const d = e.clipboardData;
    if (!d) return;
    const img = [...d.files].find((f) => f.type.startsWith("image/"));
    if (img && !d.getData("application/x-papir")) {
      void this.insertImageFile(img, (m) => this.onImageError(m));
      return;
    }
    const xml = d.getData("application/x-papir");
    if (xml) {
      this.exec(() => this.s.paste_xml(xml));
      return;
    }
    // from other programs: formatting from HTML when there is any
    const html = d.getData("text/html");
    const fromHtml = html ? htmlToXml(html) : null;
    if (fromHtml && /<w:rPr>|<w:p>.*<w:p>/.test(fromHtml)) {
      this.exec(() => this.s.paste_xml(fromHtml));
      return;
    }
    const text = d.getData("text/plain");
    if (text) this.exec(() => this.s.paste_text(text));
  }

  /** Inserts a picture file at the caret. */
  async insertImageFile(f: File, fail: (msg: string) => void): Promise<void> {
    if (this.readOnly) return;
    const ext = ({ "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif" } as Record<string, string>)[f.type] ?? (f.name.split(".").pop() || "").toLowerCase();
    try {
      const bmp = await createImageBitmap(f);
      const bytes = new Uint8Array(await f.arrayBuffer());
      this.exec(() => this.s.insert_image(bytes, ext === "jpeg" ? "jpg" : ext, bmp.width, bmp.height));
    } catch {
      fail(t("Підтримуються зображення PNG, JPEG і GIF"));
    }
    this.focus();
  }

  /** Renders every page to PNG data URLs for printing. */
  async renderPages(scale = 2): Promise<string[]> {
    const out: string[] = [];
    const sizes = this.s.page_sizes();
    for (let i = 0; i < sizes.length / 2; i++) {
      const c = document.createElement("canvas");
      c.width = Math.round(sizes[i * 2] * PT * scale);
      c.height = Math.round(sizes[i * 2 + 1] * PT * scale);
      const saved = this.ctx;
      this.ctx = c.getContext("2d")!;
      this.ctx.fillStyle = "#fff";
      this.ctx.fillRect(0, 0, c.width, c.height);
      this.paint(this.dl(i), 0, 0, PT * scale);
      this.ctx = saved;
      out.push(c.toDataURL("image/png"));
      await new Promise((r) => setTimeout(r, 0));
    }
    return out;
  }
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]!));
}
