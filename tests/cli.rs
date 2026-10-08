//! Black-box CLI checks use isolated homes and fake credentials only.
use enso::{app, db::Db};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn enso(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_enso"))
        .arg("--home")
        .arg(home)
        .arg("--json")
        .args(args)
        .env_remove("ENSO_HOME")
        .env_remove("ENSO_RUN_ID")
        .env_remove("ENSO_CHANNEL")
        .env_remove("ENSO_THREAD_TS")
        .env_remove("SLACK_BOT_TOKEN")
        .env_remove("SLACK_APP_TOKEN")
        .env_remove("SLACK_USER_TOKEN")
        .output()
        .unwrap()
}

fn successful(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Runs `enso config check`, returning its exit code, report, and stderr.
fn check(home: &Path) -> (Option<i32>, Value, String) {
    let output = enso(home, &["config", "check"]);
    let report = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("no report: {}", String::from_utf8_lossy(&output.stderr)));
    (
        output.status.code(),
        report,
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn has(list: &Value, message: &str) -> bool {
    list.as_array()
        .unwrap()
        .iter()
        .any(|item| item.as_str().unwrap().contains(message))
}

fn configured_home() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    successful(enso(directory.path(), &["init"]));
    fs::write(
        directory.path().join(".env"),
        "SLACK_BOT_TOKEN=fake-bot-token\nSLACK_APP_TOKEN=fake-app-token\nSLACK_USER_TOKEN=\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("config.json"),
        json!({
            "defaults":{"provider":"main"},
            "providers":{"main":{"cli":"claude"},"opus":{"cli":"claude","model":"opus"}},
            "workspaces":{"main":{"path":"${ENSO_HOME}/workspaces/main"}},
            "slack":{"dms":{"U012345":"main"}}
        })
        .to_string(),
    )
    .unwrap();
    directory
}

#[test]
fn upgrades_from_enso_runs_are_rejected_before_network_or_home_changes() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("uninitialized");
    let output = Command::new(env!("CARGO_BIN_EXE_enso"))
        .args(["--home", home.to_str().unwrap(), "--json", "upgrade"])
        .env("ENSO_RUN_ID", "test-run")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(error["error"].as_str().unwrap().contains("outside Enso"));
    assert!(!home.exists());
}

#[test]
fn upgrade_rejects_a_service_registered_to_another_executable() {
    use std::os::unix::fs::PermissionsExt;
    let directory = configured_home();
    let account = tempfile::tempdir().unwrap();
    let bin = account.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let systemctl = bin.join("systemctl");
    fs::write(&systemctl, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_enso"))
            .arg("--home")
            .arg(directory.path())
            .arg("--json")
            .args(args)
            .env("HOME", account.path())
            .env("XDG_CONFIG_HOME", account.path().join("config"))
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env_remove("ENSO_RUN_ID")
            .output()
            .unwrap()
    };
    let dotenv = fs::read_to_string(directory.path().join(".env")).unwrap();
    fs::write(
        directory.path().join(".env"),
        "SLACK_BOT_TOKEN=fake-bot-token\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_enso"))
        .arg("--home")
        .arg(directory.path())
        .args(["--json", "service", "install"])
        .env("HOME", account.path())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("SLACK_APP_TOKEN", "inherited-app-token")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SLACK_APP_TOKEN is blank in .env"));
    fs::write(directory.path().join(".env"), dotenv).unwrap();
    add_job(directory.path(), "broken", json!({"workspace":"other"}));
    let output = run(&["service", "install"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("enso config check found problems")
            && error.contains("job broken: invalid workspace"),
        "{error}"
    );
    fs::remove_dir_all(directory.path().join("jobs/broken")).unwrap();
    let installed = successful(run(&["service", "install"]));
    let file = Path::new(installed["file"].as_str().unwrap());
    fs::write(file, "a service registered to another executable").unwrap();
    let output = run(&["upgrade"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("different Enso executable"));
    assert_eq!(
        fs::read_to_string(file).unwrap(),
        "a service registered to another executable"
    );
}

fn add_job(home: &Path, name: &str, definition: Value) {
    let job = home.join("jobs").join(name);
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("job.json"), definition.to_string()).unwrap();
    fs::write(job.join("prompt.md"), "This is a fake test job.").unwrap();
}

