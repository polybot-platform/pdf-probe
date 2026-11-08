// Classic-structure fixtures for the differential test. Synthetic; no real documents.
import { mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { $ } from 'bun'

const out = process.argv[2]
mkdirSync(out, { recursive: true })

type Obj = { num: number; body: string }

/** Builds a classic-xref PDF from objects; `trailerExtra` lets a case add keys. */
function build(objs: Obj[], root = 1, trailerExtra = ''): string {
  let s = '%PDF-1.4\n'
  const offsets = new Map<number, number>()
  for (const o of objs) {
    offsets.set(o.num, s.length)
    s += `${o.num} 0 obj\n${o.body}\nendobj\n`
  }
  const size = Math.max(...objs.map((o) => o.num)) + 1
  const xref = s.length
  s += `xref\n0 ${size}\n0000000000 65535 f \n`
  for (let i = 1; i < size; i++) {
    const off = offsets.get(i)
    s += off === undefined ? '0000000000 65535 f \n' : `${String(off).padStart(10, '0')} 00000 n \n`
  }
  s += `trailer\n<< /Size ${size} /Root ${root} 0 R ${trailerExtra} >>\nstartxref\n${xref}\n%%EOF\n`
  return s
}

/** A page tree of `n` leaf pages, each with `pageExtra`; `rootExtra` goes on /Pages. */
function pages(n: number, pageExtra = '', rootExtra = ''): Obj[] {
  const kids = Array.from({ length: n }, (_, i) => `${3 + i} 0 R`).join(' ')
  const objs: Obj[] = [
    { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
    { num: 2, body: `<< /Type /Pages /Kids [${kids}] /Count ${n} ${rootExtra} >>` },
  ]
  for (let i = 0; i < n; i++) {
    objs.push({ num: 3 + i, body: `<< /Type /Page /Parent 2 0 R ${pageExtra} >>` })
  }
  return objs
}

const w = (name: string, data: string | Uint8Array): void => {
  writeFileSync(join(out, name), data)
}
const LETTER = '/MediaBox [0 0 612 792]'

// --- hand-built, classic xref ------------------------------------------------
w('classic-1page.pdf', build(pages(1, LETTER)))
w('classic-3pages.pdf', build(pages(3, LETTER)))
w('classic-200pages.pdf', build(pages(200, LETTER)))
w('inherited-mediabox.pdf', build(pages(2, '', LETTER)))
w('a3-ok.pdf', build(pages(1, '/MediaBox [0 0 842 1191]')))
w('tabloid-ok.pdf', build(pages(1, '/MediaBox [0 0 792 1224]')))
w('a2-oversize.pdf', build(pages(1, '/MediaBox [0 0 1191 1684]')))
w('oversize-page-2-of-3.pdf', build([
  { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
  { num: 2, body: '<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>' },
  { num: 3, body: `<< /Type /Page /Parent 2 0 R ${LETTER} >>` },
  { num: 4, body: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1700 2400] >>' },
  { num: 5, body: `<< /Type /Page /Parent 2 0 R ${LETTER} >>` },
]))
w('cropbox-smaller.pdf', build(pages(1, '/MediaBox [0 0 2000 3000] /CropBox [0 0 600 800]')))
w('cropbox-larger.pdf', build(pages(1, '/MediaBox [0 0 600 800] /CropBox [-100 -100 3000 3000]')))
w('rotate-90-landscape-ok.pdf', build(pages(1, '/MediaBox [0 0 1224 792] /Rotate 90')))
w('rotate-270.pdf', build(pages(1, `${LETTER} /Rotate 270`)))
w('rotate-negative.pdf', build(pages(1, `${LETTER} /Rotate -90`)))
w('rotate-inherited.pdf', build(pages(1, LETTER, '/Rotate 90')))
w('rotate-45-invalid.pdf', build(pages(1, `${LETTER} /Rotate 45`)))
w('userunit-2-oversize.pdf', build(pages(1, '/MediaBox [0 0 612 792] /UserUnit 2')))
w('userunit-0.5.pdf', build(pages(1, '/MediaBox [0 0 1200 1600] /UserUnit 0.5')))
w('mediabox-reversed-coords.pdf', build(pages(1, '/MediaBox [612 792 0 0]')))
w('mediabox-indirect-numbers.pdf', build([
  { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
  { num: 2, body: '<< /Type /Pages /Kids [3 0 R] /Count 1 >>' },
  { num: 3, body: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 6 0 R 7 0 R] >>' },
  { num: 6, body: '612' },
  { num: 7, body: '792' },
]))
w('mediabox-missing.pdf', build(pages(1, '')))
w('mediabox-zero-area.pdf', build(pages(1, '/MediaBox [0 0 0 0]')))
w('nested-page-tree.pdf', build([
  { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
  { num: 2, body: '<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 3 >>' },
  { num: 3, body: `<< /Type /Pages /Parent 2 0 R /Kids [5 0 R 6 0 R] /Count 2 ${LETTER} >>` },
  { num: 4, body: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1700 2400] >>' },
  { num: 5, body: '<< /Type /Page /Parent 3 0 R >>' },
  { num: 6, body: '<< /Type /Page /Parent 3 0 R /Rotate 90 >>' },
]))
// /Count lies: claims 2, tree has 5 leaves (an undercount is a tier-cap bypass).
w('count-undercounts.pdf', build([
  { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
  { num: 2, body: '<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R] /Count 2 >>' },
  ...[3, 4, 5, 6, 7].map((num) => ({ num, body: `<< /Type /Page /Parent 2 0 R ${LETTER} >>` })),
]))
// /Count claims 500, tree has 1 leaf.
w('count-overcounts.pdf', build([
  { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
  { num: 2, body: '<< /Type /Pages /Kids [3 0 R] /Count 500 >>' },
  { num: 3, body: `<< /Type /Page /Parent 2 0 R ${LETTER} >>` },
]))
// Page tree cycle: /Pages kid points back at its parent.
w('page-tree-cycle.pdf', build([
  { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
  { num: 2, body: '<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 2 >>' },
  { num: 3, body: `<< /Type /Page /Parent 2 0 R ${LETTER} >>` },
]))
// Kid without /Type (pdf.js infers from /Kids).
w('kid-without-type.pdf', build([
  { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
  { num: 2, body: '<< /Kids [3 0 R] /Count 1 >>' },
  { num: 3, body: `<< /Parent 2 0 R ${LETTER} /Contents [] >>` },
]))
w('empty-page-tree.pdf', build([
  { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
  { num: 2, body: '<< /Type /Pages /Kids [] /Count 0 >>' },
]))

// --- broken structure --------------------------------------------------------
{
  const ok = build(pages(2, LETTER))
  w('broken-xref-offsets.pdf', ok.replace(/\d{10} 00000 n/g, '0000000999 00000 n'))
  w('broken-startxref.pdf', ok.replace(/startxref\n\d+/, 'startxref\n99999'))
  w('no-xref-at-all.pdf', ok.slice(0, ok.indexOf('xref\n0 ')))
  w('truncated-half.pdf', ok.slice(0, Math.floor(ok.length / 2)))
  w('garbage-header.pdf', `GARBAGE-PREFIX-BYTES\n${ok}`)
  w('trailing-junk.pdf', `${ok}${'\0'.repeat(5000)}`)
}
w('not-a-pdf.pdf', 'hello this is a text file\n')
w('empty.pdf', '')
w('header-only.pdf', '%PDF-1.7\n')
w('png-named-pdf.pdf', new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]))

// --- incremental update: page deleted by an appended section -----------------
{
  const base = build(pages(3, LETTER))
  const oldXref = Number(base.match(/startxref\n(\d+)/)?.[1])
  // New section rewrites /Pages (obj 2) with two kids, orphaning obj 5.
  let s = base
  const newPages = s.length
  s += '2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n'
  const xref = s.length
  s += `xref\n2 1\n${String(newPages).padStart(10, '0')} 00000 n \n`
  s += `trailer\n<< /Size 6 /Root 1 0 R /Prev ${oldXref} >>\nstartxref\n${xref}\n%%EOF\n`
  w('incremental-page-deleted.pdf', s)
}
// Incremental update that appends a page.
{
  const base = build(pages(1, LETTER))
  const oldXref = Number(base.match(/startxref\n(\d+)/)?.[1])
  let s = base
  const o2 = s.length
  s += '2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n'
  const o4 = s.length
  s += `4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1700 2400] >>\nendobj\n`
  const xref = s.length
  s += `xref\n2 1\n${String(o2).padStart(10, '0')} 00000 n \n4 1\n${String(o4).padStart(10, '0')} 00000 n \n`
  s += `trailer\n<< /Size 5 /Root 1 0 R /Prev ${oldXref} >>\nstartxref\n${xref}\n%%EOF\n`
  w('incremental-page-added-oversize.pdf', s)
}

// --- Ghostscript: classic structure, linearized, RC4. Needs `gs` on the PATH. ---
// (Ghostscript 10 writes neither object streams nor xref streams, and only RC4
// encryption: those cases live in generate-modern.ts.)
const gsBase = ['-q', '-dNOPAUSE', '-dBATCH', '-sDEVICE=pdfwrite']
async function gs(name: string, src: string, ...flags: string[]): Promise<void> {
  const r = await $`gs ${gsBase} ${flags} -sOutputFile=${join(out, name)} ${src}`.quiet().nothrow()
  if (r.exitCode !== 0) console.error(`gs failed for ${name}: ${r.stderr.toString().slice(0, 200)}`)
}
const ps = join(out, '_src.ps')
const psBody = (n: number, w0: number, h0: number): string =>
  Array.from({ length: n }, (_, i) =>
    `<< /PageSize [${w0} ${h0}] >> setpagedevice /Helvetica findfont 24 scalefont setfont 72 ${h0 - 100} moveto (Page ${i + 1}) show showpage`).join('\n')
writeFileSync(ps, psBody(40, 612, 792))
const ps3 = join(out, '_src3.ps')
writeFileSync(ps3, psBody(3, 612, 792))
const psBig = join(out, '_srcbig.ps')
writeFileSync(psBig, psBody(2, 1191, 1684))
await gs('gs-classic-1.4-40p.pdf', ps, '-dCompatibilityLevel=1.4')
await gs('gs-linearized-40p.pdf', ps, '-dCompatibilityLevel=1.5', '-dFastWebView=true')
await gs('gs-oversize-a2.pdf', psBig, '-dCompatibilityLevel=1.5')
// Owner password only: empty user password, opens fine (common in the wild).
await gs('gs-encrypted-rc4-owner-only.pdf', ps3, '-dCompatibilityLevel=1.4', '-sOwnerPassword=owner', '-dEncryptionR=3', '-dKeyLength=128')
// The PostScript sources are not fixtures.
for (const f of [ps, ps3, psBig]) rmSync(f, { force: true })
console.log('classic fixtures written to', out)
