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
or the job's `workspace`. Hooks run in their job directory. Incoming attachments
are saved under the workspace's `uploads/<run-id>/`.

Chat turns are serialized within each conversation. Each DM shares one session
across its threads; channel threads have independent sessions. Different jobs
and conversations run independently, with no global concurrency limit or setting.
A job's scheduled occurrence is skipped if that job is already queued or running.
A run records its provider name, CLI, executable, model, effort, arguments,
timeout, workspace name and path, and injected metadata. The workspace and
provider are selected on each turn. A conversation's native session is pinned to
the CLI and workspace path that created it: switching to another provider with
the same CLI, or to another workspace name with the same path, keeps the
session. A different CLI or workspace path fails the turn until `!clear` starts
a fresh session. Jobs start a fresh session every run.

## Prompt context

The first Slack turn gets concise delivery and workspace guidance. Every turn
gets fresh structured metadata identifying its source, provider, workspace
(`{"name", "path"}`), sender, channel, message, reply destination, and
attachments. Replies can follow a DM thread while sharing the DM's single
session. Channel threads retain independent sessions.

Jobs always get job-specific guidance and metadata. Rendered `prompt.md` is the
user request. Confirmed background messages appear separately as previous context,
with their source and time, before the next user request. Names and filenames are
data; metadata serialization never turns them into shell code. Credentials are
never part of prompt context.

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
