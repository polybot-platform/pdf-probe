// Fetches pdf.js's own test PDFs at a pinned commit into corpus/pdfjs-samples/ (git-ignored).
// They are NOT vendored in this repository: the files carry no per-file licence or provenance, and some came
// from third-party bug reports. Fetch them, run the tools over them, never redistribute them.
// Usage: bun run samples
import { existsSync } from 'node:fs'
const TAG = 'v6.4.299'
const COMMIT = 'd0991a0d53f3bb3a7307e3b83f279c1d117aafb3'
const dest = new URL('../corpus/pdfjs-samples/repo', import.meta.url).pathname
const git = (args: string[], cwd?: string) => {
  const p = Bun.spawnSync(['git', ...args], { cwd, stdout: 'pipe', stderr: 'pipe' })
  if (p.exitCode !== 0) throw new Error(`git ${args.join(' ')}: ${p.stderr.toString().trim()}`)
  return p.stdout.toString().trim()
}
if (!existsSync(dest)) {
  git(['clone', '--quiet', '--depth', '1', '--filter=blob:none', '--sparse', '--branch', TAG, 'https://github.com/mozilla/pdf.js', dest])
  git(['sparse-checkout', 'set', 'test/pdfs'], dest)
}
const head = git(['rev-parse', 'HEAD'], dest)
if (head !== COMMIT) throw new Error(`pdf.js samples are at ${head}, expected ${COMMIT} (${TAG}). Delete corpus/pdfjs-samples and retry.`)
console.log(`pdf.js ${TAG} (${COMMIT.slice(0, 7)}) test PDFs in corpus/pdfjs-samples/repo/test/pdfs`)
