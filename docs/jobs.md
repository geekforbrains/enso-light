# Jobs

Each job is a directory at `~/.enso/jobs/NAME/`:

```text
job.json       # workspace, scheduling, provider, timeout, and notification
prompt.md      # required user message
prerun.sh      # optional
postrun.sh     # optional
```

Names use letters, numbers, hyphens, and underscores. `prompt.md` must be nonempty.

```json
{
  "enabled": true,
  "cron": "0 9 * * 1-5",
  "workspace": "main",
  "provider": "opus",
  "timeout_seconds": 600,
  "notify": { "channel": "D012345" },
  "retries": 2
}
```

`notify` is optional: where the job posts when it sends a message. `channel` is a
conversation ID (`C…` for a channel, `G…` for a private channel, `D…` for a DM).
Add `"thread": "1700000000.000001"` with a root message timestamp to post in that
thread. Enso checks the format when it loads the job; the bot must be able to post
there. Leave it out for a job that never posts.

`workspace` is required and names one of the configured
[workspaces](configuration.md#workspaces); the agent runs in its directory.
`provider` optionally names one of the configured
[providers](configuration.md#providers); without it the job uses the
workspace's `provider`, then `defaults.provider`. `timeout_seconds` optionally
replaces `defaults.timeout_seconds` (greater than zero) for each of the job's
processes: every agent attempt, prerun, and postrun. A job that leaves out
`workspace` or names an unknown workspace or provider is invalid.

`retries` is how many extra attempts postrun may request in one run (default 0,
at most 10). See [Postrun and retries](#postrun-and-retries).

`cron` uses five fields (minute, hour, day of month, month, day of week) and the
machine's timezone. Sunday is `0` or `7`; weekday names are supported. When both
day-of-month and day-of-week are restricted, standard cron matches either
restriction. Omit `cron` for a manual-only job. `enabled` defaults to true and
controls scheduling; a disabled job can still be triggered manually. Missed
occurrences during downtime are not replayed. A job never overlaps itself:
scheduled occurrences while it is queued or running are skipped, and manual
triggers while busy are rejected.

```sh
enso jobs list
enso jobs run morning-report
enso jobs run morning-report --wait
```

`--wait` waits for the job's result and can be used from a shell, agent, or hook.
Without it, the command returns once the job is queued; the service owns the run,
so it continues after the calling shell or agent turn ends, and its completion
does not resume that turn. Job files are reread each scheduler minute and at run
start, so changes need no restart; `config.json` and `.env` changes do.

Every run starts a fresh native session. The agent's final output is stored, not
posted to Slack; use `enso message send` from the agent or postrun to notify.
Without `--to`, it posts to `notify`, even when the job was triggered from Slack.

## Invalid jobs

An invalid job never stops other jobs or Slack. `enso config check` lists each
one's error (prefixed `job NAME:`) and exits non-zero, the service skips it and
logs its error when it first appears or changes, `enso jobs list` shows it with
an `error` field, and `enso jobs run` refuses it.

## Hooks and variables

Existing hooks run automatically with Bash; they do not need executable bits.
Both hooks run in the job directory, not the workspace, with the Enso
environment. Send diagnostics to stderr. Prerun stdout is empty or a JSON object:

```json
{"vars":{"DATE":"2026-10-06","COUNT":3}}
```

`{{DATE}}` and `{{COUNT}}` in `prompt.md` substitute prerun scalar values once.
Names must be environment variable identifiers; values are strings, numbers,
or booleans. Spaces inside `{{ NAME }}` are allowed.
Missing variables fail before the agent starts. Prerun can return a successful
skip instead:

```json
{"skip":true,"reason":"No new items"}
```

A skip runs neither the agent nor postrun, and a failed prerun stops both.
Prerun runs once per run and receives the run's context JSON on stdin, the same
`<enso-context>` block the agent gets. A job's context is
`{"source": "job", "job": {"name", "trigger", "scheduled_for"}}`: `trigger` is
`cron` or `manual`, and `scheduled_for` is set only for cron runs. Timeouts and
cancellation stop a hook's process group, and hook failures show in the job's
outcome.

The agent and hooks get `ENSO_HOME`, `ENSO_WORKSPACE` (the workspace's absolute
path), `ENSO_RUN_ID`, `ENSO_SOURCE` (`job`), `ENSO_JOB`, and `ENSO_CHANNEL` /
`ENSO_THREAD_TS` (the `notify` destination, empty without one). Enso sets these
over any inherited or `.env` values.

### Postrun and retries

Postrun receives the prerun context plus `variables` (prerun values), `result`
(agent output), `status`, `error` (null on success), `attempt` (starting at 1),
and `max_attempts`. It runs after every agent attempt, including failure,
unless the run was cancelled. Its stdout is empty or a JSON object. Empty or
`{}` accepts the attempt, and the run ends with the agent's status. To request
another attempt:

```json
{"retry":true,"message":"The chart is missing; regenerate it."}
```

A retry starts immediately in the same run, with the same run ID, context,
variables, and settings; prerun does not run again. The agent resumes the
previous attempt's session, and its request is a retry note followed by
`message`. An attempt that failed or timed out leaves no session, so its retry
starts a fresh session with the original request followed by the note. Once
`retries` are used up, a retry request fails the run with the message in its
error. A retried run records each attempt's `status`, `error`, and `retry`
message under `attempts` in its run record.

Each attempt and its postrun get their own timeout, and the job stays busy, so
scheduled occurrences are skipped meanwhile. Messages sent during an attempt are
not withdrawn; a job that may retry should notify from postrun after accepting.
In either hook, commands that print to stdout, such as `enso message send`,
need `>&2`. Other postrun stdout, a non-zero exit, or a timeout fails the run
without a retry.
