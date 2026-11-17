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
  case 'limitExceeded': // result.limit: 'inputBytes' | 'work' | 'objects' | 'xrefSections' | 'decompressed' | 'values'
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

**This package carries the WebAssembly module (~125 KB). The native library comes in a package of its own for each platform**, listed as an optional dependency, so npm installs only the one for your machine (about 250 KB) and `createProbe()` under Bun finds it with no configuration:

| Platform | Package |
|---|---|
| Linux x64, glibc 2.28 or later | `pdf-probe-linux-x64` |
| Linux arm64, glibc 2.28 or later | `pdf-probe-linux-arm64` |
| Linux x64, musl (Alpine) | `pdf-probe-linux-x64-musl` |
| macOS, Apple silicon | `pdf-probe-darwin-arm64` |
| macOS, Intel | `pdf-probe-darwin-x64` |

Windows has no native package yet and uses WebAssembly. On any other platform, or with `npm install --omit=optional`, `engine: 'auto'` uses WebAssembly, and `engine: 'native'` throws an error that says what is missing. To use a library you built yourself (`cargo build --release -p pdf-probe-ffi` in the [repository](https://github.com/polybot-platform/pdf-probe)), pass its path as `nativeLibrary`, or set `PDF_PROBE_NATIVE_LIBRARY`; either wins over the installed package.

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
- **Structural limits:** Object count (`1,000,000`), xref chain length (`256`), decompressed bytes (`64 MiB`), and parsed values alive at once (`1,000,000`, which keeps peak memory to about 170 MB in the worst files we built) have fixed bounds inside the library and return `limitExceeded` if reached.
- **Passwords:** Only an empty user password is tried (RC4, AES-128, AES-256 R5/R6). Files requiring a user password return `unparseable`.
- **WebAssembly memory (`recycleAboveBytes`):** WebAssembly linear memory only grows; when it passes `recycleAboveBytes` (default `32 MiB`), the instance is transparently replaced with a fresh one.

The WebAssembly engine reuses input and result buffers between checks. Oversized inputs are rejected before
being copied into WebAssembly memory. Call `probe.close()` when finished to release the engine references and
retained buffers.

## License

MIT OR Apache-2.0.
