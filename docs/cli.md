# CLI and installation

```text
enso [--home PATH] init
enso [--home PATH] run
enso [--home PATH] upgrade
enso config check
enso jobs list
enso jobs run NAME [--wait]
enso message send [TEXT|-] [--text-file PATH] [--file PATH] [--to CHANNEL] [--thread TS] [--plain|--blocks PATH] [--json]
enso slack channels|channel|users|user|history|thread|message|search|link|react ...
enso service install|start|stop|restart|uninstall|status
enso service logs [--follow]
```

`init` creates the [home layout](configuration.md) and starter files, never
replacing existing ones, and the starter `workspaces/main` only together with a
new starter `config.json`. It never reads an existing configuration or
database. It runs `git init` in a home without `.git`, since Codex needs the
repository to load the shared `AGENTS.md` and `.agents/skills`; without git it
warns in its output instead of failing. Its JSON output has `initialized`, any
`warnings`, and a `next` step.

`run` starts the foreground service. A lock allows only one service per Enso
home. Startup stops on an `enso.db` from an older release; an invalid job is
logged and skipped, and runs routed to a missing workspace directory fail until
it exists. Job triggers and message sends need a running service; they submit
work to SQLite.

Install git and an authenticated Claude Code or Codex CLI first, then install Enso:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/geekforbrains/enso-light/releases/latest/download/enso-installer.sh | sh
enso init
nvim ~/.enso/config.json ~/.enso/.env
enso config check
enso service install
enso service start
enso service status
```

The installer uses `~/.local/bin`; follow its PATH instructions if needed.
Prebuilt binaries support macOS and Linux on ARM64 and x86-64. Source builds
require Rust: `cargo install --locked --path .` from this repository.

Run `enso upgrade` from a terminal to download and verify the latest release,
replace the executable at its existing path, and immediately restart this home's
installed service. Active work is interrupted. It starts an installed service
even if stopped; without an installed service it only updates the binary.
An already current version does not restart. Config, credentials, jobs,
workspace files, and database are preserved, but a release with breaking
changes may need manual steps before the new version can start. Read its
[changelog](../CHANGELOG.md) entry before upgrading; for example, upgrading
0.1.x to 0.2.0 has its own procedure.

Upgrades require a writable install directory and cannot run from an Enso agent
or hook, since restarting stops those processes. Stop a foreground `enso run`
first. A service registered to another executable requires upgrading through
that executable or explicitly reinstalling its registration. Other homes using
the same binary need their own restart. If restart verification fails, the error
reports that the new binary is installed; inspect `enso service status` and
`enso service logs`. Read the [changelog](../CHANGELOG.md) for compatibility notes.

Service installation uses launchd on macOS and systemd user services on Linux.
It enables startup at login; `start` runs it now. Linux operation after logout
requires your OS user's normal lingering/session configuration. Installation
captures `HOME`, `PATH`, and native CLI configuration paths so the service can
find the same authenticated CLI. Secrets stay in `.env` and are read at runtime;
installation requires `SLACK_BOT_TOKEN` and `SLACK_APP_TOKEN` there; see
[checking configuration](#checking-configuration).
Reinstall if the Enso executable or the relevant `PATH` changes. Uninstall stops
the service and removes registration; it preserves the entire Enso home.

## Checking configuration

`config check` loads `config.json`, `.env`, and every job, and reports every
problem at once instead of stopping at the first:

```json
{
  "valid": false,
  "errors": [
    "providers.main.cli is blank; set \"claude\" or \"codex\"",
    "SLACK_BOT_TOKEN is blank; set it in .env",
    "job report: invalid workspace: workspace \"other\" is not defined in workspaces"
  ],
  "notes": [
    "no dms or channels are configured; Enso will reply \"not configured\" to every message"
  ],
  "providers": 1,
  "workspaces": 1,
  "jobs": 1
}
```

`errors` covers blank or unknown provider CLIs and names, unknown workspace and
provider references in workspaces, routes, and jobs, relative workspace paths, a
workspace path that is not a directory, invalid route keys and job names, unknown
`mention` modes, zero timeouts, a blank `slack.working_reaction`, blank
`SLACK_BOT_TOKEN` or `SLACK_APP_TOKEN`, each invalid job, and an `enso.db` from an older release, which it reads
without changing.
`notes` are not errors: no configured `dms` or `channels`, or a Codex provider
while the home is not a git repository. With an empty `slack.unconfigured_message`, the no-routes
note says Enso will ignore every message instead. `jobs` counts job directories,
valid or not. A `config.json` that cannot be loaded at all is reported as one
error; `providers` and `workspaces` are then 0, and jobs are not validated.
The report never includes token values or other substituted values. The
command exits non-zero when there are errors, ending with an error such as
`config check found 3 errors`. `service install` checks only the configuration
and tokens: it loads the configuration with only the environment the service will
have (`.env` plus `HOME`, `PATH`, and a few CLI variables) and refuses on any
configuration or token error, so a `${NAME}` that only your shell defines must
be set in `.env`. Invalid jobs and missing workspace directories do not block
it; they fail only their own runs. It does not check `enso.db`; service startup
refuses one from an older release.

`jobs list` shows each valid job's `enabled`, `cron`, `next_run`, and
`last_run`, and each [invalid job](jobs.md#invalid-jobs) as
`{"name": ..., "error": ..., "last_run": ...}` without failing. `jobs run NAME`
on an invalid job fails with that job's error.

## Messages and files

```sh
enso message send "Report ready" --to D012345
enso message send --text-file report.md --file chart.png --to C012345
printf '%s' 'A threaded update' | enso message send - --to C012345 --thread 1234567890.123456
enso message send "Weekly metrics" --blocks metrics.json --to C012345
```

Use repeatable `--file` for outgoing attachments. `--text-file` supplies message
text; it does not attach the file. `--json` prints a machine-readable receipt.
Receipts include delivery state and a Slack reference: a message timestamp for
text, or a remote file ID for attachments. File receipts also include
`message_ts` when Slack has exposed the associated message; successful upload
does not depend on that optional timestamp being available immediately.
An explicit destination wins; otherwise a Slack turn replies to its
conversation and a job posts to its `notify` destination. Without either, such
as in a terminal or a job without `notify`, `--to` is required. Destinations are
Slack conversation IDs (`C…`, `G…`, or `D…`) and thread root timestamps. Confirmed background sends become context for the
next conversation turn.

Message text and replies are standard Markdown, which Slack renders in
`markdown` blocks, including tables and task lists. Enso escapes `&` and `<`
outside code so Slack syntax such as `<@U…>` or `<!channel>` stays literal. It
splits text over Slack's 12,000-character message limit between Markdown
blocks; an oversized code block or table repeats its opening fence or header in
each part. `--plain` sends text without Markdown rendering. `--blocks PATH`
sends a JSON array of Block Kit blocks, or a Block Kit Builder
`{"blocks": [...]}` payload, as given for native charts (`data_visualization`),
sortable tables (`data_table`), or layouts Markdown cannot express. The message
text is required as its notification fallback. Blocks are not escaped, and Enso
does not handle interactive callbacks such as button clicks.

Incoming attachments are downloaded into `uploads/<run-id>/` inside the run's
workspace with safe filenames. The current prompt includes their paths. An
incoming message supports up to 20 files; individual incoming or outgoing files
are limited to 50 MiB.
A failed download reports an error before starting the agent.

## Slack lookup and reactions

`enso slack` commands call the Web API directly using the Slack tokens from
`.env` or the environment; any assignment in `.env`, even an empty one, wins
over an exported token. They work without a running Enso service or a Socket
Mode connection and always return JSON; global `--json` makes it compact. Incoming-message routes do not limit
these calls: the token's Slack permissions and conversation access determine
what is available. Sending text and attachments still uses `enso message send`.

```text
enso slack channels [--limit 100] [--cursor CURSOR] [--types public_channel,private_channel,im,mpim]
enso slack channel CHANNEL
enso slack users [--limit 100] [--cursor CURSOR]
enso slack user USER
enso slack history CHANNEL [--limit 50] [--cursor CURSOR] [--oldest TS] [--latest TS]
enso slack thread CHANNEL ROOT_TS [--limit 50] [--cursor CURSOR] [--oldest TS] [--latest TS]
enso slack message CHANNEL TS [--thread ROOT_TS]
enso slack search QUERY [--limit 50] [--cursor CURSOR]
enso slack link CHANNEL TS
enso slack react CHANNEL TS EMOJI [--remove]
```

Use IDs returned by channel/user lookup. Treat Slack timestamps as strings and
preserve every digit. A message lookup can specify `--thread` when its timestamp
belongs to a reply. Reactions use emoji names such as `thumbsup`, without colons;
`--remove` removes only the bot's own reaction.
Follow returned cursors for additional pages; one page is not an entire channel
or thread, and Slack may return fewer items than the requested limit. History
and thread responses keep Slack's `has_more` and
`response_metadata.next_cursor` fields. Limits may be at most 200, or 100 for
search.

`enso slack search "in:engineering from:me release"` uses Slack's own
`search.messages` query syntax; add `in:channel_name` to search one channel. It
requires the optional `SLACK_USER_TOKEN` with `search:read`; a bot token alone
cannot search. See [search setup](configuration.md#workspace-search). The
response is Slack's own, including `messages.matches` and its pagination fields.
To look through one conversation without a user token, page through `history` or
`thread` instead.

```sh
enso slack history C012345 --limit 20
enso slack thread C012345 '1791295200.000100'
enso slack search "in:engineering release notes" --limit 50
enso slack link C012345 '1791295200.000100'
enso slack react C012345 '1791295200.000100' eyes
```

## Slack controls

| Command | Behavior |
|---|---|
| `!clear` | Clear an idle conversation's native session for the next turn |
| `!stop` | Cancel its running turn and queued turns |
| `!status` | Show the conversation's provider (CLI, model, effort), workspace, running/queued counts, and session state |
| `!help` | Show the controls |

```text
Enso: opus (claude / opus / high)
Workspace: acme
Running: 0 · queued: 0
Session: active
```

`!status` shows `native default` for an unset model or effort.

Enso adds a working reaction while a turn runs, acknowledges queued turns, and
reports failures and timeouts. Thread participation survives `!clear`.
