#!/usr/bin/env bash
# Coverage-guided fuzzing of `probe` with libFuzzer, on the STABLE toolchain: no nightly Rust and no cargo-fuzz.
# It does what cargo-fuzz does without a sanitizer: builds fuzz/ with sanitizer-coverage instrumentation
# (ordinary rustc flags), overflow checks and debug assertions, then runs the fuzzer in fork mode.
# libfuzzer-sys compiles libFuzzer itself, so a C++ compiler is needed (clang++ or g++), nothing else.
#
#   fuzz/run.sh [seconds] [workers]        (defaults: 300 seconds, 4 workers)
#
# Seeds come from fuzz/corpus/probe; if it is empty they are copied from corpus/out (`bun run corpus`).
# New inputs it finds are added to fuzz/corpus/probe. A crash, timeout or out-of-memory input is saved in
# fuzz/artifacts/. Both directories are git-ignored.
set -euo pipefail
cd "$(dirname "$0")"

seconds="${1:-300}"
workers="${2:-4}"
triple="$(rustc -vV | sed -n 's/^host: //p')"

# --target keeps these flags off the build scripts, which must not be instrumented.
RUSTFLAGS="-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 \
-Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table \
-Cllvm-args=-sanitizer-coverage-trace-compares --cfg fuzzing -Cdebuginfo=1 -Cdebug-assertions -Coverflow-checks" \
  cargo build --release --target "$triple"

mkdir -p corpus/probe artifacts
if [ -z "$(ls -A corpus/probe 2>/dev/null)" ]; then
  if ! ls ../corpus/out/*.pdf >/dev/null 2>&1; then
    echo "no seeds: run 'bun run corpus' in the repository root first" >&2
    exit 2
  fi
  cp ../corpus/out/*.pdf corpus/probe/
fi

exec "./target/$triple/release/probe" -fork="$workers" -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
  -max_len=200000 -rss_limit_mb=1024 -timeout=5 -max_total_time="$seconds" -artifact_prefix=artifacts/ corpus/probe
