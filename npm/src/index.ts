// pdf-probe: the page count and every page's size of an untrusted PDF, or a failure. One API over two engines:
// the native shared library (Bun only, through bun:ffi) and WebAssembly (everywhere else). Both give the same
// answers, byte for byte, on the whole test corpus.
import { fileURLToPath } from 'node:url'
import { libraryFileName, loadPdfProbe as loadNative } from '../../bindings/bun.ts'
import type { ProbeResult } from '../../bindings/result.ts'
import { loadPdfProbeWasm } from '../../bindings/wasm.ts'

export type { LimitName, PageSize, ProbeResult } from '../../bindings/result.ts'

/** `auto` uses the native library under Bun when one is found for this platform, and WebAssembly otherwise. */
export type Engine = 'auto' | 'native' | 'wasm'

export interface CreateOptions {
  engine?: Engine
  /** A path to the native shared library. Default: `PDF_PROBE_NATIVE_LIBRARY`, then the package's own `native/`. */
  nativeLibrary?: string
  /** The WebAssembly module: bytes, a compiled module, a URL or a path. Default: the file shipped in the package. */
  wasm?: BufferSource | WebAssembly.Module | URL | string
  /** WebAssembly memory only grows; above this many bytes the instance is replaced. Default 32 MiB. */
  recycleAboveBytes?: number
}

export interface CheckOptions {
  /** More pages than this is reported as `tooManyPages`. At most 1,000,000. Default 10,000. */
  maxPages?: number
  /**
   * The largest input accepted, in bytes. A larger one is `limitExceeded` (`inputBytes`) before anything is
   * parsed. At most 4,294,967,295. Default 10 MiB.
   */
  maxInputBytes?: number
  /**
   * A budget of work units for the call; running out is `limitExceeded` (`work`). A unit is roughly a
   * nanosecond of work on a modern core, and the count is the same on every machine, so the same input always
   * costs the same. Rounded up to a multiple of one million. Default about 1.07 billion (roughly a second).
   * It catches work that is large only in total, such as one object re-parsed for every page.
   */
  maxWork?: number
}

export interface Probe {
  /** The engine in use. */
  readonly engine: 'native' | 'wasm'
  /**
   * Reads the page count and each page's size, in points. A damaged, encrypted or otherwise unreadable PDF is
   * a result (`unparseable`, `empty`, `tooManyPages`, `limitExceeded`), not an exception. It throws only for
   * a bad call.
   */
  check(pdf: Uint8Array | ArrayBuffer, options?: CheckOptions): ProbeResult
  /** Releases the native library. A probe cannot be used afterwards. */
  close(): void
}

const DEFAULT_MAX_PAGES = 10_000
const MAX_PAGES_CEILING = 1_000_000
const U32_MAX = 0xffff_ffff

const isBun = (): boolean => typeof (globalThis as { Bun?: unknown }).Bun !== 'undefined'
const isNodeLike = (): boolean => typeof process !== 'undefined' && typeof process.versions?.node === 'string'

/** The arguments for the engines: the bytes, then the three limits (0 for the last two means the default). */
function prepare(
  pdf: Uint8Array | ArrayBuffer,
  options: CheckOptions | undefined,
): [bytes: Uint8Array, maxPages: number, maxInputBytes: number, maxWorkMillions: number] {
  const bytes = pdf instanceof Uint8Array ? pdf : pdf instanceof ArrayBuffer ? new Uint8Array(pdf) : undefined
  if (!bytes) throw new TypeError('pdf must be a Uint8Array (a Buffer is one) or an ArrayBuffer')
  const maxPages = options?.maxPages ?? DEFAULT_MAX_PAGES
  if (!Number.isInteger(maxPages) || maxPages < 0 || maxPages > MAX_PAGES_CEILING) {
    throw new RangeError(`maxPages must be an integer from 0 to ${MAX_PAGES_CEILING}`)
  }
  const maxInputBytes = options?.maxInputBytes ?? 0
  if (!Number.isInteger(maxInputBytes) || maxInputBytes < 0 || maxInputBytes > U32_MAX || (options?.maxInputBytes !== undefined && maxInputBytes < 1)) {
    throw new RangeError(`maxInputBytes must be an integer from 1 to ${U32_MAX}`)
  }
  const maxWork = options?.maxWork
  if (maxWork !== undefined && (!Number.isInteger(maxWork) || maxWork < 1 || Math.ceil(maxWork / 1e6) > U32_MAX)) {
    throw new RangeError('maxWork must be an integer of at least 1 (and at most about 4.29e15)')
  }
  return [bytes, maxPages, maxInputBytes, maxWork === undefined ? 0 : Math.ceil(maxWork / 1e6)]
}

