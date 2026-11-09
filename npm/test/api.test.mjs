// API tests. Run under both runtimes: `node --test npm/test/api.test.mjs` and `bun test npm/test/api.test.mjs`.
import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const isBun = typeof Bun !== 'undefined'
const { test } = isBun ? await import('bun:test') : await import('node:test')
const { createProbe } = await import('../dist/index.js')

const pdf = new TextEncoder().encode(
  '%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >>\nendobj\n4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] /Rotate 90 >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n',
)
const wasmPath = fileURLToPath(new URL('../dist/pdf_probe.wasm', import.meta.url))
const nativePath = fileURLToPath(
  new URL(`../native/${process.platform}-${process.arch}/libpdf_probe_ffi.${process.platform === 'darwin' ? 'dylib' : 'so'}`, import.meta.url),
)
const hasNative = isBun && existsSync(nativePath)

test('reads the page count and sizes (wasm)', async () => {
  const probe = await createProbe({ engine: 'wasm' })
  assert.equal(probe.engine, 'wasm')
  assert.deepEqual(probe.check(pdf), { status: 'ok', pages: 2, sizes: [[300, 400], [200, 100]] })
})

test('accepts a Buffer and an ArrayBuffer', async () => {
  const probe = await createProbe({ engine: 'wasm' })
  assert.equal(probe.check(Buffer.from(pdf)).status, 'ok')
  assert.equal(probe.check(pdf.buffer.slice(pdf.byteOffset, pdf.byteOffset + pdf.byteLength)).status, 'ok')
})

test('a bad PDF is a result, not an exception', async () => {
  const probe = await createProbe({ engine: 'wasm' })
  assert.equal(probe.check(new TextEncoder().encode('not a pdf')).status, 'unparseable')
  assert.equal(probe.check(new Uint8Array(0)).status, 'unparseable')
  assert.equal(probe.check(new Uint8Array(1000).fill(255)).status, 'unparseable')
})

test('maxPages caps the answer', async () => {
  const probe = await createProbe({ engine: 'wasm' })
  assert.deepEqual(probe.check(pdf, { maxPages: 1 }), { status: 'tooManyPages', pages: 2 })
  assert.equal(probe.check(pdf, { maxPages: 2 }).status, 'ok')
})

test('a bad call throws', async () => {
  const probe = await createProbe({ engine: 'wasm' })
  assert.throws(() => probe.check('a string'), TypeError)
  assert.throws(() => probe.check(pdf, { maxPages: -1 }), RangeError)
  assert.throws(() => probe.check(pdf, { maxPages: 1.5 }), RangeError)
  assert.throws(() => probe.check(pdf, { maxPages: 2_000_000 }), RangeError)
  await assert.rejects(createProbe({ engine: 'fast' }), TypeError)
})

test('a closed probe refuses to run', async () => {
  const probe = await createProbe({ engine: 'wasm' })
  probe.close()
  probe.close() // closing twice is fine
  assert.throws(() => probe.check(pdf), /closed/)
})

test('the wasm module can be given as bytes, a compiled module, a URL or a path', async () => {
  const bytes = readFileSync(wasmPath)
  for (const wasm of [bytes, new WebAssembly.Module(bytes), new URL(`file://${wasmPath}`), wasmPath]) {
    const probe = await createProbe({ engine: 'wasm', wasm })
    assert.equal(probe.check(pdf).status, 'ok')
  }
  await assert.rejects(createProbe({ engine: 'wasm', wasm: '/no/such/file.wasm' }))
})

test('the wasm instance is replaced above the memory threshold, with the same answers', async () => {
  const probe = await createProbe({ engine: 'wasm', recycleAboveBytes: 1 })
  for (let i = 0; i < 5; i++) assert.equal(probe.check(pdf).status, 'ok')
})

if (!isBun) {
  test('under Node, auto picks wasm and native explains why it cannot', async () => {
    assert.equal((await createProbe()).engine, 'wasm')
    await assert.rejects(createProbe({ engine: 'native' }), /needs Bun/)
  })
} else {
  test('under Bun, auto picks the native library when there is one', async () => {
    const probe = await createProbe()
    assert.equal(probe.engine, hasNative ? 'native' : 'wasm')
    assert.equal(probe.check(pdf).status, 'ok')
  })

  test('under Bun, the native engine gives the same answer and can be closed', { skip: !hasNative }, async () => {
    const probe = await createProbe({ engine: 'native' })
    assert.equal(probe.engine, 'native')
    assert.deepEqual(probe.check(pdf), { status: 'ok', pages: 2, sizes: [[300, 400], [200, 100]] })
    assert.deepEqual(probe.check(pdf, { maxPages: 1 }), { status: 'tooManyPages', pages: 2 })
    probe.close()
    assert.throws(() => probe.check(pdf), /closed/)
  })

  test('under Bun, an explicit native path is honoured and a bad one fails loudly', { skip: !hasNative }, async () => {
    assert.equal((await createProbe({ engine: 'native', nativeLibrary: nativePath })).engine, 'native')
    await assert.rejects(createProbe({ engine: 'native', nativeLibrary: '/no/such/lib.so' }), /could not load/)
    // `auto` falls back to wasm when the explicit library cannot be loaded.
    assert.equal((await createProbe({ nativeLibrary: '/no/such/lib.so' })).engine, 'wasm')
  })
}
