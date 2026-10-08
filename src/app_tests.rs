use super::*;
use crate::config::{Destination, Tokens};
use std::{fs, os::unix::fs::PermissionsExt};

const SESSION: &str = "00000000-0000-0000-0000-000000000123";

#[test]
fn runtime_errors_redact_literal_search_credentials_and_environment_values() {
    let loaded = Loaded {
        config: serde_json::from_str(r#"{"defaults":{"provider":"main"}}"#).unwrap(),
        env: BTreeMap::from([("PROVIDER_SECRET".into(), "fake-provider-credential".into())]),
        tokens: Tokens {
            bot: "fake-bot-credential".into(),
            app: "fake-app-credential".into(),
            user: Some("fake-user-credential".into()),
        },
    };
    assert_eq!(
        redact(
            "failure: fake-bot-credential fake-app-credential fake-user-credential fake-provider-credential",
            &loaded,
        ),
        "failure: [redacted] [redacted] [redacted] [redacted]",
    );
}

struct Fixture {
    home: tempfile::TempDir,
    db: Db,
    loaded: Loaded,
    slack: Slack,
}

impl Fixture {
    fn new(provider_fails: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        config::init(home.path()).unwrap();
        fs::write(
            home.path().join(".env"),
            "SLACK_BOT_TOKEN=test-bot-token\nSLACK_APP_TOKEN=test-app-token\nSETTING=available-from-dotenv\nENSO_CHANNEL=stale-channel\nENSO_THREAD_TS=stale-thread\nENSO_JOB=stale-job\nENSO_WORKSPACE=stale-workspace\n",
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
                "#!/bin/bash\nset -eu\ntouch \"$ENSO_HOME/provider-ran\"\ncat > \"input-$ENSO_RUN_ID.txt\"\nprintf '%s\\n' \"$@\" > \"args-$ENSO_RUN_ID.txt\"\nprintf '%s\\n' \"$SETTING\" \"$ENSO_SOURCE\" \"$ENSO_JOB\" \"$ENSO_CHANNEL\" \"$ENSO_THREAD_TS\" \"$ENSO_WORKSPACE\" > \"env-$ENSO_RUN_ID.txt\"\n{finish}"
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(home.path().join("acme")).unwrap();
        fs::write(
            home.path().join("config.json"),
            json!({
                "defaults": {"provider": "main", "timeout_seconds": 5},
                "providers": {
                    "main": {"cli": "claude", "executable": executable},
                    "opus": {"cli": "claude", "executable": executable, "model": "opus", "effort": "low", "args": ["--opus"]}
                },
                "workspaces": {
                    "main": {"path": "${ENSO_HOME}/workspace"},
                    "acme": {"path": "${ENSO_HOME}/acme", "provider": "opus"}
                },
                "slack": {"dms": {"U1": "main", "UACME": "acme"}}
            })
            .to_string(),
        )
        .unwrap();
        let loaded = config::load(home.path()).unwrap();
        loaded.validate().unwrap();
        let db = Db::open(home.path()).unwrap();
        let slack = Slack::new(&loaded.config.slack, &loaded.tokens).unwrap();
        Self {
            home,
            db,
            loaded,
            slack,
        }
    }

    fn job(&self, name: &str, prompt: &str, prerun: &str) -> (Run, PathBuf) {
        let directory = self.home.path().join("jobs").join(name);
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("job.json"),
            r#"{"workspace":"main","notify":{"channel":"DREPORT","thread":"1700000000.000001"}}"#,
        )
        .unwrap();
        fs::write(directory.join("prompt.md"), prompt).unwrap();
        fs::write(directory.join("prerun.sh"), prerun).unwrap();
        fs::write(
            directory.join("postrun.sh"),
            "set -eu\ncat > postrun-input.json\nprintf '%s\\n' \"$SETTING\" \"$ENSO_SOURCE\" \"$ENSO_JOB\" \"$ENSO_CHANNEL\" \"$ENSO_THREAD_TS\" \"$ENSO_WORKSPACE\" > postrun-env.txt\n",
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
            &self.loaded,
            run,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    }

    fn captured(&self, prefix: &str, run: &Run) -> String {
        self.captured_in("workspace", prefix, run)
    }

    fn captured_in(&self, workspace: &str, prefix: &str, run: &Run) -> String {
        fs::read_to_string(
            self.home
                .path()
                .join(workspace)
                .join(format!("{prefix}-{}.txt", run.id)),
        )
        .unwrap()
    }

    fn workspace(&self, name: &str) -> String {
        self.home.path().join(name).to_string_lossy().into_owned()
    }

    async fn finish(&self, run: &Run, input: Option<&Incoming>, loaded: &Loaded) -> Completion {
        let completion = execute_inner(
            self.home.path(),
            &self.db,
            &self.slack,
            loaded,
            run,
            input,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        self.db
            .finish(
                &run.id,
                &completion.0,
                &completion.1,
                completion.2.as_deref(),
                completion.3.as_ref(),
            )
            .unwrap();
        completion
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
            .outgoing(
                destination,
                formatting::messages(text, true).unwrap(),
                &[],
                None,
                true,
            )
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
        "set -eu\ncat > prerun-input.json\nprintf '%s\\n' \"$SETTING\" \"$ENSO_SOURCE\" \"$ENSO_JOB\" \"$ENSO_WORKSPACE\" > prerun-env.txt\nprintf '%s\\n' '{\"vars\":{\"COUNT\":3,\"NAME\":\"Gavin\",\"READY\":true}}'\n",
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
    assert_eq!(settings["provider"], "main");
    assert_eq!(settings["cli"], "claude");
    assert!(settings["model"].is_null() && settings["effort"].is_null());
    assert_eq!(settings["args"], json!([]));
    assert_eq!(settings["timeout_seconds"], 5);
    let workspace = fixture.workspace("workspace");
    let expected_workspace = json!({"name":"main","path":workspace});
    assert_eq!(settings["workspace"], expected_workspace);
    assert_eq!(header["provider"], "main");
    assert_eq!(header["workspace"], expected_workspace);
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
        (post["attempt"].clone(), post["max_attempts"].clone()),
        (json!(1), json!(1))
    );
    assert!(outcome["attempts"].is_null());
    assert_eq!(
        fs::read_to_string(directory.join("prerun-env.txt")).unwrap(),
        format!("available-from-dotenv\njob\nreport\n{workspace}\n")
    );
    let expected_env =
        format!("available-from-dotenv\njob\nreport\nDREPORT\n1700000000.000001\n{workspace}\n");
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

/// Saves each attempt's postrun input, agent prompt and agent arguments as `*-N`.
const RECORDING_POSTRUN: &str = "set -eu\nn=$(($(cat attempts 2>/dev/null || echo 0) + 1))\necho \"$n\" > attempts\ncat > \"post-$n.json\"\ncp \"$ENSO_HOME/workspace/input-$ENSO_RUN_ID.txt\" \"input-$n.txt\"\ncp \"$ENSO_HOME/workspace/args-$ENSO_RUN_ID.txt\" \"args-$n.txt\"\n";

fn retrying_job(fixture: &Fixture, name: &str, definition: &str, postrun: &str) -> (Run, PathBuf) {
    let (run, directory) = fixture.job(
        name,
        "Use the data.",
        "cat >/dev/null\necho ran >> prerun-count.txt\n",
    );
    fs::write(directory.join("job.json"), definition).unwrap();
    fs::write(
        directory.join("postrun.sh"),
        format!("{RECORDING_POSTRUN}{postrun}"),
    )
    .unwrap();
    (run, directory)
}

fn read(directory: &Path, name: &str) -> String {
    fs::read_to_string(directory.join(name)).unwrap()
}

#[tokio::test]
async fn postrun_retry_resumes_the_session_with_its_message_until_accepted() {
    let fixture = Fixture::new(false);
    let (run, directory) = retrying_job(
        &fixture,
        "retry",
        r#"{"workspace":"main","retries":2}"#,
        "echo 'checking output' >&2\n[ \"$n\" = 1 ] && printf '%s\\n' '{\"retry\":true,\"message\":\"The chart is missing.\"}'\nexit 0\n",
    );
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "succeeded", "{outcome}");
    assert_eq!(outcome["result"], "Provider finished");
    assert_eq!(
        outcome["attempts"],
        json!([
            {"attempt":1,"status":"succeeded","error":null,"retry":"The chart is missing."},
            {"attempt":2,"status":"succeeded","error":null,"retry":null}
        ])
    );
    assert_eq!(read(&directory, "attempts"), "2\n");
    assert_eq!(read(&directory, "prerun-count.txt"), "ran\n");
    let (first, second) = (
        json_file(&directory.join("post-1.json")),
        json_file(&directory.join("post-2.json")),
    );
    assert_eq!(
        (first["attempt"].clone(), first["max_attempts"].clone()),
        (json!(1), json!(3))
    );
    assert_eq!(second["attempt"], 2);
    assert_eq!(second["run_id"], run.id);
    let initial = read(&directory, "input-1.txt");
    assert!(initial.ends_with("Current request:\nUse the data."));
    assert_eq!(fixture.snapshot(&run).0, initial);
    assert!(!read(&directory, "args-1.txt").contains("--resume"));
    assert!(read(&directory, "args-2.txt").contains(&format!("--resume\n{SESSION}\n")));
    let retry = read(&directory, "input-2.txt");
    assert!(!retry.contains("running an unattended job"));
    assert!(retry.ends_with(
        "Current request:\npostrun.sh asked for a retry (attempt 2 of 3):\nThe chart is missing."
    ));
}

#[tokio::test]
async fn postrun_retries_after_agent_failure_start_fresh_and_are_limited() {
    let fixture = Fixture::new(true);
    let (run, directory) = retrying_job(
        &fixture,
        "limited",
        r#"{"workspace":"main","retries":1}"#,
        "printf '%s\\n' '{\"retry\":true,\"message\":\"Try again.\"}'\n",
    );
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "failed");
    assert_eq!(
        outcome["error"],
        "postrun requested a retry with no retries left (retries: 1): Try again."
    );
    assert_eq!(read(&directory, "attempts"), "2\n");
    assert_eq!(outcome["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(outcome["attempts"][1]["status"], "failed");
    assert!(!read(&directory, "args-2.txt").contains("--resume"));
    let retry = read(&directory, "input-2.txt");
    assert!(retry.contains("running an unattended job"));
    assert!(retry.ends_with(
        "Current request:\nUse the data.\n\npostrun.sh asked for a retry (attempt 2 of 2):\nTry again."
    ));
}

#[tokio::test]
async fn postrun_retry_without_retries_or_invalid_output_fails_the_run() {
    let fixture = Fixture::new(false);
    let (run, directory) = retrying_job(
        &fixture,
        "no-retries",
        r#"{"workspace":"main"}"#,
        "printf '%s\\n' '{\"retry\":true,\"message\":\"Again.\"}'\n",
    );
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "failed");
    assert_eq!(
        outcome["error"],
        "postrun requested a retry with no retries left (retries: 0): Again."
    );
    assert_eq!(read(&directory, "attempts"), "1\n");
    let (run, directory) = retrying_job(
        &fixture,
        "chatty",
        r#"{"workspace":"main","retries":3}"#,
        "echo done\n",
    );
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "failed");
    assert_eq!(outcome["result"], "Provider finished");
    assert!(
        outcome["error"]
            .as_str()
            .unwrap()
            .starts_with("postrun: postrun stdout must be empty")
    );
    assert_eq!(read(&directory, "attempts"), "1\n");
    assert!(outcome["attempts"].is_null());
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
        workspace: "main".into(),
    }
}

#[tokio::test]
async fn dispatch_runs_independent_work_without_a_cap_and_queues_busy_conversations() {
    let fixture = Fixture::new(false);
    let first = incoming("100.001", None, "U1", "Gavin", "First request");
    let first = fixture.db.accept(&first, false).unwrap().unwrap();
    let followup = incoming("101.001", Some("100.001"), "U1", "Gavin", "Follow-up");
    let followup = fixture.db.accept(&followup, false).unwrap().unwrap();
    let mut expected = std::collections::BTreeSet::from([first.run_id.unwrap()]);
    for ts in ["200.001", "300.001"] {
        let mut message = incoming(ts, None, "U1", "Gavin", "Independent thread");
        message.channel = "C1".into();
        message.channel_kind = "channel".into();
        message.conversation_thread = Some(ts.into());
        message.reply = Destination {
            channel: "C1".into(),
            thread: Some(ts.into()),
        };
        expected.insert(
            fixture
                .db
                .accept(&message, false)
                .unwrap()
                .unwrap()
                .run_id
                .unwrap(),
        );
    }
    for name in ["report", "backup", "review"] {
        expected.insert(
            fixture
                .db
                .enqueue_job(name, "manual", None)
                .unwrap()
                .unwrap(),
        );
    }

    let release = CancellationToken::new();
    let (started, mut starts) = mpsc::channel(8);
    let mut tasks = JoinSet::new();
    {
        let mut start = |work: Run| {
            let (db, release, started) = (fixture.db.clone(), release.clone(), started.clone());
            tasks.spawn(async move {
                started.send(work.id.clone()).await.unwrap();
                release.cancelled().await;
                let session = Session {
                    id: SESSION.into(),
                    cli: "claude".into(),
                    workspace: "/srv/main".into(),
                };
                db.finish(&work.id, "succeeded", "done", None, Some(&session))
                    .unwrap();
            });
        };
        dispatch_ready(&fixture.db, &mut start).unwrap();
        // New work must also start while more than four independent runs are active.
        expected.insert(
            fixture
                .db
                .enqueue_job("later", "manual", None)
                .unwrap()
                .unwrap(),
        );
        dispatch_ready(&fixture.db, &mut start).unwrap();
    }
    let mut actual = std::collections::BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..expected.len() {
            actual.insert(starts.recv().await.unwrap());
        }
    })
    .await
    .expect("all independent runs should start before any finish");
    assert_eq!(actual, expected);
    for id in &actual {
        assert_eq!(fixture.db.run(id).unwrap()["state"], "running");
    }
    assert_eq!(
        fixture.db.run(followup.run_id.as_deref().unwrap()).unwrap()["state"],
        "queued"
    );
    assert!(
        fixture
            .db
            .enqueue_job("report", "cron", Some("2026-10-06T09:00"))
            .unwrap()
            .is_none()
    );
    assert!(fixture.db.enqueue_job("report", "manual", None).is_err());

    release.cancel();
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    let mut ready = Vec::new();
    dispatch_ready(&fixture.db, |work| ready.push(work)).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, followup.run_id.unwrap());
    assert_eq!(ready[0].session.as_deref(), Some(SESSION));
    assert!(
        fixture
            .db
            .enqueue_job("report", "cron", Some("2026-10-06T09:00"))
            .unwrap()
            .is_none()
    );
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
    let completion = fixture.finish(&run, Some(&first), &fixture.loaded).await;
    assert_eq!(completion.0, "succeeded");
    let session = completion.3.unwrap();
    assert_eq!(session.id, SESSION);
    assert_eq!(session.cli, "claude");
    assert_eq!(session.workspace, fixture.workspace("workspace"));
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
        header["workspace"],
        json!({"name":"main","path":fixture.workspace("workspace")})
    );
    assert_eq!(
        fixture.captured("env", &run),
        format!(
            "available-from-dotenv\nslack\n\nDCHAT\n\n{}\n",
            fixture.workspace("workspace")
        )
    );
    assert!(
        !fixture
            .home
            .path()
            .join("workspace/uploads")
            .join(&run.id)
            .exists()
    );
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
    let mut current = fixture.loaded.clone();
    current.config.defaults.provider = "opus".into();
    current.config.defaults.timeout_seconds = 7;
    let completion = execute_inner(
        fixture.home.path(),
        &fixture.db,
        &fixture.slack,
        &current,
        &resumed,
        Some(&second),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(completion.0, "succeeded");
    assert_eq!(completion.3.unwrap().id, SESSION);
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
    assert_eq!(settings["provider"], "opus");
    assert_eq!(settings["model"], "opus");
    assert_eq!(settings["effort"], "low");
    assert_eq!(settings["args"], json!(["--opus"]));
    assert_eq!(settings["timeout_seconds"], 7);
    assert_eq!(header["provider"], "opus");
    assert_eq!(
        fixture.captured("env", &resumed),
        format!(
            "available-from-dotenv\nslack\n\nDCHAT\n1700000001.000001\n{}\n",
            fixture.workspace("workspace")
        )
    );
    let args = fixture.captured("args", &resumed);
    assert!(args.contains(&format!("--resume\n{SESSION}\n")));
    assert!(args.contains("--model\nopus\n"));
    assert!(args.contains("--effort\nlow\n"));
}