type Run = (bytes: Uint8Array, maxPages: number, maxInputBytes: number, maxWorkMillions: number) => ProbeResult

function wrap(engine: 'native' | 'wasm', run: Run, release: () => void): Probe {
  let closed = false
  return {
    engine,
    check(pdf, options) {
      if (closed) throw new Error('this probe is closed')
      return run(...prepare(pdf, options))
    },
    close() {
      if (!closed) release()
      closed = true
    },
  }
}

async function findNativeLibrary(explicit?: string): Promise<string | undefined> {
  const bun = (globalThis as { Bun?: { file(p: string): { exists(): Promise<boolean> } } }).Bun
  const candidates = [
    explicit,
    process.env.PDF_PROBE_NATIVE_LIBRARY,
    fileURLToPath(new URL(`../native/${process.platform}-${process.arch}/${await libraryFileName()}`, import.meta.url)),
  ]
  for (const path of candidates) {
    if (path && (await bun?.file(path).exists())) return path
    if (path && path === explicit) return path // let an explicit path fail loudly when it is loaded
  }
  return undefined
}

async function wasmSource(source: CreateOptions['wasm']): Promise<BufferSource | WebAssembly.Module> {
  if (source instanceof WebAssembly.Module) return source
  if (source && typeof source === 'object' && !(source instanceof URL)) return source
  const url =
    source instanceof URL
      ? source
      : typeof source === 'string'
        ? /^[a-z][a-z0-9+.-]*:/i.test(source) && !/^[a-z]:[\\/]/i.test(source)
          ? new URL(source)
          : undefined
        : new URL('./pdf_probe.wasm', import.meta.url)
  if (isNodeLike() && (url === undefined || url.protocol === 'file:')) {
    const { readFile } = await import('node:fs/promises')
    return await readFile(url ? fileURLToPath(url) : (source as string))
  }
  const response = await fetch(url as URL)
  if (!response.ok) throw new Error(`could not load the WebAssembly module: ${response.status} ${response.statusText}`)
  return await response.arrayBuffer()
}

/** Creates a probe. Do it once and reuse it: loading is the expensive part, and checks are fast. */
export async function createProbe(options: CreateOptions = {}): Promise<Probe> {
  const engine = options.engine ?? 'auto'
  if (engine !== 'auto' && engine !== 'native' && engine !== 'wasm') {
    throw new TypeError(`engine must be 'auto', 'native' or 'wasm', not ${String(engine)}`)
  }
  if (engine !== 'wasm') {
    if (!isBun()) {
      if (engine === 'native') throw new Error("the 'native' engine needs Bun (it loads the library with bun:ffi)")
    } else {
      const path = await findNativeLibrary(options.nativeLibrary)
      if (path) {
        try {
          const lib = await loadNative(path)
          return wrap('native', lib.check, lib.close)
        } catch (cause) {
          if (engine === 'native') throw new Error(`could not load the native library at ${path}`, { cause })
        }
      } else if (engine === 'native') {
        throw new Error(
          `no native library for ${process.platform}-${process.arch}: set nativeLibrary or PDF_PROBE_NATIVE_LIBRARY`,
        )
      }
    }
  }
  const wasm = loadPdfProbeWasm(await wasmSource(options.wasm), { recycleAboveBytes: options.recycleAboveBytes })
  return wrap('wasm', wasm.check, () => {})
}
