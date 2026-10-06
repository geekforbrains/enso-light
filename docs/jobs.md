# Jobs

Each job is a directory at `~/.enso/jobs/NAME/`:

```text
job.json       # scheduling and optional execution overrides
prompt.md      # required user message
prerun.sh      # optional
postrun.sh     # optional
```

Names use letters, numbers, hyphens, and underscores. `prompt.md` must be nonempty.

```json
{
  "enabled": true,
  "cron": "0 9 * * 1-5",
  "execution": { "effort": "high", "timeout_seconds": 600 },
  "notify": { "channel": "D012345" }
}
```

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
Without it, the command returns as soon as the job is queued.
Job definitions are reread each scheduler minute and at run start, so
job-file changes need no restart. Changes to `config.json` or `.env` require one.

Manual and cron triggers use the same pipeline and fresh native sessions. Jobs
inherit Enso's execution defaults and override individual fields. An explicit
`args` array replaces the defaults; changing `cli` clears inherited
CLI-specific model, effort, executable, and arguments. `prompt.md` is required
even when a prerun sometimes skips work. Final agent output is stored in SQLite;
it is not automatically posted to Slack. Use `enso message send` from the agent
or postrun when notification is wanted. `notify` supplies the default destination.

## Hooks and variables

Existing hooks run automatically with Bash; they do not need executable bits.
Both hooks run in the job directory, with the Enso environment. Send diagnostics
to stderr. Prerun stdout is empty or a JSON object:

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
from running. Prerun receives the run's context JSON on stdin. Postrun receives
the same object plus `variables` (prerun values), `result` (agent output),
`status`, and `error` (null on success). It runs after agent completion,
including failure, unless the run was cancelled. Timeouts and cancellation stop
the running process group. Hook failure is visible in the job's outcome.

Enso sets `ENSO_HOME`, `ENSO_RUN_ID`, `ENSO_SOURCE`, and `ENSO_JOB` for the agent
and hooks. `ENSO_CHANNEL` and `ENSO_THREAD_TS` provide the resolved notification
destination, or empty strings when none is configured. `ENSO_SOURCE` is `slack`
or `job`; `ENSO_JOB` is empty on Slack turns. Enso owns these metadata variables
and overrides conflicting values from the inherited environment or `.env`.

Job turns get separate guidance and metadata identifying the job, trigger,
scheduled time, workspace, and notification destination. They never inherit a
Slack sender's identity or a chat session.