/// Finishes one DM turn and admits a follow-up that resumes its session.
async fn resumable(fixture: &Fixture) -> (Incoming, Run, String) {
    let first = incoming("1700000001.000001", None, "U1", "Gavin", "First");
    let accepted = fixture.db.accept(&first, false).unwrap().unwrap();
    let run = fixture.db.claim().unwrap().unwrap();
    fixture.finish(&run, Some(&first), &fixture.loaded).await;
    let second = incoming(
        "1700000002.000001",
        Some("1700000001.000001"),
        "U1",
        "Gavin",
        "Second",
    );
    fixture.db.accept(&second, false).unwrap().unwrap();
    let resumed = fixture.db.claim().unwrap().unwrap();
    assert_eq!(resumed.session.as_deref(), Some(SESSION));
    assert_eq!(resumed.cli.as_deref(), Some("claude"));
    assert_eq!(
        resumed.workspace.as_deref(),
        Some(fixture.workspace("workspace").as_str())
    );
    (second, resumed, accepted.conversation_id)
}

async fn resume_error(fixture: &Fixture, loaded: &Loaded, run: &Run, input: &Incoming) -> String {
    let error = execute_inner(
        fixture.home.path(),
        &fixture.db,
        &fixture.slack,
        loaded,
        run,
        Some(input),
        CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        !fixture
            .home
            .path()
            .join("workspace")
            .join(format!("input-{}.txt", run.id))
            .exists()
    );
    error
}

