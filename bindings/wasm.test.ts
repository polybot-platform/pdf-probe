import { test, expect } from 'bun:test'
import { readFileSync } from 'node:fs'
import { decodeResult, DEFAULT_MAX_INPUT_BYTES } from './result.ts'
import { loadPdfProbeWasm } from './wasm.ts'

const module = new WebAssembly.Module(readFileSync(new URL('../npm/dist/pdf_probe.wasm', import.meta.url)))
const pdf = new TextEncoder().encode(
  '%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /MediaBox [0 0 300 400] >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n',
)

test('oversized inputs are rejected without allocating or recycling Wasm memory', () => {
  const probe = loadPdfProbeWasm(module, { recycleAboveBytes: 1 })
  const instance = probe.exports()
  const memory = probe.memoryBytes()
  for (const [bytes, limit] of [[new Uint8Array(20 * 1024 * 1024), 0], [pdf, pdf.length - 1]] as const) {
    expect(probe.check(bytes, 10, limit)).toEqual({ status: 'limitExceeded', limit: 'inputBytes' })
    expect(probe.exports()).toBe(instance)
    expect(probe.memoryBytes()).toBe(memory)
    expect(probe.recycleCount()).toBe(0)
  }
  expect(probe.check(pdf, 10, pdf.length).status).toBe('ok')
})

test('buffers are reused and discarded with the instance after errors', () => {
  const OriginalInstance = WebAssembly.Instance
  let allocations = 0
  let failure: Error | undefined
  // Observe the real allocator and inject a failure at the same boundary as a Wasm trap.
  WebAssembly.Instance = class extends OriginalInstance {
    constructor(module: WebAssembly.Module, imports?: WebAssembly.Imports) {
      super(module, imports)
      const exports = this.exports as ReturnType<ReturnType<typeof loadPdfProbeWasm>['exports']>
      Object.defineProperty(this, 'exports', { value: {
        ...exports,
        pdf_probe_alloc(len: number) {
          allocations++
          return exports.pdf_probe_alloc(len)
        },
        pdf_probe_check(...args: Parameters<typeof exports.pdf_probe_check>) {
          if (failure) {
            const error = failure
            failure = undefined
            throw error
          }
          return exports.pdf_probe_check(...args)
        },
      } })
    }
  }
  try {
    const probe = loadPdfProbeWasm(module)
    const first = probe.check(pdf, 10)
    expect(first.status).toBe('ok')
    const initialAllocations = allocations
    for (let i = 0; i < 5; i++) expect(probe.check(pdf, 10)).toEqual(first)
    expect(allocations).toBe(initialAllocations)
    // Growing the input buffer must not invalidate the retained output buffer or earlier results.
    // Past one 64 KiB Wasm page, so the input buffer is replaced.
    const padded = new Uint8Array(70_000).fill(32)
    padded.set(pdf)
    expect(probe.check(padded, 10)).toEqual(first)
    expect(probe.check(pdf, 10)).toEqual(first)
    expect(allocations).toBe(initialAllocations + 1)
    for (const error of [new WebAssembly.RuntimeError('injected trap'), new RangeError('injected host error')]) {
      const before = probe.exports()
      failure = error
      if (error instanceof WebAssembly.RuntimeError) expect(probe.check(pdf, 10).status).toBe('panic')
      else expect(() => probe.check(pdf, 10)).toThrow(error)
      expect(probe.exports()).not.toBe(before)
      expect(probe.check(pdf, 10)).toEqual(first)
    }
  } finally {
    WebAssembly.Instance = OriginalInstance
  }
})

test('recycling resets retained buffers while preserving answers', () => {
  const probe = loadPdfProbeWasm(module, { recycleAboveBytes: 1 })
  const expected = probe.check(pdf, 10)
  for (let i = 0; i < 5; i++) expect(probe.check(pdf, 10)).toEqual(expected)
  expect(probe.recycleCount()).toBe(6)
})

