---
name: enso
description: Use Enso's Slack lookup, messaging, attachments, scheduled jobs, and local CLI
---

# Enso

The `enso` CLI reaches Slack and jobs from inside a run. Enso sets
`$ENSO_WORKSPACE` (your working directory), `$ENSO_SOURCE` (`slack` or `job`),
`$ENSO_RUN_ID`, `$ENSO_JOB`, and `$ENSO_CHANNEL` and `$ENSO_THREAD_TS`: this
Slack conversation, or the job's `notify` destination. Incoming attachments are
under `$ENSO_WORKSPACE/uploads/$ENSO_RUN_ID/`.

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

## Send messages or files

```sh
enso message send "An update"
enso message send --text-file report.md --file chart.png
enso message send "Report ready" --to C012345 --thread 1234567890.123456
enso message send "Weekly metrics: signups up 12%" --blocks metrics.json
```

Without `--to`, messages go to `$ENSO_CHANNEL` and `$ENSO_THREAD_TS`; a job
without `notify` must name `--to`. Find other IDs with `enso slack channels`.
"Here" means `$ENSO_CHANNEL`, and its thread only when the user means this
thread. Use `--file PATH` repeatedly to attach files; `--text-file PATH` or
stdin supplies the text. Text is standard Markdown; Slack renders it, including
tables. The service must be running.

When Markdown cannot express what you need, such as native charts
(`data_visualization`) or sortable tables (`data_table`), write Block Kit JSON
after reading the block's page at https://docs.slack.dev/reference/block-kit/blocks
and send it with `--blocks PATH`; the text becomes its notification fallback.
Blocks are sent as given, so mentions in them notify people, and interactive
callbacks such as button clicks are not handled.

## Jobs

```sh
enso jobs list
enso jobs run JOB
enso jobs run JOB --wait
```

`--wait` returns the job's result; use it when the current task needs it.
Without it, the job is queued and keeps running after your turn. A job already
queued or running rejects another trigger. `enso jobs list` shows an invalid job
with its `error`; run `enso config check` after creating or editing a job.

A job is a directory `$ENSO_HOME/jobs/NAME/` (letters, numbers, `-`, `_`):

- `prompt.md` (required): the request
- `job.json`: `workspace` (required; a name from `workspaces` in `config.json`),
  `cron` (five fields, local time; omit for manual-only), `enabled` (default
  true), `provider` (defaults to the workspace's, then `defaults.provider`),
  `timeout_seconds`, `notify` (`{"channel": "C…"}` or `D…` for a DM, plus
  `"thread": "TS"`; omit for a job that never posts), and `retries` (0–10)
- `prerun.sh` (optional): stdout `{"vars": {"NAME": "value"}}` fills `{{NAME}}`
  in the prompt, or `{"skip": true, "reason": "Nothing new"}` skips the run
- `postrun.sh` (optional): runs after each attempt with the result on stdin;
  empty stdout accepts, `{"retry": true, "message": "What to fix"}` resumes the
  agent with that message while `retries` remain

Hooks run with Bash in the job directory. Send diagnostics, and commands that
print such as `enso message send`, to stderr with `>&2`. Job output is saved,
not posted; a job that may retry should notify from postrun after accepting.
Details: https://github.com/geekforbrains/enso-light/blob/main/docs/jobs.md

## Instructions and skills

`$ENSO_HOME/AGENTS.md` and `$ENSO_HOME/.agents/skills/` apply to every
workspace inside the Enso home; a workspace's own `AGENTS.md` and
`.agents/skills/` apply only to it, so put a skill there when only it needs one.
`CLAUDE.md` and `.claude/skills` link to the same files for Claude Code.

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
