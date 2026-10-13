//! docconv — .docx → PDF on the Papir core, without LibreOffice.
//!
//!   docconv convert FILE.docx [-o OUT.pdf]
//!   docconv convert FILE|DIR... -o OUTDIR [--jobs N]
//!   docconv serve        one JSON request per stdin line, one reply per line:
//!                        {"id":1,"in":"/a.docx","out":"/a.pdf"}
//!                        → {"id":1,"ok":true,"ms":12.5,"pages":2}
//!
//!   docconv [soffice arguments]
//!                        drop-in for `soffice --convert-to pdf`:
//!                        .docx is converted here, anything else is handed to
//!                        the real soffice ($PAPIR_SOFFICE or soffice on PATH).
//!
//! Fonts: --fonts DIR, $PAPIR_FONTS, or a `fonts` directory next to the
//! binary or one level up (with fonts.json).

use papir_core::fonts::Fonts;
use papir_core::pdf::{self, Options};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

struct FontSet {
    faces: Vec<(String, bool, bool, &'static [u8])>,
}

impl FontSet {
    fn load(dir: &Path) -> Result<FontSet, String> {
        let manifest = std::fs::read(dir.join("fonts.json")).map_err(|e| format!("{}: {e}", dir.join("fonts.json").display()))?;
        let list: serde_json::Value = serde_json::from_slice(&manifest).map_err(|e| e.to_string())?;
        let mut faces = Vec::new();
        for f in list.as_array().ok_or("fonts.json: expected a list")? {
            let file = f["file"].as_str().ok_or("fonts.json: file")?;
            let data = std::fs::read(dir.join(file)).map_err(|e| format!("{file}: {e}"))?;
            // read once, shared by every worker thread for the process lifetime
            let data: &'static [u8] = Box::leak(data.into_boxed_slice());
            faces.push((f["family"].as_str().unwrap_or("").to_string(), f["bold"].as_bool().unwrap_or(false), f["italic"].as_bool().unwrap_or(false), data));
        }
        Ok(FontSet { faces })
    }

    /// A registry for one thread (glyph caches are per thread).
    fn registry(&self) -> Fonts {
        let mut f = Fonts::new();
        for (fam, b, i, data) in &self.faces {
            let _ = f.add_static(fam, *b, *i, data);
        }
        f
    }
}

fn fonts_dir(arg: Option<String>) -> PathBuf {
    if let Some(a) = arg.or_else(|| std::env::var("PAPIR_FONTS").ok()) {
        return PathBuf::from(a);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let mut cands: Vec<PathBuf> = Vec::new();
    for anc in exe.ancestors().skip(1).take(4) {
        cands.push(anc.join("fonts"));
        cands.push(anc.join("share/papir/fonts"));
    }
    cands.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fonts")));
    cands.into_iter().find(|d| d.join("fonts.json").exists()).unwrap_or_else(|| PathBuf::from("fonts"))
}

/// Converts one file; returns (pages, milliseconds).
fn convert_file(fonts: &Fonts, input: &Path, out: &Path) -> Result<(usize, f64), String> {
    let t = Instant::now();
    let bytes = std::fs::read(input).map_err(|e| format!("read: {e}"))?;
    let opts = Options { fonts, limits: Default::default() };
    let (pdf, pages) = pdf::convert(&bytes, &opts).map_err(|e| e.code().to_string())?;
    // write next to the target, then rename: readers never see half a file
    let tmp = out.with_extension("pdf.part");
    std::fs::write(&tmp, &pdf).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, out).map_err(|e| format!("rename: {e}"))?;
    Ok((pages, t.elapsed().as_secs_f64() * 1000.0))
}

/// `docconv apply BASE.docx LOG.jsonl -o OUT.docx`: the room's journal
/// (one `{"v","by","c"}` per line, in order) applied to its base.
fn apply(args: Vec<String>) -> ! {
    let mut it = args.into_iter();
    let (Some(base), Some(log)) = (it.next(), it.next()) else { usage() };
    let out = match (it.next().as_deref(), it.next()) {
        (Some("-o"), Some(o)) => o,
        _ => usage(),
    };
    let run = || -> Result<usize, String> {
        let bytes = std::fs::read(&base).map_err(|e| format!("{base}: {e}"))?;
        let mut doc = papir_core::Document::open(&bytes).map_err(|e| e.to_string())?;
        let mut st = papir_core::collab::State::of(&doc);
        let mut ids = papir_core::collab::Ids::initial(&st);
        let text = std::fs::read_to_string(&log).map_err(|e| format!("{log}: {e}"))?;
        let (part, root) = (doc.part.clone(), doc.root.clone());
        let mut n = 0;
        let mut last = 0u64;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line).map_err(|e| format!("journal line {}: {e}", n + 1))?;
            let ver = v["v"].as_u64().unwrap_or(0);
            if ver != last + 1 {
                return Err(format!("journal: version {ver} after {last}"));
            }
            last = ver;
            let ch = papir_core::collab::Change::from_json(&v["c"])?;
            papir_core::collab::apply(&mut st, &mut ids, &ch, &part, &root)?;
            n += 1;
        }
        st.install(&mut doc);
        let bytes = papir_core::docx::save(&doc).map_err(|e| e.to_string())?;
        let tmp = format!("{out}.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
        Ok(n)
    };
    match run() {
        Ok(n) => {
            eprintln!("applied {n} changes");
            std::process::exit(0)
        }
        Err(e) => {
            eprintln!("docconv apply: {e}");
            std::process::exit(1)
        }
    }
}

