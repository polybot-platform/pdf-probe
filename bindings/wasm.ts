// Loads pdf-probe compiled to WebAssembly. No imports are needed and nothing is glued in: the module takes
// bytes in its own linear memory and writes the answer there. Runs wherever WebAssembly does (Bun, Node,
// Deno, browsers, workers). Plain TypeScript with no runtime-specific imports.
//
// A panic inside the module is a trap (a WebAssembly.RuntimeError): wasm has no unwinding. The wrapper
// catches it, throws the instance away, makes a fresh one from the compiled module, and reports `panic`.
import { ABI_VERSION, bufferSizeFor, decodeResult, type ProbeResult } from './result.ts'

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

  /**
   * Reads the page count and sizes of `bytes`. Never throws for a bad PDF; throws only for a bad call.
   * `maxInputBytes` and `maxWorkMillions` of 0 mean the library's defaults.
   */
  function check(bytes: Uint8Array, maxPages: number, maxInputBytes = 0, maxWorkMillions = 0): ProbeResult {
    let outCap = bufferSizeFor(maxPages)
    let inPtr = 0
    let outPtr = 0
    try {
      inPtr = x.pdf_probe_alloc(bytes.length)
      // `memory.buffer` is replaced when the memory grows, so views are made fresh after every call.
      new Uint8Array(x.memory.buffer, inPtr, bytes.length).set(bytes)
      outPtr = x.pdf_probe_alloc(outCap)
      let needed = x.pdf_probe_check(inPtr, bytes.length, maxPages, maxInputBytes, maxWorkMillions, outPtr, outCap)
      if (needed > outCap) {
        x.pdf_probe_free(outPtr, outCap)
        outCap = needed
        outPtr = x.pdf_probe_alloc(outCap)
        needed = x.pdf_probe_check(inPtr, bytes.length, maxPages, maxInputBytes, maxWorkMillions, outPtr, outCap)
      }
      if (needed < 0) throw new Error(`pdf_probe_check failed with ${needed}`)
      // Decode directly from linear memory before freeing it.
      const result = decodeResult(new Uint8Array(x.memory.buffer, outPtr, needed), needed)
      x.pdf_probe_free(outPtr, outCap)
      x.pdf_probe_free(inPtr, bytes.length)
      if (x.memory.buffer.byteLength > recycleAboveBytes) {
        x = instantiate()
        recycles++
      }
      return result
    } catch (error) {
      if (error instanceof WebAssembly.RuntimeError) {
        // A trap leaves the instance in an unknown state: discard it.
        x = instantiate()
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
