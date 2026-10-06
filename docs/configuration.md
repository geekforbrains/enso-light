# Configuration

The default home is `~/.enso`; `--home PATH` selects another home.

```text
~/.enso/
├── config.json
├── .env
├── enso.db
├── skills/
│   └── enso/SKILL.md
├── workspace/
│   ├── AGENTS.md
│   ├── CLAUDE.md -> AGENTS.md
│   ├── .agents/skills -> ../../skills
│   ├── .claude/skills -> ../../skills
│   └── uploads/
├── jobs/
└── logs/
```

`enso init` creates missing directories and starter files without overwriting
existing content. Credentials and generated files use private permissions. All
agent processes run in `workspace/`; job hooks run in their job's directory.
`skills/` holds agent skills; Claude Code and Codex find them through the
workspace links. Add your own skills beside `enso`.

## config.json

```json
{
  "execution": {
    "cli": "claude",
    "model": "sonnet",
    "effort": "high",
    "args": [],
    "timeout_seconds": 1800
  },
  "slack": {
    "bot_token": "${SLACK_BOT_TOKEN}",
    "app_token": "${SLACK_APP_TOKEN}",
    "dm_users": ["U012345"],
    "channels": {},
    "mentions": { "top_level": true, "thread": true }
  }
}
```

`cli` is `claude` or `codex`. The executable normally comes from `PATH`; set
`execution.executable` to an explicit path if needed. Optional `model` and
`effort` may be `null` to use native defaults; Codex also uses native defaults
when these fields are omitted. `args` are literal additional
arguments, not shell code. Your installed CLI must already be authenticated;
configure its permissions for unattended operation through native settings or
explicit arguments. Enso does not manage provider credentials.

Slack accepts DMs only from `dm_users`, and channels only when their IDs appear in
`channels`. Empty allowlists accept nothing. Add a channel with its mention rules:

```json
"channels": {
  "C012345": { "top_level": true, "thread": false }
}
```

Omitted per-channel mention fields inherit `slack.mentions`, whose defaults
require mentions at both levels.

When thread mentions are disabled, unmentioned replies are accepted only in a
thread the bot has already joined. DMs never need mentions. A DM and all its
threads share a native session; each channel thread has its own session.

Other optional Slack settings:

| Field | Default / purpose |
|---|---|
| `working_reaction` | `thinking_face` |
| `queued_message` | Acknowledges a turn queued behind another |
| `timeout_message` | Explains that a turn timed out |
| `notify` | Optional default `{ "channel": "D012345", "thread": null }` destination for background sends |
| `user_token` | Optional user OAuth token for workspace message search; omit unless configured |

## Environment

```dotenv
SLACK_BOT_TOKEN=xoxb-...
SLACK_APP_TOKEN=xapp-...
```

Enso loads `.env` for agent CLIs and hooks, overriding inherited values of the
same name. `${NAME}` in configuration string values uses this combined
environment. Substitution happens once, with no shell evaluation; missing
variables are errors. Quote literal values in `.env` using standard dotenv
syntax. Never put credentials in prompts, starter guidance, or source control.
Restart after changing `config.json` or `.env`.
Direct `enso slack` commands reload these files on each invocation.

## Slack app

Use a Slack app with Socket Mode enabled and an app-level token with
`connections:write`. Install its bot with these scopes:

| Scope | Used for |
|---|---|
| `chat:write` | Replies and `enso message send` |
| `files:write` | Outgoing attachments |
| `files:read` | Incoming attachments and upload receipts |
| `reactions:write` | The working reaction and `enso slack react` |
| `users:read` | Sender names and user lookup |
| `im:read`, `im:history` | DMs |
| `app_mentions:read` | Channel mentions |
| `channels:read`, `channels:history` | Public channels |
| `groups:read`, `groups:history` | Private channels |
| `mpim:read`, `mpim:history` | Optional group DM lookups |

Subscribe to the `message.im`, `app_mention`, `message.channels`, and
`message.groups` bot events, and invite the bot to any configured channel. A
DM-only installation can omit `app_mentions:read`, the `channels` and `groups`
scopes, and their events. Enso does not use other scopes such as
`chat:write.public`, `im:write`, or `users:read.email`.

Only one service should consume the same Socket Mode app token. Enso preserves
Slack event IDs to ignore duplicate deliveries. Use `enso service status` to
check both the service and Slack connection; process startup alone is not a
successful Slack connection.

The [Slack CLI commands](cli.md#slack-lookup-and-reactions) use the bot token for
channel/user lookup, history, threads, links, and reactions, so they need the
matching scopes above and access to the conversation. Incoming `dm_users` and
`channels` settings control which messages start agent turns; they do not
restrict direct Web API lookups or reactions.

## Workspace search

Search without `--channel` uses Slack's `search.messages` method, which requires
a **user OAuth token** with `search:read`. Its results follow that user's Slack
access and search settings. [Slack's method reference](https://docs.slack.dev/reference/methods/search.messages/)
documents the supported query and pagination behavior.

To enable it, add `"user_token": "${SLACK_USER_TOKEN}"` to `slack` in
`config.json`, then set `SLACK_USER_TOKEN` in `.env`. Keep it absent when no
authorized user token is available. Adding this optional key does not change
the bot token or Socket Mode connection.

The existing bot setup is enough for `search --channel CHANNEL`, which scans
one page of accessible history with a literal text filter. That operation does
not enable workspace-wide Slack search; pass the returned cursor to scan more.
