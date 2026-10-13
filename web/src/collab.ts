import type { Session } from "../pkg/papir_wasm.js";
import type { DocView, Remote } from "./view.js";

export interface Person { id: string; name: string; color: string; write: boolean }

/** how long a person's name stays over their caret after a move */
const NAME_MS = 2500;

interface Cursor { v: number; a: unknown; f: unknown; story?: boolean; at?: number }

/** What the room says about changes; the merging side plugs in here. */
export interface ChangeSink {
  /** changes of others in room order (on joining and while editing) */
  remote(version: number, by: string, change: unknown): void;
  ack(version: number): void;
  /** refused: `head` is the room's version */
  nack(head: number | null): void;
  /** version this copy is at */
  version(): number;
  /** joined (again): who I am now, the room's version and epoch (new after unsaved changes were dropped), the changes I have not seen */
  joined(me: Person & { no: number }, version: number, changes: [number, string, unknown][], again: boolean, epoch: string): void;
}

/**
 * Connection of the editor to its document's room: who else is here, their
 * cursors, and the stream of changes. Drops reconnect by themselves with a
 * growing pause; nothing typed meanwhile is lost (it waits in the editor).
 */
export class Collab {
  private ws: WebSocket | null = null;
  me: (Person & { no: number }) | null = null;
  people: Person[] = [];
  private cursors = new Map<string, Cursor>();
  private retry = 0;
  private cursorTimer = 0;
  private lastCursor = "";
  private everJoined = false;
  private closed = false;
  onPeople: (p: Person[]) => void = () => {};
  onStatus: (online: boolean) => void = () => {};
  onSaved: (by: string) => void = () => {};
  sink: ChangeSink | null = null;

  private lastSeen = Date.now();

  constructor(private url: string, private session: string, private s: Session, private v: DocView) {
    this.connect();
    // heartbeat: a connection that went quiet is dead even if the browser has not noticed
    window.setInterval(() => {
      if (this.ws?.readyState !== WebSocket.OPEN) return;
      if (Date.now() - this.lastSeen > 20000) this.ws.close();
      else this.send({ t: "ping" });
    }, 8000);
    window.addEventListener("offline", () => this.ws?.close());
    window.addEventListener("online", () => {
      if (this.ws?.readyState !== WebSocket.OPEN) {
        this.retry = 0;
        this.connect();
      }
    });
  }

  get online(): boolean {
    return this.ws?.readyState === WebSocket.OPEN && this.me != null;
  }

  private connect(): void {
    if (this.closed) return;
    if (this.ws && this.ws.readyState <= WebSocket.OPEN) this.ws.close();
    this.lastSeen = Date.now();
    const ws = new WebSocket(this.url);
    this.ws = ws;
    ws.onopen = () => {
      this.retry = 0;
      ws.send(JSON.stringify({ t: "join", session: this.session, have: this.everJoined ? (this.sink?.version() ?? null) : null }));
    };
    ws.onmessage = (e) => {
      this.lastSeen = Date.now();
      this.message(JSON.parse(e.data));
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.me = null;
      this.onStatus(false);
      this.cursors.clear();
      this.refreshRemote();
      if (this.closed) return;
      // 0.5 s, 1 s, 2 s ... up to 15 s
      const wait = Math.min(15000, 500 * 2 ** this.retry++);
      window.setTimeout(() => this.connect(), wait);
    };
  }

  close(): void {
    this.closed = true;
    this.ws?.close();
  }

  /** The page is going away: the room need not wait for me. */
  leave(): void {
    this.send({ t: "leave" });
    this.close();
  }

