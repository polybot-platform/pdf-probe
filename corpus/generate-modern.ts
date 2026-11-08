// Modern-structure fixtures for the corpus: xref streams, object streams,
// predictors, incremental updates, hybrid files, and RC4/AES-128/AES-256
// encryption with an empty user password. Synthetic; no real documents.
import { createCipheriv, createHash, randomBytes } from 'node:crypto'
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { deflateSync } from 'node:zlib'

const out = process.argv[2]
mkdirSync(out, { recursive: true })

const md5 = (...b: Buffer[]): Buffer => createHash('md5').update(Buffer.concat(b)).digest()
const hex = (b: Buffer): string => `<${b.toString('hex')}>`
const PAD = Buffer.from('28BF4E5E4E758A4164004E56FFFA01082E2E00B6D0683E802F0CA9FE6453697A', 'hex')

function rc4(key: Buffer, data: Buffer): Buffer {
  const s = Array.from({ length: 256 }, (_, i) => i)
  let j = 0
  for (let i = 0; i < 256; i++) {
    j = (j + s[i] + key[i % key.length]) & 255
    ;[s[i], s[j]] = [s[j], s[i]]
  }
  const o = Buffer.alloc(data.length)
  let a = 0, b = 0
  for (let k = 0; k < data.length; k++) {
    a = (a + 1) & 255
    b = (b + s[a]) & 255
    ;[s[a], s[b]] = [s[b], s[a]]
    o[k] = data[k] ^ s[(s[a] + s[b]) & 255]
  }
  return o
}
const aesCbc = (alg: string, key: Buffer, iv: Buffer, data: Buffer, pad: boolean): Buffer => {
  const c = createCipheriv(alg, key, iv)
  c.setAutoPadding(pad)
  return Buffer.concat([c.update(data), c.final()])
}

/** PDF 2.0 algorithm 2.B (R6). */
function hash2B(pw: Buffer, salt: Buffer): Buffer {
  let k = createHash('sha256').update(Buffer.concat([pw, salt])).digest()
  for (let i = 0; ; i++) {
    const k1 = Buffer.concat(Array.from({ length: 64 }, () => Buffer.concat([pw, k])))
    const e = aesCbc('aes-128-cbc', k.subarray(0, 16), k.subarray(16, 32), k1, false)
    let sum = 0
    for (let n = 0; n < 16; n++) sum += e[n]
    const alg = ['sha256', 'sha384', 'sha512'][sum % 3]
    k = createHash(alg).update(e).digest()
    if (i >= 63 && e[e.length - 1] <= i - 31) break
  }
  return k.subarray(0, 32)
}

type EncKind = 'rc4-128' | 'aes-128' | 'aes-256'
interface Enc {
  kind: EncKind
  userPassword?: string
  dict: string
  id: Buffer
  encrypt(data: Buffer, num: number): Buffer
}
function makeEnc(kind: EncKind, userPassword = ''): Enc {
  const id = randomBytes(16)
  const pw = Buffer.from(userPassword, 'latin1')
  if (kind === 'aes-256') {
    const fileKey = randomBytes(32)
    const vs = randomBytes(8), ks = randomBytes(8)
    const u = Buffer.concat([hash2B(pw, vs), vs, ks])
    const ue = aesCbc('aes-256-cbc', hash2B(pw, ks), Buffer.alloc(16), fileKey, false)
    const dict = `<< /Filter /Standard /V 5 /R 6 /Length 256 /CF << /StdCF << /CFM /AESV3 /AuthEvent /DocOpen /Length 32 >> >> /StmF /StdCF /StrF /StdCF /O ${hex(randomBytes(48))} /OE ${hex(randomBytes(32))} /U ${hex(u)} /UE ${hex(ue)} /Perms ${hex(randomBytes(16))} /P -4 >>`
    return { kind, userPassword, dict, id, encrypt: (d) => { const iv = randomBytes(16); return Buffer.concat([iv, aesCbc('aes-256-cbc', fileKey, iv, d, true)]) } }
  }
  const o = randomBytes(32)
  const p = Buffer.from([0xfc, 0xff, 0xff, 0xff])
  let key = md5(Buffer.concat([pw, PAD]).subarray(0, 32), o, p, id)
  for (let i = 0; i < 50; i++) key = md5(key.subarray(0, 16))
  key = key.subarray(0, 16)
  let x = rc4(key, md5(PAD, id))
  for (let i = 1; i <= 19; i++) x = rc4(Buffer.from(key.map((b) => b ^ i)), x)
  const u = Buffer.concat([x, randomBytes(16)])
  const aes = kind === 'aes-128'
  const dict = aes
    ? `<< /Filter /Standard /V 4 /R 4 /Length 128 /CF << /StdCF << /CFM /AESV2 /AuthEvent /DocOpen /Length 16 >> >> /StmF /StdCF /StrF /StdCF /O ${hex(o)} /U ${hex(u)} /P -4 >>`
    : `<< /Filter /Standard /V 2 /R 3 /Length 128 /O ${hex(o)} /U ${hex(u)} /P -4 >>`
  const objKey = (num: number): Buffer =>
    md5(key, Buffer.from([num & 255, (num >> 8) & 255, (num >> 16) & 255, 0, 0]), ...(aes ? [Buffer.from('sAlT')] : [])).subarray(0, 16)
  return {
    kind, userPassword, dict, id,
    encrypt: (d, num) => {
      if (!aes) return rc4(objKey(num), d)
      const iv = randomBytes(16)
      return Buffer.concat([iv, aesCbc('aes-128-cbc', objKey(num), iv, d, true)])
    },
  }
}

