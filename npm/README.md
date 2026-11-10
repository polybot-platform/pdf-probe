# pdf-probe

How many pages does an untrusted PDF have, and how big is each page? A small reader that answers only those two questions, and **fails closed**: a file it cannot read with confidence is reported as a failure, never guessed at. It does not render, extract text or decode images.

> **Status: version 0.1, pre-1.0.** The parser matches pdf.js on pdf.js's own 987 test PDFs (983 identical, 4 intended differences that are all refusals, 0 different answers), but it is young, has not been independently audited, and the API may change. See the [project README](https://github.com/polybot-platform/pdf-probe#readme) for the evidence and what is not done.

```js
import { createProbe } from 'pdf-probe'

const probe = await createProbe() // once: loading is the expensive part
const result = probe.check(bytes, { maxPages: 200 }) // bytes: a Uint8Array or Buffer

switch (result.status) {
  case 'ok': // result.pages, result.sizes: [[width, height], ...] in points (1/72 inch), plus version, encrypted, repaired, cropped
  case 'tooManyPages': // result.pages
  case 'empty': // well-formed, no pages
  case 'unparseable': // result.reason: damaged, unsupported or password-protected
  case 'limitExceeded': // result.limit: 'inputBytes', 'work', 'objects', 'xrefSections' or 'decompressed'
}
```

A damaged or password-protected PDF is a **result**, not an exception, and so is a limit. `check` throws only for a bad call (a wrong argument type, an option out of range, a closed probe).

## Engines

Two engines give the same answers, byte for byte, on the whole test corpus.

| | Where | Memory in Bun (measured locally) |
|---|---|---|
| `native` | Bun only, through `bun:ffi` | about +3 MB at idle |
| `wasm` | Bun, Node 20+, Deno, browsers, workers (no browser run yet) | about +15 MB at idle in Bun |

`createProbe()` uses `engine: 'auto'`: the native library under Bun when one is found, WebAssembly otherwise. **This package ships WebAssembly only**; the native library is built from the [repository](https://github.com/polybot-platform/pdf-probe) and passed in by path. Ask for one with `{ engine: 'native' }` or `{ engine: 'wasm' }`; `native` throws a clear error if it cannot be loaded.

**No native library is included.** Build one with `cargo build --release -p pdf-probe-ffi` and give its path with the `nativeLibrary` option or `PDF_PROBE_NATIVE_LIBRARY`; the package also looks in `native/<platform>-<arch>/` next to itself. Anywhere without one, `auto` uses WebAssembly.

## `bun build --compile`

A compiled binary has no `node_modules` on disk, so embed the engine files and pass their paths:

```js
import { createProbe } from 'pdf-probe'
import wasmFile from 'pdf-probe/pdf_probe.wasm' with { type: 'file' }
const probe = await createProbe({ wasm: wasmFile })
```

This was tried with a binary built from the installed package and run from another directory. A native library you built yourself can be embedded the same way and passed as `nativeLibrary`.

## Things to know

- **Passwords.** Only an empty user password is tried (RC4, AES-128, AES-256). A file that needs a password is `unparseable`.
- **Page cap.** `maxPages` (default 10,000, at most 1,000,000) bounds the work and the answer. More pages than that is `tooManyPages`.
- **Input size.** `maxInputBytes` (default 10 MiB, at most 4,294,967,295) is checked first: a larger input is `limitExceeded` (`inputBytes`) before anything is parsed.
- **Work budget.** `maxWork` (default about 1.07 billion units, roughly a second) bounds the total work of one call; running out is `limitExceeded` (`work`). A unit is about a nanosecond of work, but the count is deterministic: the same input costs the same everywhere. It catches work that is large only in total, such as one object re-parsed for every page (a 1 MB file can cost minutes without it). It is rounded up to a multiple of one million.
- **Other limits.** Object count, cross-reference chain length and decompressed size are bounded inside the library with fixed defaults; hitting one is `limitExceeded` naming it. They are not options.
- **A panic** cannot take the host down: the native library catches it at its boundary and reports `panic`; the WebAssembly engine sees a trap, discards the instance and reports `panic`. None has been found by fuzzing so far.
- **WebAssembly memory only grows.** The WebAssembly engine replaces its instance when its memory passes `recycleAboveBytes` (default 32 MiB).
- **Page sizes** are the effective size: MediaBox intersected with CropBox, with `/Rotate` and `/UserUnit` applied. A size limit is your policy, not the library's.
- **Metadata on `ok`.** `version` (`'1.4'`, `'1.7'`, `'2.0'`, or `null`), `encrypted` (opened with an empty user password), `repaired` (the cross-reference was damaged and recovered by scanning), and `cropped` (at least one page's visible box is smaller than its MediaBox).

## Licence

MIT OR Apache-2.0.
