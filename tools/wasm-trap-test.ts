// Checks the premise behind the wasm binding's panic handling: wasm has no unwinding, so a panic or an
// allocation failure is a trap, which the host sees as a catchable WebAssembly.RuntimeError, and a fresh
// instance of the same compiled module works afterwards. Runs under Bun and Node.
// Usage: node tools/wasm-trap-test.ts [path-to.wasm]
import { readFileSync } from 'node:fs'
import { loadPdfProbeWasm } from '../bindings/wasm.ts'

const wasmPath = process.argv[2] ?? new URL('../target/wasm32-unknown-unknown/wasm/pdf_probe_ffi.wasm', import.meta.url).pathname
const module = new WebAssembly.Module(readFileSync(wasmPath))
const pdf = new TextEncoder().encode(
  '%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n',
)

let failures = 0
const expect = (name: string, ok: boolean, detail = '') => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? `: ${detail}` : ''}`)
  if (!ok) failures++
}

const first = new WebAssembly.Instance(module, {}).exports as any
let trap: unknown
try {
  first.pdf_probe_alloc(0xfffffff0) // far more than wasm32 can ever give: an allocation failure aborts
} catch (e) {
  trap = e
}
expect('an allocation failure traps', trap instanceof WebAssembly.RuntimeError, String((trap as Error)?.message))

let again: unknown
try {
  first.pdf_probe_abi_version()
} catch (e) {
  again = e
}
expect('the trapped instance is not relied on afterwards', true, again ? 'it also throws' : 'it still answers, but is not trusted')

const fresh = loadPdfProbeWasm(module)
const r = fresh.check(pdf, 10)
expect('a fresh instance reads a PDF', r.status === 'ok' && r.sizes[0][0] === 300 && r.sizes[0][1] === 400, JSON.stringify(r))
expect('bad input is a status, not an exception', fresh.check(new TextEncoder().encode('not a pdf'), 10).status === 'unparseable')
expect('empty input is a status, not an exception', fresh.check(new Uint8Array(0), 10).status === 'unparseable')
process.exit(failures ? 1 : 0)
