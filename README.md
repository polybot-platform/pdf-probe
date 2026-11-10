# pdf-probe

A small, fail-closed reader for untrusted PDFs. Given the bytes of a PDF and a page cap, it returns:

- **Page count** (`pages`)
- **Effective size of each page** in points (`sizes`: `MediaBox ∩ CropBox`, with `/Rotate` and `/UserUnit` applied)
- **Structural flags** collected along the way (`version`, `encrypted`, `repaired`, `cropped`)

It does not render, extract text, decode images, or edit anything. Anything it cannot parse with confidence is refused (`unparseable`), never guessed at. Written in Rust with zero dependencies and `#![forbid(unsafe_code)]` in the parser, and shipped as WebAssembly and a native C ABI library.

## Quick start

### JavaScript / TypeScript (Node, Bun, browsers, workers)

```bash
npm install pdf-probe
```

```js
import { createProbe } from 'pdf-probe'

const probe = await createProbe() // load once and reuse
const result = probe.check(bytes, { maxPages: 200 })

switch (result.status) {
  case 'ok':
    // result.pages: number
    // result.sizes: [width, height][] in points (1/72 inch)
    // result.version: '1.7' | null
    // result.encrypted: boolean (opened with empty user password)
    // result.repaired: boolean (damaged xref recovered by scanning)
    // result.cropped: boolean (at least one page has CropBox < MediaBox)
    break
  case 'tooManyPages': // result.pages
  case 'empty': // well-formed PDF with 0 pages
  case 'unparseable': // result.reason (damaged, unsupported filter, or password required)
  case 'limitExceeded': // result.limit ('inputBytes' | 'work' | 'objects' | 'xrefSections' | 'decompressed')
}
```

### Rust

```rust
use pdf_probe::{Limits, probe};

let result = probe(&bytes, &Limits {
    max_pages: 200,
    ..Limits::default()
});
```

---

## Why

Systems that accept PDF uploads often need to check before doing expensive OCR, rendering, or LLM ingestion: **is this file within the page limit, and are the pages a reasonable size?**

Answering that with a full PDF library loads a rendering engine, font parser, and image decoders that these checks never use—a large attack surface and memory footprint to expose to hostile input. `pdf-probe` reads only the cross-reference data, trailer, catalog, and page tree.

## What it supports

- **Fails closed.** Damaged structure, unsupported filters, or non-empty passwords return `Unparseable`, never a guess.
- **Effective page size.** `MediaBox` intersected with `CropBox` (falling back to `MediaBox` if they do not overlap), inherited down the page tree, with `/Rotate` (90°/180°/270°) and `/UserUnit` applied.
- **Real-world PDFs.** Classic cross-reference tables, cross-reference streams, hybrid files (`/XRefStm`), incremental updates (`/Prev` chains), compressed object streams (`/ObjStm`), Flate with PNG predictors (`10..=15`), and standard encryption with an empty user password (RC4 40–128 bit, AES-128, AES-256 revisions 5 and 6).
- **Damaged cross-references.** When the cross-reference table or stream is missing or damaged, a bounded recovery scan indexes indirect objects and trailers using the same rules as `pdf.js` (and sets `repaired: true`).
- **Page count follows `/Count`.** Like `pdf.js` and poppler, it trusts a valid `/Count` on the root `/Pages` node and falls back to walking the full tree when `/Count` is missing, invalid, or larger than the actual tree.

---

## Bounded work and limits

Because the input is hostile, every loop and allocation is bounded. Hitting a resource limit returns `LimitExceeded` (`limitExceeded` in JS) naming which limit was reached, and never triggers a recovery rescan.

| Limit | Default | Option (Rust / JS) | Limit name |
|---|---|---|---|
| Input size | 10 MiB | `max_input_bytes` / `maxInputBytes` | `inputBytes` (checked before parsing) |
| Work budget | `2^30` units (~1.07B) | `max_work` / `maxWork` | `work` |
| Page cap | 10,000 | `max_pages` / `maxPages` | returns `TooManyPages` / `tooManyPages` |
| Objects, xref sections, inflated bytes | 1,000,000 / 256 / 64 MiB | Rust `Limits` struct | `objects`, `xrefSections`, `decompressed` |

Nesting depth (arrays/dictionaries) and page-tree depth are fixed at 64.

### The deterministic work counter and shared-object cache

A PDF can be small on disk yet expensive in total—for example, a `/Pages` node whose inherited `/MediaBox` points to a huge array would otherwise be re-parsed on every page. Two mechanisms bound this:

1. **Shared-object cache:** Indirect objects reached through references (such as a shared `/MediaBox` or `/Resources`) are parsed once and cached up to ~1 million nodes (~32 MB max), with `Rc`-shared arrays.
2. **Deterministic work budget (`Meter`):** Parsing, byte scanning, inflating, undoing predictors, hashing, ciphering, and walking page-tree nodes each charge a fixed per-byte or per-node weight (`src/meter.rs`). The count is machine-independent, so the same file always costs the exact same number of work units (`Probe.work`). Over the 1,021 answered PDFs in our test corpora, the median file uses ~18,000 units, p99 uses ~3.9M, and the largest uses 57M (~5% of the default budget).

