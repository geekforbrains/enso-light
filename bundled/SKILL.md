---
name: enso
description: Use Enso's Slack lookup, messaging, attachments, scheduled jobs, and local CLI
---

# Enso

Enso runs your installed agent CLI in a workspace: the directory configured for
this conversation or job, also in `$ENSO_WORKSPACE`. Each turn's injected context
identifies Slack versus a job, the workspace, the sender and destination, and
local attachments.
DMs share one session, including DM threads. Channel threads have separate sessions.
Messages in a busy conversation queue in order. Different conversations and jobs
run in parallel with no global concurrency limit or setting.

For Slack turns, your final response is sent automatically to the current reply
destination. Use ordinary Markdown. Do not send that same answer again with the CLI.
Incoming attachments are local files under the workspace's `uploads/`; inspect
the supplied paths.

## Read Slack context

```sh
enso slack channels
enso slack users
enso slack history C012345 --limit 20
enso slack thread C012345 '1791295200.000100'
enso slack message C012345 '1791295300.000200' --thread '1791295200.000100'
enso slack search "release notes" --channel C012345
enso slack link C012345 '1791295200.000100'
enso slack react C012345 '1791295200.000100' eyes
```

These return JSON directly from configured Slack access; no running service is
needed. Use `channel ID` or `user ID` for details; `react ... --remove` removes
the bot's own reaction. Preserve timestamps as strings. Follow returned cursors for
more channel, user, history, thread, or search results.

`search --channel` filters one history page using a literal, case-insensitive
match; add `--thread ROOT_TS` for a thread. Check scanned count and coverage,
then paginate before treating absence as conclusive. Channel history excludes
thread replies that were not returned; inspect those threads separately.
Search without `--channel`
uses Slack query syntax and needs an optional user token with `search:read`;
do not assume the bot's existing credentials support workspace search.

## Send additional messages or files

```sh
enso message send "An update"
enso message send --text-file report.md --file chart.png
enso message send "Report ready" --to C012345 --thread 1234567890.123456
enso message send "Weekly metrics: signups up 12%" --blocks metrics.json
```

Use `--file PATH` repeatedly to attach files; `--text-file PATH` or stdin supplies
message text. Text is standard Markdown; Slack renders it, including tables.

When Markdown cannot express what you need, write Block Kit JSON
(https://docs.slack.dev/reference/block-kit/blocks) and send it with
`--blocks PATH`; the text becomes its notification fallback. Slack draws native
charts with a `data_visualization` block (line, bar, area, or pie; at most two
per message) and sortable, paginated tables with a `data_table` block (up to
200 rows; use `raw_number` cells for numeric sorting). Read the block's
reference page before writing one. A bar chart:

```json
[{"type": "data_visualization", "title": "Signups by month",
  "chart": {"type": "bar",
    "series": [{"name": "Signups", "data": [
      {"label": "Sep", "value": 1350}, {"label": "Oct", "value": 1520}]}],
    "axis_config": {"categories": ["Sep", "Oct"], "y_label": "Accounts"}}}]
```

Every series needs one point per category; titles are at most 50 characters
and labels at most 20. A pie chart uses
`"segments": [{"label": "Twix", "value": 28}]` instead of `series` and
`axis_config`. Blocks are sent as given, so mention syntax in them notifies
people, and interactive callbacks such as button clicks are not handled.

Without `--to`, Enso uses this run's Slack conversation or the job's `notify`
destination. Use an explicit destination for another conversation. The service
must be running.
Only send additional/background messages when the user's request calls for them.

## Jobs

```sh
enso jobs list
enso jobs run JOB
```

Add `--wait` when the current task needs the job's result before continuing.
It is supported inside agent runs and hooks. Without it, the command returns as
soon as the job is queued; the service owns that job and it can continue after
the calling turn ends. A job already queued or running skips scheduled
occurrences without catch-up and rejects additional manual triggers.

Jobs live at `$ENSO_HOME/jobs/JOB/`. A required `prompt.md` is the user request;
`job.json` sets a required `workspace` (a name from `workspaces` in
`config.json`), `cron`, `enabled`, an optional `provider` (a name from
`providers`; defaults to the workspace's provider, then `defaults.provider`),
optional `timeout_seconds` per process, and
optional `notify`: where the job posts, `{"channel":"C…"}` for a channel or
`D…` for a DM, plus `"thread":"TS"` for a thread. Omit it for a job that never
posts. "Here" means this run's reply channel, and its thread only when the
user means this thread; find other IDs with `enso slack channels`.
Runs use the job's workspace and fresh native sessions. Optional `prerun.sh` and
`postrun.sh` run with Bash from the job directory. Prerun stdout provides JSON;
return `{"vars":{"NAME":"value"}}` to expand `{{NAME}}` in the prompt or
`{"skip":true,"reason":"Nothing new"}` to skip. Diagnostics go to stderr.
Postrun runs after each agent attempt; stdin contains run context plus
`variables`, `result`, `status`, `error`, `attempt`, and `max_attempts`. Its
stdout is empty to accept, or `{"retry":true,"message":"What to fix"}` to resume
the agent's session with that message right away. `retries` in `job.json`
(default 0, at most 10) limits extra attempts; then the run fails. In either
hook, redirect commands that print, such as `enso message send`, with `>&2`.
Job output is saved, not sent to Slack automatically. Use `enso message send`
when the job asks for a notification; a job that may retry should notify from
postrun after accepting, since sent messages are not withdrawn.

## Configuration and status

`$ENSO_HOME` defaults to `~/.enso`. `config.json` has `defaults`, named
`providers` and `workspaces`, and Slack settings, including `slack.dms` and
`slack.channels`, which route conversations to workspaces; `.env` holds the
Slack tokens and supplies child-process variables and `${NAME}` substitutions in
configuration. Keep secrets in `.env`, never in prompts or replies. Restart after
changing `config.json` or `.env`; job files are reread each minute and at run
start. `enso config check`, `enso service status`, and `enso service logs`
diagnose operation. `enso --help` shows the command syntax.

Slack supports `!clear`, `!stop`, `!status`, and `!help`. These are user controls;
do not edit the state database to simulate them.
