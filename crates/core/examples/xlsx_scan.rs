//! Opens every .xlsx under the given directories; prints failures and totals.
use std::time::Instant;

fn main() {
    let mut files = Vec::new();
    for d in std::env::args().skip(1) {
        let mut stack = vec![std::path::PathBuf::from(d)];
        while let Some(p) = stack.pop() {
            if p.is_dir() {
                for e in std::fs::read_dir(&p).unwrap().flatten() {
                    stack.push(e.path());
                }
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xlsx")) {
                files.push(p);
            }
        }
    }
    let (mut ok, mut bad, mut cells) = (0, 0, 0usize);
    let t0 = Instant::now();
    let mut slowest = (0.0f64, String::new());
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let t = Instant::now();
        let r = std::panic::catch_unwind(|| papir_core::xlsx::Book::open(&bytes));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms > slowest.0 {
            slowest = (ms, f.display().to_string());
        }
        match r {
            Ok(Ok(b)) => {
                ok += 1;
                for s in &b.sheets {
                    for r in &s.rows {
                        for c in &r.cells {
                            let _ = b.display(c);
                            cells += 1;
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                bad += 1;
                println!("error {}: {e}", f.display());
            }
            Err(_) => {
                bad += 1;
                println!("PANIC {}", f.display());
            }
        }
    }
    println!("{} files: {ok} ok, {bad} failed, {cells} cells formatted, {:.0} ms total, slowest {:.0} ms {}", files.len(), t0.elapsed().as_secs_f64() * 1000.0, slowest.0, slowest.1);
}