#[test]
fn init_creates_a_git_home_with_shared_guidance_and_a_scaffolded_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let output = successful(enso(home, &["init"]));
    assert_eq!(output["warnings"], json!([]));
    assert_eq!(
        output["workspaces_created"],
        json!([home.join("workspaces/main")])
    );
    assert_eq!(
        output["next"],
        "Set providers.main.cli and the Slack tokens in .env, then run enso config check."
    );
    for name in [
        "enso.db",
        "config.json",
        ".env",
        ".gitignore",
        "AGENTS.md",
        ".agents/skills/enso/SKILL.md",
        "workspaces/main/AGENTS.md",
    ] {
        assert!(home.join(name).is_file(), "missing {name}");
    }
    for name in [".git", "jobs", "logs", "workspaces/main/.agents/skills"] {
        assert!(home.join(name).is_dir(), "missing {name}");
    }
    for root in [home.to_path_buf(), home.join("workspaces/main")] {
        assert_eq!(
            fs::read_link(root.join("CLAUDE.md")).unwrap(),
            Path::new("AGENTS.md")
        );
        assert_eq!(
            fs::read_link(root.join(".claude/skills")).unwrap(),
            Path::new("../.agents/skills")
        );
    }
    for old in ["skills", "workspace", "workspaces/main/uploads"] {
        assert!(!home.join(old).exists(), "unexpected {old}");
    }
    let custom = [
        ("AGENTS.md", "# Local identity\nKeep this exact text.\n"),
        (".gitignore", "custom-ignore\n"),
        (".agents/skills/enso/SKILL.md", "# Local Enso skill\n"),
        ("workspaces/main/AGENTS.md", "# Main focus\n"),
        ("workspaces/main/personal.txt", "Keep personal files."),
        (
            "config.json",
            r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"codex"}},"workspaces":{"main":{"path":"${ENSO_HOME}/workspaces/main"}}}"#,
        ),
    ];
    for (name, content) in custom {
        fs::write(home.join(name), content).unwrap();
    }
    let original_env = fs::read(home.join(".env")).unwrap();
    let again = successful(enso(home, &["init"]));
    assert_eq!(again["workspaces_created"], json!([]));
    for (name, content) in custom {
        assert_eq!(
            fs::read_to_string(home.join(name)).unwrap(),
            content,
            "{name}"
        );
    }
    assert_eq!(fs::read(home.join(".env")).unwrap(), original_env);
}

#[test]
fn init_without_git_warns_that_codex_needs_a_repository() {
    let directory = tempfile::tempdir().unwrap();
    let empty_path = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_enso"))
        .arg("--home")
        .arg(directory.path())
        .args(["--json", "init"])
        .env("PATH", empty_path.path())
        .env_remove("ENSO_HOME")
        .output()
        .unwrap();
    let result = successful(output);
    let warnings = result["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    let warning = warnings[0].as_str().unwrap();
    assert!(
        warning.contains("Codex needs the Enso home to be a git repository"),
        "{warning}"
    );
    assert!(!directory.path().join(".git").exists());
    assert!(directory.path().join("workspaces/main/AGENTS.md").is_file());
}

#[test]
fn init_creates_the_home_repository_despite_git_location_variables() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let other = directory.path().join("other/.git");
    let output = Command::new(env!("CARGO_BIN_EXE_enso"))
        .arg("--home")
        .arg(&home)
        .args(["--json", "init"])
        .env("GIT_DIR", &other)
        .env("GIT_WORK_TREE", directory.path())
        .env_remove("ENSO_HOME")
        .output()
        .unwrap();
    assert_eq!(successful(output)["warnings"], json!([]));
    assert!(home.join(".git").is_dir());
    assert!(!other.exists());
}

#[test]
fn init_creates_the_repository_before_a_failing_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let file = home.join("not-a-directory");
    fs::write(&file, "").unwrap();
    fs::write(
        home.join("config.json"),
        json!({"defaults":{"provider":"main"},"workspaces":{"main":{"path":file}}}).to_string(),
    )
    .unwrap();
    let result = enso(home, &["init"]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("exists but is not a directory"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(home.join(".git").is_dir());
}

#[test]
fn init_with_an_invalid_env_writes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join(".env"), "this is not valid").unwrap();
    let result = enso(directory.path(), &["init"]);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("invalid Enso .env"), "{stderr}");
    assert!(!stderr.contains("this is not valid"), "{stderr}");
    let names: Vec<_> = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [".env"]);
}