#[tokio::test]
async fn resuming_a_session_under_a_provider_with_another_cli_requires_clear() {
    let fixture = Fixture::new(false);
    let (second, resumed, _) = resumable(&fixture).await;
    let mut current = fixture.loaded.clone();
    let mut codex = current.config.providers["main"].clone();
    codex.cli = "codex".into();
    current.config.providers.insert("codex".into(), codex);
    current.config.defaults.provider = "codex".into();
    let error = resume_error(&fixture, &current, &resumed, &second).await;
    assert!(error.contains("Configured CLI changed"), "{error}");
}

#[tokio::test]
async fn a_session_is_pinned_to_its_workspace_path_until_clear() {
    let fixture = Fixture::new(false);
    let (mut second, resumed, conversation) = resumable(&fixture).await;
    // The same path under another name keeps the session; a new path does not.
    let mut current = fixture.loaded.clone();
    current.config.workspaces.get_mut("main").unwrap().path = fixture.home.path().join("acme");
    let error = resume_error(&fixture, &current, &resumed, &second).await;
    assert_eq!(
        error,
        "This conversation's workspace changed. Use !clear to start a fresh session in main."
    );
    fixture
        .db
        .finish(&resumed.id, "failed", "", Some(&error), None)
        .unwrap();
    let mut alias = fixture.loaded.clone();
    let main = alias.config.workspaces["main"].clone();
    alias.config.workspaces.insert("same".into(), main);
    second.message_ts = "1700000003.000001".into();
    second.workspace = "same".into();
    fixture.db.accept(&second, false).unwrap().unwrap();
    let run = fixture.db.claim().unwrap().unwrap();
    fixture.finish(&run, Some(&second), &alias).await;
    assert!(
        fixture
            .captured("args", &run)
            .contains(&format!("--resume\n{SESSION}\n"))
    );
    fixture.db.clear(&conversation).unwrap();
    let status = fixture.db.conversation_status(&conversation).unwrap();
    assert_eq!(
        (&status["has_session"], &status["cli"], &status["workspace"]),
        (&json!(false), &Value::Null, &Value::Null)
    );
    second.message_ts = "1700000004.000001".into();
    second.workspace = "acme".into();
    fixture.db.accept(&second, false).unwrap().unwrap();
    let run = fixture.db.claim().unwrap().unwrap();
    assert!(run.session.is_none() && run.cli.is_none() && run.workspace.is_none());
    let completion = fixture.finish(&run, Some(&second), &fixture.loaded).await;
    assert_eq!(completion.3.unwrap().workspace, fixture.workspace("acme"));
}

