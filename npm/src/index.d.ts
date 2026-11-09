export type PageSize = [width: number, height: number]

export type ProbeResult =
  | { status: 'ok'; pages: number; sizes: PageSize[] }
  | { status: 'unparseable'; reason: string }
  | { status: 'empty' }
  | { status: 'tooManyPages'; pages: number }
  | { status: 'limitExceeded'; limit: LimitName }
  | { status: 'panic'; reason: string }

/** Which limit was hit. */
export type LimitName = 'inputBytes' | 'work' | 'objects' | 'xrefSections' | 'decompressed'

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

/** Creates a probe. Do it once and reuse it: loading is the expensive part, and checks are fast. */
export function createProbe(options?: CreateOptions): Promise<Probe>
