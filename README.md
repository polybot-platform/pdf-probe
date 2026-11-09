# pdf-probe

A small, fail-closed reader that answers two questions about an untrusted PDF: **how many pages does it have, and how big is each page?** It does not render, extract text, decode images or edit anything.

> **Status: all the planned parsing is in; not yet hardened. Do not depend on it.** The crate reads classic and stream cross-references (hybrid files and `/Prev` chains included), object streams, Flate with PNG predictors, encryption with an empty user password (RC4, AES-128, AES-256 revisions 5 and 6), the page tree and page sizes, and it rescans the file for objects when the cross-reference is damaged, as pdf.js does. A file it cannot read with confidence is refused (`Unparseable`), never guessed at. Against pdf.js's own 987 test PDFs it gives the same answer on 983, differs on 4 by design (all four are refusals), and **gives a different answer on none**. Fuzzing has found nothing so far (see [Fuzzing](#fuzzing)). Not yet done: a coverage-guided fuzz run, the native/wasm/npm packaging, and the measurement in a container. See [the status table](#status) below.

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

Speed and memory, native build, on the 966 real files that both readers answer identically (123 MB): pdf.js 546 ms warm (696 ms first run), pdf-probe 100 ms warm, about 5 times faster. On a set of 20 of those files: pdf.js 6.8 ms warm and 71 ms first run, pdf-probe 2.2 ms and 2.6 ms; the ratio depends on which files, because AES-256 revision 6 (an iterated key derivation, 9.4 ms against 23.5 ms in pdf.js) dominates when it is present. Peak RSS for the whole process on those 20 files (1.9 MB of input): Bun reading the files 20 MB, Bun plus pdf.js 66 to 68 MB, the pdf-probe binary 3.9 MB. Per damaged fixture needing the recovery scan: 0.02 ms against 2.0 ms for pdf.js (tiny files). These are a standalone binary, not `bun:ffi`, and not at idle in a container; treat them as an ordering, not a result. They also exclude the files each reader refuses.

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

### Fuzzing

`probe` is fuzzed two ways. `cargo run --profile fuzz --example fuzz -- --seeds corpus/out --seconds 60 --rng 1` is a dependency-free mutation fuzzer that runs on stable: it mutates the corpus (byte edits, deleting, duplicating and splicing ranges, awkward numbers, PDF keywords, and length-preserving edits that keep the cross-reference valid), keeps inputs that reach a new outcome, and reports any panic, any input slower than 100 ms or that never returns, any memory jump over 128 MB, any successful answer that breaks a rule (page count matches the sizes, within the cap, finite positive sizes), and any input that gives two answers. The `fuzz` profile unwinds and turns on overflow checks, which the release profile would hide. `fuzz/` holds the conventional libFuzzer target for coverage-guided runs (`cargo +nightly fuzz run probe`; it compiles on stable but has not been run, because it needs a nightly toolchain).

About 28 million mutated inputs have been run so far, from the 68 synthetic fixtures and about 890 of pdf.js's test PDFs under 150 KB: no panic, no hang, no memory spike, no broken rule. The only reports were two inputs over 100 ms, both AES-256 revision 6 files under heavy load (about 20 ms alone at the time; the hash is an iterated key derivation, bounded at about 290 rounds, and is now about 5.6 ms). A blind mutation fuzzer reaches the deeper code less well than a coverage-guided one, so this is evidence, not proof; the libFuzzer target is the next step.

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
