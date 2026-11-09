// Memory of one reader inside a Bun process, by the method of chatbot-agent #1778: RSS deltas against a
// process that has read the input but loaded no reader, after Bun.gc(true). Also after an idle wait.
// Usage: bun run tools/rss.ts native|pdfjs <idle-seconds> <file>...
import { checkCurrent } from './pdfjs-reference'

const [mode, idle, ...files] = process.argv.slice(2)
const mb = () => process.memoryUsage().rss / 1048576
const inputs = await Promise.all(files.map(async (f) => new Uint8Array(await Bun.file(f).arrayBuffer())))
Bun.gc(true)
const base = mb()

let check: (bytes: Uint8Array) => Promise<unknown> | unknown
let close = () => {}
const t0 = performance.now()
if (mode === 'native') {
  const { loadPdfProbe } = await import('../bindings/bun')
  const probe = loadPdfProbe()
  check = (b) => probe.check(b, 10_000)
  close = probe.close
} else {
  const { getResolvedPDFJS } = await import('unpdf')
  await getResolvedPDFJS()
  check = (b) => checkCurrent(b, 10_000)
}
const loaded = mb()
const loadMs = performance.now() - t0

const t1 = performance.now()
for (const b of inputs) await check(b)
const checkMs = performance.now() - t1
const after = mb()
Bun.gc(true)
const afterGc = mb()
await new Promise((r) => setTimeout(r, Number(idle) * 1000))
Bun.gc(true)
const atIdle = mb()
const r = (x: number) => +x.toFixed(1)
console.log(
  JSON.stringify({ mode, files: inputs.length, baseMB: r(base), loadMB: r(loaded - base), afterChecksMB: r(after - base), afterGcMB: r(afterGc - base), idleMB: r(atIdle - base), loadMs: r(loadMs), checkMsTotal: r(checkMs) }),
)
close()