/** Incremental byte writer that knows its own offsets. */
class Out {
  parts: Buffer[] = []
  len = 0
  put(s: string | Buffer): void {
    const b = typeof s === 'string' ? Buffer.from(s, 'latin1') : s
    this.parts.push(b)
    this.len += b.length
  }
  done(): Buffer { return Buffer.concat(this.parts) }
}

interface PageSpec { box: string; extra?: string }
const letter: PageSpec = { box: '/MediaBox [0 0 612 792]' }

/** One stream object. `enc` encrypts it; `cmp` Flate-compresses it first. */
function streamObj(o: Out, num: number, dict: string, data: Buffer, enc?: Enc, cmp = true): number {
  const off = o.len
  const body = cmp ? deflateSync(data) : data
  const stored = enc ? enc.encrypt(body, num) : body
  o.put(`${num} 0 obj\n<< ${dict}${cmp ? ' /Filter /FlateDecode' : ''} /Length ${stored.length} >>\nstream\n`)
  o.put(stored)
  o.put('\nendstream\nendobj\n')
  return off
}

function objStm(objs: { num: number; body: string }[]): { dict: string; data: Buffer } {
  let header = ''
  let body = ''
  for (const ob of objs) {
    header += `${ob.num} ${body.length} `
    body += `${ob.body}\n`
  }
  return { dict: `/Type /ObjStm /N ${objs.length} /First ${header.length}`, data: Buffer.from(header + body, 'latin1') }
}

type XEntry = [num: number, type: 1 | 2, a: number, b: number]
function xrefStream(o: Out, num: number, entries: XEntry[], trailer: string, predictor: boolean): number {
  const off = o.len
  entries.push([num, 1, off, 0])
  entries.sort((x, y) => x[0] - y[0])
  const W = [1, 4, 2]
  const rowLen = 7
  const rows = entries.map(([, t, a, b]) => Buffer.from([t, (a >>> 24) & 255, (a >>> 16) & 255, (a >>> 8) & 255, a & 255, (b >> 8) & 255, b & 255]))
  let raw: Buffer
  let parms = ''
  if (predictor) {
    const filtered: Buffer[] = []
    let prev = Buffer.alloc(rowLen)
    for (const r of rows) {
      filtered.push(Buffer.concat([Buffer.from([2]), Buffer.from(r.map((v, i) => (v - prev[i]) & 255))]))
      prev = r
    }
    raw = Buffer.concat(filtered)
    parms = `/DecodeParms << /Predictor 12 /Columns ${rowLen} >>`
  } else raw = Buffer.concat(rows)
  const index = entries.map(([n]) => `${n} 1`).join(' ')
  const size = Math.max(...entries.map((e) => e[0])) + 1
  const data = deflateSync(raw)
  o.put(`${num} 0 obj\n<< /Type /XRef /W [${W.join(' ')}] /Index [${index}] /Size ${size} ${parms} ${trailer} /Filter /FlateDecode /Length ${data.length} >>\nstream\n`)
  o.put(data)
  o.put('\nendstream\nendobj\n')
  return off
}

interface Opts { pages: PageSpec[]; predictor?: boolean; enc?: Enc; version?: string }

/** xref-stream + object-stream file: the page tree lives only inside compressed data. */
function modern({ pages, predictor = false, enc, version = '1.5' }: Opts): { bytes: Buffer; o: Out; n: number; xrefOff: number; objstmNum: number } {
  const o = new Out()
  o.put(`%PDF-${version}\n%\xe2\xe3\xcf\xd3\n`)
  const n = 2 + pages.length
  const objs = [
    { num: 1, body: '<< /Type /Catalog /Pages 2 0 R >>' },
    { num: 2, body: `<< /Type /Pages /Kids [${pages.map((_, i) => `${3 + i} 0 R`).join(' ')}] /Count ${pages.length} >>` },
    ...pages.map((p, i) => ({ num: 3 + i, body: `<< /Type /Page /Parent 2 0 R ${p.box} ${p.extra ?? ''} >>` })),
  ]
  const objstmNum = n + 1, xrefNum = n + 2, encNum = n + 3
  const entries: XEntry[] = objs.map((ob, i) => [ob.num, 2, objstmNum, i])
  const os = objStm(objs)
  entries.push([objstmNum, 1, streamObj(o, objstmNum, os.dict, os.data, enc), 0])
  let trailer = `/Root 1 0 R`
  if (enc) {
    const off = o.len
    o.put(`${encNum} 0 obj\n${enc.dict}\nendobj\n`)
    entries.push([encNum, 1, off, 0])
    trailer += ` /Encrypt ${encNum} 0 R /ID [${hex(enc.id)} ${hex(enc.id)}]`
  }
  const xrefOff = xrefStream(o, xrefNum, entries, trailer, predictor)
  o.put(`startxref\n${xrefOff}\n%%EOF\n`)
  return { bytes: o.done(), o, n, xrefOff, objstmNum }
}

