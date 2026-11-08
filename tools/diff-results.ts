// Differential gate: candidate must equal the pdf.js baseline on verdict, page count and every page size.
// Fixtures listed in the optional intended-differences file must instead match the listed expectation, and
// must NOT equal pdf.js (a stale entry is reported). Any other difference is a failure.
const [baseF, candF, intendedF = new URL('../corpus/intended-differences.json', import.meta.url).pathname] = process.argv.slice(2)
const base = await Bun.file(baseF).json(), cand = await Bun.file(candF).json()
const intended = (await Bun.file(intendedF).exists()) ? await Bun.file(intendedF).json() : {}
type R = { verdict: string | null; pages?: number; sizes?: number[][]; err?: string }
const same = (b: R, c: R) =>
  b.verdict === c.verdict && (b.verdict === 'unparseable' || (b.pages === c.pages && JSON.stringify(b.sizes) === JSON.stringify(c.sizes)))
let ok = 0, allowed = 0; const diffs: string[] = []
for (const f of Object.keys(base)) {
  const b: R = base[f], c: R = cand[f] ?? { verdict: 'MISSING' }
  const want: R | undefined = intended[f]?.expected
  const show = `pdf.js=${b.verdict}/${b.pages ?? '-'} candidate=${c.verdict}/${c.pages ?? '-'} ${c.err ?? ''}`
  if (want) {
    if (same(b, c)) diffs.push(`${f.padEnd(42)} STALE intended difference: candidate now equals pdf.js`)
    else if (same(want, c)) allowed++
    else diffs.push(`${f.padEnd(42)} intended=${want.verdict} but ${show}`)
  } else if (same(b, c)) ok++
  else diffs.push(`${f.padEnd(42)} ${show}`)
}
console.log(`${ok} identical, ${allowed} intended difference(s), ${diffs.length} unexpected, of ${Object.keys(base).length}`)
diffs.forEach((d) => console.log('  DIFF', d))
process.exit(diffs.length ? 1 : 0)
