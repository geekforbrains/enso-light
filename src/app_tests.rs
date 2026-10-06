use super::*;
use crate::config::Destination;
use std::{fs, os::unix::fs::PermissionsExt};

const SESSION: &str = "00000000-0000-0000-0000-000000000123";

#[test]
fn runtime_errors_redact_literal_search_credentials_and_environment_values() {
    let mut config = Config::default();
    config.slack.bot_token = "fake-bot-credential".into();
    config.slack.app_token = "fake-app-credential".into();
    config.slack.user_token = Some("fake-user-credential".into());
    let env = BTreeMap::from([("PROVIDER_SECRET".into(), "fake-provider-credential".into())]);
    assert_eq!(
        redact(
            "failure: fake-bot-credential fake-app-credential fake-user-credential fake-provider-credential",
            &env,
            &config,
        ),
        "failure: [redacted] [redacted] [redacted] [redacted]",
    );
}

struct Fixture {
    home: tempfile::TempDir,
    db: Db,
    config: Config,
    env: BTreeMap<String, String>,
    slack: Slack,
}

impl Fixture {
    fn new(provider_fails: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        config::init(home.path()).unwrap();
        fs::write(
            home.path().join(".env"),
            "SLACK_BOT_TOKEN=test-bot-token\nSLACK_APP_TOKEN=test-app-token\nSETTING=available-from-dotenv\nENSO_CHANNEL=stale-channel\nENSO_THREAD_TS=stale-thread\nENSO_JOB=stale-job\n",
        )
        .unwrap();
        let executable = home.path().join("fake-claude");
        let finish = if provider_fails {
            "printf '%s\\n' 'authentication failed: API key private-provider-detail' >&2\nexit 7\n"
                .to_owned()
        } else {
            format!(
                "printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"{SESSION}\",\"result\":\"Provider finished\"}}'\n"
            )
        };
        fs::write(
            &executable,
            format!(
                "#!/bin/bash\nset -eu\ntouch \"$ENSO_HOME/provider-ran\"\ncat > \"input-$ENSO_RUN_ID.txt\"\nprintf '%s\\n' \"$@\" > \"args-$ENSO_RUN_ID.txt\"\nprintf '%s\\n' \"$SETTING\" \"$ENSO_SOURCE\" \"$ENSO_JOB\" \"$ENSO_CHANNEL\" \"$ENSO_THREAD_TS\" > \"env-$ENSO_RUN_ID.txt\"\n{finish}"
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let loaded = config::load(home.path()).unwrap();
        let mut config = loaded.config;
        config.execution.executable = Some(executable.to_string_lossy().into_owned());
        config.execution.timeout_seconds = 5;
        config.slack.notify = None;
        let db = Db::open(home.path()).unwrap();
        let slack = Slack::new(&config.slack).unwrap();
        Self {
            home,
            db,
            config,
            env: loaded.env,
            slack,
        }
    }

    fn job(&self, name: &str, prompt: &str, prerun: &str) -> (Run, PathBuf) {
        let directory = self.home.path().join("jobs").join(name);
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("job.json"),
            r#"{"notify":{"channel":"DREPORT","thread":"1700000000.000001"}}"#,
        )
        .unwrap();
        fs::write(directory.join("prompt.md"), prompt).unwrap();
        fs::write(directory.join("prerun.sh"), prerun).unwrap();
        fs::write(
            directory.join("postrun.sh"),
            "set -eu\ncat > postrun-input.json\nprintf '%s\\n' \"$SETTING\" \"$ENSO_SOURCE\" \"$ENSO_JOB\" \"$ENSO_CHANNEL\" \"$ENSO_THREAD_TS\" > postrun-env.txt\n",
        )
        .unwrap();
        self.db.enqueue_job(name, "manual", None).unwrap();
        (self.db.claim().unwrap().unwrap(), directory)
    }

    async fn run_job(&self, run: &Run) {
        execute(
            self.home.path(),
            &self.db,
            &self.slack,
            &self.config,
            &self.env,
            run,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    }

    fn captured(&self, prefix: &str, run: &Run) -> String {
        fs::read_to_string(
            self.home
                .path()
                .join("workspace")
                .join(format!("{prefix}-{}.txt", run.id)),
        )
        .unwrap()
    }

    fn snapshot(&self, run: &Run) -> (String, Value, Value) {
        rusqlite::Connection::open(self.home.path().join("enso.db"))
            .unwrap()
            .query_row(
                "SELECT resolved_prompt,context,settings FROM runs WHERE id=?1",
                [&run.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        serde_json::from_str(&row.get::<_, String>(1)?).unwrap(),
                        serde_json::from_str(&row.get::<_, String>(2)?).unwrap(),
                    ))
                },
            )
            .unwrap()
    }

