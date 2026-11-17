// The platforms the native library is built for, and how a machine finds its own. Each one ships as its own npm
// package (`pdf-probe-<key>`), listed under `optionalDependencies` of `pdf-probe`, so an install downloads one
// library, not all of them. This table is the one source for the loader, the package builder and the tests, and
// a test checks it against package.json and the CI matrix in .github/workflows/native.yml.

export interface Platform {
  /** `process.platform` */
  os: 'linux' | 'darwin' | 'win32'
  /** `process.arch` */
  cpu: 'x64' | 'arm64'
  /** The C library on Linux, which is what `libc` in package.json is for; absent elsewhere. */
  libc?: 'glibc' | 'musl'
  /** The file name Cargo gives the library on this platform. */
  file: string
}

// Windows (win32-x64, file pdf_probe_ffi.dll) is built and tested in CI but not shipped yet: npm's spam filter
// rejected the package name pdf-probe-win32-x64 (E403, November 2026). Until it ships, Windows uses WebAssembly.
// To add it back: an entry here, its optionalDependencies entry, `ship: true` on its native.yml row and its
// install row, and a first publish by hand (a package must exist before npm can trust a workflow for it).
export const PLATFORMS: Readonly<Record<string, Platform>> = {
  'linux-x64': { os: 'linux', cpu: 'x64', libc: 'glibc', file: 'libpdf_probe_ffi.so' },
  'linux-arm64': { os: 'linux', cpu: 'arm64', libc: 'glibc', file: 'libpdf_probe_ffi.so' },
  'linux-x64-musl': { os: 'linux', cpu: 'x64', libc: 'musl', file: 'libpdf_probe_ffi.so' },
  'darwin-arm64': { os: 'darwin', cpu: 'arm64', file: 'libpdf_probe_ffi.dylib' },
  'darwin-x64': { os: 'darwin', cpu: 'x64', file: 'libpdf_probe_ffi.dylib' },
}

/** The key of a machine: `<platform>-<arch>`, with `-musl` on Linux when the C library is musl. */
export function platformKey(platform: string, arch: string, musl: boolean): string {
  return `${platform}-${arch}${platform === 'linux' && musl ? '-musl' : ''}`
}

/** The npm package that holds the native library for a key. */
export const packageName = (key: string): string => `pdf-probe-${key}`

/**
 * Whether this Linux process runs on musl (Alpine, say) rather than glibc. Node and Bun both put the glibc
 * version in the diagnostic report, and leave it out on musl. Anywhere but Linux the answer is no.
 */
export function isMusl(): boolean {
  if (process.platform !== 'linux') return false
  const report = (process as { report?: { getReport?: () => unknown } }).report?.getReport?.() as
    | { header?: { glibcVersionRuntime?: string } }
    | undefined
  return report?.header !== undefined && !report.header.glibcVersionRuntime
}

/** The key of the machine this process runs on. */
export const currentPlatformKey = (): string => platformKey(process.platform, process.arch, isMusl())
