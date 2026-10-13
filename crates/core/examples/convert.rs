//! Converts .docx files to PDF in memory and prints timings.
//! usage: cargo run --release --example convert -- [--runs N] [--jobs N] file.docx|dir...
//!
//! Per file: pages, PDF size, best and median time of N runs. Totals: wall
//! time, documents and pages per second, peak memory of the process.

use papir_core::fonts::Fonts;
use papir_core::pdf::{convert, Options};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn fonts() -> Fonts {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fonts");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("{dir}/fonts.json")).unwrap()).unwrap();
    let mut fonts = Fonts::new();
    for f in manifest.as_array().unwrap() {
        let data = std::fs::read(format!("{dir}/{}", f["file"].as_str().unwrap())).unwrap();
        fonts.add(f["family"].as_str().unwrap(), f["bold"].as_bool().unwrap(), f["italic"].as_bool().unwrap(), data).unwrap();
    }
    fonts
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p).unwrap().filter_map(|e| e.ok().map(|e| e.path())).collect();
        v.sort();
        for e in v {
            collect(&e, out);
        }
    } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("docx")) {
        out.push(p.to_path_buf());
    }
}

/// Peak resident memory of this process, MB (Linux).
fn peak_mb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: u64 = s.lines().find(|l| l.starts_with("VmHWM:"))?.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

struct Done {
    file: usize,
    ms: f64,
    pages: usize,
    bytes: usize,
    error: Option<String>,
}

fn main() {
    let mut runs = 3usize;
    let mut jobs = 1usize;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => runs = args.next().and_then(|v| v.parse().ok()).unwrap_or(runs).max(1),
            "--jobs" => jobs = args.next().and_then(|v| v.parse().ok()).unwrap_or(jobs).max(1),
            _ => collect(Path::new(&a), &mut files),
        }
    }
    if files.is_empty() {
        eprintln!("usage: convert [--runs N] [--jobs N] file.docx|dir...");
        std::process::exit(2);
    }
    let inputs: Arc<Vec<Vec<u8>>> = Arc::new(files.iter().map(|f| std::fs::read(f).unwrap()).collect());
    // every file `runs` times; workers take the next one off the queue
    let queue = Arc::new(Mutex::new((0..runs).flat_map(|_| 0..files.len()).rev().collect::<Vec<usize>>()));
    let done = Arc::new(Mutex::new(Vec::new()));
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..jobs.min(files.len() * runs) {
            let (queue, done, inputs) = (queue.clone(), done.clone(), inputs.clone());
            // a deep document is walked recursively
            std::thread::Builder::new()
                .stack_size(64 << 20)
                .spawn_scoped(s, move || {
                    let fonts = fonts();
                    let opts = Options { fonts: &fonts, limits: Default::default() };
                    loop {
                        let Some(file) = queue.lock().unwrap().pop() else { break };
                        let t = Instant::now();
                        let r = convert(&inputs[file], &opts);
                        let ms = t.elapsed().as_secs_f64() * 1000.0;
                        let d = match r {
                            Ok((pdf, pages)) => Done { file, ms, pages, bytes: pdf.len(), error: None },
                            Err(e) => Done { file, ms, pages: 0, bytes: 0, error: Some(e.code().to_string()) },
                        };
                        done.lock().unwrap().push(d);
                    }
                })
                .unwrap();
        }
    });
    let wall = t0.elapsed().as_secs_f64();
    let done = done.lock().unwrap();

    println!("{:<40} {:>6} {:>9} {:>9} {:>9}", "file", "pages", "pdf KB", "best ms", "median ms");
    let (mut docs, mut pages, mut failed) = (0usize, 0usize, 0usize);
    for (i, f) in files.iter().enumerate() {
        let mine: Vec<&Done> = done.iter().filter(|d| d.file == i).collect();
        let name = f.file_name().unwrap().to_string_lossy();
        let name: String = name.chars().take(40).collect();
        if let Some(e) = mine.iter().find_map(|d| d.error.as_deref()) {
            println!("{name:<40} error: {e}");
            failed += 1;
            continue;
        }
        let mut ms: Vec<f64> = mine.iter().map(|d| d.ms).collect();
        ms.sort_by(|a, b| a.total_cmp(b));
        let (p, kb) = (mine[0].pages, mine[0].bytes / 1024);
        println!("{name:<40} {p:>6} {kb:>9} {:>9.1} {:>9.1}", ms[0], ms[ms.len() / 2]);
        docs += mine.len();
        pages += p * mine.len();
    }
    println!();
    println!(
        "{} files x {runs} runs, {jobs} jobs: {:.2} s, {:.1} documents/s, {:.0} pages/s{}{}",
        files.len(),
        wall,
        docs as f64 / wall,
        pages as f64 / wall,
        peak_mb().map(|m| format!(", peak {m} MB")).unwrap_or_default(),
        if failed > 0 { format!(", {failed} failed") } else { String::new() },
    );
}
