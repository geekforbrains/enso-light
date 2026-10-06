# CLI and installation

```text
enso [--home PATH] init
enso [--home PATH] run
enso config check
enso jobs list
enso jobs run NAME [--wait]
enso message send [TEXT|-] [--text-file PATH] [--file PATH] [--to CHANNEL] [--thread TS] [--plain] [--json]
enso service install|start|stop|restart|uninstall|status
enso service logs [--follow]
```

`init` creates starter files, never replaces existing ones. `run` starts the
foreground service. A lock allows only one service per Enso home. Job triggers
and message sends need a running service; they submit work to SQLite.

Install an authenticated Claude Code or Codex CLI first, then build Enso:

```sh
cargo install --path .
enso init
nvim ~/.enso/config.json ~/.enso/.env
enso config check
enso service install
enso service start
enso service status
```

Service installation uses launchd on macOS and systemd user services on Linux.
It enables startup at login; `start` runs it now. Linux operation after logout
requires your OS user's normal lingering/session configuration. Installation
captures `HOME`, `PATH`, and native CLI configuration paths so the service can
find the same authenticated CLI. Secrets stay in `.env` and are read at runtime.
Reinstall if the Enso executable or the relevant `PATH` changes. Uninstall stops
the service and removes registration; it preserves the entire Enso home.

## Messages and files

```sh
enso message send "Report ready" --to D012345
enso message send --text-file report.md --file chart.png --to C012345
printf '%s' 'A threaded update' | enso message send - --to C012345 --thread 1234567890.123456
```

Use repeatable `--file` for outgoing attachments. `--text-file` supplies message
text; it does not attach the file. `--json` prints a machine-readable receipt.
Receipts include delivery state and a Slack reference: a message timestamp for
text, or a remote file ID for attachments. File receipts also include
`message_ts` when Slack has exposed the associated message; successful upload
does not depend on that optional timestamp being available immediately.
Use `--plain` to disable Markdown-to-Slack formatting for message text.
An explicit destination wins; otherwise the current chat's reply destination,
job notification destination, or configured Slack notification destination is
used. A missing destination is an error. Enso formats Markdown for Slack and
splits long replies. Confirmed background sends become context for the next
conversation turn.

Incoming attachments are downloaded into `workspace/uploads/<run-id>/` with safe
filenames. The current prompt includes their paths. An incoming message supports
up to 20 files; individual incoming or outgoing files are limited to 50 MiB.
A failed download reports an error before starting the agent.

## Slack controls

| Command | Behavior |
|---|---|
| `!clear` | Clear an idle conversation's native session for the next turn |
| `!stop` | Cancel its running turn and queued turns |
| `!status` | Show execution settings and running/queued state |
| `!help` | Show the controls |

Enso adds a working reaction while a turn runs, acknowledges queued turns, and
reports failures and timeouts. Thread participation survives `!clear`.
