// Runs the wasm build over a directory of PDFs and writes results in the corpus shape, so the differential
// test covers the whole path. Works under Bun and under Node 24 (which strips the types).
// Usage: bun run tools/run-wasm.ts corpus/out corpus/wasm-results.json [path-to.wasm]
//        node tools/run-wasm.ts corpus/out corpus/wasm-results.json [path-to.wasm]
import { readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { loadPdfProbeWasm } from '../bindings/wasm.ts'

const MAX = { long: 17.5, short: 12 } // the example policy of the other runners, not part of the library
const round3 = (x: number) => Math.round(x * 1000) / 1000
const wasmPath = process.argv[4] ?? new URL('../target/wasm32-unknown-unknown/wasm/pdf_probe_ffi.wasm', import.meta.url).pathname
// RECYCLE_BYTES lowers the memory threshold, to exercise the instance replacement.
const probe = loadPdfProbeWasm(readFileSync(wasmPath), process.env.RECYCLE_BYTES ? { recycleAboveBytes: Number(process.env.RECYCLE_BYTES) } : {})
const dir = process.argv[2]
const out: Record<string, unknown> = {}
for (const f of readdirSync(dir).filter((x) => x.endsWith('.pdf')).sort()) {
  const r = probe.check(new Uint8Array(readFileSync(`${dir}/${f}`)), 10_000)
  if (r.status === 'ok') {
    const over = r.sizes.some(([w, h]) => Math.max(w, h) / 72 > MAX.long || Math.min(w, h) / 72 > MAX.short)
    out[f] = { verdict: over ? 'pageSize' : null, pages: r.pages, sizes: r.sizes.map(([w, h]) => [round3(w), round3(h)]) }
  } else if (r.status === 'unparseable') out[f] = { verdict: 'unparseable', err: r.reason }
  else if (r.status === 'limitExceeded') out[f] = { verdict: 'limit', err: r.limit }
  else if (r.status === 'empty') out[f] = { verdict: 'empty' }
  else if (r.status === 'tooManyPages') out[f] = { verdict: 'pages', pages: r.pages }
  else out[f] = { verdict: 'PANIC', err: r.reason }
}
writeFileSync(process.argv[3], JSON.stringify(out, null, 1))
const verdicts: Record<string, number> = {}
for (const r of Object.values(out) as { verdict: string | null }[]) verdicts[String(r.verdict)] = (verdicts[String(r.verdict)] ?? 0) + 1
console.log(`${Object.keys(out).length} files through wasm, memory ${(probe.memoryBytes() / 1048576).toFixed(1)} MB, ${probe.recycleCount()} recycles`, JSON.stringify(verdicts))
