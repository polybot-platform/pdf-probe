// Builds the per-platform npm packages (`pdf-probe-<platform>-<arch>[-musl]`), each holding one native library.
//
//   bun npm/native-packages.ts <libraries-dir> [out-dir]   one package per <libraries-dir>/<key>/<library file>
//   bun npm/native-packages.ts --local [out-dir]           one package, for the library built on this machine
//
// <libraries-dir> is laid out like the artifacts of .github/workflows/native.yml, a directory per platform that
// is named <key> or pdf-probe-native-<key>. The packages get the version of npm/package.json and are written to
// out-dir (default npm/native-packages/).
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { currentPlatformKey, packageName, PLATFORMS } from './src/platform.ts'

const here = import.meta.dir
const root = join(here, '..')

/** Writes the package for one platform to `<outDir>/<key>/` and returns that directory. */
export function buildPlatformPackage(key: string, library: string, outDir: string): string {
  const platform = PLATFORMS[key]
  if (!platform) throw new Error(`unknown platform ${key} (known: ${Object.keys(PLATFORMS).join(', ')})`)
  if (!existsSync(library)) throw new Error(`${library} does not exist`)
  const main = JSON.parse(readFileSync(join(here, 'package.json'), 'utf8'))
  const dir = join(outDir, key)
  rmSync(dir, { recursive: true, force: true })
  mkdirSync(dir, { recursive: true })
  copyFileSync(library, join(dir, platform.file))
  for (const f of ['LICENSE-MIT', 'LICENSE-APACHE']) copyFileSync(join(root, f), join(dir, f))
  writeFileSync(
    join(dir, 'README.md'),
    `# ${packageName(key)}\n\nThe native library of [pdf-probe](https://www.npmjs.com/package/pdf-probe) for ${key}, ` +
      `loaded with \`bun:ffi\`. It is installed for you when you install \`pdf-probe\`: don't depend on it directly.\n`,
  )
  const manifest = {
    name: packageName(key),
    version: main.version,
    description: `The native library of pdf-probe for ${key}. Installed by pdf-probe; not meant to be used on its own.`,
    license: main.license,
    repository: main.repository,
    homepage: main.homepage,
    bugs: main.bugs,
    os: [platform.os],
    cpu: [platform.cpu],
    ...(platform.libc && { libc: [platform.libc] }),
    // Yarn Plug'n'Play keeps packages in zip files, and a shared library can't be loaded from inside one.
    preferUnplugged: true,
    files: [platform.file, 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE'],
  }
  writeFileSync(join(dir, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`)
  return dir
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  const local = args[0] === '--local'
  const source = local ? undefined : args[0]
  const outDir = args[1] ?? join(here, 'native-packages')
  if (!local && !source) {
    console.error('usage: bun npm/native-packages.ts <libraries-dir> [out-dir]  |  --local [out-dir]')
    process.exit(2)
  }
  if (local) {
    const key = currentPlatformKey()
    const platform = PLATFORMS[key]
    if (!platform) throw new Error(`no native build is shipped for ${key}`)
    console.log(buildPlatformPackage(key, join(root, 'target/release', platform.file), outDir))
  } else {
    const dirs = readdirSync(source!, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name)
    const byKey = new Map(dirs.map((d) => [d.replace(/^pdf-probe-native-/, ''), d]))
    const missing = Object.keys(PLATFORMS).filter((k) => !byKey.has(k))
    if (missing.length) throw new Error(`no library in ${source} for: ${missing.join(', ')}`)
    const extra = [...byKey.keys()].filter((k) => !(k in PLATFORMS))
    if (extra.length) throw new Error(`${source} has directories that are not platforms: ${extra.join(', ')}`)
    for (const [key, dir] of [...byKey].sort()) console.log(buildPlatformPackage(key, join(source!, dir, PLATFORMS[key]!.file), outDir))
  }
}
