import { readdirSync } from 'node:fs'
import { checkCurrent } from './pdfjs-reference'
const dir = process.argv[2]
const out: Record<string, unknown> = {}
for (const f of readdirSync(dir).filter((x) => x.endsWith('.pdf')).sort()) {
  const bytes = new Uint8Array(await Bun.file(`${dir}/${f}`).arrayBuffer())
  out[f] = await checkCurrent(bytes, 10_000)
}
await Bun.write(process.argv[3], JSON.stringify(out, null, 1))
for (const [f, r] of Object.entries(out)) {
  const x = r as { verdict: string | null; pages?: number; sizes?: number[][]; err?: string }
  console.log(f.padEnd(40), String(x.verdict).padEnd(12), String(x.pages ?? '-').padEnd(4), JSON.stringify(x.sizes?.slice(0, 3) ?? x.err ?? ''))
}
