import type { Session } from "../pkg/papir_wasm.js";
import type { DocView } from "./view.js";

/** A word on a page: paragraph path, offsets. */
interface Occ { path: number[]; a: number; b: number; word: string }

// letters with apostrophes and hyphens inside (м'ята, будь-який)
const WORD = /[\p{L}\p{M}]+(?:['’ʼ`-][\p{L}\p{M}]+)*/gu;

/**
 * Spelling of the pages on screen: words go to a worker with the
 * dictionaries, misspelled ones get a red wavy line.
 */
export class Spelling {
  private worker: Worker | null = null;
  private known = new Map<string, boolean>();
  private pending = new Set<string>();
  private waits = new Map<number, (v: any) => void>();
  private seq = 0;
  private ready = false;
  private timer = 0;
  private ignored = new Set<string>();
  bad: Occ[] = [];
  enabled = true;

  constructor(private s: Session, private v: DocView, private base: string) {
    try {
      this.enabled = localStorage.getItem("papir:spell") !== "off";
    } catch {
      /* storage unavailable */
    }
    if (this.enabled) this.start();
  }

  private personal(): string[] {
    try {
      return JSON.parse(localStorage.getItem("papir:dict") || "[]");
    } catch {
      return [];
    }
  }

  private start(): void {
    if (this.worker) return;
    this.worker = new Worker(new URL("./spell-worker.js", import.meta.url), { type: "module" });
    this.worker.onmessage = (e) => {
      const m = e.data;
      if (m.t === "ready") {
        this.ready = true;
        this.schedule();
      } else if (m.id != null && this.waits.has(m.id)) {
        this.waits.get(m.id)!(m);
        this.waits.delete(m.id);
      }
    };
    this.worker.postMessage({ t: "load", base: this.base, dicts: [["uk", "uk_UA"], ["en", "en_US"]], personal: this.personal() });
  }

  private ask(msg: any): Promise<any> {
    const id = ++this.seq;
    return new Promise((res) => {
      this.waits.set(id, res);
      this.worker!.postMessage({ ...msg, id });
    });
  }

  /** `remember`: keep the choice for next time (not for viewing-only documents) */
  setEnabled(on: boolean, remember = true): void {
    this.enabled = on;
    if (remember) {
      try {
        localStorage.setItem("papir:spell", on ? "on" : "off");
      } catch {
        /* ignore */
      }
    }
    if (on) {
      this.start();
      this.schedule();
    } else {
      this.bad = [];
      this.v.spell = [];
      this.v.requestDraw();
    }
  }

  /** Checks again a little after the last change or scroll. */
  schedule(): void {
    if (!this.enabled || !this.ready) return;
    clearTimeout(this.timer);
    this.timer = window.setTimeout(() => void this.run(), 350);
  }

  private async run(): Promise<void> {
    const cur = this.v.currentPage();
    const occ: Occ[] = [];
    for (const page of [cur - 1, cur, cur + 1, cur + 2]) {
      if (page < 0 || page >= this.v.pageCount()) continue;
      for (const [path, text] of JSON.parse(this.s.page_texts(page)) as [number[], string][]) {
        // offsets in characters (as the editor counts them)
        const chars = [...text];
        const idx: number[] = [];
        let n = 0;
        for (const ch of chars) {
          idx.push(n);
          n += ch.length;
        }
        for (const m of text.matchAll(WORD)) {
          const a = idx.findIndex((x) => x === m.index);
          occ.push({ path, a, b: a + [...m[0]].length, word: m[0] });
        }
      }
    }
    const fresh = [...new Set(occ.map((o) => o.word))].filter((w) => !this.known.has(w) && !this.pending.has(w));
    if (fresh.length) {
      fresh.forEach((w) => this.pending.add(w));
      const r = await this.ask({ t: "check", words: fresh });
      const bad = new Set<number>(r.bad);
      fresh.forEach((w, i) => {
        this.known.set(w, !bad.has(i));
        this.pending.delete(w);
      });
    }
    const st = this.v.state;
    // the word being typed is not marked yet
    const typing = (o: Occ) => st.collapsed && st.focus.path.join() === o.path.join() && st.focus.off >= o.a && st.focus.off <= o.b;
    this.bad = occ.filter((o) => this.known.get(o.word) === false && !this.ignored.has(o.word) && !typing(o));
    this.paint();
  }

  /** Rectangles of the misspelled words for the view. */
  paint(): void {
    const rects: [number, number, number, number, number][] = [];
    for (const o of this.bad) {
      const r = JSON.parse(this.s.rects_for(JSON.stringify({ path: o.path, off: o.a }), JSON.stringify({ path: o.path, off: o.b })));
      if (r) rects.push(...r.sel);
    }
    this.v.spell = rects;
    this.v.requestDraw();
  }

  /** The misspelled word at a position, if any. */
  at(path: number[], off: number): Occ | null {
    return this.bad.find((o) => o.path.join() === path.join() && off >= o.a && off <= o.b) ?? null;
  }

  async suggest(word: string): Promise<string[]> {
    if (!this.ready) return [];
    return (await this.ask({ t: "suggest", word })).words;
  }

  ignore(word: string): void {
    this.ignored.add(word);
    this.bad = this.bad.filter((o) => o.word !== word);
    this.paint();
  }

  addWord(word: string): void {
    const p = this.personal();
    if (!p.includes(word)) p.push(word);
    try {
      localStorage.setItem("papir:dict", JSON.stringify(p));
    } catch {
      /* ignore */
    }
    this.known.set(word, true);
    this.worker?.postMessage({ t: "add", word });
    this.ignore(word);
  }
}
