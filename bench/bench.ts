// Compares the service's page check (checkReadablePdf: unpdf) with pdf-probe on one directory of PDFs.
//
//   bun run bench.ts --all --dir <pdf-dir> [--limit 300] [--rounds 3] [--idle 15] [--max-pages 50]
//                    [--concurrency 1,8] [--native-lib /path/to/libpdf_probe_ffi.so]
//
// `--all` runs every engine in a FRESH process (memory is per process), once for each concurrency, and prints
// a table. One engine alone: `--engine unpdf|wasm|native`, which prints a JSON line.
//
// The unpdf check below is a copy of checkReadablePdf (backend/src/services/attachments/attachment-reading.service.ts),
// the page limits included. The pdf-probe checks apply the same rules to the sizes it returns.
//
// What it records, per engine: time to load the reader, the wall time of each round over the whole set (round 1
// is cold: JIT and module load), per-file latency (p50, p95, p99, max) and the longest event-loop stall in the
// last round, memory (RSS) at start, after load, after the first and the last round, and after the idle wait,
// plus the process's peak RSS. Memory is NOT forced down with gc after the baseline: an idle server does not
// get to run gc on demand, and Bun discards JIT code on its own, which is exactly the effect to see.
// Numbers from a laptop are an ordering, not a result: run it in the target container.
import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'

const args = process.argv.slice(2)
const opt = (name: string, fallback?: string) => {
  const i = args.indexOf(`--${name}`)
  return i >= 0 ? args[i + 1] : fallback
}
const MAX_INPUT = 10 * 1024 * 1024 // the service's per-file cap
const dir = opt('dir')
const limit = Number(opt('limit', '300'))
const rounds = Number(opt('rounds', '3'))
const idleSeconds = Number(opt('idle', '15'))
const maxPages = Number(opt('max-pages', '50')) // BUSINESS tier; STARTUP is 20
const nativeLib = opt('native-lib')

const MAX_READ_PAGE_INCHES = { long: 17.5, short: 12 } as const
const isOversizedPage = (width: number, height: number) =>
  Math.max(width, height) / 72 > MAX_READ_PAGE_INCHES.long || Math.min(width, height) / 72 > MAX_READ_PAGE_INCHES.short

type Verdict = 'pages' | 'pageSize' | 'unparseable' | null
const mb = () => process.memoryUsage().rss / 1048576
const r1 = (x: number) => Math.round(x * 10) / 10
const peakRssMB = () => {
  try {
    const m = /VmHWM:\s+(\d+) kB/.exec(readFileSync('/proc/self/status', 'utf8'))
    return m ? r1(Number(m[1]) / 1024) : null
  } catch {
    return null
  }
}
const read = (path: string) => {
  try {
    return readFileSync(path, 'utf8').trim()
  } catch {
    return null
  }
}

function listPdfs(): string[] {
  if (!dir) throw new Error('--dir <directory of PDFs> is required')
  const files = readdirSync(dir)
    .filter((f) => f.toLowerCase().endsWith('.pdf'))
    .sort()
    .map((f) => join(dir, f))
    .filter((f) => statSync(f).size <= MAX_INPUT)
  return files.slice(0, limit)
}

async function runChild(engine: string, concurrency: number) {
  const files = listPdfs()
  const inputs = files.map((f) => new Uint8Array(readFileSync(f)))
  Bun.gc(true) // the baseline only; nothing after this is forced
  const rssStart = mb()

  let check: (bytes: Uint8Array) => Promise<Verdict> | Verdict
  const t0 = performance.now()
  if (engine === 'unpdf') {
    const { getDocumentProxy } = await import('unpdf')
    check = async (bytes) => {
      try {
        const document = await getDocumentProxy(bytes.slice(), { verbosity: 0 })
        try {
          if (document.numPages > maxPages) return 'pages'
          for (let number = 1; number <= document.numPages; number++) {
            const page = await document.getPage(number)
            const { width, height } = page.getViewport({ scale: 1 })
            if (isOversizedPage(width, height)) return 'pageSize'
          }
          return null
        } finally {
          await document.cleanup()
        }
      } catch {
        return 'unparseable'
      }
    }
    await import('unpdf').then((m) => m.getResolvedPDFJS())
  } else {
    const { createProbe } = await import('pdf-probe')
    const probe = await createProbe({ engine: engine as 'wasm' | 'native', ...(nativeLib ? { nativeLibrary: nativeLib } : {}) })
    check = (bytes) => {
      const result = probe.check(bytes, { maxPages })
      if (result.status === 'tooManyPages') return 'pages'
      if (result.status !== 'ok') return 'unparseable'
      return result.sizes.some(([w, h]) => isOversizedPage(w, h)) ? 'pageSize' : null
    }
  }
  const loadMs = performance.now() - t0
  const rssLoaded = mb()

  // Event-loop stalls: a 1 ms timer; the longest gap beyond 1 ms in the round being watched.
  let maxStall = 0
  let last = performance.now()
  const timer = setInterval(() => {
    const now = performance.now()
    maxStall = Math.max(maxStall, now - last - 1)
    last = now
  }, 1)

  const verdicts: Verdict[] = new Array(inputs.length)
  const roundMs: number[] = []
  let latencies: number[] = []
  let stallLastRound = 0
  let rssAfterFirst = 0
  for (let round = 0; round < rounds; round++) {
    maxStall = 0
    last = performance.now()
    latencies = []
    let next = 0
    const start = performance.now()
    const worker = async () => {
      for (;;) {
        const i = next++
        if (i >= inputs.length) return
        const t = performance.now()
        verdicts[i] = await check(inputs[i])
        latencies.push(performance.now() - t)
        // A server handles each file in its own turn of the event loop, so timers (and other requests) get to run
        // between files. Without this a synchronous engine would hold the loop for the whole round and the stall
        // timer could never fire. The same yield is applied to every engine.
        await new Promise<void>((resolve) => setImmediate(resolve))
      }
    }
    await Promise.all(Array.from({ length: concurrency }, worker))
    roundMs.push(performance.now() - start)
    stallLastRound = maxStall
    if (round === 0) rssAfterFirst = mb()
  }
  clearInterval(timer)
  const rssAfterLast = mb()
  await new Promise((resolve) => setTimeout(resolve, idleSeconds * 1000)) // idle: no gc, no work
  const rssIdle = mb()

  latencies.sort((a, b) => a - b)
  const q = (p: number) => latencies[Math.min(latencies.length - 1, Math.floor(p * latencies.length))] ?? 0
  const out = {
    engine,
    concurrency,
    files: inputs.length,
    inputMB: r1(inputs.reduce((s, b) => s + b.length, 0) / 1048576),
    loadMs: r1(loadMs),
    roundMs: roundMs.map(r1),
    latencyMs: { p50: r1(q(0.5)), p95: r1(q(0.95)), p99: r1(q(0.99)), max: r1(latencies[latencies.length - 1] ?? 0) },
    maxStallMs: r1(stallLastRound),
    rssMB: { start: r1(rssStart), loaded: r1(rssLoaded), afterFirstRound: r1(rssAfterFirst), afterLastRound: r1(rssAfterLast), idle: r1(rssIdle), peak: peakRssMB() },
    verdicts: Object.fromEntries(files.map((f, i) => [f.split('/').pop(), verdicts[i]])),
  }
  console.log(JSON.stringify(out))
  process.exit(0)
}