const w = (name: string, b: Buffer | string): void => writeFileSync(join(out, name), b)
const many = (n: number, p: PageSpec = letter): PageSpec[] => Array.from({ length: n }, () => p)
const big: PageSpec = { box: '/MediaBox [0 0 1700 2400]' }

w('xrefstm-objstm-3p.pdf', modern({ pages: many(3) }).bytes)
w('xrefstm-objstm-predictor-50p.pdf', modern({ pages: many(50), predictor: true }).bytes)
w('xrefstm-objstm-oversize-in-objstm.pdf', modern({ pages: [letter, big, letter] }).bytes)
w('xrefstm-objstm-rotate-userunit.pdf', modern({ pages: [{ box: '/MediaBox [0 0 1224 792]', extra: '/Rotate 90' }, { box: '/MediaBox [0 0 612 792]', extra: '/UserUnit 2' }] }).bytes)

// Incremental update over an xref-stream file: a later section replaces /Pages, dropping a page.
{
  const base = modern({ pages: many(4) })
  const o = new Out()
  o.put(base.bytes)
  const objstm2 = base.n + 4, xref2 = base.n + 5
  const os = objStm([{ num: 2, body: '<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>' }])
  const entries: XEntry[] = [[2, 2, objstm2, 0]]
  entries.push([objstm2, 1, streamObj(o, objstm2, os.dict, os.data), 0])
  const off = xrefStream(o, xref2, entries, `/Root 1 0 R /Prev ${base.xrefOff}`, false)
  o.put(`startxref\n${off}\n%%EOF\n`)
  w('xrefstm-incremental-pages-removed.pdf', o.done())
}
// Incremental update that adds an oversize page.
{
  const base = modern({ pages: many(1) })
  const o = new Out()
  o.put(base.bytes)
  const newPage = base.n + 4, objstm2 = base.n + 5, xref2 = base.n + 6
  const os = objStm([
    { num: 2, body: `<< /Type /Pages /Kids [3 0 R ${newPage} 0 R] /Count 2 >>` },
    { num: newPage, body: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1700 2400] >>' },
  ])
  const entries: XEntry[] = [[2, 2, objstm2, 0], [newPage, 2, objstm2, 1]]
  entries.push([objstm2, 1, streamObj(o, objstm2, os.dict, os.data), 0])
  const off = xrefStream(o, xref2, entries, `/Root 1 0 R /Prev ${base.xrefOff}`, false)
  o.put(`startxref\n${off}\n%%EOF\n`)
  w('xrefstm-incremental-oversize-added.pdf', o.done())
}
// Hybrid-reference file: classic table + /XRefStm for the compressed objects.
{
  const o = new Out()
  o.put('%PDF-1.5\n')
  const catOff = o.len
  o.put('1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n')
  const os = objStm([
    { num: 2, body: '<< /Type /Pages /Kids [3 0 R] /Count 1 >>' },
    { num: 3, body: '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1700 2400] >>' },
  ])
  const osOff = streamObj(o, 4, os.dict, os.data)
  const xsOff = xrefStream(o, 5, [[2, 2, 4, 0], [3, 2, 4, 1], [4, 1, osOff, 0]], '/Root 1 0 R', false)
  const xr = o.len
  const f = (v: number): string => `${String(v).padStart(10, '0')} 00000 n \n`
  o.put(`xref\n0 6\n0000000000 65535 f \n${f(catOff)}0000000000 00000 f \n0000000000 00000 f \n${f(osOff)}${f(xsOff)}`)
  o.put(`trailer\n<< /Size 6 /Root 1 0 R /XRefStm ${xsOff} >>\nstartxref\n${xr}\n%%EOF\n`)
  w('hybrid-reference-oversize.pdf', o.done())
}
// Encrypted: the page tree is inside an encrypted object stream, so it cannot be read without decrypting.
for (const kind of ['rc4-128', 'aes-128', 'aes-256'] as const) {
  const version = kind === 'aes-256' ? '1.7' : '1.6'
  w(`enc-${kind}-empty-pw-3p.pdf`, modern({ pages: many(3), enc: makeEnc(kind), version }).bytes)
  w(`enc-${kind}-empty-pw-oversize.pdf`, modern({ pages: [letter, big], enc: makeEnc(kind), version }).bytes)
  w(`enc-${kind}-user-password-set.pdf`, modern({ pages: many(3), enc: makeEnc(kind, 'secret'), version }).bytes)
}
console.log('modern fixtures written')
