import type { Session } from "../pkg/papir_wasm.js";
import type { DocView } from "./view.js";
import type { State } from "./types.js";

const FONTS = ["Times New Roman", "Arial", "Calibri", "Cambria", "Courier New", "Verdana", "Tahoma", "Georgia", "Liberation Serif", "Liberation Sans", "DejaVu Sans"];
const SIZES = [8, 9, 10, 10.5, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 36, 48, 72];
const COLORS = [
  "000000", "434343", "666666", "999999", "B7B7B7", "D9D9D9", "EFEFEF", "FFFFFF",
  "C00000", "FF0000", "FFC000", "FFFF00", "92D050", "00B050", "00B0F0", "0070C0",
  "002060", "7030A0", "E6B8B7", "F8CBAD", "FFE699", "C6E0B4", "BDD7EE", "D9D2E9",
];
const HIGHLIGHTS: [string, string][] = [
  ["yellow", "#ffff00"], ["green", "#00ff00"], ["cyan", "#00ffff"], ["magenta", "#ff00ff"],
  ["blue", "#0000ff"], ["red", "#ff0000"], ["darkBlue", "#000080"], ["darkCyan", "#008080"],
  ["darkGreen", "#008000"], ["darkMagenta", "#800080"], ["darkRed", "#800000"], ["darkYellow", "#808000"],
  ["darkGray", "#808080"], ["lightGray", "#c0c0c0"], ["black", "#000000"], ["white", "#ffffff"],
];

export interface Host {
  save(): Promise<void>;
  download(): void;
  downloadPdf(): void;
  print(): Promise<void>;
  openLocal(): void;
  toast(msg: string): void;
  ensureFont(name: string): Promise<void>;
  newComment(): void;
  toggleComments(): void;
  /** CSS font family that draws a document font */
  cssFont(name: string | null): string;
  /** spelling on/off; returns the new state */
  toggleSpell(): boolean;
}

interface StyleInfo { id: string; name: string; q: boolean; font: string | null; size: number | null; color: string | null; b: boolean; i: boolean; caps: boolean }


export class Ribbon {
  private color = "C00000";
  private hl = "yellow";
  private styleButtons = new Map<string, HTMLButtonElement>();
  private tiles = new Map<string, HTMLButtonElement>();
  /** format painter: off, once, or until Esc */
  private painter: "" | "once" | "sticky" = "";

  constructor(private s: Session, private v: DocView, private host: Host) {
    this.tabs();
    this.commands();
    this.fontInputs();
    this.menus();
    this.styles();
    this.find();
    this.pageMenus();
    this.tablePicker();
    this.imageInput();
    this.linkDialog();
    this.painterHooks();
    new ResizeObserver(() => this.fit()).observe(document.querySelector("#ribbon")!);
    document.querySelectorAll<HTMLButtonElement>(".tab").forEach((t) => t.addEventListener("click", () => requestAnimationFrame(() => this.fit())));
    v.onShortcut = (e) => this.shortcut(e);
  }

  private setPainter(mode: "" | "once" | "sticky"): void {
    this.painter = mode;
    document.querySelectorAll('[data-cmd="painter"]').forEach((b) => b.classList.toggle("active", !!mode));
    this.v.scroller.classList.toggle("painting", !!mode);
  }

  /** Format painter: the next selection takes the copied formatting. */
  private painterHooks(): void {
    const btn = document.querySelector('[data-cmd="painter"]');
    btn?.addEventListener("dblclick", () => {
      this.s.copy_format();
      this.setPainter("sticky");
    });
    this.v.scroller.addEventListener("mouseup", () => {
      if (!this.painter) return;
      // after the view has set the selection
      setTimeout(() => {
        if (!this.painter || this.v.state.collapsed) return;
        this.v.exec(() => this.s.paste_format());
        if (this.painter === "once") this.setPainter("");
      }, 0);
    });
    document.addEventListener("keydown", (e) => {
      if (e.key !== "Escape") return;
      if (this.painter) this.setPainter("");
      if (document.body.classList.contains("focus")) this.focusMode(false);
    }, true);
  }

  /** groups moved under «⋯»: the group and the panel it belongs to */
  private overflow: { g: HTMLElement; panel: HTMLElement; before: Element | null }[] = [];

