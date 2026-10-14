import type { Session } from "../pkg/papir_wasm.js";
import type { DocView } from "./view.js";
import type { CommentItem, State } from "./types.js";
import { escapeHtml } from "./view.js";
import { t } from "./i18n.js";

/** Comments panel: threads, replies, resolve, delete, new comment. */
export class CommentsPanel {
  private panel: HTMLElement;
  private list: HTMLElement;
  private seen = -1;
  private active: string | null = null;
  private wanted = false;

  constructor(private s: Session, private v: DocView, private author: () => string) {
    this.panel = document.getElementById("comments-panel")!;
    this.list = document.getElementById("cp-list")!;
    this.panel.querySelector("[data-cp-close]")!.addEventListener("click", () => this.show(false));
    this.panel.querySelector("[data-cp-cancel]")!.addEventListener("click", () => this.closeNew());
    this.panel.querySelector("[data-cp-save]")!.addEventListener("click", () => this.saveNew());
    const ta = document.getElementById("cp-new-text") as HTMLTextAreaElement;
    ta.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) this.saveNew();
      if (e.key === "Escape") this.closeNew();
    });
  }

  private now(): string {
    return new Date().toISOString().replace(/\.\d+Z$/, "Z");
  }

  /** comments switched off (review off in the config) */
  disabled = false;

  show(on: boolean): void {
    if (this.disabled) on = false;
    this.wanted = on;
    this.panel.hidden = !on;
    document.getElementById("viewport")!.classList.toggle("with-comments", on);
    // the page area changed width
    window.dispatchEvent(new Event("resize"));
    this.v.refresh(false);
  }

  toggle(): void {
    this.show(this.panel.hidden);
  }

  /** Opens the form for a new comment on the selection. */
  startNew(): void {
    if (this.v.readOnly) return;
    if (this.panel.hidden) this.show(true);
    const box = document.getElementById("cp-new")!;
    box.hidden = false;
    const ta = document.getElementById("cp-new-text") as HTMLTextAreaElement;
    ta.value = "";
    ta.focus();
  }

  private closeNew(): void {
    document.getElementById("cp-new")!.hidden = true;
    this.v.focus();
  }

  private saveNew(): void {
    const ta = document.getElementById("cp-new-text") as HTMLTextAreaElement;
    const text = ta.value.trim();
    if (!text) return;
    this.v.exec(() => this.s.add_comment(text, this.author(), this.now()));
    this.closeNew();
  }

  update(st: State): void {
    // shown automatically when the document has comments
    if (st.comments > 0 && !this.wanted && this.seen === -1) this.show(true);
    if (st.version === this.seen && st.comment === this.active) return;
    this.seen = st.version;
    this.active = st.comment;
    (document.getElementById("cp-title") as HTMLElement).textContent = st.comments ? t("Коментарі ({0})", st.comments) : t("Коментарі");
    if (this.panel.hidden) return;
    this.render(JSON.parse(this.s.comments()) as CommentItem[]);
  }

  private render(items: CommentItem[]): void {
    const ro = this.v.readOnly;
    const roots = items.filter((c) => !c.parent || !items.some((p) => p.paraId === c.parent));
    const replies = (c: CommentItem) => items.filter((r) => r.parent && r.parent === c.paraId);
    const date = (d: string) => (d ? new Date(d).toLocaleString("uk-UA", { dateStyle: "short", timeStyle: "short" }) : "");
    const card = (c: CommentItem, reply: boolean) =>
      `<div class="${reply ? "reply" : ""}" data-id="${escapeHtml(c.id)}"><div class="who"><b>${escapeHtml(c.author || t("Невідомо"))}</b><span>${escapeHtml(date(c.date))}</span></div><div class="txt">${escapeHtml(c.text)}</div></div>`;
    this.list.innerHTML = roots.length
      ? roots
          .map((c) => {
            const active = c.id === this.active || replies(c).some((r) => r.id === this.active);
            const acts = ro
              ? ""
              : `<div class="acts"><button data-act="reply">${t("Відповісти")}</button><button data-act="${c.done ? "reopen" : "resolve"}">${c.done ? t("Відкрити знову") : t("Закрити")}</button><button data-act="delete">${t("Видалити")}</button></div><div class="cp-reply" hidden><textarea rows="2" placeholder="${t("Відповідь")}"></textarea><div class="cp-actions"><button data-act="cancel">${t("Скасувати")}</button><button class="primary" data-act="send">${t("Відповісти")}</button></div></div>`;
            return `<div class="cp-card${active ? " active" : ""}${c.done ? " done" : ""}" data-root="${escapeHtml(c.id)}">${c.done ? `<div class="badge">${t("Закрито")}</div>` : ""}${card(c, false)}${replies(c).map((r) => card(r, true)).join("")}${acts}</div>`;
          })
          .join("")
      : `<div class="cp-empty">${t("Коментарів немає. Виділіть текст і натисніть «Новий коментар».")}</div>`;
    this.list.querySelectorAll<HTMLElement>(".cp-card").forEach((el) => {
      const id = el.dataset.root!;
      el.addEventListener("click", (e) => {
        const act = (e.target as HTMLElement).closest("[data-act]") as HTMLElement | null;
        const box = el.querySelector(".cp-reply") as HTMLElement | null;
        if (!act) {
          if ((e.target as HTMLElement).closest("textarea")) return;
          this.v.exec(() => this.s.select_comment(id), false);
          this.v.focus();
          return;
        }
        e.stopPropagation();
        switch (act.dataset.act) {
          case "reply":
            box!.hidden = false;
            (box!.querySelector("textarea") as HTMLTextAreaElement).focus();
            break;
          case "cancel":
            box!.hidden = true;
            break;
          case "send": {
            const t = (box!.querySelector("textarea") as HTMLTextAreaElement).value.trim();
            if (t) this.v.exec(() => this.s.reply_comment(id, t, this.author(), this.now()));
            break;
          }
          case "resolve":
            this.v.exec(() => this.s.resolve_comment(id, true));
            break;
          case "reopen":
            this.v.exec(() => this.s.resolve_comment(id, false));
            break;
          case "delete":
            this.v.exec(() => this.s.delete_comment(id));
            break;
        }
      });
    });
  }
}
