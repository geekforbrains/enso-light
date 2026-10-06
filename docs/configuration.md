# Configuration

The default home is `~/.enso`; `--home PATH` selects another home.

```text
~/.enso/
├── config.json
├── .env
├── enso.db
├── workspace/
│   ├── AGENTS.md
│   ├── CLAUDE.md -> AGENTS.md
│   ├── .skills/enso/SKILL.md
│   └── uploads/
├── jobs/
└── logs/
```

`enso init` creates missing directories and starter files without overwriting
existing content. Credentials and generated files use private permissions. All
agent processes run in `workspace/`; job hooks run in their job's directory.

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

## Slack app

Use a Slack app with Socket Mode enabled and an app-level token with
`connections:write`. Install its bot in the workspace with `chat:write`,
`reactions:write`, `files:read`, `files:write`, `users:read`, `im:read`,
`im:history`, `channels:read`, `channels:history`, `groups:read`, and
`groups:history` as needed for the configured conversations. Subscribe to
`message.im`, `app_mention`, `message.channels`, and `message.groups` events.
Invite the bot to any configured channel. DM-only installations need only the
corresponding DM capabilities, alongside chat, reactions, users, and files.

Only one service should consume the same Socket Mode app token. Enso preserves
Slack event IDs to ignore duplicate deliveries. Use `enso service status` to
check both the service and Slack connection; process startup alone is not a
successful Slack connection.
