// Decodes the result buffer written by pdf_probe_check. The layout is documented in ffi/src/lib.rs. Shared by
// the bun:ffi binding and the wasm binding. Plain TypeScript with no runtime-specific imports.

export type PageSize = [width: number, height: number]
export type ProbeResult =
  | { status: 'ok'; pages: number; sizes: PageSize[] }
  | { status: 'unparseable'; reason: string }
  | { status: 'empty' }
  | { status: 'tooManyPages'; pages: number }
  | { status: 'panic'; reason: string }

export const ABI_VERSION = 1
export const HEADER = 16
const STATUS = ['ok', 'unparseable', 'empty', 'tooManyPages', 'panic'] as const

/** The buffer size that fits any answer for a page cap, so a retry is rarely needed. */
export function bufferSizeFor(maxPages: number): number {
  return HEADER + 16 * Math.min(maxPages, 10_000) + 256
}

/** `buf` holds a result of `length` bytes. */
export function decodeResult(buf: Uint8Array, length: number): ProbeResult {
  const view = new DataView(buf.buffer, buf.byteOffset, length)
  const code = view.getUint32(0, true)
  const status = STATUS[code]
  const pages = view.getUint32(4, true)
  const count = view.getUint32(8, true)
  const reasonLength = view.getUint32(12, true)
  const reason = () =>
    new TextDecoder().decode(buf.subarray(HEADER + 16 * count, HEADER + 16 * count + reasonLength))
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
      throw new Error(`unknown status ${code}`)
  }
}
