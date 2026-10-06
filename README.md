# Enso

A small Rust service that connects Slack to an installed Claude Code or Codex CLI,
plus scheduled jobs using the same runner. One binary, one shared workspace, and
one SQLite database. Native CLIs keep their own authentication and sessions.

```sh
cargo install --path .
enso init
# Set ~/.enso/.env credentials and permitted Slack users in config.json.
enso config check
enso service install
enso service start
```

Start with one permitted DM user. Enso accepts no conversations until you configure
the allowlist. Each DM shares one session across its threads; channel threads get
separate sessions. Messages in a busy conversation queue behind its current turn.
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

Requires Rust for building, an authenticated agent CLI, Bash for job hooks, and
macOS or Linux. Service installation uses your user's launchd or systemd manager.
