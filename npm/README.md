# pdf-probe

How many pages does an untrusted PDF have, and how big is each page? A small reader that answers only those two questions, and **fails closed**: a file it cannot read with confidence is reported as a failure, never guessed at. It does not render, extract text or decode images.

> **Status: not published, and marked `private` so it cannot be published by accident.** The parser matches pdf.js on pdf.js's own 987 test PDFs (983 identical, 4 intended differences that are all refusals, 0 different answers), but it is young. See the [project README](https://github.com/polybot-platform/pdf-probe#readme) for the evidence and what is not done.

```js
import { createProbe } from 'pdf-probe'

const probe = await createProbe() // once: loading is the expensive part
const result = probe.check(bytes, { maxPages: 200 }) // bytes: a Uint8Array or Buffer

switch (result.status) {
  case 'ok': // result.pages, result.sizes: [[width, height], ...] in points (1/72 inch)
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

`createProbe()` uses `engine: 'auto'`: the native library under Bun when one is found for the platform, WebAssembly otherwise. Ask for one with `{ engine: 'native' }` or `{ engine: 'wasm' }`; `native` throws a clear error if it cannot be loaded.

**Native libraries ship for one platform only so far** (the one the package was built on; here Linux x86_64, glibc). A release needs one per platform from CI, and the package looks for it in `native/<platform>-<arch>/`, then at `PDF_PROBE_NATIVE_LIBRARY`, then at the `nativeLibrary` option. Anywhere without one, `auto` falls back to WebAssembly.

## `bun build --compile`

A compiled binary has no `node_modules` on disk, so embed the engine files and pass their paths:

```js
import { createProbe } from 'pdf-probe'
import wasmFile from 'pdf-probe/pdf_probe.wasm' with { type: 'file' }
import nativeFile from 'pdf-probe/native/linux-x64/libpdf_probe_ffi.so' with { type: 'file' }

const probe = await createProbe({ nativeLibrary: nativeFile, wasm: wasmFile })
```

With both given, `auto` uses the native library and falls back to WebAssembly if it cannot be loaded. This was tried with a binary built from the installed package and run from another directory.

## Things to know

- **Passwords.** Only an empty user password is tried (RC4, AES-128, AES-256). A file that needs a password is `unparseable`.
- **Page cap.** `maxPages` (default 10,000, at most 1,000,000) bounds the work and the answer. More pages than that is `tooManyPages`.
- **Input size.** `maxInputBytes` (default 10 MiB, at most 4,294,967,295) is checked first: a larger input is `limitExceeded` (`inputBytes`) before anything is parsed.
- **Work budget.** `maxWork` (default about 1.07 billion units, roughly a second) bounds the total work of one call; running out is `limitExceeded` (`work`). A unit is about a nanosecond of work, but the count is deterministic: the same input costs the same everywhere. It catches work that is large only in total, such as one object re-parsed for every page (a 1 MB file can cost minutes without it). It is rounded up to a multiple of one million.
- **Other limits.** Object count, cross-reference chain length and decompressed size are bounded inside the library with fixed defaults; hitting one is `limitExceeded` naming it. They are not options.
- **A panic** cannot take the host down: the native library catches it at its boundary and reports `panic`; the WebAssembly engine sees a trap, discards the instance and reports `panic`. None has been found by fuzzing so far.
- **WebAssembly memory only grows.** The WebAssembly engine replaces its instance when its memory passes `recycleAboveBytes` (default 32 MiB).
- **Page sizes** are the effective size: MediaBox intersected with CropBox, with `/Rotate` and `/UserUnit` applied. A size limit is your policy, not the library's.

## Licence

MIT OR Apache-2.0.
