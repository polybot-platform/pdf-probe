# pdf-probe

A small, fail-closed reader for untrusted PDFs. Given the bytes of a PDF and a page cap, it returns:

- **Page count** (`pages`)
- **Effective size of each page** in points (`sizes`: `MediaBox ∩ CropBox`, with `/Rotate` and `/UserUnit` applied)
- **Structural flags** collected along the way (`version`, `encrypted`, `repaired`, `cropped`)

It does not render, extract text, or decode images. Anything it cannot parse with confidence is reported as a failure (`unparseable`), never guessed at.

```js
import { createProbe } from 'pdf-probe'

const probe = await createProbe() // load once and reuse
const result = probe.check(bytes, { maxPages: 200 }) // bytes: Uint8Array, Buffer, or ArrayBuffer

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
  case 'empty': // well-formed, no pages
  case 'unparseable': // result.reason: damaged, unsupported, or password-protected
  case 'limitExceeded': // result.limit: 'inputBytes' | 'work' | 'objects' | 'xrefSections' | 'decompressed'
}
```

A damaged or password-protected PDF is a **result**, not an exception, and so is hitting a resource limit. `check` throws only for a bad call (wrong argument type, option out of range, or a closed probe).

## Engines

Both engines return byte-for-byte identical answers across the synthetic corpus (68 fixtures) and `pdf.js`'s 987 test PDFs.

| Engine | Runtime | Idle memory delta in Bun |
|---|---|---|
| `wasm` | Node 20+, Bun, Deno, browsers, workers (shipped in this package) | ~+15 MB |
| `native` | Bun only (`bun:ffi`) | ~+3 MB |

`createProbe()` defaults to `engine: 'auto'`: under Bun it uses the native library when one is found, and WebAssembly everywhere else.

**This package ships the WebAssembly module only** (~125 KB). To use the native engine under Bun, build `libpdf_probe_ffi` from the [repository](https://github.com/polybot-platform/pdf-probe) (`cargo build --release -p pdf-probe-ffi`) and pass its path via `nativeLibrary` or `PDF_PROBE_NATIVE_LIBRARY` (or place it in `native/<platform>-<arch>/`).

## `bun build --compile`

A compiled single-file executable has no `node_modules` on disk, so embed the engine file and pass its path:

```js
import { createProbe } from 'pdf-probe'
import wasmFile from 'pdf-probe/pdf_probe.wasm' with { type: 'file' }

const probe = await createProbe({ wasm: wasmFile })
```

A native shared library can be embedded the same way and passed as `nativeLibrary`.

## Options and limits

- **Page cap (`maxPages`):** Default `10,000` (max `1,000,000`). More pages than that returns `{ status: 'tooManyPages', pages }`.
- **Input size (`maxInputBytes`):** Default `10 MiB` (max `4,294,967,295`). Checked first: a larger input returns `{ status: 'limitExceeded', limit: 'inputBytes' }` before anything is parsed.
- **Work budget (`maxWork`):** Default `~1.07 billion` units (roughly a second). A unit is roughly a nanosecond of work, counted deterministically so the same file costs the same units on every machine. Rounded up to a multiple of one million. Running out returns `{ status: 'limitExceeded', limit: 'work' }`.
- **Structural limits:** Object count (`1,000,000`), xref chain length (`256`), and decompressed bytes (`64 MiB`) have fixed bounds inside the library and return `limitExceeded` if reached.
- **Passwords:** Only an empty user password is tried (RC4, AES-128, AES-256 R5/R6). Files requiring a user password return `unparseable`.
- **WebAssembly memory (`recycleAboveBytes`):** WebAssembly linear memory only grows; when it passes `recycleAboveBytes` (default `32 MiB`), the instance is transparently replaced with a fresh one.

## License

MIT OR Apache-2.0.
