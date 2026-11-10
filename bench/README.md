# Benchmark: checkReadablePdf (unpdf) against pdf-probe

Answers, on one machine and one set of PDFs: is pdf-probe faster, does it stall the event loop less, and does it
use less memory, for the check chatbot-agent runs on every attached PDF? It installs the two **published**
packages (`unpdf@2.1.0` as the service pins it, and `pdf-probe@0.1.0`) and runs a copy of `checkReadablePdf`
(page cap, then every page's size against 17.5 x 12 inches), so what it measures is what the service does.

Each engine runs in a fresh process, once per "in flight" setting (1 file at a time, then 8 at once, like a sync
checking a batch). Per engine it reports:

| Column | Meaning |
|---|---|
| load ms | Time to load the reader (import, WebAssembly compile) |
| round 1 ms (cold) / last round ms | Wall time over the whole set: the first pass pays for JIT warm-up and module loading, the last is warm |
| per file p50 / p95 / p99 / max | Latency of one check in the last round |
| longest stall ms | Longest time the event loop was blocked in the last round: what other requests would feel. Between files every engine yields to the event loop, as a server does between requests |
| +MB loaded / after burst / at idle | RSS above the process's own baseline, after the reader loaded, after the rounds, and after the idle wait. **No gc is forced after the baseline**, as in a real idle server |
| peak RSS MB | The process's high-water mark (absolute) |

It also compares verdicts file by file. Differences on pdf.js's test set should be only the intended ones (`Brotli-Prototype-FileA.pdf`,
`PDFBOX-4352-0.pdf`, `bug1250079.pdf`: pdf-probe refuses them on purpose, see the project README).

## Run it

```sh
bun install
bun run bench.ts --all --dir <directory of PDFs> --limit 300 --concurrency 1,8
# optional: --rounds 3 --idle 15 --max-pages 50 --native-lib /path/to/libpdf_probe_ffi.so
```

It writes `bench-results.json` (every number and every verdict) next to where you run it. The CPU governor and the
container's cgroup limits are printed in the header: on a laptop on `powersave`, timings are not meaningful.

**Use PDFs that are not tenant files.** pdf.js's own test PDFs work: from the repository root, `bun install && bun run samples` fetches 987 of them into
`corpus/pdfjs-samples/repo/test/pdfs` (not redistributable, never commit them). Real office-style documents (reports,
invoices, slide exports, scans) of your own make a better test of what a sync would see.

## In the Render staging container

The reason to run it there is the 0.5 CPU share and the 512 MB limit, which a workstation does not have. **Staging
only**: the benchmark runs in the same container as the service and shares its 512 MB, so it can push the service
into an out-of-memory restart.

1. Render dashboard, the staging backend service, **Shell**.
2. Get the code and the PDFs (this uses `git`; if it is missing, download the repository tarball with `curl` instead):
   ```sh
   git clone --depth 1 https://github.com/polybot-platform/pdf-probe /tmp/pdf-probe
   cd /tmp/pdf-probe && bun install && bun run samples
   cd bench && bun install
   ```
3. Run, and let the service sit idle meanwhile (no chat traffic), so the idle column means something:
   ```sh
   bun run bench.ts --all --dir ../corpus/pdfjs-samples/repo/test/pdfs --limit 300 --concurrency 1,8
   ```
4. Paste the table and the header lines into chatbot-agent #1781.
5. Delete `/tmp/pdf-probe` (it is on the container's disk, not the repository).

The native engine cannot be measured on Render's runtime: there is no Rust toolchain in `buildCommand`, and a
prebuilt `.so` is not in the npm package. Native would mean committing or downloading a binary for linux x86_64;
that cost belongs in the adoption decision.

The numbers in the table are for this process alone. The question for the service is the **difference** between the
`unpdf` rows and the `wasm` rows (the "+MB" columns), not the absolute figure.
