import init, { Session } from "../pkg/papir_wasm.js";
import { DocView } from "./view.js";
import { Ribbon, initials, type Host } from "./ribbon.js";
import type { State } from "./types.js";
import { FontLoader } from "./fonts.js";
import { Ruler } from "./ruler.js";
import { CommentsPanel } from "./comments.js";
import { Collab, Sync, type Person } from "./collab.js";
import { Spelling } from "./spell.js";
import { lang, t, translatePage } from "./i18n.js";

/** Settings the server puts into the page (absent in standalone mode). */
interface Config {
  /** session id at the document server */
  session?: string;
  api?: string;
  fileName?: string;
  canWrite?: boolean;
  user?: string;
  fontsUrl?: string;
  /** host's id of the document owner (process), for restoring the view */
  owner?: string;
  /** ping period, seconds */
  draftInterval?: number;
  /** unsaved edits are written to the host when the editor is closed */
  saveOnExit?: boolean;
  /** WebSocket of the document's room (editing together) */
  ws?: string;
  /** spelling dictionaries */
  dictUrl?: string;
  /** the «Рецензування» tab (comments, tracked changes): off for now */
  review?: boolean;
  collab?: boolean;
}

const cfg: Config = (() => {
  const el = document.getElementById("papir-config");
  try {
    return el ? JSON.parse(el.textContent || "{}") : {};
  } catch {
    return {};
  }
})();

const $ = <T extends HTMLElement>(s: string) => document.querySelector(s) as T;

function toast(msg: string, ms = 3500): void {
  const t = $("#toast");
  t.textContent = msg;
  t.hidden = false;
  clearTimeout((toast as any).timer);
  (toast as any).timer = setTimeout(() => (t.hidden = true), ms);
}

/** Right-click menu: clipboard, links, table commands. */
function contextMenu(s: Session, view: DocView, ribbon: Ribbon, host: Host, spell: Spelling): void {
  const m = $("#context-menu");
  const box = m.querySelector(".ctx-spell") as HTMLElement;
  let link: string | null = null;
  view.onContextMenu = (x, y, inTable, l) => {
    link = l;
    // a misspelled word under the caret: suggestions first
    const st = view.state;
    const occ = !view.readOnly && st.collapsed ? spell.at(st.focus.path, st.focus.off) : null;
    box.hidden = !occ;
    box.innerHTML = "";
    if (occ) {
      const pick = (b: HTMLButtonElement, f: () => void) => {
        b.addEventListener("mousedown", (e) => e.preventDefault());
        b.addEventListener("click", () => {
          ribbon.closeMenus();
          f();
          view.focus();
        });
      };
      box.innerHTML = `<div class="none">${t("Варіанти…")}</div>`;
      void spell.suggest(occ.word).then((words) => {
        box.innerHTML = "";
        if (!words.length) box.innerHTML = `<div class="none">${t("Немає варіантів")}</div>`;
        for (const w of words.slice(0, 6)) {
          const b = document.createElement("button");
          b.className = "sg";
          b.textContent = w;
          pick(b, () => {
            s.set_selection(JSON.stringify({ path: occ.path, off: occ.a }), JSON.stringify({ path: occ.path, off: occ.b }));
            view.exec(() => s.insert(w));
          });
          box.appendChild(b);
        }
        const skip = document.createElement("button");
        skip.textContent = t("Пропустити все");
        pick(skip, () => spell.ignore(occ.word));
        const add = document.createElement("button");
        add.textContent = t("Додати до словника");
        pick(add, () => spell.addWord(occ.word));
        box.append(document.createElement("hr"), skip, add, document.createElement("hr"));
      });
    }
    (m.querySelector(".ctx-table") as HTMLElement).hidden = !inTable || view.readOnly;
    (m.querySelector(".ctx-link") as HTMLElement).hidden = !l;
    (m.querySelector(".ctx-edit") as HTMLElement).hidden = view.readOnly;
    m.querySelectorAll<HTMLButtonElement>('[data-ctx="cut"], [data-ctx="paste"]').forEach((b) => (b.hidden = view.readOnly));
    ribbon.openMenu("context-menu", { left: x, bottom: y });
  };
  m.querySelectorAll<HTMLButtonElement>("[data-ctx]").forEach((b) => {
    b.addEventListener("mousedown", (e) => e.preventDefault());
    b.addEventListener("click", () => {
      ribbon.closeMenus();
      const c = b.dataset.ctx!;
      if (c === "openlink" && link) window.open(link, "_blank", "noopener,noreferrer");
      else if (c === "newcomment") host.newComment();
      else if (c === "cut" || c === "copy" || c === "paste") ribbon.run(c);
      else view.exec(() => s.table_op(c));
      if (c !== "newcomment") view.focus();
    });
  });
}