    fn background(&self, destination: &Destination, text: &str) -> String {
        let id = self
            .db
            .outgoing(destination, text, true, &[], None, true)
            .unwrap()
            .remove(0);
        self.db
            .delivered(&id, "sent", Some("1700000000.000010"), None)
            .unwrap();
        id
    }
}

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn job_pipeline_carries_prerun_context_variables_environment_and_postrun_result() {
    let fixture = Fixture::new(false);
    let (run, directory) = fixture.job(
        "report",
        "Produce {{COUNT}} reports for {{NAME}}. Ready: {{READY}}.",
        "set -eu\ncat > prerun-input.json\nprintf '%s\\n' \"$SETTING\" \"$ENSO_SOURCE\" \"$ENSO_JOB\" > prerun-env.txt\nprintf '%s\\n' '{\"vars\":{\"COUNT\":3,\"NAME\":\"Gavin\",\"READY\":true}}'\n",
    );
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "succeeded");
    assert_eq!(outcome["result"], "Provider finished");
    let prompt = fixture.captured("input", &run);
    assert!(prompt.contains("running an unattended job"));
    assert!(prompt.ends_with("Current request:\nProduce 3 reports for Gavin. Ready: true."));
    assert!(!prompt.contains("private-provider-detail"));
    let (stored_prompt, header, settings) = fixture.snapshot(&run);
    assert_eq!(stored_prompt, prompt);
    assert_eq!(header["source"], "job");
    assert_eq!(header["run_id"], run.id);
    assert_eq!(header["job"]["name"], "report");
    assert_eq!(header["job"]["trigger"], "manual");
    assert_eq!(header["reply"]["mode"], "none");
    assert_eq!(header["notification_target"]["channel"], "DREPORT");
    assert!(header.get("sender").is_none());
    assert!(header.get("conversation_id").is_none());
    assert_eq!(settings["cli"], "claude");
    assert_eq!(settings["timeout_seconds"], 5);
    assert_eq!(json_file(&directory.join("prerun-input.json")), header);
    let post = json_file(&directory.join("postrun-input.json"));
    assert_eq!(post["run_id"], run.id);
    assert_eq!(post["source"], "job");
    assert_eq!(
        post["variables"],
        json!({"COUNT":3,"NAME":"Gavin","READY":true})
    );
    assert_eq!(post["result"], "Provider finished");
    assert_eq!(post["status"], "succeeded");
    assert!(post["error"].is_null());
    assert_eq!(
        fs::read_to_string(directory.join("prerun-env.txt")).unwrap(),
        "available-from-dotenv\njob\nreport\n"
    );
    let expected_env = "available-from-dotenv\njob\nreport\nDREPORT\n1700000000.000001\n";
    assert_eq!(fixture.captured("env", &run), expected_env);
    assert_eq!(
        fs::read_to_string(directory.join("postrun-env.txt")).unwrap(),
        expected_env
    );
    assert!(!fixture.captured("args", &run).contains("--resume"));
    assert!(fixture.db.claim_delivery().unwrap().is_none());
}

#[tokio::test]
async fn failed_provider_still_calls_postrun_with_safe_error_and_job_failure() {
    let fixture = Fixture::new(true);
    let (run, directory) = fixture.job(
        "failure",
        "Use {{VALUE}}.",
        "cat >/dev/null\nprintf '%s\\n' '{\"vars\":{\"VALUE\":\"input\"}}'\n",
    );
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "failed");
    let post = json_file(&directory.join("postrun-input.json"));
    assert_eq!(post["status"], "failed");
    assert_eq!(post["result"], "");
    assert_eq!(post["variables"]["VALUE"], "input");
    assert!(
        post["error"]
            .as_str()
            .unwrap()
            .contains("authentication_failed")
    );
    assert!(
        !post["error"]
            .as_str()
            .unwrap()
            .contains("private-provider-detail")
    );
    assert_eq!(post["error"], outcome["error"]);
}

#[tokio::test]
async fn prerun_skip_does_not_invoke_provider_postrun_or_prompt_interpolation() {
    let fixture = Fixture::new(false);
    let (run, directory) = fixture.job(
        "skip",
        "This {{MISSING}} must not be interpolated.",
        "cat >/dev/null\nprintf '%s\\n' '{\"skip\":true,\"reason\":\"Nothing changed\"}'\n",
    );
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "skipped");
    assert_eq!(outcome["result"], "Nothing changed");
    assert!(!fixture.home.path().join("provider-ran").exists());
    assert!(!directory.join("postrun-input.json").exists());
    assert!(fixture.db.claim_delivery().unwrap().is_none());
}

fn incoming(ts: &str, thread: Option<&str>, user: &str, name: &str, text: &str) -> Incoming {
    Incoming {
        event_id: format!("event-{ts}"),
        channel: "DCHAT".into(),
        channel_kind: "im".into(),
        user_id: user.into(),
        user_name: Some(name.into()),
        text: text.into(),
        message_ts: ts.into(),
        thread_ts: thread.map(str::to_owned),
        conversation_thread: None,
        reply: Destination {
            channel: "DCHAT".into(),
            thread: thread.map(str::to_owned),
        },
        files: vec![],
    }
}

