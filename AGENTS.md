# Working on Enso

Enso is a minimal Rust Slack-to-CLI service with scheduled jobs. Read the
[README](README.md) and the owning page in `docs/` before changing behavior.
Keep configuration in files and runtime state in `enso.db`; use one runner and
one Slack delivery path. Avoid adding frameworks or unrelated features.

Follow [the release flow](docs/releases.md) for user-visible changes and release
requests. It owns changelog maintenance, versioning, validation, and publication.
GitHub Actions run only for release tags; validate ordinary changes locally
before committing and pushing. Keep the Linux/macOS checks required for release.

Preserve unrelated work. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
and `cargo test` before completing code changes. Use Conventional Commits. Do not
create branches, push, or open PRs unless explicitly requested. Never commit secrets.

`bundled/` contains starter files copied by `enso init`; those instructions apply
to an installed workspace, not this source checkout. Keep source `CLAUDE.md` a
relative symlink to this file. Installation and service changes require authorization.

macOS signing credentials: 1Password vault `Enso`, item
`Enso - Release - Apple Signing`; fields `CODESIGN_IDENTITY`,
`CODESIGN_CERTIFICATE` (base64 PKCS#12), and `CODESIGN_CERTIFICATE_PASSWORD`.
Use the `1password` skill. GitHub Actions uses repository secrets with the same
names. Follow `docs/releases.md` when signing local builds or changing signing.
Before replacing an installed macOS service binary, sign the staged build with
`scripts/sign-macos.sh` and the release identity. Do not install an ad-hoc build
over it with `cargo install`; that changes its privacy-permission identity.
