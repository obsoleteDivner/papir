/// <reference lib="webworker" />
// Spelling in its own thread: loading a dictionary takes a while.
import init, { Speller } from "../pkg/papir_wasm.js";

const ctx = self as unknown as DedicatedWorkerGlobalScope;
let sp: Speller | null = null;
const ready = (async () => {
  await init();
  sp = new Speller();
})();

const text = async (url: string) => {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return r.text();
};

ctx.onmessage = async (e: MessageEvent) => {
  const m = e.data;
  await ready;
  try {
    if (m.t === "load") {
      for (const [lang, file] of m.dicts as [string, string][]) {
        const [aff, dic] = await Promise.all([text(`${m.base}${file}.aff`), text(`${m.base}${file}.dic`)]);
        sp!.load(lang, aff, dic);
      }
      for (const w of m.personal as string[]) sp!.add(w);
      ctx.postMessage({ t: "ready" });
    } else if (m.t === "check") {
      ctx.postMessage({ t: "checked", id: m.id, bad: JSON.parse(sp!.check(JSON.stringify(m.words))) });
    } else if (m.t === "suggest") {
      ctx.postMessage({ t: "suggested", id: m.id, words: JSON.parse(sp!.suggest(m.word)) });
    } else if (m.t === "add") {
      sp!.add(m.word);
    }
  } catch (err) {
    ctx.postMessage({ t: "error", id: m.id, error: String(err) });
  }
};