async function runAll() {
  const concurrencies = (opt('concurrency', '1,8') ?? '1').split(',').map(Number)
  const engines = ['unpdf', 'wasm', ...(nativeLib ? ['native'] : [])]
  const files = listPdfs()
  const env = {
    bun: Bun.version,
    cpus: navigator.hardwareConcurrency,
    governor: read('/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor'),
    cgroupCpuMax: read('/sys/fs/cgroup/cpu.max'),
    cgroupMemoryMax: read('/sys/fs/cgroup/memory.max'),
  }
  console.log(`# ${files.length} files (${r1(files.reduce((s, f) => s + statSync(f).size, 0) / 1048576)} MB), maxPages ${maxPages}, ${rounds} rounds, idle ${idleSeconds}s`)
  console.log(`# ${JSON.stringify(env)}`)
  const results: any[] = []
  for (const concurrency of concurrencies) {
    for (const engine of engines) {
      const childArgs = ['run', import.meta.path, '--engine', engine, '--concurrency', String(concurrency), '--dir', dir!, '--limit', String(limit), '--rounds', String(rounds), '--idle', String(idleSeconds), '--max-pages', String(maxPages), ...(nativeLib ? ['--native-lib', nativeLib] : [])]
      const p = Bun.spawnSync([process.execPath, ...childArgs], { stdout: 'pipe', stderr: 'pipe' })
      const text = p.stdout.toString().trim().split('\n').pop() ?? ''
      try {
        results.push(JSON.parse(text))
      } catch {
        console.error(`${engine} (concurrency ${concurrency}) failed: ${p.stderr.toString().slice(0, 500)}`)
      }
    }
  }
  const row = (r: any) =>
    `| ${r.engine} | ${r.concurrency} | ${r.loadMs} | ${r.roundMs[0]} | ${r.roundMs[r.roundMs.length - 1]} | ${r.latencyMs.p50} / ${r.latencyMs.p95} / ${r.latencyMs.p99} / ${r.latencyMs.max} | ${r.maxStallMs} | ${r1(r.rssMB.loaded - r.rssMB.start)} | ${r1(r.rssMB.afterLastRound - r.rssMB.start)} | ${r1(r.rssMB.idle - r.rssMB.start)} | ${r.rssMB.peak ?? '?'} |`
  console.log('\n| engine | in flight | load ms | round 1 ms (cold) | last round ms | per file p50 / p95 / p99 / max ms | longest stall ms | +MB loaded | +MB after burst | +MB at idle | peak RSS MB |')
  console.log('|---|---|---|---|---|---|---|---|---|---|---|')
  for (const r of results) console.log(row(r))
  // Do the engines agree? Same files, same rules.
  const base = results.find((r) => r.engine === 'unpdf')
  for (const r of results.filter((x) => x.engine !== 'unpdf' && x.concurrency === concurrencies[0])) {
    if (!base) break
    const differ = Object.keys(base.verdicts).filter((f) => base.verdicts[f] !== r.verdicts[f])
    console.log(`\n${r.engine} gives the same verdict as unpdf on ${files.length - differ.length} of ${files.length} files`)
    for (const f of differ.slice(0, 15)) console.log(`  ${f}: unpdf ${base.verdicts[f]}, ${r.engine} ${r.verdicts[f]}`)
  }
  await Bun.write('bench-results.json', JSON.stringify({ env, results }, null, 2))
  console.log('\nFull results (with every verdict): bench-results.json')
}

const engine = opt('engine')
if (engine) await runChild(engine, Number(opt('concurrency', '1')))
else if (args.includes('--all')) await runAll()
else console.log('usage: bun run bench.ts --all --dir <pdf-dir> [options] (see the top of this file)')
