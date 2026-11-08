// Third opinion: poppler's `pdfinfo` over the corpus, in the same results shape as run-reference.ts.
// Usage: bun run tools/run-poppler.ts corpus/out corpus/poppler-results.json
// Needs `pdfinfo` (poppler-utils) on the PATH. Reports what poppler says; it does not try to agree with pdf.js.
//   - exit code non-zero, or no "Pages:" line   -> verdict 'unparseable' (with poppler's first error line)
//   - "Pages: 0"                                -> verdict 'empty'
//   - otherwise page sizes as printed, with /Rotate applied (pdfinfo prints the unrotated size and a separate rot line)
// pdfinfo does not apply /UserUnit, so its sizes differ from pdf.js on those fixtures; that is a limit of the tool.
// The size limit (17.5 x 12 in) is the same example policy as the pdf.js runner, not part of the library.
import { readdirSync } from 'node:fs'
const MAX = { long: 17.5, short: 12 }
const run = (args: string[]) => {
  const p = Bun.spawnSync(['pdfinfo', ...args], { timeout: 20_000 })
  return { code: p.exitCode ?? -1, out: p.stdout.toString(), err: p.stderr.toString() }
}
const round = (n: number) => Math.round(n * 1000) / 1000

export function checkPoppler(file: string) {
  const head = run([file])
  const count = head.out.match(/^Pages:\s+(\d+)/m)
  const firstErr = (head.err || head.out).split('\n').find((l) => l.trim()) ?? `exit ${head.code}`
  if (head.code !== 0 || !count) return { verdict: 'unparseable', err: firstErr.slice(0, 80) }
  const pages = Number(count[1])
  if (pages === 0) return { verdict: 'empty', pages: 0, sizes: [] }
  const all = run(['-f', '1', '-l', String(pages), file])
  if (all.code !== 0) return { verdict: 'unparseable', err: (all.err.split('\n').find((l) => l.trim()) ?? `exit ${all.code}`).slice(0, 80) }
  const sizes: [number, number][] = []
  let verdict: string | null = null
  for (let n = 1; n <= pages; n++) {
    const s = all.out.match(new RegExp(`^Page\\s+${n} size:\\s+([\\d.]+) x ([\\d.]+) pts`, 'm'))
    const r = all.out.match(new RegExp(`^Page\\s+${n} rot:\\s+(-?\\d+)`, 'm'))
    if (!s) return { verdict: 'unparseable', err: `no size for page ${n}` }
    let [w, h] = [Number(s[1]), Number(s[2])]
    if (r && Math.abs(Number(r[1])) % 180 === 90) [w, h] = [h, w]
    sizes.push([round(w), round(h)])
    const long = Math.max(w, h) / 72, short = Math.min(w, h) / 72
    if (!verdict && (long > MAX.long || short > MAX.short)) verdict = 'pageSize'
  }
  // pdfinfo can exit 0 after logging a problem (e.g. "Loop in Pages tree"); keep the first line so it is visible.
  const note = (all.err.split('\n').find((l) => l.trim()) ?? '').slice(0, 80)
  return note ? { verdict, pages, sizes, note } : { verdict, pages, sizes }
}

if (import.meta.main) {
  const [dir, outFile] = process.argv.slice(2)
  const out: Record<string, unknown> = {}
  for (const f of readdirSync(dir).filter((x) => x.endsWith('.pdf')).sort()) out[f] = checkPoppler(`${dir}/${f}`)
  await Bun.write(outFile, JSON.stringify(out, null, 1))
  for (const [f, r] of Object.entries(out)) {
    const x = r as { verdict: string | null; pages?: number; sizes?: number[][]; err?: string }
    console.log(f.padEnd(40), String(x.verdict).padEnd(12), String(x.pages ?? '-').padEnd(4), JSON.stringify(x.sizes?.slice(0, 3) ?? x.err ?? ''), (x as { note?: string }).note ?? '')
  }
}
