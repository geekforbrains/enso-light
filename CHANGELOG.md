# Changelog

Notable changes follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning and publication follow the [release flow](docs/releases.md).

## [Unreleased]

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

[Unreleased]: https://github.com/geekforbrains/enso-light/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/geekforbrains/enso-light/releases/tag/v0.1.0
