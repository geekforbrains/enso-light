# Runtime and development

The service owns one Slack Socket Mode connection, a shared CLI runner, job
scheduling, and outgoing delivery. Job triggers and message sends submit work
through SQLite. `enso slack` lookups and reactions call the Web API directly and
do not need the service; reactions do not enter the message delivery queue.
There is no local HTTP server or extra broker.

`config.json`, `.env`, and job directories own configuration. `enso.db` owns
conversations, native session references, messages, attachments, run status,
delivery status, and scheduling cursors. Native CLIs own authentication, tools,
permissions, and their underlying session files. Each agent runs in a
configured workspace directory: the workspace a Slack conversation is routed to,
or the job's `workspace`. Service startup creates missing workspace directories
with starter instructions and skill links; existing directories are left as
they are. Hooks run in their job directory. Incoming attachments
are saved under the workspace's `uploads/<run-id>/`. Enso adds no settings overrides;
each CLI loads its own configuration as usual.

Chat turns are serialized within each conversation. Each DM shares one session
across its threads; channel threads have independent sessions. Different jobs
and conversations run independently, with no global concurrency limit or setting.
A job's scheduled occurrence is skipped if that job is already queued or running.
A run records its provider name, CLI, executable, model, effort, arguments,
timeout, workspace name and path, and injected metadata. The workspace and
provider are selected on each turn. A conversation's native session is pinned to
the CLI and workspace path that created it: switching to another provider with
the same CLI, or to another workspace name with the same path, keeps the
session. Paths are compared after removing redundant separators, so
`/x/acme/` and `/x/acme` are the same workspace. A different CLI or workspace
path fails the turn until `!clear` starts a fresh session. Jobs start a fresh
session every run.

A failed turn's error, which is also the Slack reply, says only that the CLI
exited with a status or reported an error, or that its session could not be
resumed and `!clear` starts fresh. The last lines of the CLI's own output go to
the service log (`enso service logs`) with `.env` values and Slack tokens
redacted.

## Prompt context

Enso adds no instructions of its own; guidance lives in the home's `AGENTS.md`,
which you edit. Each prompt is an `<enso-context>` JSON block, any background
messages, and the request. The block has `source` (`slack` or `job`). Slack turns
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
