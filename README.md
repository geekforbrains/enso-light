# Enso

A small Rust service that connects Slack to an installed Claude Code or Codex CLI,
plus scheduled jobs using the same runner. One binary, named workspaces, and one
SQLite database. Native CLIs keep their own authentication and sessions.

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/geekforbrains/enso-light/releases/latest/download/enso-installer.sh | sh
enso init
# Set providers.main.cli in ~/.enso/config.json and the Slack tokens in ~/.enso/.env.
enso config check
enso service install
enso service start
```

Then DM the bot. Enso runs nothing for a DM that `slack.dms` does not route and
replies with your user ID; add it as `"dms": {"U012345": "main"}` in
`config.json` and run `enso service restart`.

`~/.enso` is a git repository whose `AGENTS.md` and `.agents/skills/` are shared
by every workspace inside it. Each workspace is a directory where its
conversations and jobs run, with its own `AGENTS.md` and skills; the starter is
`~/.enso/workspaces/main`. Claude Code runs ignore your personal `~/.claude`
configuration, so grant permissions in provider `args`. See
[configuration](docs/configuration.md#instructions-and-skills).

Each DM shares one session across its threads; channel threads get separate
sessions. Messages in a busy conversation queue behind its current turn.
Different jobs and conversations run in parallel with no global concurrency limit
or setting. Scheduled occurrences of an already busy job are skipped.
Slack controls: `!clear`, `!stop`, `!status`, and `!help`.

Agents can use `enso slack` to inspect channels, users, history, and threads,
search messages, get links, and manage reactions. These commands call Slack
directly; outgoing messages use `enso message send` for tracked delivery.

- [Configuration and Slack setup](docs/configuration.md)
- [Jobs and hooks](docs/jobs.md)
- [CLI and installation](docs/cli.md)
- [Runtime and development](docs/runtime.md)
- [Changelog](CHANGELOG.md)
- [Release flow](docs/releases.md)

Requires an authenticated agent CLI, Bash for job hooks, git (Codex needs it for
the shared instructions and skills), and macOS or Linux on ARM64 or x86-64.
Service installation uses your user's launchd or systemd manager.
The installer places Enso in `~/.local/bin`; follow its PATH instructions if needed.
To build from source, install Rust and run `cargo install --locked --path .`.

Run `enso upgrade` from a terminal to install the latest release and immediately
restart the service. Active work is interrupted. Configuration and runtime state
are preserved, but a release with breaking changes may need manual steps first;
read its [changelog](CHANGELOG.md) entry before upgrading. See
[installation and upgrades](docs/cli.md).

Released under the [MIT license](LICENSE).
