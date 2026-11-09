// Runs the package, with one engine, over a corpus and compares with the Rust runner's results.
// Usage: node npm/test/parity.mjs <wasm|native|auto> [--samples]   (or with bun)
// Needs `bun run slice` first (the Rust runner's results). With --samples, runs pdf.js's test PDFs.
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { createProbe } from '../dist/index.js'

const engine = process.argv[2] ?? 'auto'
const samples = process.argv.includes('--samples')
const repo = new URL('../../', import.meta.url).pathname
const dir = samples ? `${repo}corpus/pdfjs-samples/repo/test/pdfs` : `${repo}corpus/out`
const expectedFile = samples ? `${repo}corpus/pdfjs-samples/pdfprobe-results.json` : `${repo}corpus/pdfprobe-results.json`
if (!existsSync(dir) || !existsSync(expectedFile)) {
  console.error(`missing ${existsSync(dir) ? expectedFile : dir}: run \`bun run corpus && bun run slice\` (and \`bun run samples\` for --samples)`)
  process.exit(2)
}
const expected = JSON.parse(readFileSync(expectedFile, 'utf8'))
const MAX = { long: 17.5, short: 12 } // the example policy of the runners, not part of the library
const round3 = (x) => Math.round(x * 1000) / 1000

const probe = await createProbe({ engine })
let compared = 0
const diffs = []
for (const f of readdirSync(dir).filter((x) => x.endsWith('.pdf')).sort()) {
  const r = probe.check(new Uint8Array(readFileSync(`${dir}/${f}`)), 10_000)
  let got
  if (r.status === 'ok') {
    const over = r.sizes.some(([w, h]) => Math.max(w, h) / 72 > MAX.long || Math.min(w, h) / 72 > MAX.short)
    got = { verdict: over ? 'pageSize' : null, pages: r.pages, sizes: r.sizes.map(([w, h]) => [round3(w), round3(h)]) }
  } else if (r.status === 'unparseable') got = { verdict: 'unparseable', err: r.reason }
  else if (r.status === 'empty') got = { verdict: 'empty' }
  else if (r.status === 'tooManyPages') got = { verdict: 'pages', pages: r.pages }
  else got = { verdict: 'PANIC', err: r.reason }
  compared++
  if (JSON.stringify(got) !== JSON.stringify(expected[f])) diffs.push(`${f}: ${JSON.stringify(got).slice(0, 100)} vs ${JSON.stringify(expected[f]).slice(0, 100)}`)
}
probe.close()
const runtime = typeof Bun !== 'undefined' ? `bun ${Bun.version}` : `node ${process.version}`
console.log(`${runtime}, engine ${probe.engine} (asked for ${engine}): ${compared} files, ${diffs.length} differ from the Rust runner`)
for (const d of diffs.slice(0, 10)) console.log('  DIFF', d)
process.exit(diffs.length ? 1 : 0)