  /** The tabs go into the top bar in compact mode, back above the tools otherwise. */
  placeTabs(): void {
    const tabs = document.querySelector<HTMLElement>(".tabs")!;
    if (document.body.classList.contains("compact")) {
      document.querySelector('#appbar .ab-btn[data-cmd="redo"]')!.after(tabs);
    } else {
      document.querySelector("#ribbon")!.prepend(tabs);
    }
  }

  /** One row: words of the small buttons go first, then of the main ones, then whole groups under «⋯». */
  fit(): void {
    // everything back where it was
    for (const o of this.overflow.reverse()) o.panel.insertBefore(o.g, o.before);
    this.overflow = [];
    const panel = document.querySelector<HTMLElement>(".panel:not([hidden])");
    if (!panel) return;
    panel.classList.remove("no-col", "no-big");
    let more = panel.querySelector<HTMLButtonElement>(".more-tools");
    if (!more) {
      more = document.createElement("button");
      more.className = "more-tools";
      more.title = "Ще інструменти";
      more.innerHTML = "⋯";
      more.style.fontSize = "18px";
      more.addEventListener("mousedown", (e) => e.preventDefault());
      more.addEventListener("click", (e) => {
        e.stopPropagation();
        this.openMenu("tools-more", more!);
      });
      panel.appendChild(more);
    }
    more.classList.remove("on");
    if (!document.body.classList.contains("compact")) return;
    const fits = () => panel.scrollWidth <= panel.clientWidth + 1;
    if (!fits()) panel.classList.add("no-col");
    if (!fits()) panel.classList.add("no-big");
    if (fits()) return;
    more.classList.add("on");
    const pop = document.querySelector<HTMLElement>("#tools-more")!;
    const groups = [...panel.querySelectorAll<HTMLElement>(":scope > .grp")];
    while (!fits() && groups.length > 1) {
      const g = groups.pop()!;
      this.overflow.push({ g, panel, before: g.nextElementSibling });
      pop.prepend(g);
    }
  }

  focusMode(on: boolean): void {
    document.body.classList.toggle("focus", on);
    this.closeMenus();
    if (on) this.host.toast("Режим фокусування. Esc — вийти");
    this.v.focus();
  }

  private tablePicker(): void {
    const g = this.$("#tgrid");
    const label = this.$("#tgrid-label");
    const R = 8;
    const C = 10;
    const cells: HTMLButtonElement[] = [];
    for (let r = 0; r < R; r++) {
      for (let c = 0; c < C; c++) {
        const b = document.createElement("button");
        b.addEventListener("mouseenter", () => {
          cells.forEach((x, i) => x.classList.toggle("on", Math.floor(i / C) <= r && i % C <= c));
          label.textContent = `Таблиця ${c + 1} × ${r + 1}`;
        });
        b.addEventListener("mousedown", (e) => e.preventDefault());
        b.addEventListener("click", () => {
          this.closeMenus();
          this.v.exec(() => this.s.insert_table(r + 1, c + 1));
          this.v.focus();
        });
        cells.push(b);
        g.appendChild(b);
      }
    }
  }

  private imageInput(): void {
    const inp = this.$("#image-file") as HTMLInputElement;
    inp.addEventListener("change", async () => {
      const f = inp.files?.[0];
      inp.value = "";
      if (f) await this.v.insertImageFile(f, (m) => this.host.toast(m));
    });
    this.v.onImageError = (m) => this.host.toast(m);
  }

  private showLink(): void {
    const d = this.$("#link-dialog");
    const url = this.$("#link-url") as HTMLInputElement;
    const text = this.$("#link-text") as HTMLInputElement;
    text.value = this.v.state.collapsed ? "" : this.s.copy_text();
    url.value = "";
    d.hidden = false;
    url.focus();
  }

  private linkDialog(): void {
    const d = this.$("#link-dialog");
    const url = this.$("#link-url") as HTMLInputElement;
    const text = this.$("#link-text") as HTMLInputElement;
    const close = () => {
      d.hidden = true;
      this.v.focus();
    };
    const ok = () => {
      let u = url.value.trim();
      if (u && !/^(https?:|mailto:)/i.test(u)) u = (u.includes("@") && !u.includes("/") ? "mailto:" : "https://") + u;
      try {
        this.v.exec(() => this.s.insert_link(u, text.value));
        close();
      } catch {
        this.host.toast("Вкажіть адресу http(s) або пошту");
      }
    };
    this.$("#link-ok").addEventListener("click", ok);
    d.querySelectorAll("[data-close]").forEach((b) => b.addEventListener("click", close));
    for (const i of [url, text]) {
      i.addEventListener("keydown", (e) => {
        if (e.key === "Enter") ok();
        else if (e.key === "Escape") close();
      });
    }
  }

