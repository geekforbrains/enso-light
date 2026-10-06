# Working on Enso

Enso is a minimal Rust Slack-to-CLI service with scheduled jobs. Read the
[README](README.md) and the owning page in `docs/` before changing behavior.
Keep configuration in files and runtime state in `enso.db`; use one runner and
one Slack delivery path. Avoid adding frameworks or unrelated features.

Preserve unrelated work. Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
and `cargo test` before completing code changes. Use Conventional Commits. Do not
create branches, push, or open PRs unless explicitly requested. Never commit secrets.

`bundled/` contains starter files copied by `enso init`; those instructions apply
to an installed workspace, not this source checkout. Keep source `CLAUDE.md` a
relative symlink to this file. Installation and service changes require authorization.