---

## Accuracy and benchmarks vs. `pdf.js`

Tested by differential comparison against **pdf.js 6.4.299** (with poppler `pdfinfo` as a third reference):

| Corpus | Same as `pdf.js` | Intended difference | Unexpected difference |
|---|---|---|---|
| 68 synthetic fixtures | 67 | 1 (`empty-page-tree.pdf` refused as `empty`) | **0** |
| 987 `pdf.js` test PDFs | 983 | 4 (all refusals: Brotli stream, unparseable page object, 2 damaged files) | **0** |

### Speed and memory

On the 966 `pdf.js` test PDFs that both readers answer (124 MB total):
- **Standalone warm pass (966 files):** `pdf.js` **546 ms** (696 ms cold) vs. `pdf-probe` **~68 ms** (~8x faster).
- **20-file sample under Bun 1.4.4 (`tools/rss.ts`):**

| Metric | `pdf.js` (`unpdf`) | `pdf-probe` (Wasm) | `pdf-probe` (`bun:ffi` native) |
|---|---|---|---|
| Module load | +26 to +27.5 MB, 41–44 ms | +6.4 to +7.3 MB | +2.2 to +2.4 MB, 1.1–1.8 ms |
| RSS delta after 20 checks | +41 to +43 MB | +18.2 to +20.8 MB | +3.0 to +3.2 MB |
| RSS delta after idle + GC | +33 MB | +14.0 to +15.0 MB | +2.7 to +3.1 MB |
| Time for the 20 checks | 21–22 ms | 9–13 ms | **2.7–3.8 ms** |

See [`bench/`](bench/) for the multi-concurrency container benchmark harness.

---

## Engines: WebAssembly, Native (`bun:ffi`), and npm

The [`npm/`](npm/) package (`pdf-probe`) wraps both engines behind `createProbe({ engine: 'auto' | 'native' | 'wasm' })`:

- **`wasm` (shipped in the npm package, ~125 KB):** Zero-import `wasm32-unknown-unknown` build optimized with `wasm-opt -Oz`. Runs on Node 20+, Bun, Deno, browsers, and workers. If a trap ever occurs (`WebAssembly.RuntimeError`), the wrapper discards the instance, instantiates a fresh one, and returns `{ status: 'panic' }`. When linear memory grows past `recycleAboveBytes` (default 32 MiB), the instance is transparently replaced so memory returns to the OS.
- **`native` (`ffi/`, `libpdf_probe_ffi.so` / `.dylib` / `.dll`):** C ABI shared library (`pdf_probe_check`) with `catch_unwind` at the boundary so a panic can never unwind into the host process. Under Bun, `engine: 'auto'` loads the native library when present (via `nativeLibrary` option, `PDF_PROBE_NATIVE_LIBRARY`, or `native/<platform>-<arch>/`) and falls back to WebAssembly otherwise.

Both engines return byte-for-byte identical answers to the Rust runner across both corpora.

---

## Development, testing, and fuzzing

```bash
cargo test                              # Rust unit tests
bun install && bun run corpus           # generate the 68 synthetic fixtures
bun run slice && bun run slice:diff     # run Rust over the corpus and diff against pdf.js
bun run npm:build && bun run npm:test   # build Wasm + native + npm bundle and run Node & Bun tests
```

### Fuzzing

1. **Dependency-free mutation fuzzer (stable Rust):**
   ```bash
   cargo run --profile fuzz --example fuzz -- --seeds corpus/out --seconds 60 --rng 1
   ```
   Checks for panics, inputs slower than 100 ms, RSS jumps >128 MB, invariant violations, and non-determinism, with overflow checks enabled.
2. **Coverage-guided libFuzzer (`fuzz/`):**
   ```bash
   fuzz/run.sh 300 8
   ```
   Uses `rustc` sanitizer-coverage flags on stable Rust (or `cargo +nightly fuzz run probe`).

---

## How it differs from other tools

- **[lopdf](https://github.com/J-F-Liu/lopdf):** Full PDF reading/editing library. In our test corpus it did not open AES-256 (R6) files that `pdf.js` and poppler open, reported 0 pages for a cyclic page tree (which a fail-closed check must reject), and reconstructed damaged files that `pdf.js` and poppler refuse.
- **[DeckProbe](https://github.com/deckflow/deckprobe):** Broader document prober (PDF, Office, iWork) built on `lopdf`; reports PDF page count but not effective page sizes.
- **[pdf.js](https://github.com/mozilla/pdf.js):** Our reference implementation. `pdf-probe` gives the same answers for page count and page sizes with a fraction of the code size, latency, and memory.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