fn usage() -> ! {
    eprintln!("usage:\n  docconv convert FILE.docx [-o OUT.pdf]\n  docconv convert FILE|DIR... -o OUTDIR [--jobs N] [--fonts DIR]\n  docconv serve [--fonts DIR]\n  docconv apply BASE.docx JOURNAL.jsonl -o OUT.docx");
    std::process::exit(2)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    if args.iter().any(|a| a == "--convert-to") {
        soffice_compat(args);
    }
    let cmd = args.remove(0);
    // no fonts needed: the document after the changes of a room
    if cmd == "apply" {
        apply(args);
    }
    let mut out: Option<PathBuf> = None;
    let mut jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut fonts_arg = None;
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" => out = it.next().map(PathBuf::from),
            "--jobs" | "-j" => jobs = it.next().and_then(|v| v.parse().ok()).unwrap_or(jobs).max(1),
            "--fonts" => fonts_arg = it.next(),
            _ => inputs.push(PathBuf::from(a)),
        }
    }
    let set = match FontSet::load(&fonts_dir(fonts_arg)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("docconv: fonts: {e}");
            std::process::exit(1);
        }
    };
    match cmd.as_str() {
        "serve" => serve(&set),
        "convert" => convert(&set, inputs, out, jobs),
        _ => usage(),
    }
}

fn serve(set: &FontSet) {
    let fonts = set.registry();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                let _ = writeln!(stdout, "{}", serde_json::json!({"ok": false, "error": "bad_request"}));
                let _ = stdout.flush();
                continue;
            }
        };
        let id = req["id"].clone();
        let reply = match (req["in"].as_str(), req["out"].as_str()) {
            (Some(i), Some(o)) => match convert_file(&fonts, Path::new(i), Path::new(o)) {
                Ok((pages, ms)) => serde_json::json!({"id": id, "ok": true, "ms": (ms * 10.0).round() / 10.0, "pages": pages}),
                Err(e) => serde_json::json!({"id": id, "ok": false, "error": e}),
            },
            _ => serde_json::json!({"id": id, "ok": false, "error": "bad_request"}),
        };
        let _ = writeln!(stdout, "{reply}");
        let _ = stdout.flush();
    }
}

