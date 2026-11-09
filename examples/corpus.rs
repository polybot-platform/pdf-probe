//! Runs pdf-probe over a directory of PDFs and writes results in the corpus JSON shape, for the
//! differential test: `cargo run --release --example corpus -- corpus/out corpus/pdfprobe-results.json`.
//!
//! The page-size verdict uses the same example policy as the pdf.js runner (17.5 x 12 inches). That limit is
//! a policy of the test, not part of the library.
use pdf_probe::{Failure, Limits, probe};
use std::{fmt::Write as _, fs};

const MAX_LONG: f64 = 17.5;
const MAX_SHORT: f64 = 12.0;

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (dir, out_file) = (&args[1], &args[2]);
    let mut files: Vec<_> = fs::read_dir(dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();
    let limits = Limits::default();
    let mut entries = Vec::new();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = fs::read(&path).expect("read file");
        let body = match probe(&bytes, &limits) {
            Ok(p) => {
                let over = p.sizes.iter().any(|s| {
                    s.width.max(s.height) / 72.0 > MAX_LONG
                        || s.width.min(s.height) / 72.0 > MAX_SHORT
                });
                let sizes: Vec<String> = p
                    .sizes
                    .iter()
                    .map(|s| format!("[{},{}]", round3(s.width), round3(s.height)))
                    .collect();
                let verdict = if over { "\"pageSize\"" } else { "null" };
                format!(
                    "{{\"verdict\":{verdict},\"pages\":{},\"sizes\":[{}]}}",
                    p.pages,
                    sizes.join(",")
                )
            }
            Err(Failure::Empty) => "{\"verdict\":\"empty\"}".to_string(),
            Err(Failure::TooManyPages { pages }) => {
                format!("{{\"verdict\":\"pages\",\"pages\":{pages}}}")
            }
            Err(Failure::Unparseable(why)) => {
                format!("{{\"verdict\":\"unparseable\",\"err\":{}}}", json_str(why))
            }
        };
        entries.push(format!("{}:{body}", json_str(&name)));
    }
    fs::write(out_file, format!("{{{}}}\n", entries.join(",\n"))).expect("write results");
}
