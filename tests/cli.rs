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

fn configured_home() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    successful(enso(directory.path(), &["init"]));
    fs::write(
        directory.path().join(".env"),
        "SLACK_BOT_TOKEN=fake-bot-token\nSLACK_APP_TOKEN=fake-app-token\n",
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
fn init_creates_guidance_and_database_without_overwriting_local_content() {
    let directory = configured_home();
    let home = directory.path();
    for name in [
        "enso.db",
        "config.json",
        ".env",
        "workspace/AGENTS.md",
        "skills/enso/SKILL.md",
    ] {
        assert!(home.join(name).is_file(), "missing {name}");
    }
    for name in ["jobs", "logs", "workspace/uploads"] {
        assert!(home.join(name).is_dir(), "missing {name}");
    }
    assert_eq!(
        fs::read_link(home.join("workspace/CLAUDE.md")).unwrap(),
        Path::new("AGENTS.md")
    );
    for tool in [".agents", ".claude"] {
        assert_eq!(
            fs::read_link(home.join("workspace").join(tool).join("skills")).unwrap(),
            Path::new("../../skills")
        );
    }
    let custom_agents = "# Local identity\nKeep this exact text.\n";
    let custom_skill = "# Local Enso skill\nKeep this exact skill.\n";
    fs::write(home.join("workspace/AGENTS.md"), custom_agents).unwrap();
    fs::write(home.join("skills/enso/SKILL.md"), custom_skill).unwrap();
    fs::write(home.join("workspace/personal.txt"), "Keep personal files.").unwrap();
    let original_config = fs::read(home.join("config.json")).unwrap();
    let original_env = fs::read(home.join(".env")).unwrap();
    successful(enso(home, &["init"]));
    assert_eq!(fs::read(home.join("config.json")).unwrap(), original_config);
    assert_eq!(fs::read(home.join(".env")).unwrap(), original_env);
    assert_eq!(
        fs::read_to_string(home.join("workspace/AGENTS.md")).unwrap(),
        custom_agents
    );
    assert_eq!(
        fs::read_to_string(home.join("skills/enso/SKILL.md")).unwrap(),
        custom_skill
    );
    assert_eq!(
        fs::read_to_string(home.join("workspace/personal.txt")).unwrap(),
        "Keep personal files."
    );
}

#[test]
fn config_check_validates_jobs_locally_without_exposing_credentials() {
    let directory = configured_home();
    add_job(directory.path(), "report", json!({"cron":"0 9 * * *"}));
    let valid = successful(enso(directory.path(), &["config", "check"]));
    assert_eq!(valid["valid"], true);
    assert_eq!(valid["jobs"], 1);
    assert!(!valid.to_string().contains("fake-bot-token"));
    fs::remove_file(directory.path().join("jobs/report/prompt.md")).unwrap();
    let output = enso(directory.path(), &["config", "check"]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("prompt.md"));
    assert!(!error.contains("fake-bot-token"));
    assert!(!error.contains("fake-app-token"));
}

#[test]
fn jobs_list_reports_definitions_and_trigger_requires_a_running_service() {
    let directory = configured_home();
    add_job(directory.path(), "report", json!({"cron":"0 9 * * *"}));
    add_job(directory.path(), "manual", json!({"enabled":false}));
    let list = successful(enso(directory.path(), &["jobs", "list"]));
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["name"], "manual");
    assert_eq!(list[0]["enabled"], false);
    assert_eq!(list[0]["next_run"], Value::Null);
    assert_eq!(list[1]["name"], "report");
    assert!(list[1]["next_run"].is_string());
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
    add_job(directory.path(), "child", json!({}));
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
            db.finish(
                &work.id,
                "succeeded",
                "Child finished",
                None,
                None,
                "claude",
            )
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
    assert!(!directory.path().join("workspace").exists());
    assert!(!directory.path().join("enso.db").exists());
}

#[test]
fn slack_search_without_user_token_fails_locally_without_requiring_a_daemon() {
    let directory = configured_home();
    let result = enso(directory.path(), &["slack", "search", "planning"]);
    assert!(!result.status.success());
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    let message = error["error"].as_str().unwrap();
    assert!(message.contains("user_token"), "{message}");
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
