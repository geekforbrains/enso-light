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
processes: every agent attempt, prerun, and postrun. Enso rejects a job that
leaves out `workspace` or names an unknown workspace or provider when it loads
the job: in `enso config check`, at service start, in the scheduler, in
`enso jobs list` and `enso jobs run`, and at run start.

`retries` is how many extra attempts postrun may request in one run (default 0,
at most 10). See [Postrun and retries](#postrun-and-retries).

`cron` uses five fields (minute, hour, day of month, month, day of week) and the
machine's timezone. Omit it for a manual-only job. `enabled` defaults to true and
controls scheduling; a disabled job can still be triggered manually.
Missed occurrences during downtime are not replayed. A job never overlaps itself:
scheduled occurrences while it is queued or running are skipped without catch-up,
and manual triggers while busy are rejected. Different jobs and Slack
conversations can run in parallel without a global concurrency limit.
Sunday is `0` or `7`; weekday names are supported. When both day-of-month and
day-of-week are restricted, standard cron matches either restriction.

```sh
enso jobs list
enso jobs run morning-report
enso jobs run morning-report --wait
```

`--wait` waits for the job's result and can be used from a shell, agent, or hook.
Without it, the command returns as soon as the job is queued. The service owns
the submitted job, so it can continue after the calling shell or agent turn ends.
Its completion does not resume the calling turn; send any requested notification
from the job itself.
Job definitions are reread each scheduler minute and at run start, so
job-file changes need no restart. Changes to `config.json` or `.env` require one.

Manual and cron triggers use the same pipeline and fresh native sessions. A job
uses its provider's settings as defined; to change the model, effort, or
arguments, define another provider and name it. `prompt.md` is required
even when a prerun sometimes skips work. Final agent output is stored in SQLite;
it is not automatically posted to Slack. Use `enso message send` from the agent
or postrun when notification is wanted. Without `--to`, it posts to `notify`; a
job without `notify` must name `--to`. Triggering a job from Slack does not change
its destination.

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

A skip runs neither the agent nor postrun. A failed prerun prevents the agent
and postrun from running. Prerun receives the run's context JSON on stdin and
runs once per run. Timeouts and cancellation stop a hook's process group. Hook
failure is visible in the job's outcome.

Enso sets `ENSO_HOME`, `ENSO_WORKSPACE` (the workspace's absolute path),
`ENSO_RUN_ID`, `ENSO_SOURCE`, and `ENSO_JOB` for the agent and hooks.
`ENSO_CHANNEL` and `ENSO_THREAD_TS` provide the Slack conversation or the job's
`notify` destination, and are empty without one.
`ENSO_SOURCE` is `slack` or `job`; `ENSO_JOB` is empty on Slack turns. Enso owns
these metadata variables and overrides conflicting values from the inherited
environment or `.env`.

Job turns get separate guidance and metadata identifying the job, provider,
trigger, scheduled time, workspace name and path, and notification destination.
They never inherit a Slack sender's identity or a chat session.

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