#[test]
fn config_check_validates_jobs_locally_without_exposing_credentials() {
    let directory = configured_home();
    add_job(
        directory.path(),
        "report",
        json!({"workspace":"main","cron":"0 9 * * *"}),
    );
    let valid = successful(enso(directory.path(), &["config", "check"]));
    assert_eq!(valid["valid"], true);
    assert_eq!(valid["jobs"], 1);
    assert_eq!(valid["errors"], json!([]));
    assert!(!valid.to_string().contains("fake-bot-token"));
    fs::remove_file(directory.path().join("jobs/report/prompt.md")).unwrap();
    let (code, report, stderr) = check(directory.path());
    assert_eq!(code, Some(1));
    assert_eq!(report["valid"], false);
    assert!(has(&report["errors"], "job report: prompt.md is required"));
    assert!(stderr.contains("config check found 1 error\""), "{stderr}");
    for output in [report.to_string(), stderr] {
        assert!(!output.contains("fake-bot-token"));
        assert!(!output.contains("fake-app-token"));
    }
}

#[test]
fn config_check_reports_every_error_and_note_at_once() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    fs::create_dir_all(home.join("jobs")).unwrap();
    fs::write(home.join("file"), "").unwrap();
    fs::write(home.join(".env"), "SLACK_APP_TOKEN=fake-app-token\n").unwrap();
    fs::write(
        home.join("config.json"),
        json!({
            "defaults": {"provider": "missing", "timeout_seconds": 0},
            "providers": {"main": {"cli": ""}, "codex": {"cli": "codex"}},
            "workspaces": {
                "main": {"path": "${ENSO_HOME}/workspaces/main"},
                "file": {"path": "${ENSO_HOME}/file"},
                "relative": {"path": "workspaces/relative", "provider": "nope"}
            },
            "slack": {"dms": {}, "channels": {}}
        })
        .to_string(),
    )
    .unwrap();
    add_job(home, "bad-workspace", json!({"workspace":"other"}));
    add_job(
        home,
        "bad-provider",
        json!({"workspace":"main","provider":"opus"}),
    );
    add_job(home, "good", json!({"workspace":"main"}));
    add_job(home, "bad name", json!({"workspace":"main"}));
    let (code, report, stderr) = check(home);
    assert_eq!(code, Some(1), "{stderr}");
    assert_eq!(report["valid"], false);
    assert_eq!(
        (&report["providers"], &report["workspaces"], &report["jobs"]),
        (&json!(2), &json!(3), &json!(4))
    );
    let errors = &report["errors"];
    for message in [
        r#"providers.main.cli is blank; set "claude" or "codex""#,
        "defaults.provider is not defined in providers",
        "defaults.timeout_seconds must be greater than zero",
        "workspaces.relative.path must be absolute",
        "workspaces.relative.provider is not defined in providers",
        "workspaces.file.path exists but is not a directory",
        "SLACK_BOT_TOKEN is blank",
        "job bad-provider: invalid provider",
        "job bad-workspace: invalid workspace",
        r#"job "bad name": job names may contain only"#,
    ] {
        assert!(has(errors, message), "{message}: {errors:#}");
    }
    assert_eq!(errors.as_array().unwrap().len(), 10, "{errors:#}");
    assert!(stderr.contains("config check found 10 errors"), "{stderr}");
    let notes = &report["notes"];
    for message in [
        r#"no dms or channels are configured; Enso will reply "not configured" to every message"#,
        "workspaces.main.path does not exist yet; it will be created on init or service start",
        "Codex will not load the shared AGENTS.md",
    ] {
        assert!(has(notes, message), "{message}: {notes:#}");
    }
    assert_eq!(notes.as_array().unwrap().len(), 3, "{notes:#}");
    assert!(!report.to_string().contains(&home.display().to_string()));
}

