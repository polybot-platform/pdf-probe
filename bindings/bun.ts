// Loads the pdf-probe shared library with bun:ffi. A thin wrapper over one C function: see ffi/src/lib.rs
// for the result layout. `bun:ffi` is imported lazily so that this file can be bundled into a package that
// also runs under Node, where the native engine is simply never loaded.
import { ABI_VERSION, bufferSizeFor, decodeResult, HEADER, type ProbeResult } from './result.ts'

export type { PageSize, ProbeResult } from './result.ts'

/** The library file name for this platform, as `bun:ffi` names it. */
export async function libraryFileName(): Promise<string> {
  const { suffix } = await import('bun:ffi')
  return `${suffix === 'dll' ? '' : 'lib'}pdf_probe_ffi.${suffix}`
}

export async function defaultLibraryPath(): Promise<string> {
  return new URL(`../target/release/${await libraryFileName()}`, import.meta.url).pathname
}

export async function loadPdfProbe(path?: string) {
  const { dlopen, FFIType, ptr } = await import('bun:ffi')
  const lib = dlopen(path ?? (await defaultLibraryPath()), {
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
    const want = bufferSizeFor(maxPages)
    if (out.length < want) out = new Uint8Array(want)
    const call = () =>
      lib.symbols.pdf_probe_check(bytes.length ? ptr(bytes) : null, bytes.length, maxPages, ptr(out), out.length)
    let needed = call()
    if (needed > out.length) {
      out = new Uint8Array(needed)
      needed = call()
    }
    if (needed < 0) throw new Error(`pdf_probe_check failed with ${needed}`)
    return decodeResult(out, needed)
  }

  return { check, close: () => lib.close() }
}
