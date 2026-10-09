# Release flow

Enso ships from `main` as a single binary for macOS and Linux on ARM64 and
x86-64. GitHub Releases hold the binaries, checksums, installer, and release
notes. Windows and crates.io publication are not supported.

## During development

GitHub Actions run only when a release tag is pushed. Ordinary branch pushes
and pull requests do not start workflows; run the local checks in
[Runtime and development](runtime.md#develop) before committing and pushing.
`ci.yml` is callable only by the Release workflow, which requires its Linux and
macOS checks before publishing. `pr-run-mode = "skip"` in `dist-workspace.toml`
also disables release planning on pull requests. This avoids checking the same
commit twice when `main` and its release tag are pushed together.

Add a concise user-facing entry to `CHANGELOG.md` under `Unreleased` whenever
behavior changes. Use `Added`, `Changed`, `Fixed`, `Removed`, `Deprecated`, or
`Security` as needed; omit empty sections. Internal refactoring and routine test
changes need no entry. Keep the owning documentation current in the same change.
Continue using Conventional Commits; release notes describe outcomes for users.

The changelog is the canonical source. cargo-dist copies the version's section
into the GitHub release, so there is no second set of notes to maintain.
Use absolute URLs inside release entries so links work on the release page too.

## Versions and compatibility

The first public version is `0.1.0`. Before `1.0`, fixes increment the patch
version and new features or breaking changes increment the minor version.
Once `1.0` establishes the compatibility contract, follow Semantic Versioning:
patch for compatible fixes, minor for compatible features, major for breaks.
The contract includes CLI behavior, configuration, hooks, and persisted state.

Choose the smallest appropriate bump from `Unreleased` and the changes since
the last release unless the user specifies a version. Document breaking changes
and required operator actions explicitly. A change to the database schema needs
a tested migration from supported released schemas, except that a pre-1.0
minor release may instead require a new database: `Db::open` must then refuse
older schemas with a clear message, and the changelog must give the manual
upgrade steps. Keep configuration backward compatible where possible. Never
advertise a binary-only upgrade as migrating an incompatible home. Automatic
binary rollback is not a database rollback.

## Making a release

A request such as “let's do a new release” authorizes this complete workflow,
including committing the release changes, pushing `main` and its release tag,
and publishing the GitHub release. Ordinary development does not authorize a
push, tag, or release. Do not create a release branch or PR unless requested.

1. Inspect `git status`, recent commits, and the changes since the last release.
   Preserve unrelated work. Review behavior, documentation, and upgrade
   compatibility; resolve release blockers before tagging.
2. Update `[package].version` in `Cargo.toml` and refresh `Cargo.lock` with
   `cargo check`. Move the `Unreleased` entries under `## [X.Y.Z] - YYYY-MM-DD`,
   retain an empty `Unreleased` heading, and update the comparison links.
3. Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
   `cargo test --locked`, and `python3 scripts/check-release.py`.
4. Use the cargo-dist version pinned in `dist-workspace.toml` (currently 0.33.0).
   Run `dist generate --check` and `dist plan --tag vX.Y.Z`. Confirm that the
   plan contains exactly the four supported targets, their `.tar.gz` archives,
   checksums, and `enso-installer.sh`. If release configuration changes, run
   `dist generate`; do not hand-edit its generated release workflow.
5. Commit the relevant changes using a subject such as `chore: release X.Y.Z`.
   Push `main`, create an annotated `vX.Y.Z` tag on that exact commit, and push
   only that tag. Do not push all local tags. The tag starts validation and
   publication; the `main` push alone starts no workflow.
6. Watch the Release workflow through completion. Its reusable Check workflow
   runs formatting, Clippy, tests, and release metadata checks on macOS and
   Linux. cargo-dist builds the four archives and shell installer, then uploads
   all artifacts before publishing. A failed build or check must not publish.
7. Verify the published tag and commit, release notes, Latest designation, and
   every expected asset. Download and verify the checksums. Smoke-test `--version`,
   `init`, and `config check` with temporary homes on macOS and Linux. For updater
   changes, also run the local HTTP upgrade tests, including failed verification.
   Check the public installer in an isolated install directory.
8. Report the version, release URL, checks, and any limitations. Leave the
   repository clean and the tag pointing at the released commit.

Never move a published tag or replace its binaries. A correction gets a new
release. GitHub immutable releases are enabled for this repository. Their
required order is draft, upload every asset, then publish.
Prereleases are marked as prereleases and do not replace Latest.

## Installation and upgrades

The generated shell installer places `enso` in `~/.local/bin` by default.
The supported asset names are `enso-TARGET.tar.gz`; each archive contains
`enso-TARGET/enso`. `sha256.sum` contains the required archive checksums.
Keep this contract compatible with already-released updaters.

`enso upgrade` resolves the latest non-prerelease GitHub release, requires a
matching checksum, checks the staged executable's version, and replaces the
current executable at the same path. It does not downgrade. It immediately
restarts the installed service for the selected Enso home and verifies a new
process connects to Slack. Active work is interrupted; a stopped installed
service is started. Without a service it only replaces the binary. An already
current installation is left running.

Run upgrades from a terminal, outside Enso's own agent or hook processes.
Stop a foreground `enso run` before upgrading. A service registered to another
executable must be upgraded using that executable or reinstalled explicitly.
When several homes share a binary, restart the other homes separately. Config,
credentials, jobs, workspace files, and SQLite state are preserved. A failed
download or verification leaves the old binary in place; a restart failure
reports that the new binary is installed and points to service status and logs.

## macOS signing

macOS executables embed `macos/Info.plist` with the fixed signing identifier
`com.geekforbrains.enso`. cargo-dist signs them with a Developer ID Application
certificate before creating archives and checksums. Its generated workflow uses
the repository secrets `CODESIGN_IDENTITY`, `CODESIGN_CERTIFICATE` (base64-encoded
PKCS#12 certificate and private key), and `CODESIGN_CERTIFICATE_PASSWORD`.
`.github/build-setup.yml` requires all three on macOS so missing credentials
cannot silently produce an unsigned release, and enables hardened runtime.
The credential source for maintainers is named in `AGENTS.md`.

Keep the signing identifier and Developer ID team consistent across updates.
Apple Development and Developer ID Application signatures have different default
designated requirements. Grant macOS privacy permissions once after moving from
the old ad-hoc binary to the signed binary; changing back to an ad-hoc or
development-signed build may require another approval. A fixed identifier alone
does not make ad-hoc builds retain their identity.

For local builds installed as the service, use the same Developer ID Application
identity as releases. With that certificate and private key in your Keychain:

```sh
cargo build --locked --release
export CODESIGN_IDENTITY='Developer ID Application: Your Name (TEAMID)'
./scripts/sign-macos.sh target/release/enso
# Stop active work first; replace the executable rather than modifying it in place.
install -m 755 target/release/enso ~/.local/bin/.enso-new
mv -f ~/.local/bin/.enso-new ~/.local/bin/enso
enso service restart
```

Do not use an ordinary `cargo install` to replace a signed service binary: it
installs an ad-hoc build. Certificate signing currently does not submit releases
to Apple's notarization service; notarization is separate from the stable code
identity needed to retain privacy approvals.

When validating signing changes, inspect `codesign -d -r- BINARY` on two different
builds and confirm that each satisfies the other's designated requirement. Check
the actual executable extracted from a release archive, not only a build output.

## References

- [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
- [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
- [cargo-dist](https://axodotdev.github.io/cargo-dist/book/)
- [GitHub immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases)
