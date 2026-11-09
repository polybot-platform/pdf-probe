// Runs the native library, loaded through bun:ffi, over a directory of PDFs and writes results in the corpus
// shape, so the differential test covers the whole path: Rust, the C ABI, and the Bun binding.
// Usage: bun run tools/run-native.ts corpus/out corpus/native-results.json
import { readdirSync } from 'node:fs'
import { loadPdfProbe } from '../bindings/bun.ts'

const MAX = { long: 17.5, short: 12 } // the example policy of the other runners, not part of the library
const round3 = (x: number) => Math.round(x * 1000) / 1000
const probe = await loadPdfProbe()
const dir = process.argv[2]
const out: Record<string, unknown> = {}
for (const f of readdirSync(dir).filter((x) => x.endsWith('.pdf')).sort()) {
  const bytes = new Uint8Array(await Bun.file(`${dir}/${f}`).arrayBuffer())
  const r = probe.check(bytes, 10_000)
  if (r.status === 'ok') {
    const over = r.sizes.some(([w, h]) => Math.max(w, h) / 72 > MAX.long || Math.min(w, h) / 72 > MAX.short)
    out[f] = { verdict: over ? 'pageSize' : null, pages: r.pages, sizes: r.sizes.map(([w, h]) => [round3(w), round3(h)]) }
  } else if (r.status === 'unparseable') out[f] = { verdict: 'unparseable', err: r.reason }
  else if (r.status === 'empty') out[f] = { verdict: 'empty' }
  else if (r.status === 'tooManyPages') out[f] = { verdict: 'pages', pages: r.pages }
  else out[f] = { verdict: 'PANIC', err: r.reason }
}
await Bun.write(process.argv[3], JSON.stringify(out, null, 1))
const verdicts: Record<string, number> = {}
for (const r of Object.values(out) as { verdict: string | null }[]) verdicts[String(r.verdict)] = (verdicts[String(r.verdict)] ?? 0) + 1
console.log(`${Object.keys(out).length} files through bun:ffi`, verdicts)
probe.close()
