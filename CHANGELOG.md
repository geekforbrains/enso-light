# Changelog

Notable changes follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning and publication follow the [release flow](docs/releases.md).

## [Unreleased]

### Added

- Named `providers` in `config.json`, each a `cli` (`claude` or `codex`) with
  optional `model`, `effort`, `executable`, and `args`. Runs use
  `defaults.provider` unless their workspace or job names another. See
  [configuration](https://github.com/geekforbrains/enso-light/blob/main/docs/configuration.md#providers).
- `defaults.timeout_seconds`, overridable per job with `timeout_seconds`.
- `${ENSO_HOME}` in `config.json` expands to the Enso home in use.
- `!status`, the run context, and stored run settings name the provider.
- Named `workspaces` in `config.json`, each an absolute `path` with an optional
  `provider`. Agents run in their conversation's or job's workspace, attachments
  go to its `uploads/<run-id>/`, and `ENSO_WORKSPACE` gives agents and hooks its
  path. See
  [workspaces](https://github.com/geekforbrains/enso-light/blob/main/docs/configuration.md#workspaces).
- `slack.dms` and `slack.channels` route users and channels to workspaces, with
  `"*"` for any user or channel not listed. Channel `mention` modes (`always`,
  `first`, `never`) and `defaults.mention` set when a mention is needed. See
  [Slack routing](https://github.com/geekforbrains/enso-light/blob/main/docs/configuration.md#slack).
- `!status` shows the conversation's workspace; the run context and stored run
  settings include the workspace name and path.
- An unrouted DM, or an @mention in an unrouted channel, gets one reply with
  the user or channel ID to add to `slack.dms` or `slack.channels`. Customize
  the text with `slack.unconfigured_message`, or set it to `""` to stay silent.
  See
  [unconfigured conversations](https://github.com/geekforbrains/enso-light/blob/main/docs/configuration.md#unconfigured-conversations).
- The Enso home is a git repository with a shared `AGENTS.md` and
  `.agents/skills/` for every workspace inside it (`CLAUDE.md` and
  `.claude/skills` link to them), and a `.gitignore` for secrets and runtime
  state. `enso init` runs `git init` and warns instead of failing when git is
  unavailable; Codex needs the repository to load the shared layer. See
  [instructions and skills](https://github.com/geekforbrains/enso-light/blob/main/docs/configuration.md#instructions-and-skills).
- `enso init` and service startup create each missing workspace directory with
  a starter `AGENTS.md`, `CLAUDE.md` link, and `.agents/skills/`. Existing
  directories are left untouched. See
  [workspace scaffolding](https://github.com/geekforbrains/enso-light/blob/main/docs/configuration.md#workspace-scaffolding).

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
- **Breaking:** jobs require `workspace` in `job.json`, naming a configured
  workspace. Add `"workspace": "main"` to each existing job.
- **Breaking:** the context header's `workspace` is an object with `name` and
  `path` instead of a path string.
- A conversation's native session is pinned to the CLI and workspace path that
  created it. Moving a conversation to another workspace path requires `!clear`.
- **Breaking:** Enso 0.2.0 needs a new `enso.db`; an older database fails to
  open. Stop the service, move `enso.db` together with any `enso.db-wal` and
  `enso.db-shm` files aside (for example `mkdir 0.1 && mv enso.db* 0.1/`), and
  start again. Conversations start fresh sessions, and old run history stays
  in the moved files.
- **Breaking:** Claude Code runs pass `--setting-sources project,local`, so
  they no longer load your personal `~/.claude` settings, skills, plugins, or
  `CLAUDE.md`. Grant unattended permissions in provider `args` (for example
  `"args": ["--dangerously-skip-permissions"]`) or a workspace's
  `.claude/settings.json`, and move personal skills Enso needs into
  `~/.enso/.agents/skills/`. The `env` block and `apiKeyHelper` in
  `~/.claude/settings.json` no longer apply either; move variables such as
  `ANTHROPIC_BASE_URL` or `CLAUDE_CODE_USE_BEDROCK` into `~/.enso/.env` or a
  workspace's `.claude/settings.json`.
- **Breaking:** the home layout moves `skills/` to `.agents/skills/` and the
  starter workspace from `workspace/` to `workspaces/main/`, whose old
  `AGENTS.md` is replaced by the shared `~/.enso/AGENTS.md`. Stop the service,
  set `workspaces.main.path` to `${ENSO_HOME}/workspaces/main` in
  `config.json`, then run:

  ```sh
  cd ~/.enso && mkdir -p .agents workspaces
  mv skills .agents/skills && mv workspace workspaces/main
  mv workspaces/main/AGENTS.md AGENTS.md.0.1
  rm .agents/skills/enso/SKILL.md
  rm workspaces/main/CLAUDE.md workspaces/main/.agents/skills workspaces/main/.claude/skills
  enso init
  ```

  `enso init` writes the 0.2.0 `AGENTS.md` and `enso` skill, which describe
  per-workspace runs and the new job fields. Copy any personal edits from
  `AGENTS.md.0.1` into `AGENTS.md`, then delete `AGENTS.md.0.1`. If you edited
  the old `enso` skill, keep those edits in a separate skill.

### Removed

- **Breaking:** `slack.dm_users`, `slack.mentions`, and per-channel
  `top_level`/`thread` rules. Rewrite them as routes: add the
  `workspaces` entry `"main": {"path": "${ENSO_HOME}/workspaces/main"}`, replace
  `"dm_users": ["U012345"]` with `"dms": {"U012345": "main"}`, and replace each
  channel entry with `"C012345": "main"` or
  `"C012345": {"workspace": "main", "mention": "first"}`. Mention rules map as
  both required → `always`, thread `false` → `first`, both `false` → `never`;
  top-level `false` with thread `true` has no equivalent.
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
