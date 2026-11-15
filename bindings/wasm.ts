// Loads pdf-probe compiled to WebAssembly. No imports are needed and nothing is glued in: the module takes
// bytes in its own linear memory and writes the answer there. Runs wherever WebAssembly does (Bun, Node,
// Deno, browsers, workers). Plain TypeScript with no runtime-specific imports.
//
// A panic inside the module is a trap (a WebAssembly.RuntimeError): wasm has no unwinding. The wrapper
// catches it, throws the instance away, makes a fresh one from the compiled module, and reports `panic`.
import { ABI_VERSION, bufferSizeFor, decodeResult, DEFAULT_MAX_INPUT_BYTES, type ProbeResult } from './result.ts'

export type { PageSize, ProbeResult } from './result.ts'

interface Exports {
  memory: WebAssembly.Memory
  pdf_probe_abi_version(): number
  pdf_probe_alloc(len: number): number
  pdf_probe_free(ptr: number, len: number): void
  pdf_probe_check(
    data: number,
    len: number,
    maxPages: number,
    maxInputBytes: number,
    maxWorkMillions: number,
    out: number,
    outCap: number,
  ): number
}

export interface WasmOptions {
  /**
   * WebAssembly memory only grows. When the module's memory is above this many bytes after a call, the
   * instance is replaced with a fresh one so the memory can be released. Default 32 MiB.
   */
  recycleAboveBytes?: number
}

export function loadPdfProbeWasm(wasm: BufferSource | WebAssembly.Module, options: WasmOptions = {}) {
  const recycleAboveBytes = options.recycleAboveBytes ?? 32 * 1024 * 1024
  const module = wasm instanceof WebAssembly.Module ? wasm : new WebAssembly.Module(wasm)
  const instantiate = (): Exports => {
    const x = new WebAssembly.Instance(module, {}).exports as unknown as Exports
    if (x.pdf_probe_abi_version() !== ABI_VERSION) throw new Error('pdf-probe wasm ABI mismatch')
    return x
  }
  let x = instantiate()
  let recycles = 0
  type Buffer = { ptr: number; capacity: number }
  let input: Buffer | undefined
  let output: Buffer | undefined

  function reset() {
    x = instantiate()
    input = output = undefined
  }

  function reserve(buffer: Buffer | undefined, needed: number, ceiling: number): Buffer | undefined {
    if (needed === 0 || (buffer && buffer.capacity >= needed)) return buffer
    const capacity = Math.max(needed, Math.min((buffer?.capacity ?? 0) * 2, ceiling))
    if (buffer) x.pdf_probe_free(buffer.ptr, buffer.capacity)
    return { ptr: x.pdf_probe_alloc(capacity), capacity }
  }

  /**
   * Reads the page count and sizes of `bytes`. Never throws for a bad PDF; throws only for a bad call.
   * `maxInputBytes` and `maxWorkMillions` of 0 mean the library's defaults.
   */
  function check(bytes: Uint8Array, maxPages: number, maxInputBytes = 0, maxWorkMillions = 0): ProbeResult {
    const inputLimit = maxInputBytes || DEFAULT_MAX_INPUT_BYTES
    if (bytes.length > inputLimit) return { status: 'limitExceeded', limit: 'inputBytes' }
    try {
      input = reserve(input, bytes.length, inputLimit)
      const inPtr = input?.ptr ?? 0
      // `memory.buffer` is replaced when the memory grows, so views are made fresh after every call.
      new Uint8Array(x.memory.buffer, inPtr, bytes.length).set(bytes)
      const want = bufferSizeFor(maxPages)
      output = reserve(output, want, want)!
      let needed = x.pdf_probe_check(inPtr, bytes.length, maxPages, maxInputBytes, maxWorkMillions, output.ptr, output.capacity)
      if (needed > output.capacity) {
        output = reserve(output, needed, needed)!
        needed = x.pdf_probe_check(inPtr, bytes.length, maxPages, maxInputBytes, maxWorkMillions, output.ptr, output.capacity)
      }
      if (needed < 0) throw new Error(`pdf_probe_check failed with ${needed}`)
      // The decoded result owns its values; buffers can be reused by the next check.
      const result = decodeResult(new Uint8Array(x.memory.buffer, output.ptr, needed), needed)
      if (x.memory.buffer.byteLength > recycleAboveBytes) {
        reset()
        recycles++
      }
      return result
    } catch (error) {
      // Discard retained pointers with the instance, including after a host-side copy/decode error.
      reset()
      if (error instanceof WebAssembly.RuntimeError) {
        // A trap leaves the instance in an unknown state: discard it.
        return { status: 'panic', reason: error.message }
      }
      throw error
    }
  }

  return {
    check,
    /** The size of the module's linear memory, in bytes. It grows with the largest input and never shrinks. */
    memoryBytes: () => x.memory.buffer.byteLength,
    /** How many times the instance was replaced because its memory passed the threshold. */
    recycleCount: () => recycles,
    /** For tests: the raw exports of the live instance. */
    exports: () => x,
  }
}
