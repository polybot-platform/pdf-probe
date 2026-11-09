//! Times pdf-probe over a list of files: `bench <reps> <file>...`. Files are read once, then checked `reps`
//! times; prints the milliseconds each repetition took, as JSON.
use pdf_probe::{Limits, probe};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let reps: usize = args[1].parse().expect("reps");
    let files: Vec<Vec<u8>> = args[2..]
        .iter()
        .map(|p| std::fs::read(p).expect("read"))
        .collect();
    let limits = Limits::default();
    let mut ok = 0usize;
    let mut times = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        ok = files.iter().filter(|b| probe(b, &limits).is_ok()).count();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    eprintln!("{ok} of {} files answered", files.len());
    println!("{times:?}");
}
