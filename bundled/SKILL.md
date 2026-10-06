---
name: enso
description: Use Enso's Slack messaging, attachments, scheduled jobs, and local CLI from its shared workspace.
---

# Enso

Enso runs your installed agent CLI in one workspace. Each turn's injected context
identifies Slack versus a job, the sender and destination, and local attachments.
DMs share one session, including DM threads. Channel threads have separate sessions.

For Slack turns, your final response is sent automatically to the current reply
destination. Use ordinary Markdown. Do not send that same answer again with the CLI.
Incoming attachments are local files under `uploads/`; inspect the supplied paths.

## Send additional messages or files

```sh
enso message send "An update"
enso message send --text-file report.md --file chart.png
enso message send "Report ready" --to C012345 --thread 1234567890.123456
```

Use `--file PATH` repeatedly to attach files; `--text-file PATH` or stdin supplies
message text. Without `--to`, Enso uses this run's reply or job notification target.
Use an explicit destination for another conversation. The service must be running.
Only send additional/background messages when the user's request calls for them.

## Jobs

```sh
enso jobs list
enso jobs run JOB
```

Trigger jobs without `--wait` inside an Enso run; waiting there is rejected to
keep workers from waiting on one another. Operators can use `--wait` from a shell.

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
