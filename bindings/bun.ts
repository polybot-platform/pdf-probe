// Loads the pdf-probe shared library with bun:ffi. A thin wrapper over one C function: see ffi/src/lib.rs
// for the result layout. Not a published package yet.
import { dlopen, FFIType, ptr, suffix } from 'bun:ffi'

export type PageSize = [width: number, height: number]
export type ProbeResult =
  | { status: 'ok'; pages: number; sizes: PageSize[] }
  | { status: 'unparseable'; reason: string }
  | { status: 'empty' }
  | { status: 'tooManyPages'; pages: number }
  | { status: 'panic'; reason: string }

const ABI_VERSION = 1
const HEADER = 16
const STATUS = ['ok', 'unparseable', 'empty', 'tooManyPages', 'panic'] as const

export function defaultLibraryPath(): string {
  return new URL(`../target/release/libpdf_probe_ffi.${suffix}`, import.meta.url).pathname
}

export function loadPdfProbe(path = defaultLibraryPath()) {
  const lib = dlopen(path, {
    pdf_probe_abi_version: { args: [], returns: FFIType.u32 },
    pdf_probe_check: {
      args: [FFIType.ptr, FFIType.u64, FFIType.u32, FFIType.ptr, FFIType.u64],
      returns: FFIType.i32,
    },
  })
  const version = lib.symbols.pdf_probe_abi_version()
  if (version !== ABI_VERSION) throw new Error(`pdf-probe ABI ${version}, expected ${ABI_VERSION}`)

  let out = new Uint8Array(HEADER + 256)

  /** Reads the page count and sizes of `bytes`. Never throws for a bad PDF; throws only for a bad call. */
  function check(bytes: Uint8Array, maxPages: number): ProbeResult {
    // Room for the sizes of every page up to the cap plus a reason; a larger answer is retried once.
    const want = HEADER + 16 * Math.min(maxPages, 10_000) + 256
    if (out.length < want) out = new Uint8Array(want)
    const call = () =>
      lib.symbols.pdf_probe_check(bytes.length ? ptr(bytes) : null, bytes.length, maxPages, ptr(out), out.length)
    let needed = call()
    if (needed > out.length) {
      out = new Uint8Array(needed)
      needed = call()
    }
    if (needed < 0) throw new Error(`pdf_probe_check failed with ${needed}`)
    const view = new DataView(out.buffer, out.byteOffset, needed)
    const status = STATUS[view.getUint32(0, true)]
    const pages = view.getUint32(4, true)
    const count = view.getUint32(8, true)
    const reasonLength = view.getUint32(12, true)
    const reason = () =>
      new TextDecoder().decode(out.subarray(HEADER + 16 * count, HEADER + 16 * count + reasonLength))
    switch (status) {
      case 'ok': {
        const sizes: PageSize[] = []
        for (let i = 0; i < count; i++) {
          sizes.push([view.getFloat64(HEADER + 16 * i, true), view.getFloat64(HEADER + 16 * i + 8, true)])
        }
        return { status, pages, sizes }
      }
      case 'unparseable':
      case 'panic':
        return { status, reason: reason() }
      case 'tooManyPages':
        return { status, pages }
      case 'empty':
        return { status }
      default:
        throw new Error(`unknown status ${view.getUint32(0, true)}`)
    }
  }

  return { check, close: () => lib.close() }
}
