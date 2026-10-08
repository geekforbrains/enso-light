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
existing content. Credentials and generated files use private permissions. Each
agent process runs in its [workspace](#workspaces)'s directory; job hooks run in
their job's directory. The starter `main` workspace is `workspace/`. `skills/`
holds agent skills; Claude Code and Codex find them through the workspace links.
Add your own skills beside `enso`.

## config.json

```json
{
  "defaults": {
    "provider": "main",
    "mention": "always",
    "timeout_seconds": 1800
  },
  "providers": {
    "main": { "cli": "claude", "model": "sonnet", "effort": "high" },
    "opus": { "cli": "claude", "model": "opus", "effort": "high", "args": ["--dangerously-skip-permissions"] },
    "codex": { "cli": "codex", "executable": "${ENSO_HOME}/bin/codex" }
  },
  "workspaces": {
    "main": { "path": "${ENSO_HOME}/workspace" },
    "acme": { "path": "${HOME}/Projects/acme", "provider": "codex" }
  },
  "slack": {
    "dms": { "U012345": "main" },
    "channels": {
      "C012345": "acme",
      "C067890": { "workspace": "acme", "mention": "first" }
    }
  }
}
```

Unknown keys are errors, so a configuration from an older release fails to load
instead of being misread.

### defaults

`defaults` holds the values that something more specific can override:

| Field | Default | Overridden by |
|---|---|---|
| `provider` | required; must name a provider | workspace `provider`, then job `provider` |
| `mention` | `always`; see [mention modes](#mention-modes) | channel `mention` |
| `timeout_seconds` | `1800`; must be greater than zero | job `timeout_seconds` |

The timeout applies to each process separately: every agent attempt, prerun,
and postrun. Attachment downloads stop after the timeout or 120 seconds,
whichever is shorter.

### providers

Each provider is a named agent CLI setup; define at least one. A run uses its
job's `provider`, else its workspace's `provider`, else `defaults.provider`.

| Field | Purpose |
|---|---|
| `cli` | Required: `claude` or `codex`. A blank value is an error. |
| `model` | Optional model name |
| `effort` | Optional reasoning effort |
| `executable` | Optional path; otherwise the `cli` name is found on `PATH` |
| `args` | Optional literal extra arguments (default `[]`), not shell code |

A left-out or blank (`""`) `model`, `effort`, or `executable` uses the CLI's own
default. `model` and `effort` must not start with `-`. Your installed CLI must
already be authenticated; configure its permissions for unattended operation
through native settings or explicit `args`. Enso does not manage provider
credentials.

### workspaces

Each workspace is a named directory where agents run; define at least one.
Names use letters, numbers, hyphens, and underscores.

| Field | Purpose |
|---|---|
| `path` | Required absolute directory, after `${NAME}` substitution. Use `${ENSO_HOME}/...` for a directory inside the Enso home. |
| `provider` | Optional provider name for runs in this workspace; blank or left out uses `defaults.provider` |

Two workspaces may share a path, for example to use one repository with
different providers. The directory must exist when a run starts; otherwise the
run fails. Each run sets `ENSO_WORKSPACE` to the absolute path, and incoming
attachments go to `uploads/<run-id>/` inside it, readable only by the Enso
user.

### slack

`dms` and `channels` route conversations to workspaces. Enso runs nothing for
a conversation they do not route; see
[unconfigured conversations](#unconfigured-conversations).

```json
"slack": {
  "dms": { "U012345": "main", "*": "main" },
  "channels": {
    "C012345": "acme",
    "C067890": { "workspace": "acme", "mention": "first" },
    "*": "main"
  }
}
```

- `dms` maps a Slack user ID (`U…` or `W…`) to a workspace name. DMs never need
  mentions.
- `channels` maps a channel ID (`C…` or `G…`) to a workspace name, or to an
  object with `workspace` and an optional `mention` mode that replaces
  `defaults.mention`.
- `"*"` matches any user or channel not listed; an exact ID wins over `"*"`.
- Every named workspace must exist. Empty `dms` and `channels` route nothing.

`"*"` lets anyone who can reach the bot use it: any member who can DM the app,
or any channel it is invited to, runs agents with your CLI's permissions in that
workspace. Prefer explicit IDs. To find your user ID, open your Slack profile
and choose **Copy member ID**.

A DM and all its threads share a native session; each channel thread has its own
session.

#### Mention modes

| Mode | Top-level messages | Thread replies |
|---|---|---|
| `always` | Need a mention | Need a mention |
| `first` | Need a mention | Need none in a thread the bot already joined |
| `never` | Need none | Need none in a thread the bot already joined |

Other thread replies need a mention.

#### Unconfigured conversations

When no route matches a DM, or an @mention in a channel, Enso replies once per
message with `unconfigured_message` and the ID to add, in the DM (following its
thread) or in the mention's thread:

```text
Enso isn't set up for this conversation.

To enable it, add `C0ABC123` to `slack.channels` in config.json.
```

DMs name the sender's user ID and `slack.dms`, so DMing the bot is a quick way
to learn your own user ID. Anyone who can reach the bot can see this reply, and
with it their own user ID or the channel ID; nothing runs, and no conversation
starts. Unmentioned messages in an unrouted channel get no reply, and neither do
routed channels whose mention mode does not match. Set `unconfigured_message` to
`""` to stay silent instead; Enso then neither replies nor records the event, so
a redelivery after you add the route is admitted.

Other optional Slack settings:

| Field | Default / purpose |
|---|---|
| `working_reaction` | `thinking_face` |
| `queued_message` | Acknowledges a turn queued behind another |
| `timeout_message` | Explains that a turn timed out |
| `unconfigured_message` | Opens the reply to an unrouted conversation; `""` disables it |

## Environment

```dotenv
# Slack app credentials. Restart Enso after changes.
SLACK_BOT_TOKEN=xoxb-...
SLACK_APP_TOKEN=xapp-...
# Optional: user token with search:read for workspace-wide search
SLACK_USER_TOKEN=
```

Slack tokens come only from the environment, never from `config.json`.
`SLACK_BOT_TOKEN` and `SLACK_APP_TOKEN` are required; an empty
`SLACK_USER_TOKEN` counts as unset. Enso keeps all three out of logs and error
messages. Direct commands such as `enso slack` also accept tokens from the
inherited environment, but the installed service reads them only from `.env`,
so `enso service install` requires both required tokens there.

Enso loads `.env` for agent CLIs and hooks, overriding inherited values of the
same name. `${NAME}` in configuration string values uses this combined
environment, plus `${ENSO_HOME}`: the Enso home in use, which `.env` and the
inherited environment cannot override. Substitution happens once, with no shell
evaluation; missing variables are errors. Quote literal values in `.env` using
standard dotenv syntax. Never put credentials in prompts, starter guidance, or
source control.
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

Only one service should consume the same Socket Mode app token. Enso records
each Slack message's channel and timestamp, so duplicate deliveries (including a
mention sent as both `message` and `app_mention`) are handled once. Use `enso service status` to
check both the service and Slack connection; process startup alone is not a
successful Slack connection.

The [Slack CLI commands](cli.md#slack-lookup-and-reactions) use the bot token for
channel/user lookup, history, threads, links, and reactions, so they need the
matching scopes above and access to the conversation. Incoming `dms` and
`channels` routes control which messages start agent turns; they do not
restrict direct Web API lookups or reactions.

## Workspace search

Search without `--channel` uses Slack's `search.messages` method, which requires
a **user OAuth token** with `search:read`. Its results follow that user's Slack
access and search settings. [Slack's method reference](https://docs.slack.dev/reference/methods/search.messages/)
documents the supported query and pagination behavior.

To enable it, set `SLACK_USER_TOKEN` in `.env`. Leave it empty when no
authorized user token is available. It does not change the bot token or Socket
Mode connection.

The existing bot setup is enough for `search --channel CHANNEL`, which scans
one page of accessible history with a literal text filter. That operation does
not enable workspace-wide Slack search; pass the returned cursor to scan more.