#[tokio::test]
async fn chat_pipeline_resumes_one_dm_session_with_current_identity_destination_and_background() {
    let fixture = Fixture::new(false);
    let first = incoming(
        "1700000001.000001",
        None,
        "UFIRST",
        "Gavin",
        "First request",
    );
    let accepted = fixture.db.accept(&first, false).unwrap().unwrap();
    let first_background = fixture.background(
        &Destination {
            channel: "DCHAT".into(),
            thread: Some("1699999999.000001".into()),
        },
        "Earlier background report",
    );
    let run = fixture.db.claim().unwrap().unwrap();
    let completion = execute_inner(
        fixture.home.path(),
        &fixture.db,
        &fixture.slack,
        &fixture.config,
        &fixture.env,
        &run,
        Some(&first),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(completion.0, "succeeded");
    assert_eq!(completion.3.as_deref(), Some(SESSION));
    let (prompt, header, _) = fixture.snapshot(&run);
    assert_eq!(prompt, fixture.captured("input", &run));
    assert!(prompt.contains("You are Enso, a personal assistant reached through Slack."));
    assert!(prompt.contains("Earlier background report"));
    assert!(prompt.ends_with("Current request:\nFirst request"));
    assert_eq!(header["source"], "slack");
    assert_eq!(header["sender"], json!({"id":"UFIRST","name":"Gavin"}));
    assert_eq!(header["channel"]["type"], "dm");
    assert_eq!(
        header["reply"],
        json!({"mode":"automatic","channel":"DCHAT","thread":null})
    );
    assert_eq!(header["attachments"], json!([]));
    assert_eq!(header["background_ids"], json!([first_background]));
    assert_eq!(
        fixture.captured("env", &run),
        "available-from-dotenv\nslack\n\nDCHAT\n\n"
    );
    assert!(
        fixture
            .home
            .path()
            .join("workspace/uploads")
            .join(&run.id)
            .is_dir()
    );
    fixture
        .db
        .finish(
            &run.id,
            &completion.0,
            &completion.1,
            completion.2.as_deref(),
            completion.3.as_deref(),
            &completion.4,
        )
        .unwrap();
    assert!(
        fixture
            .db
            .background(&accepted.conversation_id)
            .unwrap()
            .is_empty()
    );

    let second_background = fixture.background(
        &Destination {
            channel: "DCHAT".into(),
            thread: None,
        },
        "New background report",
    );
    fixture.background(
        &Destination {
            channel: "DOTHER".into(),
            thread: None,
        },
        "Unrelated conversation report",
    );
    let second = incoming(
        "1700000002.000001",
        Some("1700000001.000001"),
        "USECOND",
        "Alex",
        "Follow-up request",
    );
    let accepted_second = fixture.db.accept(&second, false).unwrap().unwrap();
    assert_eq!(accepted_second.conversation_id, accepted.conversation_id);
    let resumed = fixture.db.claim().unwrap().unwrap();
    assert_eq!(resumed.session.as_deref(), Some(SESSION));
    let mut current_config = fixture.config.clone();
    current_config.execution.model = Some("opus".into());
    current_config.execution.effort = Some("low".into());
    let completion = execute_inner(
        fixture.home.path(),
        &fixture.db,
        &fixture.slack,
        &current_config,
        &fixture.env,
        &resumed,
        Some(&second),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(completion.0, "succeeded");
    assert_eq!(completion.3.as_deref(), Some(SESSION));
    let (prompt, header, settings) = fixture.snapshot(&resumed);
    assert!(!prompt.contains("You are Enso, a personal assistant reached through Slack."));
    assert!(!prompt.contains("Earlier background report"));
    assert!(!prompt.contains("Unrelated conversation report"));
    assert!(prompt.contains("New background report"));
    assert!(prompt.ends_with("Current request:\nFollow-up request"));
    assert_eq!(header["sender"], json!({"id":"USECOND","name":"Alex"}));
    assert_eq!(header["message_ts"], second.message_ts);
    assert_eq!(header["reply"]["thread"], "1700000001.000001");
    assert_eq!(header["background_ids"], json!([second_background]));
    assert_eq!(header["attachments"], json!([]));
    assert_eq!(settings["model"], "opus");
    assert_eq!(settings["effort"], "low");
    assert_eq!(
        fixture.captured("env", &resumed),
        "available-from-dotenv\nslack\n\nDCHAT\n1700000001.000001\n"
    );
    let args = fixture.captured("args", &resumed);
    assert!(args.contains(&format!("--resume\n{SESSION}\n")));
    assert!(args.contains("--model\nopus\n"));
    assert!(args.contains("--effort\nlow\n"));
}
