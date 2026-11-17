# Releasing

A release is a version bump merged to `main`, a tag, and an approval. The crate is published to crates.io by
GitHub Actions (`.github/workflows/release.yml`) with **trusted publishing**: crates.io trusts that workflow
and gives it a token that lives for one run. There is no API token to store, rotate or leak.

## Cutting a release

1. **Bump the version** in `Cargo.toml`, `ffi/Cargo.toml` and `npm/package.json` (they move together; in
   `npm/package.json` that includes every entry of `optionalDependencies`, which a test checks), and
   refresh the lockfiles: `cargo update -w` in the repository root and in `fuzz/`. Open a PR, get CI green,
   merge it.
2. **Tag the merge commit on `main`** and push the tag:
   ```sh
   git checkout main && git pull
   tools/check-release.sh vX.Y.Z   # optional: the same check the workflow runs
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
3. **Approve the run.** The *Release* workflow verifies the tag, runs the tests and a `cargo publish --dry-run`,
   then waits for a reviewer on the `release` environment. Approve it on the workflow run's page to publish.
4. **Write the GitHub release** for the tag: what is breaking, what is new, what is unchanged.
5. **Publish the npm packages** (not done by CI yet), the six platform packages first and `pdf-probe` last,
   because `pdf-probe` lists them as optional dependencies at its own version. Until they exist, an install
   quietly skips them and falls back to WebAssembly.
   ```sh
   # The libraries are the artifacts of the Native library workflow run on the tagged commit:
   gh run download <run-id> --dir libs --pattern 'pdf-probe-native-*'   # libs/pdf-probe-native-<key>/<file>
   for d in libs/pdf-probe-native-*; do mkdir -p libs-by-key && cp -r "$d" "libs-by-key/${d#libs/pdf-probe-native-}"; done
   bun npm/native-packages.ts libs-by-key                              # writes npm/native-packages/<key>/
   for p in npm/native-packages/*/; do (cd "$p" && npm publish); done
   bun run npm:build && (cd npm && npm publish)
   ```
   npm can take several minutes to show a new version.
6. **Check it:** the version on https://crates.io/crates/pdf-probe, the docs.rs build, and `cargo add pdf-probe`
   in an empty project.

Publishing is irreversible: a version can be yanked, never deleted or replaced. If the workflow fails after the
tag is pushed, fix the problem, delete the tag (`git push origin :refs/tags/vX.Y.Z`) and tag again, as long as
nothing reached crates.io.

## What the workflow checks

`tools/check-release.sh` refuses the run unless the tag is a plain `vMAJOR.MINOR.PATCH`, equals the version in
`Cargo.toml`, and points at a commit that is on `main`. Anyone who can push a tag could otherwise publish from
a side branch. The `publish` job is the only one with permission to ask crates.io for a token.

## One-time setup (already done once; here for the record)

- **crates.io:** the crate's *Settings → Trusted Publishing → Add* has repository owner `polybot-platform`,
  repository `pdf-probe`, workflow `release.yml` and environment `release`. Only an owner of the crate can
  change it.
- **GitHub:** the `release` environment (repository *Settings → Environments*) lists the required reviewers
  and only accepts deployments from `v*` tags.
- The very first publish of a crate needs an API token; every later one does not. If a token is ever needed
  again, make it expire within days and revoke it afterwards.
