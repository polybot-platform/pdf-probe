// Installs the packed npm packages into an empty project, the way a user would, and checks that `createProbe()`
// finds the native library with no configuration under Bun, and uses WebAssembly under Node.
// Run after `bun run npm:build` (it needs npm/dist and the native library built on this machine):
//   bun run tools/test-install.ts
import { mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { currentPlatformKey, packageName, PLATFORMS } from '../npm/src/platform.ts'

const root = join(import.meta.dir, '..')
const key = currentPlatformKey()
if (!(key in PLATFORMS)) throw new Error(`no native package is shipped for ${key}`)

function run(cmd: string[], cwd: string): string {
  const r = Bun.spawnSync(cmd, { cwd, stdout: 'pipe', stderr: 'pipe' })
  const out = `${r.stdout}${r.stderr}`
  if (r.exitCode !== 0) throw new Error(`${cmd.join(' ')} (in ${cwd}) exited ${r.exitCode}:\n${out}`)
  return out
}

const tmp = mkdtempSync(join(tmpdir(), 'pdf-probe-install-'))
try {
  const tarballs = join(tmp, 'tarballs')
  mkdirSync(tarballs)
  const platformDir = run(['bun', 'npm/native-packages.ts', '--local', join(tmp, 'native-packages')], root).trim()
  const pack = (dir: string) => join(tarballs, run(['npm', 'pack', '--json', '--pack-destination', tarballs], dir).match(/"filename": "([^"]+)"/)![1]!)
  const mainTarball = pack(join(root, 'npm'))
  const platformTarball = pack(platformDir)

  const project = join(tmp, 'project')
  mkdirSync(project)
  writeFileSync(join(project, 'package.json'), '{"name":"probe-install-test","private":true,"type":"module"}\n')
  // Both tarballs in one install: pdf-probe's optionalDependencies name the platform package at its own version,
  // which npm takes from the tarball given here, not from the registry.
  run(['npm', 'install', '--no-audit', '--no-fund', '--offline', mainTarball, platformTarball], project)
  const installed = readdirSync(join(project, 'node_modules')).sort()
  if (!installed.includes(packageName(key))) throw new Error(`${packageName(key)} was not installed: ${installed}`)

  const script = `
    import { createProbe } from 'pdf-probe'
    const probe = await createProbe()
    console.log(probe.engine)
  `
  writeFileSync(join(project, 'probe.mjs'), script)
  const underBun = run(['bun', 'probe.mjs'], project).trim()
  if (underBun !== 'native') throw new Error(`under Bun, createProbe() chose ${underBun}, expected native`)
  const underNode = run(['node', 'probe.mjs'], project).trim()
  if (underNode !== 'wasm') throw new Error(`under Node, createProbe() chose ${underNode}, expected wasm`)

  // Without the platform package (npm --omit=optional): no native library, so WebAssembly, and 'native' says why.
  rmSync(join(project, 'node_modules', packageName(key)), { recursive: true })
  const fallback = run(['bun', 'probe.mjs'], project).trim()
  if (fallback !== 'wasm') throw new Error(`without the platform package, createProbe() chose ${fallback}, expected wasm`)
  writeFileSync(join(project, 'native.mjs'), `
    import { createProbe } from 'pdf-probe'
    try { await createProbe({ engine: 'native' }); console.log('loaded') } catch (e) { console.log(e.message) }
  `)
  const message = run(['bun', 'native.mjs'], project).trim()
  if (!message.includes(packageName(key))) throw new Error(`the error does not name ${packageName(key)}: ${message}`)
  console.log(`${key}: native under Bun, wasm under Node, wasm without ${packageName(key)} (${message})`)
} finally {
  rmSync(tmp, { recursive: true, force: true })
}
