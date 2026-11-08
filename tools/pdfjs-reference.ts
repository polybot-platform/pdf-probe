// The reference implementation: pdf.js, through unpdf. Returns the page count and
// every page's size, plus a verdict against an example size limit (17.5 x 12 in).
// The limit is an example policy for the test, not part of the library.
const MAX = { long: 17.5, short: 12 }
export async function checkCurrent(bytes: Uint8Array, maxPages: number) {
  try {
    const { getDocumentProxy } = await import('unpdf')
    const document = await getDocumentProxy(bytes.slice(), { verbosity: 0 })
    try {
      if (document.numPages > maxPages) return { verdict: 'pages', pages: document.numPages }
      const sizes: [number, number][] = []
      let verdict: string | null = null
      for (let n = 1; n <= document.numPages; n++) {
        const page = await document.getPage(n)
        const { width, height } = page.getViewport({ scale: 1 })
        sizes.push([Math.round(width * 1000) / 1000, Math.round(height * 1000) / 1000])
        const long = Math.max(width, height) / 72, short = Math.min(width, height) / 72
        if (!verdict && (long > MAX.long || short > MAX.short)) verdict = 'pageSize'
      }
      return { verdict, pages: document.numPages, sizes }
    } finally { await document.cleanup() }
  } catch (e) { return { verdict: 'unparseable', err: String((e as Error).message).slice(0, 80) } }
}
