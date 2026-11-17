#!/usr/bin/env bash
# Checks that a release tag is safe to publish to crates.io. Run by .github/workflows/release.yml, and by hand
# before tagging: `tools/check-release.sh v0.5.0`. It needs a checkout of the commit the tag points to, with
# `origin/main` available, and `cargo` and `jq` on the PATH.
set -euo pipefail

fail() {
  echo "check-release: $*" >&2
  exit 1
}

tag="${1:?usage: tools/check-release.sh vMAJOR.MINOR.PATCH}"

# A plain version tag only: a stray tag such as `vnext` or `v1.0.0-test` must never publish.
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "'$tag' is not a plain version tag (vMAJOR.MINOR.PATCH)"

version=$(cargo metadata --no-deps --format-version 1 --locked |
  jq -r '.packages[] | select(.name == "pdf-probe") | .version')
[ "v$version" = "$tag" ] || fail "the tag is $tag but Cargo.toml says version $version"

# Anyone who can push a tag could tag a commit from a side branch. Only what is already on main is released.
git fetch --quiet origin main
git merge-base --is-ancestor HEAD origin/main || fail "$(git rev-parse --short HEAD) is not on main"

echo "check-release: $tag matches Cargo.toml and is on main"