#[test]
fn config_check_passes_with_notes() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    successful(enso(home, &["init"]));
    fs::write(
        home.join(".env"),
        "SLACK_BOT_TOKEN=fake-bot-token\nSLACK_APP_TOKEN=fake-app-token\n",
    )
    .unwrap();
    fs::write(
        home.join("config.json"),
        json!({
            "defaults": {"provider": "main"},
            "providers": {"main": {"cli": "claude"}},
            "workspaces": {
                "main": {"path": "${ENSO_HOME}/workspaces/main"},
                "later": {"path": "${ENSO_HOME}/workspaces/later"}
            }
        })
        .to_string(),
    )
    .unwrap();
    let report = successful(enso(home, &["config", "check"]));
    assert_eq!(report["valid"], true);
    assert_eq!(report["errors"], json!([]));
    assert_eq!(
        report["notes"],
        json!([
            r#"no dms or channels are configured; Enso will reply "not configured" to every message"#,
            "workspaces.later.path does not exist yet; it will be created on init or service start"
        ])
    );
    fs::write(
        home.join("config.json"),
        json!({
            "defaults": {"provider": "main"},
            "providers": {"main": {"cli": "claude"}},
            "workspaces": {"main": {"path": "${ENSO_HOME}/workspaces/main"}},
            "slack": {"unconfigured_message": ""}
        })
        .to_string(),
    )
    .unwrap();
    let report = successful(enso(home, &["config", "check"]));
    assert_eq!(
        report["notes"],
        json!(["no dms or channels are configured; Enso will ignore every message"])
    );
}

#[test]
fn config_check_never_echoes_substituted_values() {
    let directory = configured_home();
    let home = directory.path();
    fs::write(
        home.join(".env"),
        "SLACK_BOT_TOKEN=fake-bot-token\nSLACK_APP_TOKEN=fake-app-token\nOTHER_SECRET=fake-other-secret\n",
    )
    .unwrap();
    fs::write(
        home.join("config.json"),
        json!({
            "defaults": {"provider": "${SLACK_BOT_TOKEN}"},
            "providers": {"main": {"cli": "claude"}},
            "workspaces": {"main": {"path": "${ENSO_HOME}/workspaces/main", "provider": "${OTHER_SECRET}"}},
            "slack": {"dms": {"*": "${SLACK_APP_TOKEN}"}, "channels": {"*": "${OTHER_SECRET}"}}
        })
        .to_string(),
    )
    .unwrap();
    let (code, report, stderr) = check(home);
    assert_eq!(code, Some(1));
    assert_eq!(
        report["errors"],
        json!([
            "defaults.provider is not defined in providers",
            "workspaces.main.provider is not defined in providers",
            "slack.dms.* names a workspace that is not defined in workspaces",
            "slack.channels.* names a workspace that is not defined in workspaces"
        ])
    );
    for output in [report.to_string(), stderr] {
        for secret in ["fake-bot-token", "fake-app-token", "fake-other-secret"] {
            assert!(!output.contains(secret), "{output}");
        }
    }
}

