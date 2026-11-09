//! A stress case the fuzzers cannot find: the work is quadratic in the input, not large in any one place.
//! The root Pages node inherits `/MediaBox 3 0 R`, and object 3 is a huge array. Every page resolves the
//! inherited MediaBox, which re-parses the array each time, so the cost is pages x array size while the file
//! stays small. `cargo run --release --example hostile -- <pages> <array-elements>`
use pdf_probe::{Limits, probe};
use std::time::Instant;

fn build(pages: usize, elements: usize) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<usize> = vec![0];
    let mut obj = |out: &mut Vec<u8>, body: &str| {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", offsets.len() - 1).bytes());
    };
    obj(&mut out, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 4 + i)).collect();
    obj(
        &mut out,
        &format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox 3 0 R >>"),
    );
    obj(&mut out, &format!("[{}]", "0 ".repeat(elements)));
    for _ in 0..pages {
        obj(&mut out, "<< /Type /Page /Parent 2 0 R >>");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).bytes());
    for off in &offsets[1..] {
        out.extend(format!("{off:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .bytes(),
    );
    out
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("number"))
        .collect();
    let (pages, elements) = (args[0], args[1]);
    let file = build(pages, elements);
    // Pass a third argument (a budget in units) to see the budget stop it; by default it is off here.
    let max_work = args.get(2).map_or(u64::MAX, |w| *w as u64);
    let limits = Limits {
        max_pages: 1_000_000,
        max_work,
        ..Limits::default()
    };
    let t = Instant::now();
    let result = probe(&file, &limits);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let summary = match &result {
        Ok(p) => format!(
            "ok, {} pages, {} work units, {:.2} ns per unit",
            p.pages,
            p.work,
            ms * 1e6 / p.work as f64
        ),
        Err(e) => format!("{e:?}"),
    };
    println!(
        "{} pages x {} elements: file {} KB, {ms:.0} ms, {summary}",
        pages,
        elements,
        file.len() / 1024
    );
}