  private $(sel: string): HTMLElement {
    return document.querySelector(sel) as HTMLElement;
  }

  private tabs(): void {
    document.querySelectorAll<HTMLButtonElement>(".tab").forEach((t) => {
      t.addEventListener("mousedown", (e) => e.preventDefault());
      t.addEventListener("click", () => {
        const name = t.dataset.tab!;
        if (name === "file") {
          this.openMenu("file-menu", t);
          return;
        }
        document.querySelectorAll(".tab").forEach((x) => x.classList.toggle("active", x === t));
        document.querySelectorAll<HTMLElement>(".panel").forEach((p) => (p.hidden = p.dataset.panel !== name));
        this.v.focus();
      });
    });
  }

  run(cmd: string): void {
    const s = this.s;
    const v = this.v;
    const fmt = v.state.fmt;
    const size = fmt.size ?? 11;
    const map: Record<string, () => void> = {
      undo: () => v.exec(() => s.undo()),
      redo: () => v.exec(() => s.redo()),
      b: () => v.exec(() => s.toggle("b")),
      i: () => v.exec(() => s.toggle("i")),
      u: () => v.exec(() => s.toggle("u")),
      s: () => v.exec(() => s.toggle("s")),
      sub: () => v.exec(() => s.toggle("sub")),
      sup: () => v.exec(() => s.toggle("sup")),
      grow: () => v.exec(() => s.set_size(SIZES.find((x) => x > size) ?? size + 2)),
      shrink: () => v.exec(() => s.set_size([...SIZES].reverse().find((x) => x < size) ?? Math.max(1, size - 1))),
      color: () => v.exec(() => s.set_color(this.color)),
      highlight: () => v.exec(() => s.set_highlight(this.hl)),
      left: () => v.exec(() => s.align("left")),
      center: () => v.exec(() => s.align("center")),
      right: () => v.exec(() => s.align("right")),
      both: () => v.exec(() => s.align("both")),
      indent: () => v.exec(() => s.indent(720)),
      outdent: () => v.exec(() => s.indent(-720)),
      after0: () => v.exec(() => s.space_after(0)),
      after8: () => v.exec(() => s.space_after(8)),
      pagebreak: () => v.exec(() => s.page_break()),
      selectall: () => v.exec(() => s.select_all(), false),
      cut: () => this.clip("cut"),
      copy: () => this.clip("copy"),
      paste: () => this.clip("paste"),
      find: () => this.showFind(false),
      replace: () => this.showFind(true),
      save: () => void this.host.save(),
      download: () => this.host.download(),
      pdf: () => this.host.downloadPdf(),
      print: () => void this.host.print(),
      openlocal: () => this.host.openLocal(),
      zoomin: () => ((v.autoFit = false), v.setZoom(Math.round(v.zoom * 10 + 1) / 10)),
      zoomout: () => ((v.autoFit = false), v.setZoom(Math.round(v.zoom * 10 - 1) / 10)),
      zoom100: () => ((v.autoFit = false), v.setZoom(1)),
      zoomwidth: () => ((v.autoFit = false), v.fitWidth()),
      ruler: () => {
        const r = this.$("#ruler");
        r.hidden = !r.hidden;
        try {
          localStorage.setItem("papir:ruler", r.hidden ? "off" : "on");
        } catch {
          /* ignore */
        }
        document.querySelector('[data-cmd="ruler"]')!.classList.toggle("active", !r.hidden);
        document.querySelector("#viewport")!.classList.toggle("with-ruler", !r.hidden);
        v.requestDraw();
      },
      painter: () => {
        if (this.painter) return this.setPainter("");
        s.copy_format();
        this.setPainter("once");
      },
      focus: () => this.focusMode(!document.body.classList.contains("focus")),
      compact: () => {
        const on = !document.body.classList.contains("compact");
        document.body.classList.toggle("compact", on);
        this.placeTabs();
        document.querySelectorAll('[data-cmd="compact"]').forEach((b) => b.classList.toggle("active", on));
        requestAnimationFrame(() => this.fit());
        try {
          localStorage.setItem("papir:compact", on ? "on" : "off");
        } catch {
          /* ignore */
        }
      },
      spell: () => {
        const on = this.host.toggleSpell();
        document.querySelectorAll('[data-cmd="spell"]').forEach((b) => b.classList.toggle("active", on));
        this.host.toast(on ? "Перевірка правопису увімкнена" : "Перевірку правопису вимкнено");
      },
      footer: () => {
        const page = v.currentPage();
        const h = s.page_size(page)[1];
        v.exec(() => s.edit_story_at(page, 100, h - 10), false);
      },
      date: () => {
        const d = new Date();
        const two = (n: number) => String(n).padStart(2, "0");
        v.exec(() => s.insert_field('DATE \\@ "dd.MM.yyyy"', `${two(d.getDate())}.${two(d.getMonth() + 1)}.${d.getFullYear()}`));
      },
      clear: () => v.exec(() => s.clear_format()),
      newcomment: () => this.host.newComment(),
      pagenum: () => v.exec(() => s.insert_field("PAGE", String((v.state.caret?.page ?? 0) + 1))),
      pagecount: () => v.exec(() => s.insert_field("NUMPAGES", String(v.pageCount()))),
      footnote: () => v.exec(() => s.insert_footnote()),
      closestory: () => v.exec(() => s.exit_story(), false),
      header: () => v.exec(() => s.edit_story_at(v.currentPage(), 100, 10), false),
      comments: () => this.host.toggleComments(),
      prevchange: () => v.exec(() => s.next_change(-1) || this.host.toast("Виправлень немає"), false),
      nextchange: () => v.exec(() => s.next_change(1) || this.host.toast("Виправлень немає"), false),
      accept: () => v.exec(() => s.review(true, false) || this.host.toast("Тут немає виправлень")),
      reject: () => v.exec(() => s.review(false, false) || this.host.toast("Тут немає виправлень")),
      acceptall: () => v.exec(() => this.host.toast(`Прийнято: ${s.review(true, true)}`)),
      rejectall: () => v.exec(() => this.host.toast(`Відхилено: ${s.review(false, true)}`)),
      markup: () => v.exec(() => s.set_markup(!v.state.markup), false),
      track: () => {
        v.exec(() => s.set_track(!v.state.track), false);
        this.host.toast(v.state.track ? "Правки записуються як виправлення" : "Запис виправлень вимкнено");
      },
      bullets: () => v.exec(() => s.toggle_list(true)),
      numbering: () => v.exec(() => s.toggle_list(false)),
      image: () => (document.getElementById("image-file") as HTMLInputElement).click(),
      link: () => this.showLink(),
      row_above: () => v.exec(() => s.table_op("row_above")),
      row_below: () => v.exec(() => s.table_op("row_below")),
      col_left: () => v.exec(() => s.table_op("col_left")),
      col_right: () => v.exec(() => s.table_op("col_right")),
      del_row: () => v.exec(() => s.table_op("del_row")),
      del_col: () => v.exec(() => s.table_op("del_col")),
      del_table: () => v.exec(() => s.table_op("del_table")),
      merge: () => v.exec(() => s.table_op("merge") || this.host.toast("Виділіть кілька клітинок одного рядка або колонки")),
      distribute: () => v.exec(() => s.table_op("distribute")),
    };
    map[cmd]?.();
  }

