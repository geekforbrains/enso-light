# Changelog

Notable changes follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning and publication follow the [release flow](docs/releases.md).

## [Unreleased]

### Upgrading from 0.2.0

1. `enso init` never overwrites files, so merge the new starter
   [`AGENTS.md`](https://github.com/geekforbrains/enso-light/blob/main/bundled/AGENTS.md)
   and [`enso` skill](https://github.com/geekforbrains/enso-light/blob/main/bundled/SKILL.md)
   into `~/.enso/AGENTS.md` and `~/.enso/.agents/skills/enso/SKILL.md` by hand.
   Enso no longer adds this guidance in code, so without the merge agents lose it.
2. Hooks that read `.workspace.path`, `.run_id`, `.provider`, or
   `.notification_target` from stdin switch to `$ENSO_WORKSPACE`,
   `$ENSO_RUN_ID`, and `$ENSO_CHANNEL` / `$ENSO_THREAD_TS`.
3. Create any configured workspace directory that does not exist yet, as in
   [adding a workspace](https://github.com/geekforbrains/enso-light/blob/main/docs/configuration.md#adding-a-workspace);
   `enso config check` lists them.

### Changed

- Claude Code runs use your normal `~/.claude` settings, skills, plugins, and
  `CLAUDE.md` again: Enso no longer passes `--setting-sources project,local`.
  Both CLIs now run as they do in a terminal opened in the workspace. Step 7 of
  [Upgrading from 0.1.x](https://github.com/geekforbrains/enso-light/blob/v0.2.0/CHANGELOG.md#upgrading-from-01x)
  is no longer needed; anything you moved into provider `args`, `.env`, or a
  workspace's `.claude/settings.json` keeps working.
- Claude Code runs no longer get `--permission-prompts none`; print mode already
  denies anything that would prompt, and your settings or provider `args` decide
  the rest.
- A failed turn reports that the CLI exited with a status or reported an error,
  instead of a guessed category such as `authentication_failed`. The end of the
  CLI's output now goes to the service log, with secrets redacted; a session
  that cannot be resumed still says to use `!clear`.
- **Breaking:** Enso adds no guidance of its own to prompts. The delivery,
  status-update, and job guidance it used to prepend to a new session now lives
  in the starter `AGENTS.md`, where you can edit it, and the `enso` skill is now
  only the CLI reference.
- **Breaking:** the `<enso-context>` block, which prerun and postrun also get on
  stdin, keeps only what an agent cannot otherwise know: `source`; `sender`,
  `channel`, `message_ts`, `thread_ts`, and `attachments` for Slack turns; and
  `job` (`name`, `trigger`, `scheduled_for`) for jobs. `run_id`, `provider`,
  `workspace`, `received_at`, `started_at`, `conversation_id`, `reply`,
  `notification_target`, and `job.directory` are gone; use the `ENSO_*`
  variables and the working directory.
- **Breaking:** Codex runs no longer get `--skip-git-repo-check`. Workspaces
  inside the Enso home are in its git repository; for a Codex workspace
  elsewhere, trust the folder in Codex or add `--skip-git-repo-check` to the
  provider's `args`.
- **Breaking:** Enso no longer creates workspace folders. `enso init` still
  creates the starter `workspaces/main` with a new starter `config.json`;
  service startup creates nothing, and `config check` reports a workspace path
  that is not a directory as an error.
- **Breaking:** a conversation's session is no longer pinned to the CLI and
  workspace that started it. The next turn resumes it with whatever the
  conversation is routed to; if that CLI cannot resume it, the turn fails with
  the existing advice to use `!clear`. `!status` no longer reports a session
  from another CLI or workspace.
- Existing 0.2.0 databases keep working without a reset; the session-pinning
  columns and two unused tables stay in them, unused.
- `enso init` prints one fixed next step and no longer reads an existing
  `config.json`, `.env`, or `enso.db` or creates the database; the service
  creates it on start.
- `enso service install` checks only what service startup needs: the
  configuration and Slack tokens as the service will load them. An invalid job
  no longer blocks installing.
- Workspace names may use any characters; job names still use letters,
  numbers, hyphens, and underscores.
- An invalid `job.json` reports the JSON parser's own message.

## [0.2.0] - 2026-10-08

### Upgrading from 0.1.x

Enso 0.2.0 needs a rewritten `config.json`, a new home layout, and a new
database. Follow these steps in order; 0.2.0 cannot start until they are done.

1. Stop the service: `enso service stop`.
2. Install 0.2.0 while the service is stopped by rerunning the installer:

   ```sh
   curl --proto '=https' --tlsv1.2 -LsSf https://github.com/geekforbrains/enso-light/releases/latest/download/enso-installer.sh | sh
   ```

   `enso upgrade` from 0.1.x also installs it, but then restarts the service,
   which fails until the remaining steps are done; run `enso service stop`
   straight after it. Confirm with `enso --version`.
3. Only now move the old database aside, since the 0.1.x updater recreates an
   old one when it restarts the service. Conversations start fresh sessions,
   and old run history stays in the moved files.

   ```sh
   cd ~/.enso && mkdir 0.1 && mv enso.db* 0.1/
   ```

4. Move `skills/` to `.agents/skills/` and the starter workspace from
   `workspace/` to `workspaces/main/`. Its old `AGENTS.md` makes way for the
   shared `~/.enso/AGENTS.md`, and its skill links become an empty
   `.agents/skills/` for workspace-only skills. The commands stop without
   changing anything if `.agents/skills` or `workspaces/main` already exists;
   move your files into those by hand instead.

   ```sh
   cd ~/.enso && test ! -e .agents/skills && test ! -e workspaces/main &&
     mkdir -p .agents workspaces && mv skills .agents/skills &&
     mv workspace workspaces/main && mv workspaces/main/AGENTS.md AGENTS.md.0.1 &&
     rm -f .agents/skills/enso/SKILL.md workspaces/main/CLAUDE.md \
       workspaces/main/.agents/skills workspaces/main/.claude/skills &&
     mkdir -p workspaces/main/.agents/skills workspaces/main/.claude &&
     ln -s ../.agents/skills workspaces/main/.claude/skills
   ```

   `init` leaves this existing workspace as it is, so it has no `AGENTS.md` of
   its own. For workspace-only instructions, add `workspaces/main/AGENTS.md`
   and link `CLAUDE.md` to it with `ln -s AGENTS.md CLAUDE.md`.

5. Rewrite `config.json` to the
   [new shape](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/configuration.md#configjson):
   - Move `execution`'s `cli`, `model`, `effort`, `executable`, and `args` to
     `providers.main`, add `"defaults": {"provider": "main"}`, and move
     `execution.timeout_seconds` to `defaults.timeout_seconds`. Set `model`
     and `effort` to keep 0.1.x's Claude defaults of `sonnet` and `high`.
   - Add `"workspaces": {"main": {"path": "${ENSO_HOME}/workspaces/main"}}`.
   - Replace `"dm_users": ["U012345"]` with `"dms": {"U012345": "main"}`, and
     each channel entry with `"C012345": "main"` or
     `"C012345": {"workspace": "main", "mention": "first"}`. Replace
     `slack.mentions` with `defaults.mention`. Mention rules map as both
     required → `always`, thread `false` → `first`, and both `false` →
     `never`; top-level `false` with thread `true` has no equivalent.
   - Remove `bot_token`, `app_token`, and `user_token` from `slack`. Keep
     `SLACK_BOT_TOKEN` and `SLACK_APP_TOKEN` in `.env`, and add
     `SLACK_USER_TOKEN` there to keep workspace search.
6. Add `"workspace": "main"` to each `job.json`, and replace a job's
   `execution` block with a named `provider` and `timeout_seconds`. Hooks that
   read `.workspace` from stdin now use `.workspace.path` or `$ENSO_WORKSPACE`.
7. Grant Claude Code unattended permissions in provider `args` or a workspace's
   `.claude/settings.json`, since runs no longer load `~/.claude` (see below).
8. Run `enso init`, then `enso config check`, and fix any errors. `init` adds
   the 0.2.0 `AGENTS.md`, `enso` skill, `.gitignore`, and git repository. Copy
   personal edits from `AGENTS.md.0.1` into `AGENTS.md`, then delete
   `AGENTS.md.0.1`. Keep edits to the old `enso` skill in a separate skill.
9. Start the service: `enso service start`, then check `enso service status`.

### Added

- Named `providers` in `config.json`, each a `cli` (`claude` or `codex`) with
  optional `model`, `effort`, `executable`, and `args`. Runs use
  `defaults.provider` unless their workspace or job names another. See
  [configuration](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/configuration.md#providers).
- `defaults.timeout_seconds`, overridable per job with `timeout_seconds`.
- `${ENSO_HOME}` in `config.json` expands to the Enso home in use.
- Named `workspaces` in `config.json`, each an absolute `path` with an optional
  `provider`. Agents run in their conversation's or job's workspace, attachments
  go to its `uploads/<run-id>/`, and `ENSO_WORKSPACE` gives agents and hooks its
  path. See
  [workspaces](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/configuration.md#workspaces).
- `slack.dms` and `slack.channels` route users and channels to workspaces, with
  `"*"` for any user or channel not listed. Channel `mention` modes (`always`,
  `first`, `never`) and `defaults.mention` set when a mention is needed. See
  [Slack routing](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/configuration.md#slack).
- `!status`, the run context, and stored run settings name the provider and
  workspace. `!status` also says when the stored session belongs to another CLI
  or workspace and needs `!clear`.
- An unrouted DM, or an @mention in an unrouted channel, gets one reply with
  the user or channel ID to add to `slack.dms` or `slack.channels`. Customize
  the text with `slack.unconfigured_message`, or set it to `""` to stay silent.
  See
  [unconfigured conversations](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/configuration.md#unconfigured-conversations).
- The Enso home is a git repository with a shared `AGENTS.md` and
  `.agents/skills/` for every workspace inside it (`CLAUDE.md` and
  `.claude/skills` link to them), and a `.gitignore` for secrets and runtime
  state. `enso init` runs `git init` and warns instead of failing when git is
  unavailable; Codex needs the repository to load the shared layer. See
  [instructions and skills](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/configuration.md#instructions-and-skills).
- `enso init` and service startup create each missing workspace directory with
  a starter `AGENTS.md`, `CLAUDE.md` link, and `.agents/skills/`. Existing
  directories are left untouched. Service startup logs a workspace it cannot
  create and keeps running. See
  [workspace scaffolding](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/configuration.md#workspace-scaffolding).

### Changed

- **Breaking:** `config.json` replaces `execution` with `defaults` and
  `providers`, and an old file fails to load with an error instead of being
  misread. See [Upgrading from 0.1.x](https://github.com/geekforbrains/enso-light/blob/v0.2.0/CHANGELOG.md#upgrading-from-01x).
- **Breaking:** Slack tokens come only from `SLACK_BOT_TOKEN`,
  `SLACK_APP_TOKEN`, and optional `SLACK_USER_TOKEN` in `.env` or the
  environment, never from `config.json`. An assignment in `.env`, even an empty
  one, wins over an exported token, and the installed service reads tokens only
  from `.env`.
- A blank or left-out `model`, `effort`, or `executable` uses the CLI's own
  default for Claude Code too; Enso no longer defaults Claude to `sonnet`/`high`.
- `enso config check` reports every problem at once as `errors`, plus `notes`
  for things to know (no routes yet, a workspace directory still to be
  created, Codex without a git home), and counts providers, workspaces, and
  jobs instead of reporting `cli`. It exits non-zero when there are errors.
  `enso service install` refuses until they are fixed, and while `config.json`
  uses a variable that only your shell defines rather than `.env`. See
  [checking configuration](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/cli.md#checking-configuration).
- An invalid job no longer stops the service, the scheduler, or other jobs:
  it is skipped with a logged error, and `enso jobs list` shows it with its
  `error`. See
  [invalid jobs](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/jobs.md#invalid-jobs).
- A `config.json` or `job.json` with an unknown or missing field names it in
  the error, such as ``unknown field `execution` ``.
- **Breaking:** jobs require `workspace` in `job.json`, naming a configured
  workspace.
- **Breaking:** the context header's `workspace`, which prerun hooks receive on
  stdin, is an object with `name` and `path` instead of a path string. Hooks
  that read `.workspace` should use `.workspace.path` or `$ENSO_WORKSPACE`.
- A conversation's native session is pinned to the CLI and workspace path that
  created it. Moving a conversation to another workspace path requires `!clear`.
- **Breaking:** Enso 0.2.0 needs a new `enso.db`; an older database fails to
  open, and the service then creates nothing in the home. `enso config check`
  reports it too. See
  [Upgrading from 0.1.x](https://github.com/geekforbrains/enso-light/blob/v0.2.0/CHANGELOG.md#upgrading-from-01x).
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
  starter workspace from `workspace/` to `workspaces/main/`. See
  [Upgrading from 0.1.x](https://github.com/geekforbrains/enso-light/blob/v0.2.0/CHANGELOG.md#upgrading-from-01x).

### Removed

- **Breaking:** `slack.dm_users`, `slack.mentions`, and per-channel
  `top_level`/`thread` rules, replaced by `slack.dms`, `slack.channels`
  routes, and `mention` modes.
- **Breaking:** the job `execution` overrides object. Define a provider and set
  the job's `provider` and `timeout_seconds` instead; see
  [jobs](https://github.com/geekforbrains/enso-light/blob/v0.2.0/docs/jobs.md).

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

[Unreleased]: https://github.com/geekforbrains/enso-light/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/geekforbrains/enso-light/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/geekforbrains/enso-light/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/geekforbrains/enso-light/releases/tag/v0.1.0
