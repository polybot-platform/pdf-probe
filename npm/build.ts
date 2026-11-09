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
const rawWasm = join(root, 'target/wasm32-unknown-unknown/wasm/pdf_probe_ffi.wasm')
const shippedWasm = join(dist, 'pdf_probe.wasm')

// Run the module through wasm-opt (binaryen) if it is installed: `cargo install wasm-opt --locked`. Measured:
// 12% smaller raw, 1.5% smaller gzipped, no change in Bun memory or speed. Without it the plain build ships.
// The feature flags are the ones rustc turns on for wasm32; wasm-opt refuses a module that uses a feature it
// was not told about, and then the plain build ships.
const wasmOpt = Bun.which('wasm-opt') ?? (existsSync(`${process.env.HOME}/.cargo/bin/wasm-opt`) ? `${process.env.HOME}/.cargo/bin/wasm-opt` : null)
let optimised = false
if (wasmOpt) {
  const features = ['bulk-memory', 'nontrapping-float-to-int', 'sign-ext', 'multivalue', 'reference-types', 'mutable-globals']
  const r = Bun.spawnSync([wasmOpt, '-Oz', ...features.map((f) => `--enable-${f}`), rawWasm, '-o', shippedWasm], { stderr: 'inherit' })
  optimised = r.exitCode === 0
  if (!optimised) console.warn('wasm-opt failed; shipping the unoptimised module')
}
if (!optimised) copyFileSync(rawWasm, shippedWasm)
console.log(`wasm: ${Bun.file(shippedWasm).size} bytes (${optimised ? 'wasm-opt -Oz' : 'not optimised: wasm-opt not found or failed'})`)

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