#[test]
fn config_check_reports_one_safe_error_when_config_json_cannot_load() {
    let directory = configured_home();
    let home = directory.path();
    let path = home.join("config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["execution"] = json!({"cli": "claude"});
    config["providers"]["main"]["cli"] = json!("");
    fs::write(&path, config.to_string()).unwrap();
    let (code, report, _) = check(home);
    assert_eq!(code, Some(1));
    assert_eq!(report["errors"].as_array().unwrap().len(), 1, "{report:#}");
    assert!(
        has(
            &report["errors"],
            "invalid config.json: unknown field `execution`"
        ),
        "{report:#}"
    );
    config.as_object_mut().unwrap().remove("execution");
    config["defaults"]["timeout_seconds"] = json!("${SLACK_BOT_TOKEN}");
    fs::write(&path, config.to_string()).unwrap();
    let (code, report, stderr) = check(home);
    assert_eq!(code, Some(1));
    assert_eq!(
        report["errors"],
        json!(["invalid config.json fields or value types; see docs/configuration.md"])
    );
    assert!(!report.to_string().contains("fake-bot-token"));
    assert!(!stderr.contains("fake-bot-token"));
}

#[test]
fn config_check_requires_a_provider_cli_known_job_providers_and_env_tokens() {
    let starter = tempfile::tempdir().unwrap();
    successful(enso(starter.path(), &["init"]));
    let dotenv = fs::read_to_string(starter.path().join(".env")).unwrap();
    assert!(dotenv.contains("SLACK_BOT_TOKEN=\nSLACK_APP_TOKEN=\n"));
    assert!(dotenv.contains("\nSLACK_USER_TOKEN=\n"));
    let (code, report, _) = check(starter.path());
    assert_eq!(code, Some(1));
    assert_eq!(
        report["errors"],
        json!([
            r#"providers.main.cli is blank; set "claude" or "codex""#,
            "SLACK_BOT_TOKEN is blank; set it in .env",
            "SLACK_APP_TOKEN is blank; set it in .env"
        ])
    );

    let directory = configured_home();
    add_job(
        directory.path(),
        "report",
        json!({"workspace":"main","provider":"opus"}),
    );
    let valid = successful(enso(directory.path(), &["config", "check"]));
    assert_eq!(valid["providers"], 2);
    for (definition, message) in [
        (
            json!({"workspace":"main","provider":"missing"}),
            "job report: invalid provider",
        ),
        (
            json!({}),
            "job report: invalid job.json: missing field `workspace`",
        ),
        (
            json!({"workspace":"other"}),
            r#"workspace "other" is not defined in workspaces"#,
        ),
    ] {
        add_job(directory.path(), "report", definition);
        let (code, report, _) = check(directory.path());
        assert_eq!(code, Some(1));
        assert!(has(&report["errors"], message), "{report:#}");
    }
    add_job(directory.path(), "report", json!({"workspace":"main"}));
    fs::write(
        directory.path().join(".env"),
        "SLACK_APP_TOKEN=fake-app-token\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_enso"))
        .arg("--home")
        .arg(directory.path())
        .args(["--json", "config", "check"])
        .env("SLACK_BOT_TOKEN", "inherited-bot-token")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (code, report, _) = check(directory.path());
    assert_eq!(code, Some(1));
    assert_eq!(
        report["errors"],
        json!(["SLACK_BOT_TOKEN is blank; set it in .env"])
    );
}

