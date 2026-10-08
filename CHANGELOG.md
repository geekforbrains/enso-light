# Changelog

Notable changes follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning and publication follow the [release flow](docs/releases.md).

## [Unreleased]

### Added

- Named `providers` in `config.json`, each a `cli` (`claude` or `codex`) with
  optional `model`, `effort`, `executable`, and `args`. Conversations use
  `defaults.provider`; a job can name another with `provider`. See
  [configuration](https://github.com/geekforbrains/enso-light/blob/main/docs/configuration.md#providers).
- `defaults.timeout_seconds`, overridable per job with `timeout_seconds`.
- `${ENSO_HOME}` in `config.json` expands to the Enso home in use.
- `!status`, the run context, and stored run settings name the provider.

### Changed

- **Breaking:** `config.json` replaces `execution` with `defaults` and
  `providers`. Rewrite it to the new shape; an old file fails to load with an
  error instead of being misread.
- **Breaking:** Slack tokens come only from `SLACK_BOT_TOKEN`,
  `SLACK_APP_TOKEN`, and optional `SLACK_USER_TOKEN` in `.env` or the
  environment; the installed service reads them only from `.env`. Remove
  `bot_token`, `app_token`, and `user_token` from `slack` in `config.json`, and
  set `SLACK_USER_TOKEN` in `.env` to keep workspace search.
- A blank or left-out `model`, `effort`, or `executable` uses the CLI's own
  default for Claude Code too; Enso no longer defaults Claude to `sonnet`/`high`.
- `enso config check` reports the default `provider` name instead of `cli`.

### Removed

- **Breaking:** the job `execution` overrides object. Define a provider and set
  the job's `provider` and `timeout_seconds` instead; see
  [jobs](https://github.com/geekforbrains/enso-light/blob/main/docs/jobs.md).

## [0.1.1] - 2026-10-06

### Fixed

- Enso connects to Slack Socket Mode. In 0.1.0, every build panicked while
  opening the connection, so a running service never received messages.
- Linux services start. systemd rejected the quoted `WorkingDirectory` in units
  written by 0.1.0. After upgrading on Linux, run `enso service install`, then
  `enso service restart`.

## [0.1.0] - 2026-10-06

First public release of Enso Light, the standalone Rust Slack-to-CLI service.

### Added

- Slack conversations through authenticated Claude Code and Codex CLIs, with
  allowlisted access, resumable sessions, queued turns, and cancellation.
- Scheduled and manual jobs with Bash hooks, prompt variables, optional Slack
  notifications, and bounded retries requested by postrun.
- Markdown and Block Kit messages, file attachments, Slack lookups, search,
  and reactions.
- Shared workspace instructions and skills for Claude Code and Codex.
- SQLite runtime state, durable outgoing delivery, and restart recovery.
- User services for macOS launchd and Linux systemd.
- Prebuilt macOS and Linux binaries for ARM64 and x86-64, a shell installer, and
  `enso upgrade` to verify and install the latest release and restart the service.

### Fixed

- Messages with too many attachments fail once without forcing a Slack reconnect.

### Changed

- Pre-release source installations must remove `slack.notify` from `config.json`.
  Jobs use their own optional `notify`; standalone sends require `--to`.
- Postrun stdout is now empty or structured JSON. Redirect diagnostic output and
  commands such as `enso message send` to stderr; see
  [jobs](https://github.com/geekforbrains/enso-light/blob/v0.1.0/docs/jobs.md).
- This release does not migrate homes from the previous Python Enso project.
  Initialize a separate home when moving from that application.

[Unreleased]: https://github.com/geekforbrains/enso-light/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/geekforbrains/enso-light/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/geekforbrains/enso-light/releases/tag/v0.1.0
