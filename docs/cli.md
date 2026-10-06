# CLI and installation

```text
enso [--home PATH] init
enso [--home PATH] run
enso config check
enso jobs list
enso jobs run NAME [--wait]
enso message send [TEXT|-] [--text-file PATH] [--file PATH] [--to CHANNEL] [--thread TS] [--plain|--blocks PATH] [--json]
enso slack channels|channel|users|user|history|thread|message|search|link|react ...
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
enso message send "Weekly metrics" --blocks metrics.json --to C012345
```

Use repeatable `--file` for outgoing attachments. `--text-file` supplies message
text; it does not attach the file. `--json` prints a machine-readable receipt.
Receipts include delivery state and a Slack reference: a message timestamp for
text, or a remote file ID for attachments. File receipts also include
`message_ts` when Slack has exposed the associated message; successful upload
does not depend on that optional timestamp being available immediately.
An explicit destination wins; otherwise the current chat's reply destination,
job notification destination, or configured Slack notification destination is
used. A missing destination is an error. Confirmed background sends become
context for the next conversation turn.

Message text and replies are standard Markdown, which Slack renders in
`markdown` blocks, including tables and task lists. Enso escapes `&` and `<`
outside code so Slack syntax such as `<@U…>` or `<!channel>` stays literal. It
splits text over Slack's 12,000-character message limit between Markdown
blocks; an oversized code block or table repeats its opening fence or header in
each part. `--plain` sends text without Markdown rendering. `--blocks PATH`
sends a JSON array of Block Kit blocks, or a Block Kit Builder
`{"blocks": [...]}` payload, as given for native charts (`data_visualization`),
sortable tables (`data_table`), or layouts Markdown cannot express. The message text is required as its notification
fallback. Blocks are not escaped, and Enso does not handle interactive callbacks
such as button clicks.

Incoming attachments are downloaded into `workspace/uploads/<run-id>/` with safe
filenames. The current prompt includes their paths. An incoming message supports
up to 20 files; individual incoming or outgoing files are limited to 50 MiB.
A failed download reports an error before starting the agent.

## Slack lookup and reactions

`enso slack` commands call the Web API directly using configured tokens. They
work without a running Enso service or a Socket Mode connection and always return
JSON; global `--json` makes it compact. Incoming-message allowlists do not limit
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
enso slack search QUERY [--channel CHANNEL] [--thread ROOT_TS] [--limit 50] [--cursor CURSOR]
enso slack link CHANNEL TS
enso slack react CHANNEL TS EMOJI [--remove]
```

Use IDs returned by channel/user lookup. Treat Slack timestamps as strings and
preserve every digit. A message lookup can specify `--thread` when its timestamp
belongs to a reply. Reactions use emoji names such as `thumbsup`, without colons.
Follow returned cursors for additional pages; one page is not an entire channel
or thread, and Slack may return fewer items than the requested limit. History
and thread responses keep Slack's `has_more` and
`response_metadata.next_cursor` fields. Limits may be at most 200, or 100 for
search.

Search has two modes:

- `enso slack search "release notes" --channel C012345` scans one page of
  channel history for a literal, case-insensitive text match. Add `--thread`
  to scan a thread instead. Results include scanned count and coverage/pagination
  information (`scanned`, `scope`, `has_more`, and `next_cursor`). The limit
  requests a page size, not a match count; `scanned` counts the actual messages
  returned, including a thread's parent when Slack adds it to the page.
  Channel history does not include
  unreturned thread replies; read the relevant thread separately.
  Continue with the returned cursor when more history is needed;
  an empty page of matches does not establish that the whole channel has none.
- `enso slack search "in:engineering from:me release"` uses Slack's native
  query syntax and requires the optional `slack.user_token` with `search:read`.
  A bot token alone cannot perform this workspace search. See
  [search setup](configuration.md#workspace-search). Results retain Slack's
  `messages.matches` and native pagination fields.

```sh
enso slack history C012345 --limit 20
enso slack thread C012345 '1791295200.000100'
enso slack search "release notes" --channel C012345 --limit 50
enso slack link C012345 '1791295200.000100'
enso slack react C012345 '1791295200.000100' eyes
```

## Slack controls

| Command | Behavior |
|---|---|
| `!clear` | Clear an idle conversation's native session for the next turn |
| `!stop` | Cancel its running turn and queued turns |
| `!status` | Show execution settings and running/queued state |
| `!help` | Show the controls |

Enso adds a working reaction while a turn runs, acknowledges queued turns, and
reports failures and timeouts. Thread participation survives `!clear`.