#[test]
fn jobs_list_reports_definitions_and_trigger_requires_a_running_service() {
    let directory = configured_home();
    add_job(
        directory.path(),
        "report",
        json!({"workspace":"main","cron":"0 9 * * *"}),
    );
    add_job(
        directory.path(),
        "manual",
        json!({"workspace":"main","enabled":false}),
    );
    add_job(directory.path(), "broken", json!({"workspace":"other"}));
    let list = successful(enso(directory.path(), &["jobs", "list"]));
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(list[0]["name"], "broken");
    assert!(
        list[0]["error"]
            .as_str()
            .unwrap()
            .starts_with("job broken: invalid workspace"),
        "{list:#}"
    );
    assert_eq!(list[0].get("enabled"), None);
    assert_eq!(list[1]["name"], "manual");
    assert_eq!(list[1]["enabled"], false);
    assert_eq!(list[1]["next_run"], Value::Null);
    assert_eq!(list[1].get("error"), None);
    assert_eq!(list[2]["name"], "report");
    assert!(list[2]["next_run"].is_string());
    let stopped = enso(directory.path(), &["jobs", "run", "report"]);
    assert!(!stopped.status.success());
    assert!(String::from_utf8_lossy(&stopped.stderr).contains("not running"));
    assert!(
        Db::open(directory.path())
            .unwrap()
            .claim()
            .unwrap()
            .is_none()
    );
    // Holding the daemon lock simulates liveness without launching any network
    // service. The command must only enqueue, never execute a native CLI itself.
    let _lock = app::lock(directory.path()).unwrap();
    let broken = enso(directory.path(), &["jobs", "run", "broken"]);
    assert!(!broken.status.success());
    let error = String::from_utf8_lossy(&broken.stderr);
    assert!(
        error.contains("job broken: invalid workspace")
            && error.contains(r#"workspace \"other\" is not defined"#),
        "{error}"
    );
    let receipt = successful(enso(directory.path(), &["jobs", "run", "manual"]));
    assert_eq!(receipt["state"], "queued");
    let db = Db::open(directory.path()).unwrap();
    let work = db.claim().unwrap().unwrap();
    assert_eq!(work.id, receipt["run_id"]);
    assert_eq!(work.job.as_deref(), Some("manual"));
    assert_eq!(work.trigger, "manual");
    let overlap = enso(directory.path(), &["jobs", "run", "manual"]);
    assert!(!overlap.status.success());
    assert!(String::from_utf8_lossy(&overlap.stderr).contains("already queued or running"));
}

#[test]
fn a_run_can_wait_for_another_jobs_result() {
    let directory = configured_home();
    add_job(directory.path(), "child", json!({"workspace":"main"}));
    let _lock = app::lock(directory.path()).unwrap();
    let db = Db::open(directory.path()).unwrap();
    let parent = db.enqueue_job("parent", "manual", None).unwrap().unwrap();
    assert_eq!(db.claim().unwrap().unwrap().id, parent);
    let mut command = Command::new(env!("CARGO_BIN_EXE_enso"))
        .arg("--home")
        .arg(directory.path())
        .args(["--json", "jobs", "run", "child", "--wait"])
        .env("ENSO_RUN_ID", &parent)
        .env_remove("ENSO_HOME")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut child = None;
    loop {
        if let Some(work) = db.claim().unwrap() {
            assert_eq!(work.job.as_deref(), Some("child"));
            db.finish(&work.id, "succeeded", "Child finished", None, None)
                .unwrap();
            child = Some(work.id);
        }
        if command.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            command.kill().unwrap();
            let output = command.wait_with_output().unwrap();
            panic!(
                "job wait did not finish: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = successful(command.wait_with_output().unwrap());
    assert_eq!(result["id"], child.unwrap());
    assert_eq!(result["state"], "succeeded");
    assert_eq!(result["result"], "Child finished");
    assert_eq!(db.run(&parent).unwrap()["state"], "running");
}

#[test]
fn invalid_existing_configuration_is_not_reseeded_by_init() {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    fs::write(&config_path, "old incompatible config").unwrap();
    let result = enso(directory.path(), &["init"]);
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(&config_path).unwrap(),
        "old incompatible config"
    );
    for name in ["workspaces", "AGENTS.md", ".git", "enso.db"] {
        assert!(!directory.path().join(name).exists(), "{name}");
    }
}

#[test]
fn config_check_rejects_old_routing_keys_and_unknown_workspace_routes() {
    let directory = configured_home();
    let path = directory.path().join("config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for (slack, message) in [
        (
            json!({"dm_users":["U012345"]}),
            "invalid config.json: unknown field `dm_users`",
        ),
        (
            json!({"channels":{"C012345":{"top_level":true,"thread":false}}}),
            "invalid config.json: unknown field `thread`, expected `workspace` or `mention`",
        ),
        (
            json!({"channels":{"C012345":"other"}}),
            "slack.channels.C012345 names a workspace that is not defined",
        ),
    ] {
        config["slack"] = slack;
        fs::write(&path, config.to_string()).unwrap();
        let (code, report, _) = check(directory.path());
        assert_eq!(code, Some(1));
        assert!(has(&report["errors"], message), "{report:#}");
    }
    config["slack"] =
        json!({"channels":{"C012345":{"workspace":"main","mention":"first"}},"dms":{"*":"main"}});
    fs::write(&path, config.to_string()).unwrap();
    successful(enso(directory.path(), &["config", "check"]));
}

#[test]
fn slack_search_without_user_token_fails_locally_without_requiring_a_daemon() {
    let directory = configured_home();
    let result = enso(directory.path(), &["slack", "search", "planning"]);
    assert!(!result.status.success());
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    let message = error["error"].as_str().unwrap();
    assert!(message.contains("SLACK_USER_TOKEN"), "{message}");
    assert!(message.contains("search:read"), "{message}");
    assert!(!message.contains("not running"));
    assert!(!message.contains("fake-bot-token"));
    assert!(!message.contains("fake-app-token"));
}

#[test]
fn slack_rejects_unbounded_pages_and_unscoped_thread_search_before_config_load() {
    // An uninitialized home guarantees these checks run in argument parsing,
    // before configuration, credentials, or network requests are involved.
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["slack", "history", "D012345", "--limit", "0"],
        vec!["slack", "channels", "--limit", "201"],
        vec!["slack", "search", "word", "--thread", "1234567890.123456"],
    ] {
        let result = enso(directory.path(), &args);
        assert_eq!(result.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&result.stderr).contains("init first"));
    }
}
