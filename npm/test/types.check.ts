// Type-checked with `tsc --noEmit --strict` against dist/index.d.ts: it must accept the documented use and
// reject the documented misuse.
import { createProbe, type CheckOptions, type CreateOptions, type Probe, type ProbeResult } from '../dist/index.js'

const options: CreateOptions = { engine: 'auto', nativeLibrary: '/x.so', wasm: new Uint8Array(0), recycleAboveBytes: 1 }
const probe: Probe = await createProbe(options)
const engine: 'native' | 'wasm' = probe.engine
const checkOptions: CheckOptions = { maxPages: 100, maxInputBytes: 1 << 20, maxWork: 5_000_000 }
const result: ProbeResult = probe.check(new Uint8Array(0), checkOptions)
switch (result.status) {
  case 'ok': {
    const [width, height]: [number, number] = result.sizes[0]
    const pages: number = result.pages
    const version: string | null = result.version
    const encrypted: boolean = result.encrypted
    const repaired: boolean = result.repaired
    const cropped: boolean = result.cropped
    void [width, height, pages, version, encrypted, repaired, cropped]
    break
  }
  case 'unparseable':
  case 'panic':
    void result.reason.length
    break
  case 'tooManyPages':
    void result.pages
    break
  case 'limitExceeded': {
    const limit: 'inputBytes' | 'work' | 'objects' | 'xrefSections' | 'decompressed' = result.limit
    void limit
    break
  }
  case 'empty':
    break
}
probe.close()
void engine

// @ts-expect-error an engine that does not exist
void createProbe({ engine: 'fast' })
// @ts-expect-error pages is only on some results
void result.pages
// @ts-expect-error an unknown limit name
void (result.status === 'limitExceeded' && result.limit === 'time')
// @ts-expect-error a string is not a PDF
void probe.check('text')
