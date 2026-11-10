# pdf-probe

A small, fail-closed reader that answers two questions about an untrusted PDF: **how many pages does it have, and how big is each page?** It does not render, extract text, decode images or edit anything.

> **Status: all the planned parsing is in; not yet hardened. Do not depend on it.** The crate reads classic and stream cross-references (hybrid files and `/Prev` chains included), object streams, Flate with PNG predictors, encryption with an empty user password (RC4, AES-128, AES-256 revisions 5 and 6), the page tree and page sizes, and it rescans the file for objects when the cross-reference is damaged, as pdf.js does. A file it cannot read with confidence is refused (`Unparseable`), never guessed at. Against pdf.js's own 987 test PDFs it gives the same answer on 983, differs on 4 by design (all four are refusals), and **gives a different answer on none**. Fuzzing has found nothing so far (see [Fuzzing](#fuzzing)). A native shared library for `bun:ffi` ([Native library](#native-library)) a WebAssembly build ([WebAssembly](#webassembly)) and an npm package wrapping both ([npm package](#npm-package), not published) exist. Not yet done: a longer fuzz run, running `wasm-opt`, publishing the npm package, and the measurement in a container. See [the status table](#status) below.

```
cargo test                      # unit tests
bun install && bun run corpus   # generate the synthetic corpus (see corpus/README.md)
bun run slice && bun run slice:diff   # run the crate over it and compare with pdf.js
```

## Status

Measured on 2026-11-08 against pdf.js 6.4.299 (about 3,400 lines of Rust including tests and 130 lines of generated constant tables; no dependencies, no `unsafe`; its own inflate, MD5, SHA-2, AES and RC4):

| Corpus | Same as pdf.js | Intended difference | Refused (fail closed) | Different answer |
|---|---|---|---|---|
| 68 synthetic fixtures | 67 | 1 (`empty-page-tree.pdf`) | 0 | 0 |
| 987 pdf.js test PDFs | 983 | 4 (Brotli, an unparseable page object, two damaged files pdf.js reads and we refuse) | 0 | 0 |

The synthetic corpus passes the differential gate (`bun run slice:diff` exits 0). All 11 files that pdf.js rejects for a password are rejected here too.

