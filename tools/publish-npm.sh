#!/usr/bin/env bash
# Publishes the npm tarballs that native.yml packed (the npm-tarballs artifact): the platform packages first and
# pdf-probe last, because pdf-probe depends on them. A version that is already on npm is skipped, so the run can
# be repeated after a failure part way. Run by .github/workflows/release.yml, which has the permission to publish.
#   tools/publish-npm.sh <tarballs-dir>      DRY_RUN=1 to run `npm publish --dry-run` instead
set -euo pipefail

fail() {
  echo "publish-npm: $*" >&2
  exit 1
}

dir="${1:?usage: tools/publish-npm.sh <tarballs-dir>}"
manifest() { tar -xzOf "$1" package/package.json; }

main=""
platforms=()
for f in "$dir"/*.tgz; do
  [ -e "$f" ] || fail "no .tgz files in $dir"
  if [ "$(manifest "$f" | jq -r .name)" = pdf-probe ]; then main="$f"; else platforms+=("$f"); fi
done
[ -n "$main" ] || fail "pdf-probe's own tarball is not in $dir"

# What pdf-probe depends on must be what is about to be published, at the version it names: otherwise an install
# would look for a package that does not exist.
version=$(manifest "$main" | jq -r .version)
wanted=$(manifest "$main" | jq -r '.optionalDependencies | to_entries[] | "\(.key)@\(.value)"' | sort)
have=$(for f in "${platforms[@]}"; do manifest "$f" | jq -r '"\(.name)@\(.version)"'; done | sort)
[ "$wanted" = "$have" ] || fail "pdf-probe's optionalDependencies and the platform tarballs differ:
$(diff <(echo "$wanted") <(echo "$have") || true)"
[ -n "$wanted" ] || fail "pdf-probe has no optionalDependencies"

# Exits 0 if name@version is on npm, 1 if it is not, and stops on any other error (a network failure must not
# look like "not published").
published() {
  local out
  if out=$(npm view "$1@$2" version 2>&1); then return 0; fi
  grep -q 'E404' <<<"$out" && return 1
  fail "could not ask npm about $1@$2: $out"
}

for f in "${platforms[@]}" "$main"; do
  name=$(manifest "$f" | jq -r .name)
  v=$(manifest "$f" | jq -r .version)
  [ "$v" = "$version" ] || fail "$name is $v but pdf-probe is $version"
  if published "$name" "$v"; then
    echo "publish-npm: $name@$v is already on npm, skipping"
  elif [ -n "${DRY_RUN:-}" ]; then
    npm publish "$f" --access public --provenance --dry-run
  else
    echo "publish-npm: publishing $name@$v"
    npm publish "$f" --access public --provenance
  fi
done