  private commands(): void {
    document.querySelectorAll<HTMLButtonElement>("[data-cmd]").forEach((b) => {
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", () => {
        this.closeMenus();
        this.run(b.dataset.cmd!);
        if (!b.closest(".dialog")) this.v.focus();
      });
    });
    const zoom = this.$("#zoom") as HTMLInputElement;
    zoom.addEventListener("input", () => {
      this.v.autoFit = false;
      this.v.setZoom(Number(zoom.value) / 100);
    });
    document.querySelectorAll<HTMLButtonElement>(".tab:not(.file)").forEach((t) => t.addEventListener("click", () => this.closeMenus()));
  }

  private async clip(kind: "cut" | "copy" | "paste"): Promise<void> {
    const v = this.v;
    try {
      if (kind === "paste") {
        const t = await navigator.clipboard.readText();
        v.exec(() => this.s.paste_text(t));
      } else {
        await navigator.clipboard.writeText(this.s.copy_text());
        if (kind === "cut") v.exec(() => this.s.cut());
      }
    } catch {
      this.host.toast(kind === "paste" ? "Браузер не дав доступу до буфера. Скористайтеся Ctrl+V" : `Скористайтеся ${kind === "cut" ? "Ctrl+X" : "Ctrl+C"}`);
    }
    v.focus();
  }

  private fontInputs(): void {
    const font = this.$("#font") as HTMLInputElement;
    const size = this.$("#size") as HTMLInputElement;
    const setFont = async (val: string) => {
      if (!val || val === this.v.state.fmt.font) return;
      await this.host.ensureFont(val);
      this.v.exec(() => this.s.set_font(val));
    };
    const setSize = (val: string) => {
      const n = parseFloat(val.replace(",", "."));
      if (n > 0 && n <= 1638 && n !== this.v.state.fmt.size) this.v.exec(() => this.s.set_size(n));
    };
    const list = (id: string, values: string[], css: (v: string) => string, pick: (v: string) => void) => {
      const m = this.$(`#${id}`);
      m.innerHTML = "";
      for (const val of values) {
        const b = document.createElement("button");
        b.textContent = val;
        const st = css(val);
        if (st) b.style.fontFamily = st;
        b.addEventListener("mousedown", (e) => e.preventDefault());
        b.addEventListener("click", () => {
          this.closeMenus();
          pick(val);
          this.v.focus();
        });
        m.appendChild(b);
      }
    };
    list("font-menu", FONTS, (f) => this.host.cssFont(f), (f) => void setFont(f));
    list("size-menu-pt", SIZES.map((x) => String(x).replace(".", ",")), () => "", setSize);
    const apply = (inp: HTMLInputElement, menu: string, f: (val: string) => void) => {
      inp.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          this.closeMenus();
          f(inp.value.trim());
          this.v.focus();
        } else if (e.key === "Escape") {
          this.closeMenus();
          this.v.focus();
        } else if (e.key === "ArrowDown" && e.altKey) {
          e.preventDefault();
          this.openMenu(menu, inp);
        }
      });
      inp.addEventListener("change", () => f(inp.value.trim()));
      inp.addEventListener("focus", () => inp.select());
      // the arrow on the right opens the list
      inp.addEventListener("mousedown", (e) => {
        if (e.offsetX > inp.clientWidth - 22) {
          e.preventDefault();
          this.openMenu(menu, inp);
        }
      });
    };
    apply(font, "font-menu", (v) => void setFont(v));
    apply(size, "size-menu-pt", setSize);
  }

  openMenu(id: string, anchor: HTMLElement | { left: number; bottom: number }): void {
    const m = document.getElementById(id)!;
    const wasOpen = !m.hidden;
    this.closeMenus();
    if (wasOpen) return;
    const r = anchor instanceof HTMLElement ? anchor.getBoundingClientRect() : anchor;
    m.hidden = false;
    m.style.left = `${Math.min(r.left, window.innerWidth - m.offsetWidth - 8)}px`;
    m.style.top = `${Math.min(r.bottom + 2, window.innerHeight - m.offsetHeight - 8)}px`;
  }

  closeMenus(): void {
    document.querySelectorAll<HTMLElement>(".menu").forEach((m) => (m.hidden = true));
  }

  private menus(): void {
    document.querySelectorAll<HTMLButtonElement>("[data-menu]").forEach((b) => {
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", (e) => {
        e.stopPropagation();
        this.openMenu(b.dataset.menu!, b);
      });
    });
    document.addEventListener("mousedown", (e) => {
      if (!(e.target as HTMLElement).closest(".menu, [data-menu], .tab.file, input.font, input.size")) this.closeMenus();
    });
    const cm = this.$("#color-menu");
    cm.classList.add("colors");
    cm.innerHTML =
      `<button class="wide" data-color="">Автоматичний</button>` +
      COLORS.map((c) => `<button data-color="${c}" title="#${c}" style="background:#${c}"></button>`).join("");
    cm.querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", () => {
        const c = b.dataset.color!;
        if (c) {
          this.color = c;
          this.$("#color-swatch").style.background = `#${c}`;
        }
        this.closeMenus();
        this.v.exec(() => this.s.set_color(c));
        this.v.focus();
      });
    });
    const hm = this.$("#highlight-menu");
    hm.classList.add("colors");
    hm.innerHTML =
      `<button class="wide" data-hl="">Без кольору</button>` +
      HIGHLIGHTS.map(([n, c]) => `<button data-hl="${n}" title="${n}" style="background:${c}"></button>`).join("");
    hm.querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", () => {
        const n = b.dataset.hl!;
        if (n) {
          this.hl = n;
          this.$("#hl-swatch").style.background = HIGHLIGHTS.find((h) => h[0] === n)![1];
        }
        this.closeMenus();
        this.v.exec(() => this.s.set_highlight(n));
        this.v.focus();
      });
    });
    this.$("#spacing-menu").querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", () => {
        this.closeMenus();
        if (b.dataset.line) this.v.exec(() => this.s.line_spacing(Number(b.dataset.line)));
        if (b.dataset.after) this.v.exec(() => this.s.space_after(Number(b.dataset.after)));
        this.v.focus();
      });
    });
  }

  private styleNames: Record<string, string> = {
    Normal: "Звичайний", "No Spacing": "Без інтервалів", "heading 1": "Заголовок 1", "heading 2": "Заголовок 2",
    "heading 3": "Заголовок 3", "heading 4": "Заголовок 4", Title: "Назва", Subtitle: "Підзаголовок", Quote: "Цитата",
    "List Paragraph": "Абзац списку", "Body Text": "Основний текст", Caption: "Підпис",
  };

  private styles(): void {
    const box = this.$("#styles");
    const gallery = this.$("#gallery");
    const list: StyleInfo[] = JSON.parse(this.s.styles());
    const apply = (id: string) => {
      this.closeMenus();
      this.v.exec(() => this.s.set_style(id));
      this.v.focus();
    };
    for (const st of list.slice(0, 40)) {
      const b = document.createElement("button");
      b.textContent = this.styleNames[st.name] ?? st.name;
      b.title = st.name;
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", () => apply(st.id));
      box.appendChild(b);
      this.styleButtons.set(st.id, b);
    }
    // the gallery: quick styles drawn the way they look in the document
    // Normal first, the quick styles, then the rest of the document's paragraph styles
    const quick = [...list].sort((a, b) => Number(b.name === "Normal") - Number(a.name === "Normal") || Number(b.q) - Number(a.q));
    for (const st of quick.slice(0, 16)) {
      const b = document.createElement("button");
      b.className = "tile";
      const name = this.styleNames[st.name] ?? st.name;
      b.title = name;
      const pv = document.createElement("span");
      pv.className = "pv";
      const big = (st.size ?? 11) >= 20;
      pv.textContent = big ? "Аа" : "АаБб";
      pv.style.fontFamily = this.host.cssFont(st.font);
      pv.style.fontSize = `${Math.round(Math.min(26, Math.max(16, (st.size ?? 11) * 1.1)))}px`;
      if (st.b) pv.style.fontWeight = "700";
      if (st.i) pv.style.fontStyle = "italic";
      if (st.color && /^[0-9a-f]{6}$/i.test(st.color)) pv.style.color = `#${st.color}`;
      const nm = document.createElement("span");
      nm.className = "nm";
      nm.textContent = name;
      b.append(pv, nm);
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", () => apply(st.id));
      gallery.appendChild(b);
      this.tiles.set(st.id, b);
    }
    const fit = () => {
      const right = gallery.getBoundingClientRect().right;
      let first = true;
      for (const t of this.tiles.values()) {
        t.classList.remove("cut");
        // at least one, then whole tiles only
        if (!first && t.getBoundingClientRect().right > right + 0.5) t.classList.add("cut");
        first = false;
      }
    };
    new ResizeObserver(fit).observe(gallery);
    fit();
  }

  /** People in the document (the sharing popover). */
  setPeople(people: { name: string; me?: boolean; color?: string }[]): void {
    const list = this.$("#share-list");
    list.innerHTML = "";
    for (const p of people) {
      const row = document.createElement("div");
      row.className = "sm-user";
      const av = document.createElement("span");
      av.className = "avatar";
      av.textContent = initials(p.name);
      if (p.color) av.style.background = p.color;
      const nm = document.createElement("span");
      nm.textContent = p.name;
      row.append(av, nm);
      if (p.me) {
        const me = document.createElement("small");
        me.textContent = "ви";
        row.append(me);
      }
      list.appendChild(row);
    }
  }

  private pageMenus(): void {
    const on = (sel: string, f: (b: HTMLButtonElement) => void) =>
      document.querySelectorAll<HTMLButtonElement>(sel).forEach((b) => {
        b.addEventListener("mousedown", (e) => e.preventDefault());
        b.addEventListener("click", () => {
          this.closeMenus();
          f(b);
          this.v.focus();
        });
      });
    on("[data-margins]", (b) => {
      const [t, bo, l, r] = b.dataset.margins!.split(",").map(Number);
      this.v.exec(() => this.s.set_page("margins", t, bo, l, r));
    });
    on("[data-orient]", (b) => this.v.exec(() => this.s.set_page("orient", b.dataset.orient === "landscape" ? 1 : 0, 0, 0, 0)));
    on("[data-paper]", (b) => {
      const [w, h] = b.dataset.paper!.split(",").map(Number);
      this.v.exec(() => this.s.set_page("size", w, h, 0, 0));
    });
  }

  private showFind(replace: boolean): void {
    const d = this.$("#find-dialog");
    d.hidden = false;
    d.classList.toggle("find-only", !replace);
    const t = this.$("#find-text") as HTMLInputElement;
    if (!this.v.state.collapsed) {
      const sel = this.s.copy_text();
      if (sel && !sel.includes("\n")) t.value = sel;
    }
    t.focus();
    t.select();
  }

  private find(): void {
    const d = this.$("#find-dialog");
    const t = this.$("#find-text") as HTMLInputElement;
    const r = this.$("#replace-text") as HTMLInputElement;
    const c = this.$("#find-case") as HTMLInputElement;
    const msg = this.$("#find-msg");
    const go = (what: string) => {
      const needle = t.value;
      if (!needle) return;
      let ok = true;
      if (what === "next" || what === "prev") ok = this.s.find(needle, c.checked, what === "prev");
      else if (what === "replace") ok = !this.v.readOnly && this.s.replace(needle, r.value, c.checked);
      else if (what === "all") {
        if (this.v.readOnly) return;
        let n = 0;
        this.v.exec(() => (n = this.s.replace_all(needle, r.value, c.checked)));
        msg.textContent = `Замінено: ${n}`;
        return;
      }
      if (what === "replace") this.v.exec(() => 0);
      else this.v.refresh();
      msg.textContent = ok ? "" : "Нічого не знайдено";
    };
    d.querySelectorAll<HTMLButtonElement>("[data-find]").forEach((b) => b.addEventListener("click", () => go(b.dataset.find!)));
    d.querySelector("[data-close]")!.addEventListener("click", () => {
      d.hidden = true;
      this.v.focus();
    });
    for (const inp of [t, r]) {
      inp.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          go(e.shiftKey ? "prev" : "next");
        } else if (e.key === "Escape") {
          d.hidden = true;
          this.v.focus();
        }
      });
    }
  }

  private shortcut(e: KeyboardEvent): boolean {
    if (!(e.ctrlKey || e.metaKey)) return false;
    const k = e.key.toLowerCase();
    const code = e.code;
    const by: Record<string, string> = {
      KeyB: "b", KeyI: "i", KeyU: "u", KeyZ: e.shiftKey ? "redo" : "undo", KeyY: "redo", KeyS: "save",
      KeyF: "find", KeyH: "replace", KeyE: "center", KeyL: "left", KeyR: "right", KeyJ: "both", KeyP: "print",
      KeyA: "selectall", BracketRight: "grow", BracketLeft: "shrink", Equal: e.shiftKey ? "sup" : "sub", KeyK: "link",
    };
    if (e.altKey && code === "KeyM") {
      if (!document.body.classList.contains("no-review")) this.run("newcomment");
      return true;
    }
    // Ctrl+Alt+F: a footnote, as in Word
    if (e.altKey && code === "KeyF") {
      this.run("footnote");
      return true;
    }
    const cmd = by[code] ?? (code === "Space" ? "clear" : undefined) ?? ({ "=": "sub", "+": "sup" } as Record<string, string>)[k];
    if (!cmd) return false;
    if (code === "KeyR" && e.shiftKey) return false;
    this.run(cmd);
    return true;
  }

  update(st: State): void {
    const f = st.fmt;
    const tog = (cmd: string, on: boolean) => document.querySelectorAll(`[data-cmd="${cmd}"]`).forEach((b) => b.classList.toggle("active", on));
    tog("b", f.b);
    tog("i", f.i);
    tog("u", f.u);
    tog("s", f.s);
    tog("sup", f.valign === "superscript");
    tog("sub", f.valign === "subscript");
    const jc = f.jc === "both" || f.jc === "distribute" ? "both" : f.jc === "center" ? "center" : f.jc === "right" || f.jc === "end" ? "right" : "left";
    for (const a of ["left", "center", "right", "both"]) tog(a, a === jc);
    tog("bullets", f.list?.bullet === true);
    tog("numbering", f.list?.bullet === false);
    tog("markup", st.markup);
    tog("track", st.track);
    this.$("#st-track").hidden = !st.track;
    this.$("#changes-count").textContent = st.changes ? `Виправлень: ${st.changes}` : "Виправлень немає";
    const kinds: Record<string, string> = { ins: "Вставлено", del: "Видалено", format: "Змінено формат", paragraph: "Змінено абзац" };
    const c = st.change;
    const date = c?.date ? new Date(c.date).toLocaleString("uk-UA", { dateStyle: "short", timeStyle: "short" }) : "";
    this.$("#st-change").textContent = c ? `${kinds[c.kind] ?? c.kind}: ${c.author || "невідомо"}${date ? ", " + date : ""}` : "";
    // the header/footer tab; a footnote is edited with the usual tabs
    const storyTab = document.querySelector('.tab[data-tab="story"]') as HTMLElement;
    const hf = !!st.story && st.story.kind !== "footnote";
    if (storyTab.hidden === hf) {
      storyTab.hidden = !hf;
      (document.querySelector(`.tab[data-tab="${hf ? "story" : "home"}"]`) as HTMLElement).click();
    }
    const tableTab = document.querySelector('.tab[data-tab="table"]') as HTMLElement;
    if (tableTab.hidden === st.table) {
      tableTab.hidden = !st.table;
      // leaving the table: back from its tab
      if (!st.table && tableTab.classList.contains("active")) (document.querySelector('.tab[data-tab="home"]') as HTMLElement).click();
    }
    (document.querySelector('[data-cmd="undo"]') as HTMLButtonElement).disabled = !st.undo || this.v.readOnly;
    (document.querySelector('[data-cmd="redo"]') as HTMLButtonElement).disabled = !st.redo || this.v.readOnly;
    const font = this.$("#font") as HTMLInputElement;
    const size = this.$("#size") as HTMLInputElement;
    if (document.activeElement !== font) font.value = f.font.split(";")[0];
    if (document.activeElement !== size) size.value = f.size != null ? String(f.size).replace(".", ",") : "";
    const cur = (id: string, b: HTMLButtonElement) => id === (f.style ?? "Normal") || (!f.style && (b.title === "Normal" || b.title === "Звичайний"));
    this.styleButtons.forEach((b, id) => b.classList.toggle("active", cur(id, b)));
    this.tiles.forEach((b, id) => b.classList.toggle("active", cur(id, b)));
    const sn = f.styleName ?? "Normal";
    this.$("#style-name").textContent = this.styleNames[sn] ?? sn;
    const z = Math.round(this.v.zoom * 100);
    const zoom = this.$("#zoom") as HTMLInputElement;
    zoom.value = String(z);
    zoom.style.setProperty("--fill", `${((z - 50) / 150) * 100}%`);
    this.$("#st-zoom").textContent = `${z}%`;
  }
}

/** "Шевченко Тарас Григорович" → "ШТ" */
export function initials(name: string): string {
  const w = name.trim().split(/\s+/).filter(Boolean);
  return (w.length > 1 ? w[0][0] + w[1][0] : (w[0] ?? "?").slice(0, 2)).toUpperCase();
}