  send(msg: unknown): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN) return false;
    this.ws.send(JSON.stringify(msg));
    return true;
  }

  private message(m: any): void {
    switch (m.t) {
      case "welcome":
        this.me = m.me;
        this.sink?.joined(m.me, m.version, m.changes, this.everJoined, m.epoch ?? "");
        this.everJoined = true;
        this.setPeople(m.people);
        this.onStatus(true);
        if (m.saved) this.onSaved("");
        this.lastCursor = "";
        this.sendCursor();
        break;
      case "people":
        this.setPeople(m.people);
        break;
      case "change":
        this.sink?.remote(m.v, m.by, m.c);
        this.refreshRemote();
        break;
      case "ack":
        this.sink?.ack(m.v);
        break;
      case "nack":
        this.sink?.nack(typeof m.v === "number" ? m.v : null);
        break;
      case "cursor":
        m.at = Date.now();
        this.cursors.set(m.id, m);
        this.refreshRemote();
        // the name shows for a moment after a move, then only the caret
        window.setTimeout(() => this.refreshRemote(), NAME_MS + 50);
        break;
      case "saved":
        this.onSaved(m.by ?? "");
        break;
    }
  }

  private setPeople(p: Person[]): void {
    this.people = p;
    for (const id of [...this.cursors.keys()]) if (!p.some((x) => x.id === id)) this.cursors.delete(id);
    this.onPeople(p);
    this.refreshRemote();
  }

  /** Recomputes where the others' cursors are (after any change). */
  refreshRemote(): void {
    const out: Remote[] = [];
    // viewing only: the others' carets are not shown (the avatars tell who is here)
    if (this.v.readOnly) {
      if (this.v.remote.length) {
        this.v.remote = [];
        this.v.requestDraw();
      }
      return;
    }
    for (const [id, c] of this.cursors) {
      const p = this.people.find((x) => x.id === id);
      // people who only view have no caret to show
      if (!p || !p.write || c.story) continue;
      try {
        const r = JSON.parse(this.s.rects_for(JSON.stringify(c.a), JSON.stringify(c.f)));
        if (r) out.push({ name: p.name, color: p.color, caret: r.caret, sel: r.sel, label: Date.now() - (c.at ?? 0) < NAME_MS });
      } catch {
        /* a position from an older version that no longer exists */
      }
    }
    this.v.remote = out;
    this.v.requestDraw();
  }

  /** Tells the others where my caret is (at most every 120 ms). */
  sendCursor(): void {
    if (this.cursorTimer) return;
    this.cursorTimer = window.setTimeout(() => {
      this.cursorTimer = 0;
      const st = this.v.state;
      // viewing only: nothing to show the others
      if (!st || !this.online || this.v.readOnly) return;
      const msg = { t: "cursor", v: this.sink?.version() ?? 0, a: st.anchor, f: st.focus, story: !!st.story };
      const key = JSON.stringify(msg);
      if (key === this.lastCursor) return;
      this.lastCursor = key;
      this.send(msg);
    }, 120);
  }
}

/**
 * The editor's side of the room: my changes go out one at a time, the
 * others' are merged in (the core rebases what I have not sent yet).
 */
export class Sync implements ChangeSink {
  private started = false;
  private epoch = "";
  /** the room dropped the unsaved changes while I was away: my copy is no longer its */
  private lost = false;
  onLost: () => void = () => {};
  /** my id in the room the first time (my own changes come back under it after a drop) */
  private firstId = "";
  private waitFor = 0;
  private timer = 0;
  onRemote: () => void = () => {};
  onStatus: (pending: boolean) => void = () => {};

  constructor(private s: Session, private room: Collab) {}

  version(): number {
    return this.s.collab_version() ?? 0;
  }

  joined(me: Person & { no: number }, version: number, changes: [number, string, unknown][], again: boolean, epoch: string): void {
    if (this.lost) return;
    if (this.started && epoch !== this.epoch) {
      this.lost = true;
      this.onLost();
      return;
    }
    this.epoch = epoch;
    if (!this.started) {
      // the file we opened is the room's base: version 0, then its changes
      this.s.collab_start(`${me.id}.`, me.no, 0);
      this.started = true;
      this.firstId = me.id;
    }
    void again;
    for (const [v, by, c] of changes) {
      if (v <= this.version()) continue;
      // my own change, accepted while the connection was down
      if (by === this.firstId && this.s.collab_waiting() && v === this.version() + 1) this.s.collab_ack(v);
      else this.apply(v, c);
    }
    // a change that never reached the server: send it again
    if (this.s.collab_waiting()) this.s.collab_nack();
    if (version > this.version()) this.waitFor = version;
    this.onRemote();
    this.flush();
  }

  private apply(v: number, c: unknown): void {
    try {
      this.s.collab_remote(JSON.stringify(c), v);
    } catch (e) {
      console.error("change not applied", e);
    }
  }

  remote(version: number, _by: string, change: unknown): void {
    if (this.lost || version <= this.version()) return;
    this.apply(version, change);
    this.onRemote();
    if (this.waitFor && this.version() >= this.waitFor) this.waitFor = 0;
    this.flush();
  }

  ack(version: number): void {
    this.s.collab_ack(version);
    this.flush();
  }

  nack(head: number | null): void {
    this.s.collab_nack();
    // behind the room: send again once its changes are here
    if (head != null && head > this.version()) this.waitFor = head;
    this.flush();
  }

  /** Sends what I changed, soon (typing comes in bursts). */
  edited(): void {
    if (this.timer) return;
    this.timer = window.setTimeout(() => {
      this.timer = 0;
      this.flush();
    }, 50);
  }

  flush(): void {
    if (!this.started || this.lost) return;
    const pending = this.s.collab_pending();
    this.onStatus(pending);
    if (!this.room.online || this.s.collab_waiting() || this.waitFor) return;
    const c = this.s.collab_take();
    if (c) this.room.send({ t: "push", v: this.version(), c: JSON.parse(c) });
  }

  get active(): boolean {
    return this.started;
  }
}
