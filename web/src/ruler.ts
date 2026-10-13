import type { Session } from "../pkg/papir_wasm.js";
import type { DocView } from "./view.js";

/** Horizontal ruler: page margins, centimetres, paragraph indents. */
export class Ruler {
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;

  /** marker being dragged and its x in pixels */
  private drag: { which: "first" | "left" | "right"; x: number } | null = null;
  private marks = { first: 0, left: 0, right: 0, origin: 0, rightEdge: 0, k: 1 };

  constructor(private s: Session, private v: DocView, private root: HTMLElement) {
    this.canvas = root.querySelector("canvas")!;
    this.ctx = this.canvas.getContext("2d")!;
    this.canvas.addEventListener("mousedown", (e) => this.down(e));
    window.addEventListener("mousemove", (e) => this.move(e));
    window.addEventListener("mouseup", () => this.up());
  }

  private local(e: MouseEvent): { x: number; y: number } {
    const r = this.canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  private down(e: MouseEvent): void {
    if (this.v.readOnly) return;
    const { x, y } = this.local(e);
    const top = y < this.canvas.clientHeight / 2;
    const m = this.marks;
    const pick: ["first" | "left" | "right", number][] = top ? [["first", m.first]] : [["left", m.left], ["right", m.right]];
    const hit = pick.find(([, mx]) => Math.abs(mx - x) <= 6);
    if (!hit) return;
    e.preventDefault();
    this.drag = { which: hit[0], x };
    this.canvas.classList.add("drag");
  }

  private move(e: MouseEvent): void {
    if (!this.drag) return;
    this.drag.x = this.local(e).x;
    this.draw();
  }

  private up(): void {
    const d = this.drag;
    if (!d) return;
    this.drag = null;
    this.canvas.classList.remove("drag");
    const m = this.marks;
    const f = this.v.state.fmt;
    // pixels → twips from the left margin; snap to 0.25 cm
    const snap = (pt: number) => Math.round(pt / (72 / 2.54 / 4)) * (72 / 2.54 / 4);
    const tw = (px: number) => Math.round(snap(px / m.k) * 20);
    let left = f.indLeft;
    let first = f.indFirst;
    let right = f.indRight;
    if (d.which === "left") {
      // the left marker moves the whole paragraph; first line keeps its offset
      left = Math.max(0, tw(d.x - m.origin));
    } else if (d.which === "first") {
      first = tw(d.x - m.origin) - left;
    } else {
      right = Math.max(0, tw(m.rightEdge - d.x));
    }
    this.v.exec(() => this.s.set_indents(left, first, right));
  }

  draw(): void {
    if (this.root.hidden) return;
    const st = this.v.state;
    const dpr = window.devicePixelRatio || 1;
    const cw = this.v.scroller.clientWidth;
    if (this.root.style.width !== `${cw}px`) this.root.style.width = `${cw}px`;
    const ch = this.root.clientHeight;
    if (this.canvas.width !== Math.round(cw * dpr)) this.canvas.width = Math.round(cw * dpr);
    if (this.canvas.height !== Math.round(ch * dpr)) this.canvas.height = Math.round(ch * dpr);
    const ctx = this.ctx;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cw, ch);
    const page = st?.caret?.page ?? this.v.currentPage();
    const box = this.v.pageBox(page);
    if (!box) return;
    const [, , left, right] = this.s.page_size(page);
    const k = box.scale;
    const x0 = Math.round(box.x);
    const css = getComputedStyle(document.documentElement);
    const v = (name: string, d: string) => css.getPropertyValue(name).trim() || d;
    const origin = x0 + left * k;
    const end = x0 + right * k;
    // the page: margins grey, the text area white
    const y1 = 3, y2 = ch - 3;
    ctx.fillStyle = v("--canvas", "#e6e8ec");
    ctx.fillRect(x0, y1, box.w, y2 - y1);
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(origin, y1, end - origin, y2 - y1);
    // centimetres from the left margin, a tick every quarter
    const cm = 72 / 2.54;
    ctx.strokeStyle = v("--tick", "#a3a8b0");
    ctx.fillStyle = v("--muted", "#5d6066");
    ctx.font = `9px "Papir UI", system-ui, sans-serif`;
    ctx.textAlign = "center";
    ctx.lineWidth = 1;
    const mid = ch / 2;
    ctx.beginPath();
    for (let i = -Math.floor((left / cm) * 4); (i * cm) / 4 < box.w / k - left; i++) {
      const x = Math.round(origin + ((i * cm) / 4) * k) + 0.5;
      if (x < x0 || x > x0 + box.w) continue;
      if (i % 4 === 0 && i !== 0) {
        ctx.fillText(String(Math.abs(i / 4)), x, mid + 3);
        continue;
      }
      const h = i % 2 === 0 ? 3.5 : 1.5;
      ctx.moveTo(x, mid - h);
      ctx.lineTo(x, mid + h);
    }
    ctx.stroke();
    ctx.textAlign = "start";
    // indents of the caret paragraph
    if (!st) return;
    const f = st.fmt;
    const tw = (t: number) => (t / 20) * k;
    let leftX = origin + tw(f.indLeft);
    let firstX = origin + tw(f.indLeft + f.indFirst);
    let rightX = end - tw(f.indRight);
    this.marks = { first: firstX, left: leftX, right: rightX, origin, rightEdge: end, k };
    const brand = v("--accent", "#0d63b3");
    if (this.drag) {
      if (this.drag.which === "first") firstX = this.drag.x;
      else if (this.drag.which === "left") {
        firstX += this.drag.x - leftX;
        leftX = this.drag.x;
      } else rightX = this.drag.x;
      ctx.strokeStyle = brand;
      ctx.beginPath();
      ctx.moveTo(this.drag.x + 0.5, 0);
      ctx.lineTo(this.drag.x + 0.5, ch);
      ctx.stroke();
    }
    if (this.v.readOnly) return;
    ctx.fillStyle = brand;
    const tri = (x: number, top: boolean) => {
      ctx.beginPath();
      if (top) {
        ctx.moveTo(x - 4.5, 1);
        ctx.lineTo(x + 4.5, 1);
        ctx.lineTo(x, 7);
      } else {
        ctx.moveTo(x - 4.5, ch - 1);
        ctx.lineTo(x + 4.5, ch - 1);
        ctx.lineTo(x, ch - 7);
      }
      ctx.closePath();
      ctx.fill();
    };
    tri(firstX, true);
    tri(leftX, false);
    tri(rightX, false);
  }
}
