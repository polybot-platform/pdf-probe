#!/usr/bin/env bash
# Tests tools/publish-npm.sh against a stand-in `npm`, so that it runs offline and publishes nothing: the order
# (platform packages first, pdf-probe last), skipping what is already on npm, refusing a mismatch, stopping at the
# first failure, and the form of the path given to `npm publish`.   Run: tools/test-publish-npm.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/tarballs"

# A stand-in for npm. `view <spec> version` succeeds when the spec is listed in $PUBLISHED and says E404 otherwise;
# `publish <file> ...` records the file, and fails for a package named in $FAIL_ON or for a path npm would not
# read as a file (npm reads `dir/file.tgz` as a GitHub repository).
cat > "$tmp/bin/npm" <<'STUB'
#!/usr/bin/env bash
case "$1" in
  view) grep -qxF "$2" "$PUBLISHED" && { echo "0"; exit 0; }; echo "npm error code E404" >&2; exit 1 ;;
  publish)
    [[ "$2" == /* || "$2" == ./* ]] || { echo "npm error: '$2' would be read as a GitHub repository" >&2; exit 1; }
    [ -f "$2" ] || { echo "npm error: no such file $2" >&2; exit 1; }
    [[ -n "${FAIL_ON:-}" && "$2" == *"$FAIL_ON"* ]] && { echo "npm error: refused $2" >&2; exit 1; }
    basename "$2" >> "$LOG" ;;
  *) echo "stub npm: unexpected $*" >&2; exit 2 ;;
esac
STUB
chmod +x "$tmp/bin/npm"
export PATH="$tmp/bin:$PATH" PUBLISHED="$tmp/published" LOG="$tmp/log"

# A tarball holding only a package.json, which is all the script reads.
make_tarball() { # name version [optionalDependencies json]
  local d="$tmp/pkg-$1-$2/package"
  mkdir -p "$d"
  jq -n --arg n "$1" --arg v "$2" --argjson o "${3:-null}" '{name: $n, version: $v} + (if $o then {optionalDependencies: $o} else {} end)' > "$d/package.json"
  tar -czf "$tmp/tarballs/$1-$2.tgz" -C "$tmp/pkg-$1-$2" package
}
make_all() {
  rm -f "$tmp"/tarballs/*.tgz
  make_tarball pdf-probe-linux-x64 1.2.3
  make_tarball pdf-probe-darwin-arm64 1.2.3
  make_tarball pdf-probe 1.2.3 '{"pdf-probe-linux-x64":"1.2.3","pdf-probe-darwin-arm64":"1.2.3"}'
}
fresh() { : > "$PUBLISHED"; : > "$LOG"; unset FAIL_ON; }
check() { # description, expected log
  if [ "$(cat "$LOG")" != "$2" ]; then
    echo "FAIL: $1" >&2; echo "expected:" >&2; echo "$2" >&2; echo "got:" >&2; cat "$LOG" >&2; exit 1
  fi
  echo "ok: $1"
}
cd "$tmp" # a relative directory, as the workflow gives it

make_all; fresh
"$root/tools/publish-npm.sh" tarballs >/dev/null
check "publishes platform packages first and pdf-probe last, from a relative directory" \
"pdf-probe-darwin-arm64-1.2.3.tgz
pdf-probe-linux-x64-1.2.3.tgz
pdf-probe-1.2.3.tgz"

fresh; echo "pdf-probe-darwin-arm64@1.2.3" > "$PUBLISHED"
"$root/tools/publish-npm.sh" tarballs >/dev/null
check "skips a version that is already on npm" \
"pdf-probe-linux-x64-1.2.3.tgz
pdf-probe-1.2.3.tgz"

fresh; export FAIL_ON=linux-x64
if "$root/tools/publish-npm.sh" tarballs >/dev/null 2>&1; then echo "FAIL: a failed publish must fail the script" >&2; exit 1; fi
check "stops at the first failure, before publishing pdf-probe" "pdf-probe-darwin-arm64-1.2.3.tgz"

fresh; rm "$tmp/tarballs/pdf-probe-darwin-arm64-1.2.3.tgz"
if "$root/tools/publish-npm.sh" tarballs >/dev/null 2>&1; then echo "FAIL: a missing platform tarball must refuse" >&2; exit 1; fi
check "refuses when pdf-probe's optionalDependencies and the platform tarballs differ, and publishes nothing" ""

make_all; fresh; make_tarball pdf-probe-linux-x64 1.2.4
rm "$tmp/tarballs/pdf-probe-linux-x64-1.2.3.tgz"
if "$root/tools/publish-npm.sh" tarballs >/dev/null 2>&1; then echo "FAIL: a version mismatch must refuse" >&2; exit 1; fi
check "refuses a platform package at another version than pdf-probe" ""
echo "all passed"
