use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Local, Timelike};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{self, Config, Destination};

/// Upper bound on `retries`, since each retry is a full agent turn.
pub const MAX_RETRIES: u32 = 10;

#[derive(Clone, Debug, Serialize)]
pub struct Job {
    pub name: String,
    pub directory: PathBuf,
    pub prompt: String,
    pub workspace: String,
    pub provider: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub cron: Option<String>,
    pub enabled: bool,
    pub notify: Option<Destination>,
    pub prerun: bool,
    pub postrun: bool,
    pub retries: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    #[serde(default)]
    cron: Option<String>,
    #[serde(default = "enabled")]
    enabled: bool,
    workspace: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    notify: Option<Destination>,
    #[serde(default)]
    retries: u32,
}

fn enabled() -> bool {
    true
}

pub fn list(home: &Path, config: &Config) -> Result<Vec<Job>> {
    let root = home.join("jobs");
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut jobs = Vec::new();
    for entry in fs::read_dir(&root).context("could not list jobs directory")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("job names must be UTF-8"))?;
        jobs.push(load(home, &name, config)?);
    }
    jobs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(jobs)
}

pub fn load(home: &Path, name: &str, config: &Config) -> Result<Job> {
    ensure!(
        config::valid_name(name),
        "job names may contain only letters, numbers, hyphens, and underscores"
    );
    let directory = home.join("jobs").join(name);
    let definition: Definition = serde_json::from_str(
        &fs::read_to_string(directory.join("job.json"))
            .with_context(|| format!("job {name}: could not read job.json"))?,
    )
    .with_context(|| format!("job {name}: invalid job.json"))?;
    if let Some(expression) = &definition.cron {
        schedules(expression).with_context(|| format!("job {name}: invalid cron"))?;
    }
    if let Some(destination) = &definition.notify {
        destination
            .validate()
            .with_context(|| format!("job {name}: invalid notify"))?;
    }
    config
        .workspace(&definition.workspace)
        .with_context(|| format!("job {name}: invalid workspace"))?;
    if let Some(provider) = &definition.provider {
        config
            .provider(Some(provider))
            .with_context(|| format!("job {name}: invalid provider"))?;
    }
    ensure!(
        definition.timeout_seconds != Some(0),
        "job {name}: timeout_seconds must be greater than zero"
    );
    ensure!(
        definition.retries <= MAX_RETRIES,
        "job {name}: retries must be at most {MAX_RETRIES}"
    );
    let prompt = fs::read_to_string(directory.join("prompt.md"))
        .with_context(|| format!("job {name}: prompt.md is required"))?;
    ensure!(
        !prompt.trim().is_empty(),
        "job {name}: prompt.md must not be empty"
    );
    let prerun = directory.join("prerun.sh").is_file();
    let postrun = directory.join("postrun.sh").is_file();
    Ok(Job {
        name: name.into(),
        directory,
        prompt,
        workspace: definition.workspace,
        provider: definition.provider,
        timeout_seconds: definition.timeout_seconds,
        cron: definition.cron,
        enabled: definition.enabled,
        notify: definition.notify,
        prerun,
        postrun,
        retries: definition.retries,
    })
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreRun {
    #[serde(default)]
    pub vars: BTreeMap<String, Value>,
    #[serde(default)]
    pub skip: bool,
    pub reason: Option<String>,
}

pub fn prerun_result(stdout: &str) -> Result<PreRun> {
    if stdout.trim().is_empty() {
        return Ok(PreRun::default());
    }
    let result: PreRun = serde_json::from_str(stdout)
        .context("prerun stdout must be JSON: {\"vars\":{...}} or {\"skip\":true}")?;
    for (key, value) in &result.vars {
        ensure!(
            valid_key(key),
            "prerun variable names must be environment variable identifiers"
        );
        scalar(value)?;
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PostRun {
    #[serde(default)]
    retry: bool,
    message: Option<String>,
}

/// Returns the retry message when postrun asks for another attempt.
pub fn postrun_result(stdout: &str) -> Result<Option<String>> {
    if stdout.trim().is_empty() {
        return Ok(None);
    }
    let result: PostRun = serde_json::from_str(stdout)
        .context("postrun stdout must be empty, {} or {\"retry\":true,\"message\":\"...\"}")?;
    match (result.retry, result.message) {
        (false, None) => Ok(None),
        (true, Some(message)) if !message.trim().is_empty() => Ok(Some(message)),
        (true, _) => bail!("postrun retry requires a nonempty message"),
        (false, Some(_)) => bail!("postrun message is only allowed with retry"),
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}

fn scalar(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(_) | Value::Bool(_) => Ok(value.to_string()),
        _ => bail!("prerun variables must be strings, numbers, or booleans"),
    }
}

pub fn interpolate(template: &str, vars: &BTreeMap<String, Value>) -> Result<String> {
    let mut rest = template;
    let mut result = String::with_capacity(template.len());
    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest.find("}}").context("unclosed {{NAME}} in prompt.md")?;
        let key = rest[..end].trim();
        ensure!(valid_key(key), "invalid {{NAME}} variable in prompt.md");
        result.push_str(&scalar(vars.get(key).with_context(|| {
            format!("missing prerun variable {key} in prompt.md")
        })?)?);
        rest = &rest[end + 2..];
    }
    result.push_str(rest);
    Ok(result)
}

pub fn due(expression: &str, at: DateTime<Local>) -> Result<bool> {
    let minute = at
        .with_second(0)
        .and_then(|at| at.with_nanosecond(0))
        .context("invalid local minute")?;
    Ok(schedules(expression)?
        .iter()
        .any(|schedule| schedule.includes(minute)))
}

pub fn next_run(expression: &str, after: DateTime<Local>) -> Result<Option<DateTime<Local>>> {
    Ok(schedules(expression)?
        .iter()
        .filter_map(|schedule| schedule.after(&after).next())
        .min())
}

fn schedules(expression: &str) -> Result<Vec<Schedule>> {
    let fields: Vec<_> = expression.split_whitespace().collect();
    ensure!(
        fields.len() == 5,
        "cron requires five fields: minute hour day-of-month month day-of-week"
    );
    let weekdays = weekdays(fields[4])?;
    let parse = |monthday: &str, weekday: &str| {
        Schedule::from_str(&format!(
            "0 {} {} {} {} {}",
            fields[0], fields[1], monthday, fields[3], weekday
        ))
        .context("invalid five-field cron expression")
    };
    // Standard cron numbers Sunday as 0 (or 7). Its two day restrictions use OR.
    // The cron crate numbers Sunday as 1 and combines all restrictions with AND.
    if !fields[2].starts_with('*') && !fields[4].starts_with('*') {
        Ok(vec![parse(fields[2], "*")?, parse("*", &weekdays)?])
    } else {
        Ok(vec![parse(fields[2], &weekdays)?])
    }
}

fn weekdays(field: &str) -> Result<String> {
    if field == "*" {
        return Ok("*".into());
    }
    let mut days = BTreeSet::new();
    for part in field.split(',') {
        let (span, step) = match part.split_once('/') {
            Some((span, step)) => (
                span,
                step.parse::<u32>().context("invalid cron weekday step")?,
            ),
            None => (part, 1),
        };
        ensure!(
            (1..=7).contains(&step),
            "cron weekday step must be 1 through 7"
        );
        let (start, end) = if span == "*" {
            (0, 6)
        } else if let Some((start, end)) = span.split_once('-') {
            (weekday(start)?, weekday(end)?)
        } else {
            let day = weekday(span)?;
            (day, if part.contains('/') { 7 } else { day })
        };
        ensure!(start <= end, "cron weekday ranges must be ascending");
        for day in (start..=end).step_by(step as usize) {
            days.insert(day % 7 + 1);
        }
    }
    ensure!(!days.is_empty(), "cron weekday field must not be empty");
    Ok(days
        .into_iter()
        .map(|day| day.to_string())
        .collect::<Vec<_>>()
        .join(","))
}

fn weekday(value: &str) -> Result<u32> {
    let day = match value.to_ascii_lowercase().as_str() {
        "sun" => 0,
        "mon" => 1,
        "tue" => 2,
        "wed" => 3,
        "thu" => 4,
        "fri" => 5,
        "sat" => 6,
        _ => value
            .parse()
            .context("invalid cron weekday; use 0-7 or SUN-SAT")?,
    };
    ensure!(day <= 7, "cron weekday must be 0-7 (Sunday is 0 or 7)");
    Ok(day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    fn defaults() -> Config {
        serde_json::from_str(
            r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude"},"opus":{"cli":"claude","model":"opus"}},"workspaces":{"main":{"path":"/srv/main"},"acme":{"path":"/srv/acme"}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn jobs_choose_a_configured_provider_and_timeout_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs/report");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("prompt.md"), "Write a report.").unwrap();
        let load_with = |definition: &str| {
            fs::write(directory.join("job.json"), definition).unwrap();
            load(temp.path(), "report", &defaults())
        };
        let job = load_with(r#"{"workspace":"main"}"#).unwrap();
        assert!(job.provider.is_none() && job.timeout_seconds.is_none());
        assert_eq!(job.workspace, "main");
        let job =
            load_with(r#"{"workspace":"acme","provider":"opus","timeout_seconds":600}"#).unwrap();
        assert_eq!(job.workspace, "acme");
        assert_eq!(job.provider.as_deref(), Some("opus"));
        assert_eq!(job.timeout_seconds, Some(600));
        let error = load_with(r#"{"workspace":"main","provider":"missing"}"#).unwrap_err();
        assert!(format!("{error:#}").contains("provider \"missing\" is not defined"));
        assert!(load_with(r#"{"workspace":"main","timeout_seconds":0}"#).is_err());
        assert!(load_with(r#"{"workspace":"main","execution":{"model":"opus"}}"#).is_err());
    }

    #[test]
    fn jobs_require_a_configured_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs/report");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("prompt.md"), "Write a report.").unwrap();
        let load_with = |definition: &str| {
            fs::write(directory.join("job.json"), definition).unwrap();
            format!(
                "{:#}",
                load(temp.path(), "report", &defaults()).unwrap_err()
            )
        };
        let missing = load_with("{}");
        assert!(missing.contains("missing field `workspace`"), "{missing}");
        let unknown = load_with(r#"{"workspace":"other"}"#);
        assert!(
            unknown.contains("job report: invalid workspace")
                && unknown.contains("workspace \"other\" is not defined in workspaces"),
            "{unknown}"
        );
        assert!(list(temp.path(), &defaults()).is_err());
    }

    #[test]
    fn jobs_require_prompt_and_support_optional_hooks() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs/report");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("job.json"),
            r#"{"workspace":"main","cron":"0 9 * * MON-FRI"}"#,
        )
        .unwrap();
        assert!(load(temp.path(), "report", &defaults()).is_err());
        fs::write(directory.join("prompt.md"), "Write a report.").unwrap();
        fs::write(directory.join("prerun.sh"), "").unwrap();
        let jobs = list(temp.path(), &defaults()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].enabled && jobs[0].prerun && !jobs[0].postrun);
        assert_eq!(jobs[0].retries, 0);
        fs::write(
            directory.join("job.json"),
            r#"{"workspace":"main","retries":10}"#,
        )
        .unwrap();
        assert_eq!(
            load(temp.path(), "report", &defaults()).unwrap().retries,
            10
        );
        fs::write(
            directory.join("job.json"),
            r#"{"workspace":"main","retries":11}"#,
        )
        .unwrap();
        assert!(load(temp.path(), "report", &defaults()).is_err());
        fs::write(
            directory.join("job.json"),
            r#"{"workspace":"main","retries":-1}"#,
        )
        .unwrap();
        assert!(load(temp.path(), "report", &defaults()).is_err());
        assert!(load(temp.path(), "../other", &defaults()).is_err());
    }

    #[test]
    fn job_destination_is_optional_but_must_be_a_channel_dm_or_thread() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs/report");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("prompt.md"), "Write a report.").unwrap();
        let load_with = |definition: &str| {
            fs::write(directory.join("job.json"), definition).unwrap();
            load(temp.path(), "report", &defaults())
        };
        assert!(
            load_with(r#"{"workspace":"main"}"#)
                .unwrap()
                .notify
                .is_none()
        );
        for invalid in [
            r#"{"workspace":"main","notify":{"channel":""}}"#,
            r##"{"workspace":"main","notify":{"channel":"#general"}}"##,
            r#"{"workspace":"main","notify":{"channel":"U012345"}}"#,
            r#"{"workspace":"main","notify":{"channel":"C012345","thread":""}}"#,
            r#"{"workspace":"main","notify":{"channel":"C012345","thread":"yesterday"}}"#,
        ] {
            assert!(load_with(invalid).is_err(), "{invalid}");
        }
        for channel in ["C012345", "G012345", "D012345"] {
            let job = load_with(&format!(
                r#"{{"workspace":"main","notify":{{"channel":"{channel}"}}}}"#
            ))
            .unwrap();
            assert_eq!(job.notify.unwrap().channel, channel);
        }
        let job = load_with(
            r#"{"workspace":"main","notify":{"channel":"C012345","thread":"1700000000.000001"}}"#,
        )
        .unwrap();
        assert_eq!(
            job.notify.unwrap().thread.as_deref(),
            Some("1700000000.000001")
        );
    }

    #[test]
    fn prerun_variables_are_scalar_and_expanded_once() {
        let result = prerun_result(r#"{"vars":{"NAME":"{{OTHER}}","N":3,"OK":true}}"#).unwrap();
        assert_eq!(
            interpolate("{{NAME}} {{ N }} {{OK}}", &result.vars).unwrap(),
            "{{OTHER}} 3 true"
        );
        assert!(interpolate("{{MISSING}}", &result.vars).is_err());
        assert!(interpolate("{{NAME", &result.vars).is_err());
        assert!(prerun_result(r#"{"vars":{"X":[]}}"#).is_err());
        assert!(prerun_result(r#"{"vars":{"1X":1}}"#).is_err());
        assert!(prerun_result(r#"{"vars":{"X":null}}"#).is_err());
        assert!(prerun_result("hello").is_err());
        assert!(!prerun_result("").unwrap().skip);
        assert!(
            prerun_result(r#"{"skip":true,"reason":"nothing new"}"#)
                .unwrap()
                .skip
        );
    }

    #[test]
    fn postrun_accepts_or_requests_a_retry_with_a_message() {
        assert_eq!(postrun_result("").unwrap(), None);
        assert_eq!(postrun_result("{}\n").unwrap(), None);
        assert_eq!(postrun_result(r#"{"retry":false}"#).unwrap(), None);
        assert_eq!(
            postrun_result(r#"{"retry":true,"message":"Fix the chart"}"#).unwrap(),
            Some("Fix the chart".into())
        );
        for invalid in [
            "done",
            r#"{"ok":true}"#,
            r#"{"retry":true}"#,
            r#"{"retry":true,"message":" "}"#,
            r#"{"message":"Fix the chart"}"#,
            "{}\n{}",
        ] {
            assert!(postrun_result(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn cron_uses_five_fields_local_time_and_standard_weekdays() {
        let monday = Local
            .with_ymd_and_hms(2026, 10, 5, 9, 0, 15)
            .single()
            .unwrap();
        assert!(due("0 9 * * 1-5", monday).unwrap());
        assert!(due("0 9 * * MON-FRI", monday).unwrap());
        assert!(!due("0 9 * * 0", monday).unwrap());
        assert!(!due("0 9 * * 7", monday).unwrap());
        let sunday = next_run("0 9 * * 0", monday).unwrap().unwrap();
        assert_eq!(sunday.weekday(), chrono::Weekday::Sun);
        assert_eq!(next_run("0 9 * * 7", monday).unwrap(), Some(sunday));
        assert_eq!(next_run("0 9 * * 1-5", monday).unwrap().unwrap().day(), 6);
        assert!(due("0 9 5 * 0", monday).unwrap()); // day-of-month OR day-of-week
        assert!(due("0 9 5 * 0", sunday).unwrap());
        assert!(due("* * * * * *", monday).is_err());
        assert!(due("0 25 * * *", monday).is_err());
        assert!(due("0 9 * * 8", monday).is_err());
        assert!(due("0 9 * * */0", monday).is_err());
    }
}