Speed and memory, native build, on the 966 real files that both readers answer identically (123 MB): pdf.js 546 ms warm (696 ms first run), pdf-probe 100 ms warm, about 5 times faster. On a set of 20 of those files: pdf.js 6.8 ms warm and 71 ms first run, pdf-probe 2.2 ms and 2.6 ms; the ratio depends on which files, because AES-256 revision 6 (an iterated key derivation, 9.4 ms against 23.5 ms in pdf.js) dominates when it is present. Peak RSS for the whole process on those 20 files (1.9 MB of input): Bun reading the files 20 MB, Bun plus pdf.js 66 to 68 MB, the pdf-probe binary 3.9 MB. Per damaged fixture needing the recovery scan: 0.02 ms against 2.0 ms for pdf.js (tiny files). These are a standalone binary; the `bun:ffi` figures are in [Native library](#native-library). Neither is at idle in a container; treat them as an ordering, not a result. They also exclude the files each reader refuses.

## Why

Systems that accept PDF uploads often have to decide before doing any expensive work: is this within the page limit, and are the pages a size we can handle? Page count drives cost (an LLM or OCR service bills per page), and page size decides whether the content is legible after downscaling.

Answering that today usually means loading a full PDF library. A full library carries a rendering engine, font handling and image decoders that these two questions never use, and it is a large surface to expose to hostile input.

## What it will do

Input: the bytes of a PDF and a page cap. Output: the page count and each page's effective size in points, or a failure.

- **Fails closed.** Anything it cannot parse with confidence is reported as a failure, never guessed at.
- **Effective page size.** MediaBox intersected with CropBox, inherited down the page tree, with `/Rotate` and `/UserUnit` applied.
- **Real-world files.** Cross-reference tables and streams, `/Prev` chains and incremental updates, compressed object streams, Flate with predictors, and encryption with an empty user password (RC4, AES-128, AES-256). A PDF with a user password set is a failure.
- **Broken files.** A bounded recovery scan for a damaged or missing cross-reference table, with the same outcomes the reference implementation gives.
- **Bounded work.** Limits on object count, recursion depth, cross-reference chain length and decompressed size are part of the API, because the input is hostile.
- **Page count follows `/Count`.** Like pdf.js and poppler, it trusts the page tree's `/Count` and falls back to walking the tree only when `/Count` is invalid.

It is written in Rust, with no `unsafe` in the parser, and is meant to be built as a native library and as WebAssembly.

## How it is tested

The acceptance test is a **differential test against pdf.js**: on every fixture in a synthetic corpus, `pdf-probe` must return the same page count, the same page sizes and the same failures. Any intended difference is listed explicitly in the test, not tolerated silently. poppler's `pdfinfo` is used as a third opinion where the two disagree.

The generators and the differential tooling are in [`corpus/`](corpus/) and [`tools/`](tools/); see [`corpus/README.md`](corpus/README.md). The corpus is generated by script, not stored as binary files: classic and stream cross-references, object streams, predictors, incremental updates, hybrid files, encryption in each supported form, inherited and cropped boxes, rotation, `/UserUnit`, nested and cyclic page trees, wrong `/Count`, broken or missing cross-reference data, truncation, and non-PDFs. It contains no real documents.

### Limits

The input is hostile, so every call is bounded, and a limit that is hit is reported as its own result, `LimitExceeded` (`limitExceeded` in the C ABI and the npm package), naming which one. It is not the same as `unparseable`: the file may be fine, it is more than this call will do.

| Limit | Default | Set it with | Names |
|---|---|---|---|
| Input size | 10 MiB | `max_input_bytes` / `maxInputBytes` | `inputBytes`: checked first, before anything is parsed |
| Work | 2^30 units (about 1.1 billion, roughly a second) | `max_work` / `maxWork` | `work` |
| Pages | 10,000 | `max_pages` / `maxPages` | not a limit: `tooManyPages` |
| Objects, xref sections, decompressed bytes | 1,000,000 / 256 / 64 MiB | Rust only | `objects`, `xrefSections`, `decompressed` |

Only the first three are exposed through the C ABI and the npm package; the structural ones are Rust-only because a service has no good reason to tune them. Nesting and page-tree depth are fixed at 64.

**The work counter.** One unit is roughly a nanosecond on a modern core, but the count is deterministic: the same input costs the same units on every machine, so the same file is always accepted or refused, tests are reproducible, and nothing depends on how busy the host is. Parsing, scanning, inflating, predictors, hashing and ciphering each charge a measured per-byte weight (`src/meter.rs`); a call reports what it used in `Probe.work`.

Why it exists: the fuzzers cannot find work that is large only in total. A Pages node whose inherited `/MediaBox` is a big array made every page read that array again, so the cost was pages times the array size while the file stayed small. `cargo run --release --example hostile -- 400 500000` builds a 1 MB file that took 4.4 s that way, growing linearly with the page count, so at the default 10,000-page cap a 10 MiB file would have taken about 18 minutes. Two things bound it now:

- **A shared-object cache.** An object reached through a reference from another object (the usual way a value is shared) is parsed once and kept, up to about a million nodes (roughly 32 MB at most; page tree nodes are read once and are not kept). The same 400-page file takes 17 ms and 11 million units, the 10,000-page version 22 ms, and peak memory is the same as for 3 pages (19.6 against 20.3 MB), because the cache holds the one parse that was there anyway. Arrays are reference-counted, so passing one down the page tree copies a pointer.
- **The budget, for what the cache cannot keep.** An object over the cache limit is still read again for every page. A 400-page file with a 10 MB array takes 72 s with the budget off and stops with `LimitExceeded(Work)` after 2.1 s with the default. Unit tests cover both cases.

Calibration, 2026-11-09, over the 68 synthetic fixtures and 987 pdf.js test PDFs (1,021 answered): median 5,000 units, 99th percentile about 4 million, maximum 57 million (a 1.5 MB file, 0.4 ns per unit), so the largest real file uses about 5% of the default. The hostile case measured 1.19 ns per unit, and an AES-256 revision 6 file 1.3 ns. No file in either corpus reaches any limit. The cost of counting is small: the 966-file pass went from 82.7 to 87.1 ms before to 84.8 to 85.2 ms after (within noise), and the 20-file set from 2.30 to about 2.35 ms (+2 to 4%).

### Native library

`ffi/` is a second crate, `pdf-probe-ffi`, that builds the shared library: `cargo build --release -p pdf-probe-ffi` writes `target/release/libpdf_probe_ffi.so` (468 KB, stripped, two exported symbols). It holds the only `unsafe` code in the project, in two small functions, so the parser crate keeps `#![forbid(unsafe_code)]`. The interface is one function, `pdf_probe_check(data, len, max_pages, out, out_cap)`: the host passes the bytes and a buffer and gets the answer back in the buffer. There is no allocation across the boundary and no global state, and the layout is documented in `ffi/src/lib.rs`.

A panic cannot take the host down: the call is wrapped in `catch_unwind` and a panic comes back as a status. That is why the release profile unwinds (`panic = "abort"` is left for a future wasm profile). The fuzzer has found no panic, so this is a second line of defence, not the plan.

`bindings/bun.ts` loads it with `bun:ffi`. `bun run native && bun run native:diff` runs the corpus through the whole path (Rust, the C ABI, the Bun binding) and compares with pdf.js: 67 identical, 1 intended, 0 unexpected on the 68 fixtures; on pdf.js's 987 test PDFs 983 identical, 4 intended, 0 unexpected, and the results are byte-identical to the Rust-only runner's.

Measured on 2026-11-09, Bun 1.4.4, 20 real files (`tools/rss.ts`; RSS deltas against a process that has read the input but loaded no reader, after `Bun.gc(true)`; five runs each):

| | pdf.js (unpdf) | pdf-probe via `bun:ffi` |
|---|---|---|
| load | +26 to +27.5 MB, 41 to 44 ms | +2.2 to +2.4 MB, 1.1 to 1.8 ms |
| after 20 checks | +41 to +43 MB | +3.0 to +3.2 MB |
| after GC | +38 to +39 MB | +2.9 to +3.2 MB |
| after 12 s idle and GC | +33 MB | +2.7 to +3.1 MB |
| the 20 checks | 21 to 22 ms | 2.7 to 3.8 ms |

`bun build --compile` works: a compiled binary that imports the `.so` with `{ type: 'file' }` loads it from the embedded file system and ran from a directory with no `.so` beside it. This is a local result on one machine, not the Render container: that is a Linux x86_64 glibc build made here, and the container's libc, memory limit and idle behaviour are what the adoption decision needs. The guard service's Rust FFI tokenizer once cost more than the JavaScript it replaced, so treat the table as a hypothesis confirmed locally, not as a result.

### WebAssembly

The same `ffi/` crate builds a wasm module: `cargo build --profile wasm --target wasm32-unknown-unknown -p pdf-probe-ffi` writes `target/wasm32-unknown-unknown/wasm/pdf_probe_ffi.wasm`: **132 KB (62.5 KB gzipped)**. The package build runs it through `wasm-opt -Oz` when that is installed (`cargo install wasm-opt --locked`, binaryen 116), which gives **116 KB (61.6 KB gzipped)**: 12% smaller raw but only 1.5% gzipped, and no measurable change in Bun memory or speed (medians of nine runs on the same 20 files: load 7.4 against 7.2 MB, the 20 checks 10.5 against 10.7 ms; `-O3` was no faster). The optimised module is byte-identical to the Rust runner on both corpora, on Bun and Node, and passes the trap test. The Ubuntu 24.04 `apt` package is binaryen 108, older than that. It has **no imports**, so it needs no JavaScript glue and no host functions, and exports `memory` and four functions: `pdf_probe_check` (the same function and result layout as the native library), `pdf_probe_abi_version`, and, on wasm only, `pdf_probe_alloc` and `pdf_probe_free` to place bytes in its memory.

`bindings/wasm.ts` is plain TypeScript over the `WebAssembly` API, and runs under Bun and under Node 24 (types stripped). `bun run wasm && bun run wasm:diff` (or `node tools/run-wasm.ts …`) runs the corpus through it: 67 identical, 1 intended, 0 unexpected on the 68 fixtures; on pdf.js's 987 test PDFs 983 identical, 4 intended, 0 unexpected, **byte-identical to the Rust-only runner on both Bun and Node**. It has not been run in a browser.

Two things differ from the native library:

- **A panic is a trap.** wasm32 has no unwinding, so the profile is `panic = "abort"` and a panic is a `WebAssembly.RuntimeError`. The binding catches it, discards the instance, makes a fresh one and returns the `panic` status. `tools/wasm-trap-test.ts` shows on Bun and Node that a trap is catchable and a fresh instance works afterwards; no input has been found that makes the module panic, so the binding's recovery branch itself has not run on real input.
- **Memory only grows.** WebAssembly memory is never returned, so it sits at the high-water mark of the largest input: a full run over the 987 PDFs (the largest is 6 MB) left it at 42 MB. The binding replaces the instance when memory passes `recycleAboveBytes` (default 32 MiB), which leaves that run at 10.5 MB; the results stay byte-identical even when the threshold is low enough to replace it on every file (987 times, on Bun and Node).

Measured on 2026-11-09, Bun 1.4.4, the same 20 files and method as above (`tools/rss.ts wasm`, five runs):

| | pdf.js (unpdf) | pdf-probe wasm | pdf-probe native (`bun:ffi`) |
|---|---|---|---|
| load | +26 to +27.5 MB | +6.4 to +7.3 MB | +2.2 to +2.4 MB |
| after 20 checks | +41 to +43 MB | +18.2 to +20.8 MB | +3.0 to +3.2 MB |
| after 12 s idle and GC | +33 MB | +14.0 to +15.0 MB | +2.7 to +3.1 MB |
| the 20 checks | 21 to 22 ms | 9.3 to 13.2 ms | 2.7 to 3.8 ms |

The module's own memory is only 2.8 MB, so most of the wasm figure is JavaScriptCore's compiled code, as the lopdf-based wasm showed on chatbot-agent #1778. Wasm is the portable choice, not the lean one: under Bun, use the native library.

### npm package

`npm/` is a package, `pdf-probe`, that wraps both engines in one API: `createProbe({ engine: 'auto' | 'native' | 'wasm' })` returns `{ engine, check(pdf, { maxPages }), close() }`. `auto` uses the native library under Bun when one is found for the platform and WebAssembly everywhere else. Build and test it with `bun run npm:build` and `bun run npm:test`; `bun run npm:pack` writes the tarball (8 files, 316 KB: the 8 KB bundle, the 132 KB wasm, this machine's 468 KB native library, declarations, licences). See [`npm/README.md`](npm/README.md).

It is **not published**, and `package.json` is marked `"private": true` so `npm publish` refuses. Publishing is a step for the project owner: remove that line, decide the version, and reserve the name first (see the issue). A real release also needs a native library per platform, built in CI; this package carries only the one it was built on.

What was checked, on 2026-11-09:

- Parity with the Rust runner on both corpora, for every engine and runtime that applies (wasm on Node 24 and Bun, native and `auto` on Bun): 68 synthetic fixtures and 987 pdf.js test PDFs, 0 differences each.
- 9 API tests on Node and 11 on Bun (`npm/test/api.test.mjs`): input types, `maxPages`, a closed probe, the wasm module given as bytes, a module, a URL or a path, instance recycling, engine selection and the native failure messages.
- The declarations (`npm/src/index.d.ts`) type-check under `tsc --strict` with a consumer file that must also fail where it should (`npm/test/types.check.ts`); the TypeScript compiler used was 6.0.3.
- The packed tarball installed into an empty project works on Node and Bun, and a `bun build --compile` binary built from it, run from another directory, loads both engines from its embedded files.

Not checked: a browser, Deno, a platform other than Linux x86_64 glibc, and the bundle under a bundler other than Bun's.

### Fuzzing

`probe` is fuzzed two ways, both on the stable toolchain.

1. `cargo run --profile fuzz --example fuzz -- --seeds corpus/out --seconds 60 --rng 1` is a dependency-free mutation fuzzer. It mutates the corpus (byte edits, deleting, duplicating and splicing ranges, awkward numbers, PDF keywords, and length-preserving edits that keep the cross-reference valid), keeps inputs that reach a new outcome, and reports any panic, any input slower than 100 ms or that never returns, any memory jump over 128 MB, any successful answer that breaks a rule (page count matches the sizes, within the cap, finite positive sizes), and any input that gives two answers. The `fuzz` profile unwinds and turns on overflow checks, which the release profile would hide.
2. `fuzz/run.sh [seconds] [workers]` is the coverage-guided libFuzzer target in `fuzz/`. It builds with sanitizer-coverage instrumentation through ordinary rustc flags, overflow checks and debug assertions, so it needs no nightly Rust and no `cargo-fuzz`, only a C++ compiler (`libfuzzer-sys` compiles libFuzzer itself). With nightly and `cargo-fuzz`, `cargo +nightly fuzz run probe` also works.

What has been run, on 2026-11-09:

- **Mutation fuzzer:** about 28 million inputs from the 68 synthetic fixtures and about 890 of pdf.js's test PDFs under 150 KB. No panic, no hang, no memory spike, no broken rule. The only reports were two inputs over 100 ms, both AES-256 revision 6 files under heavy load (about 20 ms alone at the time; the hash is an iterated key derivation, bounded at about 290 rounds, and is now about 5.6 ms).
- **libFuzzer:** 905 seconds with 12 workers, about 2.9 million coverage-guided executions from the same 956 seeds, with overflow checks and debug assertions on. **0 crashes, 0 timeouts, 0 out-of-memory.** Edge coverage went from 1,666 to 2,377 (of 4,975 instrumented counters) and the seed corpus from 956 to 4,105 inputs.

That is evidence, not proof: 15 minutes of coverage-guided fuzzing is short, and the seeds are a small, mostly well-formed set. There was no AddressSanitizer run (the code has no `unsafe`, so a memory error would be a Rust-level bug, but it was not run). A longer run, seeded from a larger real-world corpus, is the next step.

## How it differs from other tools

- **[lopdf](https://github.com/J-F-Liu/lopdf)** is a full PDF library for reading and editing. In our corpus it did not open AES-256 (R6) files that pdf.js and poppler open, reported zero pages for a cyclic page tree (which a fail-closed check must not accept), and reconstructed files that the other two reject.
- **[DeckProbe](https://github.com/deckflow/deckprobe)** is a broader "ffprobe for documents" covering PDF, Office and iWork. As of its commit `2e5d064` (2026-09-22) it reports a PDF page count but has no page-size targets, and it parses PDFs with lopdf. `pdf-probe` is narrower: PDF only, page count and page size, with its own parser.
- **[pdf.js](https://github.com/mozilla/pdf.js)** is the reference we test against. It is a complete rendering library; `pdf-probe` aims to give the same answers for these two questions with a fraction of the code and memory.

If a tool you know already does this, please open an issue. This project is only worth having if it is smaller and stricter than the alternatives.

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Any code ported from pdf.js remains under the Apache License 2.0 with a statement of changes.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions.
