// Differential gate: candidate must equal the pdf.js baseline on verdict, page count and every page size.
const [baseF, candF] = process.argv.slice(2)
const base = await Bun.file(baseF).json(), cand = await Bun.file(candF).json()
let same = 0; const diffs: string[] = []
for (const f of Object.keys(base)) {
  const b = base[f], c = cand[f] ?? { verdict: 'MISSING' }
  const eq = b.verdict === c.verdict && (b.verdict === 'unparseable' || (b.pages === c.pages && JSON.stringify(b.sizes) === JSON.stringify(c.sizes)))
  // 'pageSize' stops at the first oversize page in neither impl: both walk every page, so sizes compare fully.
  if (eq) same++
  else diffs.push(`${f.padEnd(42)} pdf.js=${b.verdict}/${b.pages ?? '-'} candidate=${c.verdict}/${c.pages ?? '-'} ${c.err ?? ''}`)
}
console.log(`${same}/${Object.keys(base).length} identical`); diffs.forEach((d) => console.log('  DIFF', d))