#[tokio::test]
async fn chat_runs_in_the_routed_workspace_with_its_provider() {
    let fixture = Fixture::new(false);
    let mut input = incoming("1700000001.000001", None, "UACME", "Gavin", "Hi");
    input.workspace = "acme".into();
    fixture.db.accept(&input, false).unwrap().unwrap();
    let run = fixture.db.claim().unwrap().unwrap();
    fixture.finish(&run, Some(&input), &fixture.loaded).await;
    let acme = fixture.workspace("acme");
    assert!(
        fixture
            .captured_in("acme", "args", &run)
            .contains("--opus\n--model\nopus\n")
    );
    assert!(
        fixture
            .captured_in("acme", "env", &run)
            .ends_with(&format!("\n{acme}\n"))
    );
    assert!(!fixture.home.path().join("acme/uploads").exists());
    assert!(
        !fixture
            .home
            .path()
            .join("workspace/uploads")
            .join(&run.id)
            .exists()
    );
    let (_, header, settings) = fixture.snapshot(&run);
    assert_eq!(header["workspace"], json!({"name":"acme","path":acme}));
    assert_eq!(header["provider"], "opus");
    assert_eq!(settings["workspace"], json!({"name":"acme","path":acme}));
    assert_eq!(settings["provider"], "opus");
}

