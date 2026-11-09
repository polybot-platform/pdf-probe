//! The coverage-guided fuzz target for `probe`. The simple way to run it needs only the stable toolchain and a
//! C++ compiler: `fuzz/run.sh [seconds] [workers]` (it builds with sanitizer-coverage flags, as cargo-fuzz would
//! without a sanitizer). With a nightly toolchain and `cargo install cargo-fuzz`, `cargo +nightly fuzz run probe`
//! works too, and can add AddressSanitizer.
//!
//! A crash is a panic or an abort (arithmetic overflow included: the build keeps overflow checks), a timeout, or
//! a memory-limit hit. The assertions are the same rules as `examples/fuzz.rs`.
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