const LANGS: Record<string, string> = {
  uk: "Українська", en: "Англійська", ru: "Російська", pl: "Польська", de: "Німецька", fr: "Французька", es: "Іспанська", it: "Італійська",
};

/** Language of the text for the status bar: from the document, else guessed from its letters. */
function language(s: Session): string {
  const code = (s.doc_lang() ?? "").toLowerCase().split("-")[0];
  if (LANGS[code]) return t(LANGS[code]);
  return "";
}

const viewKey = () => `papir:${cfg.owner ?? ""}:${cfg.fileName ?? ""}`;

let rememberView = () => {};

function setSaveState(text: string): void {
  $("#save-state").textContent = text;
}

async function main(): Promise<void> {
  translatePage();
  // the menu offers the other language by its own name
  const sw = document.querySelector("#lang-switch span");
  if (sw) sw.textContent = lang === "uk" ? "English" : "Українська";
  await init();
  const s = new Session();
  const fonts = new FontLoader(s, cfg.fontsUrl ?? new URL("../fonts/", location.href).href);
  await fonts.init();
  const local = !cfg.session;
  let fileName = cfg.fileName ?? t("документ.docx");
  let view: DocView | null = null;
  // edits count at the last save: more means unsaved changes
  let savedEdits = 0;
  let saving: Promise<void> | null = null;
  let comments: CommentsPanel | null = null;
  let ribbonRef: Ribbon | null = null;
  let collab: Collab | null = null;
  let sync: Sync | null = null;
  let spell: Spelling | null = null;
  // the reviewer's name: the host's user, or one remembered in this browser
  const author = () => {
    if (cfg.user) return cfg.user;
    try {
      return localStorage.getItem("papir:author") || t("Користувач");
    } catch {
      return t("Користувач");
    }
  };

  const dirty = () => view != null && view.state.edits !== savedEdits;
  // saved means written to the host; edits that reached the room are not saved yet
  const showSaveState = () => {
    if (!view || view.readOnly) return setSaveState("");
    if (sync?.active && collab && !collab.online && s.collab_pending()) return setSaveState(t("Немає зв'язку — правки чекають на мережу"));
    setSaveState(dirty() ? t("Зміни не збережено") : local ? "" : t("Збережено"));
  };

  const host: Host = {
    toast,
    async save() {
      if (!view) return;
      if (local) return host.download();
      if (view.readOnly) return;
      if (saving) return saving;
      const version = view.state.version;
      const edits = view.state.edits;
      saving = (async () => {
        setSaveState(t("Збереження…"));
        try {
          const headers: Record<string, string> = { "content-type": "application/octet-stream", "x-papir-version": String(version) };
          if (sync?.active) {
            // my edits first reach the room, then its version is saved
            sync.flush();
            for (let i = 0; i < 100 && s.collab_pending(); i++) {
              await new Promise((r) => setTimeout(r, 100));
              sync.flush();
            }
            if (s.collab_pending()) throw new Error(t("немає зв'язку з сервером"));
            headers["x-papir-room-version"] = String(s.collab_version() ?? 0);
          }
          const bytes = s.save();
          const r = await fetch(`${cfg.api}/session/${cfg.session}/file`, { method: "POST", headers, body: bytes });
          if (r.status === 409 && (await r.clone().text()).includes("not_synced")) throw new Error(t("саме прийшли правки інших — збережіть ще раз"));
          if (r.status === 409) throw new Error(t("документ уже збережено в іншому вікні"));
          if (r.status === 401) throw new Error(t("сесію завершено, оновіть сторінку"));
          if (!r.ok) throw new Error(await r.text());
          savedEdits = edits;
          // the host stores a new version and reopens the editor with it
          rememberView();
          view!.readOnly = true;
          setSaveState(t("Збережено"));
          toast(t("Збережено. Відкриваємо нову версію…"), 8000);
          setTimeout(() => {
            if (view?.readOnly && cfg.canWrite) {
              $("#st-mode").textContent = t("Збережено як нову версію");
              toast(t("Документ збережено. Щоб редагувати далі, відкрийте його знову."), 10000);
            }
          }, 10000);
        } catch (e) {
          setSaveState(t("Помилка збереження"));
          toast(t("Не вдалося зберегти: {0}", (e as Error).message || e), 6000);
        } finally {
          saving = null;
        }
      })();
      return saving;
    },
    download() {
      if (!view) return;
      const blob = new Blob([s.save()], { type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = fileName;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 10000);
      if (local) {
        savedEdits = view.state.edits;
        setSaveState(t("Завантажено"));
      }
    },
    downloadPdf() {
      if (!view) return;
      const blob = new Blob([s.export_pdf()], { type: "application/pdf" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = fileName.replace(/\.docx$/i, "") + ".pdf";
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 10000);
    },
    async print() {
      if (!view) return;
      toast(t("Готуємо друк…"), 1500);
      const urls = await view.renderPages(2);
      const root = $("#print-root");
      root.innerHTML = urls.map((u) => `<img src="${u}" alt="">`).join("");
      await Promise.all([...root.querySelectorAll("img")].map((i) => i.decode().catch(() => 0)));
      window.print();
      root.innerHTML = "";
    },
    openLocal() {
      $<HTMLInputElement>("#file").click();
    },
    newComment() {
      comments?.startNew();
    },
    toggleComments() {
      comments?.toggle();
    },
    async ensureFont(name: string) {
      const fam = fonts.substitute(name);
      if (fam) await fonts.ensure([fam]);
    },
    toggleSpell() {
      if (!spell) return false;
      spell.setEnabled(!spell.enabled);
      return spell.enabled;
    },
    cssFont(name: string | null) {
      const fam = name ? fonts.substitute(name) : null;
      return fam ? `"papir ${fam}", serif` : "serif";
    },
  };

  const me = author();
  const av = $("#avatar");
  av.textContent = initials(me);
  av.title = me;

  const open = async (bytes: Uint8Array, name: string, readOnly: boolean) => {
    try {
      s.open(bytes);
    } catch (e) {
      toast(t("Не вдалося відкрити файл ({0})", (e as Error).message), 6000);
      return;
    }
    await fonts.ensure(JSON.parse(s.needed_families(JSON.stringify(fonts.families()))));
    fileName = name;
    document.title = t("{0} — Папір", name);
    $("#doc-title").textContent = name;
    $("#landing").hidden = true;
    if (!view) {
      view = new DocView(s, fonts.registered, $("#app"));
      const ribbon = new Ribbon(s, view, host);
      ribbon.setPeople([{ name: me, me: true }]);
      ribbonRef = ribbon;
      const ruler = new Ruler(s, view, $("#ruler"));
      // in an iframe or a low window: one row of tools
      const embedded = window.self !== window.top;
      let pref: string | null = null;
      try {
        pref = localStorage.getItem("papir:compact");
      } catch {
        /* ignore */
      }
      // compact by default; the full ribbon of the mockup is a choice in «Вигляд»
      document.body.classList.toggle("embedded", embedded);
      const compact = pref ? pref === "on" : true;
      // zoom: in an iframe the page fills the panel (up to 150 %); alone, 100 % unless the window is narrower
      view.autoFit = true;
      view.fitMax = embedded ? 1.5 : 1;
      document.body.classList.toggle("compact", compact);
      document.querySelectorAll('[data-cmd="compact"]').forEach((b) => b.classList.toggle("active", compact));
      ribbon.placeTabs();
      // the ruler: off unless turned on (as in Word)
      let rulerPref: string | null = null;
      try {
        rulerPref = localStorage.getItem("papir:ruler");
      } catch {
        /* ignore */
      }
      const rulerOn = rulerPref === "on";
      $("#ruler").hidden = !rulerOn;
      $("#viewport").classList.toggle("with-ruler", rulerOn);
      document.querySelectorAll('[data-cmd="ruler"]').forEach((b) => b.classList.toggle("active", rulerOn));
      spell = new Spelling(s, view, cfg.dictUrl ?? new URL("../dict/", location.href).href);
      document.querySelectorAll('[data-cmd="spell"]').forEach((b) => b.classList.toggle("active", spell!.enabled));
      contextMenu(s, view, ribbon, host, spell);
      comments = new CommentsPanel(s, view, author);
      // comments and review: off unless the config asks (review: true)
      if (document.body.classList.contains("no-review")) {
        s.set_comments(false);
        comments.disabled = true;
      }
      view.onDraw = () => ruler.draw();
      view.onState = (st: State) => {
        ribbon.update(st);
        collab?.sendCursor();
        spell?.schedule();
        ruler.draw();
        comments?.update(st);
        $("#st-page").textContent = t("Сторінка {0} з {1}", (st.caret?.page ?? view!.currentPage()) + 1, view!.pageCount());
        if (!saving) showSaveState();
      };
      view.scroller.addEventListener("scroll", () => {
        spell?.schedule();
        $("#st-page").textContent = t("Сторінка {0} з {1}", view!.currentPage() + 1, view!.pageCount());
      });
    } else {
      view.refresh(false);
    }
    // my name and the time for tracked changes
    const stamp = () => s.set_author(author(), new Date().toISOString().replace(/\.\d+Z$/, "Z"));
    stamp();
    window.setInterval(stamp, 20000);
    view.readOnly = readOnly;
    document.body.classList.toggle("readonly", readOnly);
    if (readOnly) {
      $("#ruler").hidden = true;
      $("#viewport").classList.remove("with-ruler");
    }
    if (readOnly && spell?.enabled) spell.setEnabled(false, false);
    $("#ro-pill").hidden = !readOnly;
    $("#st-lang").textContent = language(s);
    if (readOnly) (document.querySelector('.tab[data-tab="view"]') as HTMLElement).click();
    // for end-to-end tests and debugging
    (window as any).papir = { session: s, view };
    $("#st-mode").textContent = "";
    savedEdits = view.state.edits;
    view.refresh(false);
    if (view.autoFit) view.fit();
    view.focus();
    $("#st-words").textContent = t("Слів: {0}", s.word_count());
    document.body.classList.remove("pending");
  };

  document.querySelectorAll<HTMLElement>(".local-only").forEach((e) => (e.hidden = !local));
  // «Рецензування» is off for now (on with review: true, or ?review=1 locally)
  const review = cfg.review === true || (local && new URLSearchParams(location.search).get("review") === "1");
  document.body.classList.toggle("no-review", !review);

  if (local) {
    const fileInput = $<HTMLInputElement>("#file");
    const openFile = async (f: File) => open(new Uint8Array(await f.arrayBuffer()), f.name, false);
    fileInput.addEventListener("change", () => fileInput.files?.[0] && openFile(fileInput.files[0]));
    const vp = $("#viewport");
    vp.addEventListener("dragover", (e) => {
      e.preventDefault();
      vp.classList.add("dragover");
    });
    vp.addEventListener("dragleave", () => vp.classList.remove("dragover"));
    vp.addEventListener("drop", (e) => {
      e.preventDefault();
      vp.classList.remove("dragover");
      const f = e.dataTransfer?.files[0];
      // pictures go into the document (handled by the view); .docx opens
      if (f && /\.docx$/i.test(f.name)) openFile(f);
    });
    const sample = new URLSearchParams(location.search).get("open");
    if (sample) {
      const r = await fetch(sample);
      await open(new Uint8Array(await r.arrayBuffer()), decodeURIComponent(sample.split("/").pop() || t("документ.docx")), false);
    } else {
      $("#landing").hidden = false;
    }
  } else {
    const r = await fetch(`${cfg.api}/session/${cfg.session}/file`);
    if (!r.ok) {
      toast(r.status === 401 ? t("Сесію завершено. Оновіть сторінку в системі.") : t("Не вдалося отримати файл"), 10000);
      return;
    }
    await open(new Uint8Array(await r.arrayBuffer()), fileName, !cfg.canWrite);
    const v = view as DocView | null;
    if (!v) return;
    rememberView = () => {
      try {
        sessionStorage.setItem(viewKey(), JSON.stringify({ t: Date.now(), focus: v.state.focus, scroll: v.scroller.scrollTop, zoom: v.zoom }));
      } catch {
        /* storage unavailable */
      }
    };
    // after a save the host reopens the editor: continue where the user was
    try {
      const saved = JSON.parse(sessionStorage.getItem(viewKey()) || "null");
      if (saved && Date.now() - saved.t < 120000) {
        sessionStorage.removeItem(viewKey());
        v.autoFit = false;
        v.setZoom(saved.zoom);
        s.set_selection(JSON.stringify(saved.focus), JSON.stringify(saved.focus));
        v.refresh(false);
        v.scroller.scrollTop = saved.scroll;
      }
    } catch {
      /* ignore */
    }
    if (cfg.collab && cfg.ws && cfg.session) {
      const c = new Collab(cfg.ws, cfg.session, s, v);
      collab = c;
      // viewers see the saved document: the room sends them no changes
      if (!v.readOnly) {
        const sy = new Sync(s, c);
        sync = sy;
        c.sink = sy;
        sy.onRemote = () => {
          v.refresh(false);
          c.refreshRemote();
        };
        sy.onStatus = () => {
          if (!saving) showSaveState();
        };
        sy.onLost = () => {
          v.readOnly = true;
          $("#ro-pill").hidden = false;
          setSaveState("");
          toast(t("Зв'язку не було надто довго: незбережені зміни відкинуто. Відкрийте документ знову."), 15000);
        };
      }
      c.onStatus = (online) => {
        if (online) sync?.flush();
        if (!saving) showSaveState();
      };
      const peers = $("#peers");
      c.onPeople = (people: Person[]) => {
        const myId = c.me?.id;
        ribbonRef?.setPeople(people.map((p) => ({ name: p.name, color: p.id === myId ? undefined : p.color, me: p.id === myId })));
        const others = people.filter((p) => p.id !== myId);
        peers.innerHTML = "";
        for (const p of others.slice(0, 3)) {
          const a = document.createElement("span");
          a.className = "avatar";
          a.textContent = initials(p.name);
          a.title = p.name + (p.write ? "" : t(" (перегляд)"));
          a.style.background = p.color;
          peers.appendChild(a);
        }
        if (others.length > 3) {
          const a = document.createElement("span");
          a.className = "avatar more";
          a.textContent = `+${others.length - 3}`;
          a.title = others.slice(3).map((p) => p.name).join(", ");
          peers.appendChild(a);
        }
      };
      c.onSaved = (by: string) => {
        if (saving) return;
        if (!sync) {
          toast(t("{0} зберіг нову версію документа. Відкрийте її, щоб побачити зміни.", by || t("Хтось")), 12000);
          return;
        }
        v.readOnly = true;
        $("#ro-pill").hidden = false;
        toast(t("{0} зберіг документ як нову версію. Щоб редагувати далі, відкрийте його знову.", by || t("Хтось")), 12000);
      };
      window.addEventListener("pagehide", () => c.leave());
      v.scroller.addEventListener("scroll", () => c.refreshRemote(), { passive: true });
      const onDraw = v.onDraw;
      let drawnVersion = -1;
      v.onDraw = () => {
        onDraw();
        if (v.state.version !== drawnVersion) {
          drawnVersion = v.state.version;
          c.refreshRemote();
        }
      };
    }
    let draftVersion = v.state.version;
    let draftTimer = 0;
    const superseded = () => {
      if (v.readOnly) return;
      v.readOnly = true;
      $("#ro-pill").hidden = false;
      toast(t("Цей документ уже збережено або відкрито в іншому вікні. Відкрийте його знову."), 10000);
    };
    const pushDraft = async () => {
      draftTimer = 0;
      if (sync?.active) return;
      if (v.readOnly || v.state.version === draftVersion) return;
      const version = v.state.version;
      try {
        const r = await fetch(`${cfg.api}/session/${cfg.session}/draft`, {
          method: "POST",
          headers: { "content-type": "application/octet-stream", "x-papir-version": String(version) },
          body: s.save(),
        });
        if (r.status === 409) return superseded();
        if (r.ok) draftVersion = version;
      } catch {
        setSaveState(t("Немає зв'язку з сервером"));
      }
    };
    v.onEdit = () => {
      if (sync?.active) return sync.edited();
      // drafts are for writing on exit only
      if (cfg.saveOnExit && !draftTimer) draftTimer = window.setTimeout(pushDraft, 2000);
    };
    setInterval(async () => {
      if (cfg.saveOnExit && v.state.version !== draftVersion) return pushDraft();
      try {
        const r = await fetch(`${cfg.api}/session/${cfg.session}/ping`, { method: "POST" });
        const j = await r.json();
        if (j.superseded && !saving && dirty()) superseded();
      } catch {
        /* offline: the next ping retries */
      }
    }, (cfg.draftInterval ?? 10) * 1000);
    window.addEventListener("pagehide", () => {
      if (!cfg.saveOnExit || sync?.active || v.state.version === draftVersion || v.readOnly) return;
      const bytes = s.save();
      // keepalive requests are limited to 64 KB
      if (bytes.length < 60000) {
        void fetch(`${cfg.api}/session/${cfg.session}/draft`, { method: "POST", keepalive: true, headers: { "x-papir-version": String(v.state.version) }, body: bytes });
      }
    });
  }

  // unsaved edits go with the page (unless the server writes them on exit): warn
  window.addEventListener("beforeunload", (e) => {
    if (view && !view.readOnly && dirty() && (local || !cfg.saveOnExit)) {
      e.preventDefault();
      e.returnValue = "";
    }
  });
  // word count is not needed on every keystroke
  setInterval(() => view && ($("#st-words").textContent = t("Слів: {0}", s.word_count())), 3000);
}

main().catch((e) => {
  console.error(e);
  toast(t("Помилка запуску: {0}", e.message || e), 10000);
});
