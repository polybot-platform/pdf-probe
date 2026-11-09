//! The coverage-guided fuzz target for `probe`. Needs a nightly toolchain and `cargo install cargo-fuzz`:
//!
//! ```text
//! bun run corpus                                   # writes corpus/out/*.pdf
//! mkdir -p fuzz/corpus/probe && cp corpus/out/*.pdf fuzz/corpus/probe/
//! cargo +nightly fuzz run probe -- -max_len=200000 -rss_limit_mb=1024 -timeout=5
//! ```
//!
//! A crash is a panic or an abort (arithmetic overflow included: cargo-fuzz builds with overflow checks), a
//! timeout, or a memory-limit hit. The assertions are the same rules as `examples/fuzz.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_probe::{Limits, probe};

fuzz_target!(|data: &[u8]| {
    let limits = Limits {
        max_pages: 1000,
        max_objects: 200_000,
        max_xref_sections: 64,
        max_decompressed: 8 << 20,
    };
    if let Ok(p) = probe(data, &limits) {
        assert!(p.pages > 0 && p.pages == p.sizes.len() && p.pages <= limits.max_pages);
        for s in &p.sizes {
            assert!(s.width.is_finite() && s.height.is_finite() && s.width > 0.0 && s.height > 0.0);
        }
    }
});