fn convert(set: &FontSet, inputs: Vec<PathBuf>, out: Option<PathBuf>, jobs: usize) {
    let mut files: Vec<PathBuf> = Vec::new();
    for i in &inputs {
        if i.is_dir() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(i)
                .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("docx"))).collect())
                .unwrap_or_default();
            v.sort();
            files.extend(v);
        } else {
            files.push(i.clone());
        }
    }
    if files.is_empty() {
        usage();
    }
    let single = files.len() == 1 && out.as_ref().is_none_or(|o| o.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")));
    let target = |f: &Path| -> PathBuf {
        let name = Path::new(f.file_stem().unwrap_or_default()).with_extension("pdf");
        match (&out, single) {
            (Some(o), true) => o.clone(),
            (Some(o), false) => o.join(name),
            (None, _) => f.with_extension("pdf"),
        }
    };
    if !single {
        if let Some(o) = &out {
            let _ = std::fs::create_dir_all(o);
        }
    }
    let queue = Arc::new(Mutex::new(files.clone()));
    let results = Arc::new(Mutex::new(Vec::new()));
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..jobs.min(files.len()) {
            let queue = queue.clone();
            let results = results.clone();
            // a deep document is walked recursively: room for MAX_DEPTH levels
            std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(s, move || {
                let fonts = set.registry();
                loop {
                    let Some(f) = queue.lock().unwrap().pop() else { break };
                    let r = convert_file(&fonts, &f, &target(&f));
                    results.lock().unwrap().push((f, r));
                }
            })
            .expect("worker thread");
        }
    });
    let wall = t0.elapsed().as_secs_f64() * 1000.0;
    let results = results.lock().unwrap();
    let mut times: Vec<f64> = Vec::new();
    let mut failed = 0;
    for (f, r) in results.iter() {
        match r {
            Ok((_, ms)) => times.push(*ms),
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", f.display());
            }
        }
    }
    times.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| times.get(((times.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0);
    if files.len() > 1 {
        eprintln!(
            "converted {} of {} in {:.0} ms with {} jobs; per file p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms",
            times.len(),
            files.len(),
            wall,
            jobs.min(files.len()),
            pct(0.5),
            pct(0.95),
            pct(1.0)
        );
    }
    if failed > 0 {
        std::process::exit(1);
    }
}

/// `soffice --headless ... --convert-to pdf[:filter] [--infilter=F] --outdir DIR FILE`
fn soffice_compat(args: Vec<String>) -> ! {
    let mut convert_to = None;
    let mut infilter = None;
    let mut outdir = None;
    let mut input = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--convert-to" => convert_to = it.next().cloned(),
            "--outdir" => outdir = it.next().cloned(),
            a if a.starts_with("--infilter=") => infilter = Some(a["--infilter=".len()..].to_string()),
            a if a.starts_with('-') => {}
            a => input = Some(PathBuf::from(a)),
        }
    }
    let pdf = convert_to.as_deref().is_some_and(|c| c == "pdf" || c.starts_with("pdf:"));
    let word = infilter.as_deref().is_none_or(|f| f.starts_with("MS Word 2007 XML"));
    if let (true, true, Some(input), Some(outdir)) = (pdf, word, &input, &outdir) {
        let out = Path::new(outdir).join(Path::new(input.file_stem().unwrap_or_default()).with_extension("pdf"));
        if let Ok(set) = FontSet::load(&fonts_dir(None)) {
            match convert_file(&set.registry(), input, &out) {
                Ok(_) => std::process::exit(0),
                // not something the core reads: LibreOffice gets a try
                Err(e) => eprintln!("docconv: {}: {e}; passing to soffice", input.display()),
            }
        }
    }
    exec_soffice(&args)
}

fn exec_soffice(args: &[String]) -> ! {
    use std::os::unix::process::CommandExt;
    let me = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let found = std::env::var("PAPIR_SOFFICE").ok().map(PathBuf::from).or_else(|| {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .flat_map(|d| [d.join("soffice"), d.join("libreoffice")])
                .find(|p| p.exists() && p.canonicalize().ok() != me)
        })
    });
    let Some(bin) = found else {
        eprintln!("docconv: no soffice for this file (set PAPIR_SOFFICE)");
        std::process::exit(1)
    };
    let err = std::process::Command::new(bin).args(args).exec();
    eprintln!("docconv: soffice: {err}");
    std::process::exit(1)
}
