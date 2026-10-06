---
name: enso
description: Use Enso's Slack lookup, messaging, attachments, scheduled jobs, and local CLI
---

# Enso

Enso runs your installed agent CLI in one workspace. Each turn's injected context
identifies Slack versus a job, the sender and destination, and local attachments.
DMs share one session, including DM threads. Channel threads have separate sessions.
Messages in a busy conversation queue in order. Different conversations and jobs
run in parallel with no global concurrency limit or setting.

For Slack turns, your final response is sent automatically to the current reply
destination. Use ordinary Markdown. Do not send that same answer again with the CLI.
Incoming attachments are local files under `uploads/`; inspect the supplied paths.

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
needed. Use `channel ID` or `user ID` for details and `react ... --remove` to
remove a reaction. Preserve timestamps as strings. Follow returned cursors for
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
When Markdown cannot express what you need, such as a native table, chart, or
layout, write Block Kit JSON (https://docs.slack.dev/reference/block-kit/blocks)
and send it with `--blocks PATH`; the text becomes its notification fallback.
Blocks are sent as given, so mention syntax in them notifies people, and
interactive callbacks such as button clicks are not handled. Without `--to`,
Enso uses this run's reply or job notification target. Use an explicit
destination for another conversation. The service must be running.
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
`job.json` sets `cron`, `enabled`, optional `execution` overrides and `notify`.
Runs use the shared workspace and fresh native sessions. Optional `prerun.sh` and
`postrun.sh` run with Bash from the job directory. Prerun stdout provides JSON;
return `{"vars":{"NAME":"value"}}` to expand `{{NAME}}` in the prompt or
`{"skip":true,"reason":"Nothing new"}` to skip. Diagnostics go to stderr.
Postrun stdin contains run context plus `variables`, `result`, `status`, and
`error`. Job output is saved, not sent to Slack automatically. Use
`enso message send` when the job asks for a notification.

## Configuration and status

`$ENSO_HOME` defaults to `~/.enso`. `config.json` has default execution and Slack
settings; `.env` supplies child-process variables and `${NAME}` substitutions in
configuration. Keep secrets in `.env`, never in prompts or replies. Restart after
changing `config.json` or `.env`; job files are reread each minute and at run start.
`enso config check`, `enso service status`, and
`enso service logs` diagnose operation. `enso --help` shows the command syntax.

Slack supports `!clear`, `!stop`, `!status`, and `!help`. These are user controls;
do not edit the state database to simulate them.
