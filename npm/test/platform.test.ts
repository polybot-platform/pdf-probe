// The platform table, the per-platform packages and the files that must agree with them. Run: bun test npm/test
import { expect, test } from 'bun:test'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { buildPlatformPackage } from '../native-packages.ts'
import { packageName, PLATFORMS, platformKey } from '../src/platform.ts'

const read = (path: string) => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8')
const main = JSON.parse(read('npm/package.json'))

test('platformKey adds -musl on Linux only', () => {
  expect(platformKey('linux', 'x64', false)).toBe('linux-x64')
  expect(platformKey('linux', 'x64', true)).toBe('linux-x64-musl')
  expect(platformKey('linux', 'arm64', false)).toBe('linux-arm64')
  expect(platformKey('darwin', 'arm64', false)).toBe('darwin-arm64')
  expect(platformKey('win32', 'x64', false)).toBe('win32-x64')
  // musl is a Linux idea: it can't change the key of another platform.
  expect(platformKey('darwin', 'x64', true)).toBe('darwin-x64')
})

test('every key is os-cpu[-musl] of its own entry', () => {
  for (const [key, p] of Object.entries(PLATFORMS)) {
    expect(key).toBe(platformKey(p.os, p.cpu, p.libc === 'musl'))
  }
})

test('pdf-probe depends on exactly the platform packages, at its own version', () => {
  const expected = Object.fromEntries(Object.keys(PLATFORMS).map((k) => [packageName(k), main.version]))
  expect(main.optionalDependencies).toEqual(expected)
})

test('the CI matrix ships exactly the platforms in the table, with the same library file names', () => {
  const rows = [...read('.github/workflows/native.yml').matchAll(/dir: (\S+?),\s+lib: (\S+?),.*?ship: (true|false)/g)]
  expect(rows.length).toBeGreaterThan(0)
  const shipped = rows.filter((r) => r[3] === 'true')
  expect(shipped.map((r) => r[1]).sort()).toEqual(Object.keys(PLATFORMS).sort())
  for (const [, dir, lib] of shipped) expect(lib).toBe(PLATFORMS[dir!]!.file)
})

test('a platform package holds the library and says which machines may install it', () => {
  const tmp = mkdtempSync(join(tmpdir(), 'pdf-probe-pkg-'))
  try {
    const lib = join(tmp, 'lib')
    writeFileSync(lib, 'not really a library')
    const dir = buildPlatformPackage('linux-x64-musl', lib, join(tmp, 'out'))
    const manifest = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8'))
    expect(manifest).toMatchObject({
      name: 'pdf-probe-linux-x64-musl',
      version: main.version,
      license: main.license,
      os: ['linux'],
      cpu: ['x64'],
      libc: ['musl'],
      files: ['libpdf_probe_ffi.so', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE'],
    })
    for (const f of manifest.files) expect(existsSync(join(dir, f))).toBe(true)
    // No `exports`, so the loader can resolve <name>/package.json to find the directory.
    expect(manifest.exports).toBeUndefined()

    const mac = JSON.parse(readFileSync(join(buildPlatformPackage('darwin-arm64', lib, join(tmp, 'out')), 'package.json'), 'utf8'))
    expect(mac.libc).toBeUndefined()
    expect(mac.files[0]).toBe('libpdf_probe_ffi.dylib')
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('a platform package is not built from nothing, or for a platform that is not shipped', () => {
  expect(() => buildPlatformPackage('linux-ppc64', '/nonexistent', tmpdir())).toThrow(/unknown platform/)
  expect(() => buildPlatformPackage('linux-x64', '/nonexistent/lib', tmpdir())).toThrow(/does not exist/)
})
