# Runtime and development

The service owns one Slack Socket Mode connection, a shared CLI runner, job
scheduling, and outgoing delivery. Job triggers and message sends submit work
through SQLite. `enso slack` lookups and reactions call the Web API directly and
do not need the service; reactions do not enter the message delivery queue.
There is no local HTTP server or extra broker.

`config.json`, `.env`, and job directories own configuration. `enso.db` owns
conversations, native session references, messages, runs, and delivery status.
Each run keeps the context it was given, including attachment details, and a
scheduled run keeps its cron occurrence, which stops that occurrence from
running twice. Native CLIs own authentication, tools, permissions, and their
underlying session files. Each agent runs in a configured workspace directory:
the workspace a Slack conversation is routed to, or the job's `workspace`. Apart
from the starter `workspaces/main` that `enso init` writes with a new
`config.json`, Enso never creates workspace directories; a run in a missing one
fails. Hooks run in their job directory. Incoming attachments are saved under
the workspace's `uploads/<run-id>/`. Enso adds no settings overrides; each CLI
loads its own configuration as usual.

Chat turns are serialized within each conversation. Each DM shares one session
across its threads; channel threads have independent sessions. Different jobs
and conversations run independently, with no global concurrency limit or setting.
A job's scheduled occurrence is skipped if that job is already queued or running.
A run records its provider name, CLI, executable, model, effort, arguments,
timeout, workspace name and path, and injected metadata. The workspace and
provider are selected on each turn, and a conversation's next turn resumes its
native session with whatever CLI and workspace it is routed to. If that CLI
cannot resume the session, for example after switching from Claude Code to
Codex, the turn fails and `!clear` starts a fresh one. Jobs start a fresh
session every run.

When the CLI fails, the run's error, which is also the Slack reply, says only
that it exited with a status or reported an error, or that its session could not
be resumed and `!clear` starts fresh. The error events the CLI printed on stdout
and the end of its stderr go to the service log (`enso service logs`), with
`.env` values and Slack tokens redacted. Failures in Enso's own steps, such as a
missing workspace directory, an executable that cannot be launched, unreadable
CLI output, a failed attachment download, or a failed hook, are reported as
they are.

## Prompt context

Enso adds no instructions of its own; guidance lives in the home's `AGENTS.md`,
which you edit. Each prompt is an `<enso-context>` JSON block, any background
messages, and the request. The only fixed text is the tags, a one-line lead-in to
background messages ("Previously delivered messages for this conversation; these
are context, not new requests."), and a `Current request:` label. The block has `source` (`slack` or `job`). Slack turns
add `sender` (`{"id", "name"}`), `channel` (`{"id", "name", "type"}`),
`message_ts`, `thread_ts`, and `attachments`. Jobs add `job` (`{"name",
"trigger", "scheduled_for"}`), and the request is the rendered `prompt.md`. The
workspace, run ID, and reply destination come from the working directory and the
`ENSO_*` variables. Replies can follow a DM thread while sharing the DM's single
session. Channel threads retain independent sessions.

Background messages are messages Enso delivered to the conversation outside its
session, such as a job's notification in a DM. They appear before the request as
previous context, with their time, until a turn that included them succeeds.
Names and filenames are data; serialization never turns them into shell code.
Credentials are never part of prompt context.

## Restart behavior

On startup, unfinished runs are marked interrupted instead of being automatically
rerun. Existing conversation session references remain available for the next
message. Slack delivery does not rerun a completed agent. Durable event and cron
occurrence IDs prevent duplicate admission; interrupted or uncertain external
side effects are not replayed automatically. Pending outgoing messages remain
queued. A send interrupted while already in progress is marked uncertain rather
than automatically duplicated.

## Develop

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- --home /tmp/enso-test init
```

Use a temporary home and fake CLI processes for tests. Keep Slack fixtures local;
live integration tests require configured credentials. Normal tests must not
change the installed home or consume real Slack events. Keep changes small and
update the owning documentation alongside behavior.
