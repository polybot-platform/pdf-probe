export type PageSize = [width: number, height: number]

export type ProbeResult =
  | { status: 'ok'; pages: number; sizes: PageSize[] }
  | { status: 'unparseable'; reason: string }
  | { status: 'empty' }
  | { status: 'tooManyPages'; pages: number }
  | { status: 'panic'; reason: string }

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
}

export interface Probe {
  /** The engine in use. */
  readonly engine: 'native' | 'wasm'
  /**
   * Reads the page count and each page's size, in points. A damaged, encrypted or otherwise unreadable PDF is
   * a result (`unparseable`, `empty`, `tooManyPages`), not an exception. It throws only for a bad call.
   */
  check(pdf: Uint8Array | ArrayBuffer, options?: CheckOptions): ProbeResult
  /** Releases the native library. A probe cannot be used afterwards. */
  close(): void
}

/** Creates a probe. Do it once and reuse it: loading is the expensive part, and checks are fast. */
export function createProbe(options?: CreateOptions): Promise<Probe>
