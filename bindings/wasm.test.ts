import { test, expect } from 'bun:test'
import { readFileSync } from 'node:fs'
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
    const padded = new Uint8Array(pdf.length * 8).fill(32)
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
