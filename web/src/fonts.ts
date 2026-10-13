import type { Session } from "../pkg/papir_wasm.js";
import type { Face } from "./types.js";

/** Loads font faces on demand: only the families a document uses. */
export class FontLoader {
  /** faces in registration order: index = face id in display lists */
  readonly registered: Face[] = [];
  private manifest: Face[] = [];
  private loading = new Map<string, Promise<void>>();

  constructor(private s: Session, private base: string) {}

  async init(): Promise<void> {
    this.manifest = await (await fetch(this.base + "fonts.json")).json();
  }

  families(): string[] {
    return [...new Set(this.manifest.map((f) => f.family))];
  }

  /** Family that renders `name`, or null. */
  substitute(name: string): string | null {
    return this.s.substitute(name, JSON.stringify(this.families())) ?? null;
  }

  ensure(families: string[]): Promise<void> {
    return Promise.all(families.map((f) => this.loadFamily(f))).then(() => undefined);
  }

  private loadFamily(family: string): Promise<void> {
    let p = this.loading.get(family);
    if (!p) {
      const faces = this.manifest.filter((f) => f.family === family);
      p = Promise.all(faces.map((f) => fetch(this.base + f.file).then((r) => r.arrayBuffer()))).then(async (bufs) => {
        const ffs: FontFace[] = [];
        faces.forEach((f, i) => {
          this.s.add_font(f.family, f.bold, f.italic, new Uint8Array(bufs[i]));
          this.registered.push(f);
          const ff = new FontFace(`papir ${f.family}`, bufs[i], { weight: f.bold ? "700" : "400", style: f.italic ? "italic" : "normal" });
          document.fonts.add(ff);
          ffs.push(ff);
        });
        await Promise.all(ffs.map((f) => f.load().catch(() => f)));
      });
      this.loading.set(family, p);
    }
    return p;
  }
}
