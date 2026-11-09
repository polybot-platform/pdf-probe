// Builds the package: the wasm module (required), the native library for this machine (optional), the bundled
// JavaScript, the declarations, and the licences. Usage (from the repo root): bun run npm:build
import { copyFileSync, existsSync, mkdirSync, rmSync } from 'node:fs'
import { join } from 'node:path'

const here = import.meta.dir
const root = join(here, '..')
const dist = join(here, 'dist')

function cargo(args: string[], required: boolean) {
  const r = Bun.spawnSync(['cargo', ...args], { cwd: root, stdout: 'inherit', stderr: 'inherit' })
  if (r.exitCode !== 0) {
    if (required) throw new Error(`cargo ${args.join(' ')} failed`)
    console.warn(`(optional) cargo ${args.join(' ')} failed; continuing without it`)
    return false
  }
  return true
}

rmSync(dist, { recursive: true, force: true })
rmSync(join(here, 'native'), { recursive: true, force: true })
mkdirSync(dist, { recursive: true })

cargo(['build', '--profile', 'wasm', '--target', 'wasm32-unknown-unknown', '-p', 'pdf-probe-ffi'], true)
copyFileSync(join(root, 'target/wasm32-unknown-unknown/wasm/pdf_probe_ffi.wasm'), join(dist, 'pdf_probe.wasm'))

// The native library is built for this machine only. A release needs one per platform, from CI.
if (cargo(['build', '--release', '-p', 'pdf-probe-ffi'], false)) {
  const suffix = process.platform === 'darwin' ? 'dylib' : process.platform === 'win32' ? 'dll' : 'so'
  const file = `${process.platform === 'win32' ? '' : 'lib'}pdf_probe_ffi.${suffix}`
  const built = join(root, 'target/release', file)
  if (existsSync(built)) {
    const dir = join(here, 'native', `${process.platform}-${process.arch}`)
    mkdirSync(dir, { recursive: true })
    copyFileSync(built, join(dir, file))
  }
}

const build = await Bun.build({
  entrypoints: [join(here, 'src/index.ts')],
  outdir: dist,
  target: 'node',
  format: 'esm',
  external: ['bun:ffi'],
})
if (!build.success) {
  for (const log of build.logs) console.error(log)
  throw new Error('bundling failed')
}
copyFileSync(join(here, 'src/index.d.ts'), join(dist, 'index.d.ts'))
for (const f of ['LICENSE-MIT', 'LICENSE-APACHE']) copyFileSync(join(root, f), join(here, f))
console.log('built npm/dist')