#[tokio::test]
async fn a_missing_workspace_directory_fails_the_run() {
    let fixture = Fixture::new(false);
    fs::remove_dir(fixture.home.path().join("acme")).unwrap();
    let mut input = incoming("1700000001.000001", None, "UACME", "Gavin", "Hi");
    input.workspace = "acme".into();
    fixture.db.accept(&input, false).unwrap().unwrap();
    let run = fixture.db.claim().unwrap().unwrap();
    let error = resume_error(&fixture, &fixture.loaded, &run, &input).await;
    assert_eq!(
        error,
        format!(
            "workspace acme directory does not exist: {}",
            fixture.workspace("acme")
        )
    );
    assert!(!fixture.home.path().join("provider-ran").exists());
}

#[tokio::test]
async fn attachment_download_uses_the_resolved_timeout() {
    let fixture = Fixture::new(false);
    let stalled = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slack = Slack::with_test_endpoint(
        &fixture.loaded.config.slack,
        &fixture.loaded.tokens,
        format!("http://{}", stalled.local_addr().unwrap()),
    );
    let mut input = incoming("1700000001.000001", None, "U1", "Gavin", "Read this");
    input.files = vec![crate::slack::RemoteFile {
        id: "F1".into(),
        name: "report.txt".into(),
        media_type: None,
        size: None,
        download_url: None,
    }];
    fixture.db.accept(&input, false).unwrap().unwrap();
    let run = fixture.db.claim().unwrap().unwrap();
    let mut current = fixture.loaded.clone();
    current.config.defaults.timeout_seconds = 1;
    let started = std::time::Instant::now();
    let error = execute_inner(
        fixture.home.path(),
        &fixture.db,
        &slack,
        &current,
        &run,
        Some(&input),
        CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("timed_out: attachment download"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(!fixture.home.path().join("provider-ran").exists());
    for path in [
        "workspace/uploads".to_owned(),
        format!("workspace/uploads/{}", run.id),
    ] {
        let mode = fs::metadata(fixture.home.path().join(path))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}

#[tokio::test]
async fn jobs_select_a_named_provider_and_their_own_timeout() {
    let fixture = Fixture::new(false);
    let (run, directory) = fixture.job("chosen", "Use opus.", "cat >/dev/null\n");
    fs::write(
        directory.join("job.json"),
        r#"{"workspace":"main","provider":"opus","timeout_seconds":9}"#,
    )
    .unwrap();
    fixture.run_job(&run).await;
    assert_eq!(fixture.db.run(&run.id).unwrap()["state"], "succeeded");
    let (_, header, settings) = fixture.snapshot(&run);
    assert_eq!(header["provider"], "opus");
    assert_eq!(settings["provider"], "opus");
    assert_eq!(settings["timeout_seconds"], 9);
    let args = fixture.captured("args", &run);
    assert!(
        args.contains("--opus\n--model\nopus\n--effort\nlow\n"),
        "{args}"
    );
    let post = json_file(&directory.join("postrun-input.json"));
    assert_eq!(post["provider"], "opus");
}

#[tokio::test]
async fn job_provider_wins_over_the_workspace_provider_then_the_default() {
    let fixture = Fixture::new(false);
    let acme = fixture.workspace("acme");
    for (name, definition, provider) in [
        ("inherits", r#"{"workspace":"acme"}"#, "opus"),
        (
            "explicit",
            r#"{"workspace":"acme","provider":"main"}"#,
            "main",
        ),
    ] {
        let directory = fixture.home.path().join("jobs").join(name);
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("job.json"), definition).unwrap();
        fs::write(directory.join("prompt.md"), "Work.").unwrap();
        fs::write(
            directory.join("postrun.sh"),
            "cat >/dev/null\nprintf '%s\\n' \"$ENSO_WORKSPACE\" > postrun-env.txt\n",
        )
        .unwrap();
        fixture.db.enqueue_job(name, "manual", None).unwrap();
        let run = fixture.db.claim().unwrap().unwrap();
        fixture.run_job(&run).await;
        assert_eq!(fixture.db.run(&run.id).unwrap()["state"], "succeeded");
        let (_, header, settings) = fixture.snapshot(&run);
        assert_eq!(header["provider"], provider, "{name}");
        assert_eq!(settings["provider"], provider, "{name}");
        assert_eq!(header["workspace"], json!({"name":"acme","path":acme}));
        // The agent runs in the workspace; hooks stay in the job directory.
        assert!(
            fixture
                .captured_in("acme", "env", &run)
                .ends_with(&format!("\n{acme}\n"))
        );
        assert_eq!(read(&directory, "postrun-env.txt"), format!("{acme}\n"));
    }
}

// The fixture default is 5s, so each 3s process only times out under the job's 1s override.
#[tokio::test]
async fn job_timeout_overrides_the_default_for_prerun() {
    let fixture = Fixture::new(false);
    let (run, directory) = fixture.job("slow", "Never runs.", "cat >/dev/null\nsleep 3\n");
    fs::write(
        directory.join("job.json"),
        r#"{"workspace":"main","timeout_seconds":1}"#,
    )
    .unwrap();
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "timed_out", "{outcome}");
    assert!(!fixture.home.path().join("provider-ran").exists());
}

#[tokio::test]
async fn job_timeout_overrides_the_default_for_agent_attempts() {
    let fixture = Fixture::new(false);
    let slow = fixture.home.path().join("slow-claude");
    fs::write(&slow, "#!/bin/bash\ncat >/dev/null\nsleep 3\n").unwrap();
    fs::set_permissions(&slow, fs::Permissions::from_mode(0o700)).unwrap();
    let mut loaded = fixture.loaded.clone();
    let mut provider = loaded.config.providers["main"].clone();
    provider.executable = Some(slow.to_string_lossy().into());
    loaded.config.providers.insert("slow".into(), provider);
    let (run, directory) = fixture.job("slow", "Sleep.", "cat >/dev/null\n");
    fs::write(
        directory.join("job.json"),
        r#"{"workspace":"main","provider":"slow","timeout_seconds":1}"#,
    )
    .unwrap();
    execute(
        fixture.home.path(),
        &fixture.db,
        &fixture.slack,
        &loaded,
        &run,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "timed_out", "{outcome}");
    let post = json_file(&directory.join("postrun-input.json"));
    assert_eq!(post["status"], "timed_out");
}

#[tokio::test]
async fn job_timeout_overrides_the_default_for_postrun() {
    let fixture = Fixture::new(false);
    let (run, directory) = fixture.job("slow", "Finish.", "cat >/dev/null\n");
    fs::write(
        directory.join("job.json"),
        r#"{"workspace":"main","timeout_seconds":1}"#,
    )
    .unwrap();
    fs::write(directory.join("postrun.sh"), "cat >/dev/null\nsleep 3\n").unwrap();
    fixture.run_job(&run).await;
    let outcome = fixture.db.run(&run.id).unwrap();
    assert_eq!(outcome["state"], "failed", "{outcome}");
    let error = outcome["error"].as_str().unwrap();
    assert!(
        error.starts_with("postrun:") && error.contains("timed_out"),
        "{error}"
    );
}

#[test]
fn status_reports_the_workspace_provider_and_native_defaults() {
    let fixture = Fixture::new(false);
    let mut config = fixture.loaded.config.clone();
    let slack = Slack::new(&config.slack, &Tokens::default()).unwrap();
    let status = |config: &Config, ts: &str| {
        let user = if ts.ends_with("3.000001") {
            "UACME"
        } else {
            "U1"
        };
        let payload = json!({"event_id":format!("Ev{ts}"),"event":{"type":"message","channel":format!("D{user}"),"channel_type":"im","user":user,"ts":ts,"text":"!status"}});
        accept_event(&fixture.db, &slack, config, "UBOT", &payload).unwrap();
        fixture
            .db
            .claim_delivery()
            .unwrap()
            .unwrap()
            .payload
            .to_string()
    };
    let text = status(&config, "1700000001.000001");
    assert!(
        text.contains(
            "Enso: main (claude / native default / native default)\\nWorkspace: main\\nRunning: 0 · queued: 0\\nSession: not started"
        ),
        "{text}"
    );
    let text = status(&config, "1700000003.000001");
    assert!(
        text.contains("Enso: opus (claude / opus / low)\\nWorkspace: acme"),
        "{text}"
    );
    config.defaults.provider = "opus".into();
    let text = status(&config, "1700000002.000001");
    assert!(
        text.contains("Enso: opus (claude / opus / low)\\nWorkspace: main"),
        "{text}"
    );
}

#[test]
fn admission_applies_the_configured_default_mention_mode() {
    let fixture = Fixture::new(false);
    let mut config = fixture.loaded.config.clone();
    config.slack.channels.insert(
        "C1".into(),
        crate::config::ChannelRoute::Workspace("main".into()),
    );
    let slack = Slack::new(&config.slack, &Tokens::default()).unwrap();
    let payload = |ts: &str| json!({"event_id":format!("Ev{ts}"),"event":{"type":"message","channel":"C1","channel_type":"channel","user":"U1","ts":ts,"text":"hello"}});
    accept_event(
        &fixture.db,
        &slack,
        &config,
        "UBOT",
        &payload("1700000001.000001"),
    )
    .unwrap();
    // A routed channel whose mention rule does not match stays silent.
    assert!(fixture.db.claim().unwrap().is_none());
    assert!(fixture.replies().is_empty());
    assert_eq!(fixture.count("messages"), 0);
    config.defaults.mention = crate::config::Mention::Never;
    accept_event(
        &fixture.db,
        &slack,
        &config,
        "UBOT",
        &payload("1700000002.000001"),
    )
    .unwrap();
    assert!(fixture.db.claim().unwrap().is_some());
}

fn event(
    kind: &str,
    channel: &str,
    user: &str,
    ts: &str,
    thread: Option<&str>,
    text: &str,
) -> Value {
    let mut payload = json!({"event_id":format!("Ev{kind}{ts}"),"event":{"type":kind,"channel":channel,"user":user,"ts":ts,"text":text}});
    if let Some(thread) = thread {
        payload["event"]["thread_ts"] = json!(thread);
    }
    payload
}

impl Fixture {
    fn admit(&self, config: &Config, payload: &Value) {
        let slack = Slack::new(&config.slack, &Tokens::default()).unwrap();
        accept_event(&self.db, &slack, config, "UBOT", payload).unwrap();
    }

    fn replies(&self) -> Vec<(Destination, String)> {
        std::iter::from_fn(|| self.db.claim_delivery().unwrap())
            .map(|d| {
                (
                    d.destination,
                    d.payload["text"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn count(&self, table: &str) -> i64 {
        rusqlite::Connection::open(self.home.path().join("enso.db"))
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

#[test]
fn unrouted_dms_get_one_reply_with_the_user_id_in_their_thread() {
    let fixture = Fixture::new(false);
    let config = &fixture.loaded.config;
    let hint = "Enso isn't set up for this conversation.\n\nTo enable it, add `UOTHER` to `slack.dms` in config.json.";
    for (ts, thread) in [
        ("1700000001.000001", None),
        ("1700000002.000001", Some("1700000001.000001")),
    ] {
        let mut payload = event("message", "DOTHER", "UOTHER", ts, thread, "hi");
        payload["event"]["channel_type"] = json!("im");
        fixture.admit(config, &payload);
        fixture.admit(config, &payload);
        let replies = fixture.replies();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].0.channel, "DOTHER");
        assert_eq!(replies[0].0.thread.as_deref(), thread);
        assert_eq!(replies[0].1, hint);
    }
    assert_eq!(fixture.count("conversations"), 0);
    assert_eq!(fixture.count("runs"), 0);
    assert_eq!(fixture.count("messages WHERE direction='in' AND state='ignored' AND conversation_id IS NULL AND run_id IS NULL"), 2);
}

#[test]
fn unrouted_channels_reply_once_in_thread_only_when_mentioned() {
    let fixture = Fixture::new(false);
    let mut config = fixture.loaded.config.clone();
    config.slack.unconfigured_message = "Not here.".into();
    let config = &config;
    let ts = "1700000001.000001";
    fixture.admit(config, &event("message", "COTHER", "U1", ts, None, "hello"));
    fixture.admit(
        config,
        &event(
            "message",
            "COTHER",
            "U1",
            "1700000001.000002",
            Some(ts),
            "hello",
        ),
    );
    assert!(fixture.replies().is_empty());
    // Slack delivers a channel mention as both a message and an app_mention.
    for kind in ["message", "app_mention", "message"] {
        fixture.admit(
            config,
            &event(
                kind,
                "COTHER",
                "U1",
                "1700000002.000001",
                None,
                "<@UBOT> hello",
            ),
        );
    }
    fixture.admit(
        config,
        &event(
            "app_mention",
            "COTHER",
            "U1",
            "1700000003.000001",
            Some(ts),
            "<@UBOT> hi",
        ),
    );
    let replies = fixture.replies();
    assert_eq!(replies.len(), 2, "{replies:?}");
    for ((destination, text), thread) in replies.iter().zip(["1700000002.000001", ts]) {
        assert_eq!(destination.channel, "COTHER");
        assert_eq!(destination.thread.as_deref(), Some(thread));
        assert_eq!(
            text,
            "Not here.\n\nTo enable it, add `COTHER` to `slack.channels` in config.json."
        );
    }
    assert_eq!(fixture.count("conversations"), 0);
    assert_eq!(fixture.count("runs"), 0);
    assert!(
        !fixture
            .db
            .participated("COTHER", "1700000002.000001")
            .unwrap()
    );
}

#[test]
fn an_empty_unconfigured_message_disables_the_reply() {
    let fixture = Fixture::new(false);
    let mut config = fixture.loaded.config.clone();
    config.slack.unconfigured_message = String::new();
    fixture.admit(
        &config,
        &event(
            "message",
            "DOTHER",
            "UOTHER",
            "1700000001.000001",
            None,
            "hi",
        ),
    );
    fixture.admit(
        &config,
        &event(
            "app_mention",
            "COTHER",
            "U1",
            "1700000002.000001",
            None,
            "<@UBOT> hi",
        ),
    );
    assert!(fixture.replies().is_empty());
    assert_eq!(fixture.count("messages"), 0);
}

#[test]
fn routing_a_channel_later_admits_new_messages_but_not_replayed_events() {
    let fixture = Fixture::new(false);
    let mut config = fixture.loaded.config.clone();
    let first = event(
        "app_mention",
        "COTHER",
        "U1",
        "1700000001.000001",
        None,
        "<@UBOT> hi",
    );
    fixture.admit(&config, &first);
    assert_eq!(fixture.replies().len(), 1);
    config.slack.channels.insert(
        "COTHER".into(),
        crate::config::ChannelRoute::Workspace("main".into()),
    );
    // A redelivery of the event that already got the setup reply stays handled.
    fixture.admit(&config, &first);
    assert!(fixture.db.claim().unwrap().is_none());
    fixture.admit(
        &config,
        &event(
            "message",
            "COTHER",
            "U1",
            "1700000002.000001",
            Some("1700000001.000001"),
            "<@UBOT> again",
        ),
    );
    let run = fixture.db.claim().unwrap().unwrap();
    assert_eq!(run.request, "again");
    assert!(
        fixture
            .db
            .participated("COTHER", "1700000001.000001")
            .unwrap()
    );
    assert!(fixture.replies().is_empty());
}
