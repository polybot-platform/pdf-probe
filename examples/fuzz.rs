//! A mutation fuzzer for `probe`, with no dependencies, so it runs on stable.
//!
//! ```text
//! cargo run --profile fuzz --example fuzz -- --seeds corpus/out --seconds 60 --rng 1
//! ```
//!
//! It mutates seed files (bit flips, byte edits, deleting, duplicating and splicing ranges, replacing numbers
//! with awkward ones, inserting PDF keywords) and runs `probe` on each result. A result that reaches an
//! outcome not seen before (a new error, a new success shape) joins the pool of seeds, a crude stand-in for
//! coverage guidance. For real coverage-guided runs use the libFuzzer target in `fuzz/` (needs nightly).
//!
//! It reports, and saves to `fuzz-findings/`:
//! - `panic`: `probe` panicked. This must never happen.
//! - `slow`: one input took more than 100 ms, or never returned (a watchdog aborts after 10 s).
//! - `memory`: resident memory jumped by more than 128 MB on one input.
//! - `invariant`: a successful answer broke a rule (see `check`).
//! - `unstable`: the same input gave two different answers.
use pdf_probe::{Failure, Limits, probe};
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

const TOKENS: &[&str] = &[
    "obj",
    "endobj",
    "stream",
    "endstream",
    "xref",
    "trailer",
    "startxref",
    "%%EOF",
    "%PDF-1.7",
    "<<",
    ">>",
    "[",
    "]",
    "R",
    "0 0 R",
    "1 0 obj",
    "/Root",
    "/Pages",
    "/Kids",
    "/Count",
    "/Type /Page",
    "/Type /Pages",
    "/Parent",
    "/MediaBox",
    "/CropBox",
    "/Rotate",
    "/UserUnit",
    "/Prev",
    "/XRefStm",
    "/Size",
    "/W",
    "/Index",
    "/Length",
    "/Filter /FlateDecode",
    "/DecodeParms",
    "/Predictor",
    "/Columns",
    "/Colors",
    "/BitsPerComponent",
    "/Type /XRef",
    "/Type /ObjStm",
    "/N",
    "/First",
    "/Encrypt",
    "/Filter /Standard",
    "/V",
    "/R",
    "/O",
    "/U",
    "/UE",
    "/CF",
    "/StmF",
    "/StrF",
    "/CFM /AESV2",
    "/Length 5",
    "/ID",
];

const NUMBERS: &[&str] = &[
    "0",
    "1",
    "-1",
    "2",
    "255",
    "256",
    "65535",
    "65536",
    "2147483647",
    "2147483648",
    "4294967295",
    "4294967296",
    "9223372036854775807",
    "99999999999999999999",
    ".5",
    "-.5",
    "1000000000",
    "0.0000001",
    "1e9",
];

/// Mutations that keep the file length (and so every cross-reference offset) intact, so the mutant reaches
/// object streams, inflate and the page tree instead of dying in the cross-reference.
fn mutate_in_place(rng: &mut Rng, data: &mut [u8]) {
    for _ in 0..1 + rng.below(4) {
        let len = data.len();
        if len == 0 {
            return;
        }
        match rng.below(6) {
            0 => data[rng.below(len)] ^= 1 << rng.below(8),
            1 => data[rng.below(len)] = rng.next() as u8,
            2 => {
                // Overwrite with a keyword or an awkward number, truncated to fit.
                let t: &str = if rng.below(2) == 0 {
                    TOKENS[rng.below(TOKENS.len())]
                } else {
                    NUMBERS[rng.below(NUMBERS.len())]
                };
                let at = rng.below(len);
                for (i, b) in t.bytes().enumerate() {
                    if at + i < len {
                        data[at + i] = b;
                    }
                }
            }
            3 => {
                let at = rng.below(len);
                let n = (1 + rng.below(32)).min(len - at);
                data[at..at + n].fill([0u8, 0xff, b' ', b'0', b'9'][rng.below(5)]);
            }
            4 => {
                if let Some(i) = find_digit(rng, data) {
                    data[i] = b'0' + ((data[i] - b'0' + 1 + rng.below(8) as u8) % 10);
                }
            }
            _ => {
                // A same-width number replacement: keep the digit count, change the value.
                if let Some(i) = find_digit(rng, data) {
                    let (mut s, mut e) = (i, i);
                    while s > 0 && data[s - 1].is_ascii_digit() {
                        s -= 1;
                    }
                    while e < len && data[e].is_ascii_digit() {
                        e += 1;
                    }
                    let fill = b"091"[rng.below(3)];
                    data[s..e].fill(fill);
                }
            }
        }
    }
}