test('growing a result buffer keeps page sizes and earlier results intact', () => {
  const probe = loadPdfProbeWasm(module)
  const small = probe.check(pdf, 10)
  const pages = 10_050
  const many = new TextEncoder().encode(
    new TextDecoder().decode(pdf).replace('/Kids [3 0 R] /Count 1', `/Kids [${'3 0 R '.repeat(pages)}] /Count ${pages}`),
  )
  const result = probe.check(many, pages)
  expect(result.status).toBe('ok')
  if (result.status !== 'ok') throw new Error('expected page sizes')
  expect(result.pages).toBe(pages)
  expect(result.sizes.length).toBe(pages)
  expect(result.sizes.every(([w, h]) => w === 300 && h === 400)).toBe(true)
  expect(probe.check(pdf, 10)).toEqual(small)
  expect(result.sizes.length).toBe(pages)
})

test('the host-side input default matches the module default', () => {
  // Bypass the host pre-check and let the module apply its own default (maxInputBytes = 0).
  const x = loadPdfProbeWasm(module).exports()
  const out = x.pdf_probe_alloc(256)
  const statusAt = (length: number) => {
    const data = x.pdf_probe_alloc(length)
    const needed = x.pdf_probe_check(data, length, 10, 0, 0, out, 256)
    const result = decodeResult(new Uint8Array(x.memory.buffer, out, needed), needed)
    x.pdf_probe_free(data, length)
    return result
  }
  expect(statusAt(DEFAULT_MAX_INPUT_BYTES + 1)).toEqual({ status: 'limitExceeded', limit: 'inputBytes' })
  expect(statusAt(DEFAULT_MAX_INPUT_BYTES)).not.toEqual({ status: 'limitExceeded', limit: 'inputBytes' })
})

test('a failed re-instantiation after a trap does not hide the trap or keep stale state', () => {
  const OriginalInstance = WebAssembly.Instance
  let trap = false
  let failInstantiation = false
  WebAssembly.Instance = class extends OriginalInstance {
    constructor(module: WebAssembly.Module, imports?: WebAssembly.Imports) {
      if (failInstantiation) {
        failInstantiation = false
        throw new RangeError('injected out of memory')
      }
      super(module, imports)
      const exports = this.exports as ReturnType<ReturnType<typeof loadPdfProbeWasm>['exports']>
      Object.defineProperty(this, 'exports', { value: {
        ...exports,
        pdf_probe_check(...args: Parameters<typeof exports.pdf_probe_check>) {
          if (trap) {
            trap = false
            throw new WebAssembly.RuntimeError('injected trap')
          }
          return exports.pdf_probe_check(...args)
        },
      } })
    }
  }
  try {
    const probe = loadPdfProbeWasm(module)
    const first = probe.check(pdf, 10)
    const before = probe.exports()
    trap = true
    failInstantiation = true
    // The trap is reported as such; replacing the instance is left to the next call.
    expect(probe.check(pdf, 10)).toEqual({ status: 'panic', reason: 'injected trap' })
    // That call fails to instantiate and says so, without reusing the trapped instance.
    expect(() => probe.check(pdf, 10)).toThrow('injected out of memory')
    expect(probe.check(pdf, 10)).toEqual(first)
    expect(probe.exports()).not.toBe(before)
  } finally {
    WebAssembly.Instance = OriginalInstance
  }
})

test('a growing input gets a buffer sized to fit, not doubled', () => {
  const OriginalInstance = WebAssembly.Instance
  const sizes: number[] = []
  WebAssembly.Instance = class extends OriginalInstance {
    constructor(module: WebAssembly.Module, imports?: WebAssembly.Imports) {
      super(module, imports)
      const exports = this.exports as ReturnType<ReturnType<typeof loadPdfProbeWasm>['exports']>
      Object.defineProperty(this, 'exports', { value: {
        ...exports,
        pdf_probe_alloc(len: number) {
          sizes.push(len)
          return exports.pdf_probe_alloc(len)
        },
      } })
    }
  }
  try {
    const probe = loadPdfProbeWasm(module)
    const padded = (length: number) => {
      const bytes = new Uint8Array(length).fill(32)
      bytes.set(pdf)
      return bytes
    }
    const expected = probe.check(pdf, 10)
    sizes.length = 0
    for (const length of [100_000, 131_072, 140_000]) expect(probe.check(padded(length), 10)).toEqual(expected)
    // 100,000 rounds up to two 64 KiB pages, 131,072 fits in them, and 140,000 needs three pages (not four).
    expect(sizes).toEqual([131_072, 196_608])
  } finally {
    WebAssembly.Instance = OriginalInstance
  }
})
