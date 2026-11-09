//! Calibration: how many work units does each file cost, and how long does it take? One line of summary per
//! directory. `cargo run --release --example work -- <dir> [<dir>...]`
use pdf_probe::{Limits, probe};
use std::time::Instant;

fn main() {
    let limits = Limits {
        max_work: u64::MAX,
        max_input_bytes: usize::MAX,
        ..Limits::default()
    };
    for dir in std::env::args().skip(1) {
        let mut rows: Vec<(u64, usize, f64, String)> = Vec::new(); // work, bytes, ns, name
        for entry in std::fs::read_dir(&dir).expect("dir").filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "pdf") {
                continue;
            }
            let bytes = std::fs::read(&path).expect("read");
            // The best of three, to leave out one-off noise.
            let mut best = f64::MAX;
            let mut work = None;
            for _ in 0..3 {
                let t = Instant::now();
                let r = probe(&bytes, &limits);
                best = best.min(t.elapsed().as_nanos() as f64);
                if let Ok(p) = r {
                    work = Some(p.work);
                }
            }
            if let Some(w) = work {
                rows.push((
                    w,
                    bytes.len(),
                    best,
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                ));
            }
        }
        rows.sort_by_key(|r| r.0);
        let n = rows.len();
        let pct = |p: f64| rows[((n as f64 - 1.0) * p) as usize].0;
        let total_work: u64 = rows.iter().map(|r| r.0).sum();
        let total_ns: f64 = rows.iter().map(|r| r.2).sum();
        let max = rows.last().unwrap();
        let worst_ratio = rows
            .iter()
            .map(|r| r.0 as f64 / r.1.max(1) as f64)
            .fold(0.0, f64::max);
        println!(
            "{dir}: {n} files answered | work units p50 {} p90 {} p99 {} max {} ({}, {} bytes) | \\
             max units per input byte {worst_ratio:.1} | {:.2} ns per unit overall",
            pct(0.5),
            pct(0.9),
            pct(0.99),
            max.0,
            max.3,
            max.1,
            total_ns / total_work as f64
        );
    }
}