fn mutate(rng: &mut Rng, data: &mut Vec<u8>, pool: &[Vec<u8>]) {
    for _ in 0..1 + rng.below(6) {
        let len = data.len();
        match rng.below(15) {
            0 if len > 0 => data[rng.below(len)] ^= 1 << rng.below(8),
            1 if len > 0 => data[rng.below(len)] = rng.next() as u8,
            2 => {
                let at = rng.below(len + 1);
                let n = 1 + rng.below(8);
                let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                data.splice(at..at, bytes);
            }
            3 if len > 1 => {
                let at = rng.below(len);
                let n = (1 + rng.below(16)).min(len - at);
                data.drain(at..at + n);
            }
            4 if len > 8 => {
                let at = rng.below(len);
                let n = (1 + rng.below(len / 4 + 1)).min(len - at);
                data.drain(at..at + n);
            }
            5 if len > 1 => {
                let at = rng.below(len);
                let n = (1 + rng.below(64)).min(len - at);
                let copy = data[at..at + n].to_vec();
                let to = rng.below(len + 1);
                data.splice(to..to, copy);
            }
            6 if len > 1 => data.truncate(1 + rng.below(len)),
            7 => {
                let other = &pool[rng.below(pool.len())];
                if !other.is_empty() {
                    let at = rng.below(other.len());
                    let n = (1 + rng.below(256)).min(other.len() - at);
                    let to = rng.below(len + 1);
                    data.splice(to..to, other[at..at + n].iter().copied());
                }
            }
            8 => replace_number(rng, data),
            9 => {
                let t = TOKENS[rng.below(TOKENS.len())];
                let at = rng.below(len + 1);
                data.splice(
                    at..at,
                    std::iter::once(b' ')
                        .chain(t.bytes())
                        .chain(std::iter::once(b' ')),
                );
            }
            10 if len > 0 => {
                let t = TOKENS[rng.below(TOKENS.len())].as_bytes();
                let at = rng.below(len);
                for (i, b) in t.iter().enumerate() {
                    if at + i < len {
                        data[at + i] = *b;
                    }
                }
            }
            11 if len > 4 => {
                let (a, b) = (rng.below(len - 2), rng.below(len - 2));
                data.swap(a, b);
                data.swap(a + 1, b + 1);
            }
            12 if len > 0 => {
                let at = rng.below(len);
                let n = (1 + rng.below(32)).min(len - at);
                let fill = [0u8, 0xff, b' ', b'0', b'9'][rng.below(5)];
                data[at..at + n].fill(fill);
            }
            13 if len > 64 => {
                // The end of the file is where `startxref` is: hit it often.
                let at = len - 1 - rng.below(64.min(len - 1));
                data[at] = rng.next() as u8;
            }
            14 => {
                if let Some(i) = find_digit(rng, data) {
                    data[i] = b'0' + ((data[i] - b'0' + 1 + rng.below(8) as u8) % 10);
                }
            }
            _ => {}
        }
        if data.len() > 1 << 20 {
            data.truncate(1 << 20);
        }
    }
}

fn find_digit(rng: &mut Rng, data: &[u8]) -> Option<usize> {
    if data.is_empty() {
        return None;
    }
    let start = rng.below(data.len());
    (0..data.len())
        .map(|i| (start + i) % data.len())
        .find(|&i| data[i].is_ascii_digit())
}

fn replace_number(rng: &mut Rng, data: &mut Vec<u8>) {
    let Some(i) = find_digit(rng, data) else {
        return;
    };
    let mut s = i;
    while s > 0 && data[s - 1].is_ascii_digit() {
        s -= 1;
    }
    let mut e = i;
    while e < data.len() && data[e].is_ascii_digit() {
        e += 1;
    }
    data.splice(s..e, NUMBERS[rng.below(NUMBERS.len())].bytes());
}

/// A rule every successful answer must satisfy.
fn check(p: &pdf_probe::Probe, limits: &Limits) -> Result<(), String> {
    if p.pages == 0 || p.pages != p.sizes.len() {
        return Err(format!("pages {} but {} sizes", p.pages, p.sizes.len()));
    }
    if p.work == 0 || p.work > limits.max_work {
        return Err(format!("work {} outside 1..={}", p.work, limits.max_work));
    }
    if p.pages > limits.max_pages {
        return Err(format!(
            "{} pages over the cap {}",
            p.pages, limits.max_pages
        ));
    }
    for s in &p.sizes {
        if !(s.width.is_finite() && s.height.is_finite() && s.width > 0.0 && s.height > 0.0) {
            return Err(format!("bad page size {} x {}", s.width, s.height));
        }
    }
    Ok(())
}

fn outcome(r: &Result<pdf_probe::Probe, Failure>) -> String {
    match r {
        Ok(p) => format!("ok:{}", p.pages.min(3)),
        Err(Failure::Empty) => "empty".into(),
        Err(Failure::TooManyPages { .. }) => "toomany".into(),
        Err(Failure::Unparseable(why)) => format!("err:{why}"),
        Err(Failure::LimitExceeded(limit)) => format!("limit:{}", limit.name()),
    }
}

fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
        })
        .map_or(0, |pages| pages * 4096 / (1 << 20))
}

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn save(kind: &str, data: &[u8], note: &str) {
    let _ = std::fs::create_dir_all("fuzz-findings");
    let path = format!("fuzz-findings/{kind}-{:016x}.pdf", fnv(data));
    let _ = std::fs::write(&path, data);
    eprintln!("FINDING {kind}: {note} -> {path} ({} bytes)", data.len());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str, default: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or(default.into())
    };
    let dir = arg("--seeds", "corpus/out");
    let seconds: u64 = arg("--seconds", "30").parse().expect("--seconds");
    let max_seed: usize = arg("--max-seed-bytes", "200000")
        .parse()
        .expect("--max-seed-bytes");
    let mut rng = Rng(arg("--rng", "1")
        .parse::<u64>()
        .expect("--rng")
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        | 1);

    let mut pool: Vec<Vec<u8>> = std::fs::read_dir(&dir)
        .expect("read seed dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .filter_map(|p| std::fs::read(p).ok())
        .filter(|d| d.len() <= max_seed)
        .collect();
    assert!(!pool.is_empty(), "no seed files in {dir}");
    let seeds = pool.len();
    let limits = Limits {
        max_pages: 1000,
        max_objects: 200_000,
        max_xref_sections: 64,
        max_decompressed: 8 << 20,
        ..Limits::default()
    };

    // Silence the default panic message; panics are reported as findings.
    std::panic::set_hook(Box::new(|_| {}));

    // A watchdog saves the current input and exits if one input never returns. `t0` is the shared clock; the
    // millisecond at which the current input started is in `started` (u64::MAX when none is running).
    let t0 = Instant::now();
    let started = Arc::new(AtomicU64::new(u64::MAX));
    let current = Arc::new(Mutex::new(Vec::<u8>::new()));
    {
        let (started, current) = (started.clone(), current.clone());
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let s = started.load(Ordering::Relaxed);
                if s != u64::MAX && t0.elapsed().as_millis() as u64 > s + 10_000 {
                    if let Ok(data) = current.lock() {
                        save("hang", &data, "an input ran for more than 10 s");
                    }
                    std::process::exit(3);
                }
            }
        });
    }

    let clock = Instant::now();
    let mut seen: HashSet<String> = HashSet::new();
    let mut hist: HashMap<String, u64> = HashMap::new();
    let mut findings = 0u32;
    let mut runs = 0u64;
    let mut last_rss = rss_mb();
    let deadline = Duration::from_secs(seconds);
    while clock.elapsed() < deadline {
        let base = &pool[rng.below(pool.len())];
        let mut data = base.clone();
        if rng.below(2) == 0 {
            mutate_in_place(&mut rng, &mut data);
        } else {
            mutate(&mut rng, &mut data, &pool);
        }
        if let Ok(mut c) = current.lock() {
            c.clone_from(&data);
        }
        started.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        let t = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| probe(&data, &limits)));
        let took = t.elapsed();
        started.store(u64::MAX, Ordering::Relaxed);
        runs += 1;

        let result = match result {
            Ok(r) => r,
            Err(e) => {
                let msg = e
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| e.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".into());
                save("panic", &data, &msg);
                findings += 1;
                continue;
            }
        };
        if took > Duration::from_millis(100) {
            save("slow", &data, &format!("{} ms", took.as_millis()));
            findings += 1;
        }
        if let Ok(p) = &result
            && let Err(why) = check(p, &limits)
        {
            save("invariant", &data, &why);
            findings += 1;
        }
        if runs.is_multiple_of(50) {
            let rss = rss_mb();
            if rss > last_rss + 128 {
                save(
                    "memory",
                    &data,
                    &format!("resident memory {last_rss} -> {rss} MB"),
                );
                findings += 1;
            }
            last_rss = rss;
            if probe(&data, &limits) != result {
                save("unstable", &data, "two runs gave different answers");
                findings += 1;
            }
        }
        let key = outcome(&result);
        *hist.entry(key.clone()).or_default() += 1;
        if seen.insert(key) && pool.len() < 5000 {
            pool.push(data);
        }
    }

    let secs = clock.elapsed().as_secs_f64();
    println!(
        "{runs} runs in {secs:.1} s ({:.0}/s) from {seeds} seeds; pool grew to {}; {findings} findings",
        runs as f64 / secs,
        pool.len()
    );
    let mut top: Vec<_> = hist.into_iter().collect();
    top.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (k, n) in top.iter().take(12) {
        println!("  {n:>9}  {k}");
    }
    println!("  ({} distinct outcomes)", top.len());
    if findings > 0 {
        std::process::exit(1);
    }
}
