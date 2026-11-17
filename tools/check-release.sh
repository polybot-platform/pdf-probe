#!/usr/bin/env bash
# Checks that a release tag is safe to publish to crates.io and npm. Run by .github/workflows/release.yml, and by hand
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

# The three manifests move together: the FFI crate is built into the npm packages, which are published at the
# version of npm/package.json (and the platform packages are named by it).
ffi_version=$(cargo metadata --no-deps --format-version 1 --locked |
  jq -r '.packages[] | select(.name == "pdf-probe-ffi") | .version')
[ "$ffi_version" = "$version" ] || fail "ffi/Cargo.toml says version $ffi_version but Cargo.toml says $version"
npm_version=$(jq -r .version npm/package.json)
[ "$npm_version" = "$version" ] || fail "npm/package.json says version $npm_version but Cargo.toml says $version"
for dep in $(jq -r '.optionalDependencies | to_entries[] | "\(.key)@\(.value)"' npm/package.json); do
  [[ "$dep" == *"@$version" ]] || fail "npm/package.json lists the optional dependency $dep, not version $version"
done

# Anyone who can push a tag could tag a commit from a side branch. Only what is already on main is released.
git fetch --quiet origin main
git merge-base --is-ancestor HEAD origin/main || fail "$(git rev-parse --short HEAD) is not on main"

echo "check-release: $tag matches Cargo.toml, ffi/Cargo.toml and npm/package.json, and is on main"
