# Releasing

A release is a version bump merged to `main`, a tag, and an approval. GitHub Actions
(`.github/workflows/release.yml`) publishes the crate to crates.io and seven npm packages with **trusted
publishing**: crates.io and npm trust that workflow and give it a token that lives for one run. There is no API
token to store, rotate or leak, and the npm packages carry a provenance statement that names the workflow run
that built them.

The npm packages are `pdf-probe` (WebAssembly and the loader) and one package per platform that holds the native
library: `pdf-probe-linux-x64`, `-linux-arm64`, `-linux-x64-musl`, `-darwin-arm64` and `-darwin-x64`. `pdf-probe`
lists them as optional dependencies at its own version. Windows is built and tested in CI but has no package yet,
because npm's spam filter rejected the name `pdf-probe-win32-x64`; see `npm/src/platform.ts` for how to add it back.

## Cutting a release

1. **Bump the version** in `Cargo.toml`, `ffi/Cargo.toml` and `npm/package.json` (they move together; in
   `npm/package.json` that includes every entry of `optionalDependencies`), and refresh the lockfiles:
   `cargo update -w` in the repository root and in `fuzz/`. Open a PR, get CI green, merge it.
2. **Tag the merge commit on `main`** and push the tag:
   ```sh
   git checkout main && git pull
   tools/check-release.sh vX.Y.Z   # optional: the same check the workflow runs
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
3. **Wait for the checks, then approve.** The *Release* workflow verifies the tag, builds the native library on
   all six platforms from the tagged commit and runs the fixtures through each, packs the npm packages and
   installs them on every platform. Nothing is published until all of that has passed. Then the two publish jobs
   (crates.io and npm) wait for a reviewer on the `release` environment: approve them on the workflow run's page.
4. **Write the GitHub release** for the tag: what is breaking, what is new, what is unchanged.
5. **Check it:** the versions on https://crates.io/crates/pdf-probe and https://www.npmjs.com/package/pdf-probe
   (npm can take several minutes to show a new version), the docs.rs build, `cargo add pdf-probe` in an empty
   project, and `bun add pdf-probe` then `createProbe()`, which should report `engine: 'native'`.

Publishing is irreversible: a version can be yanked or deprecated, never deleted or replaced. If the workflow
fails after the tag is pushed, fix the problem, delete the tag (`git push origin :refs/tags/vX.Y.Z`) and tag
again, as long as nothing reached crates.io or npm. The npm job skips a package whose version is already on npm,
so re-running a job that failed part way is safe. The crates.io job is not repeatable: a crate version publishes
once.

## What the workflow checks

- `tools/check-release.sh` refuses the run unless the tag is a plain `vMAJOR.MINOR.PATCH`, equals the version in
  `Cargo.toml`, `ffi/Cargo.toml` and `npm/package.json` (and every optional dependency in it), and points at a
  commit that is on `main`. Anyone who can push a tag could otherwise publish from a side branch.
- `native.yml` runs on the tagged commit, so the libraries that ship are built and tested from the code that is
  tagged, not taken from an earlier run. Each Linux glibc library is checked to need no more than glibc 2.28.
- `tools/publish-npm.sh` publishes the tarballs that were installed and loaded on every platform, not a
  rebuild; it refuses to publish if `pdf-probe`'s optional dependencies are not exactly the platform tarballs it
  holds, and publishes `pdf-probe` last.
- The two `publish` jobs are the only ones with permission to ask crates.io or npm for a token.

## One-time setup

### crates.io (done)

The crate's *Settings → Trusted Publishing → Add* has repository owner `polybot-platform`, repository
`pdf-probe`, workflow `release.yml` and environment `release`. Only an owner of the crate can change it. The very
first publish of a crate needs an API token; every later one does not. If a token is ever needed again, make it
expire within days and revoke it afterwards.

### GitHub (done)

The `release` environment (repository *Settings → Environments*) lists the required reviewers and only accepts
deployments from `v*` tags.

### npm

npm trusted publishing is set up per package, in the package's *Settings → Trusted Publisher* on npmjs.com, or
with `npm trust github` (npm 11.15 or later; the first request asks for two-factor authentication). Each of the
six packages (`pdf-probe` and the five platform packages) gets the same entry: GitHub Actions, repository
`polybot-platform/pdf-probe`, workflow filename `release.yml` (the file name alone), environment `release`, and
only **Allow `npm publish`** checked (the workflow does not touch dist-tags).

- **A package must exist before it can be configured.** The six exist (the five platform packages were published
  by hand at 0.5.0, the first release that had them). A package added later, Windows for one, needs the same: a
  first publish by hand from the `npm-tarballs` artifact of a release run, before approving it.
- **A new configuration expires if no publish uses it within 2 days.** Configure the packages shortly before the
  release that uses them.
- npm shows a `0.0.0-stage` version on a new package. It is npm's own and harmless; `latest` is the real version.

Optionally, in each package's settings, set publishing access to *Require two-factor authentication and disallow
tokens*, so that only trusted publishing (and a person with two-factor authentication) can publish.
