//! Cross-module contracts exercised without Slack or native model processes.
use enso::{
    config::{Config, Destination},
    context,
    db::Db,
    formatting,
    slack::{self, Admission, Incoming},
};
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};

fn database() -> (tempfile::TempDir, Db) {
    let directory = tempfile::tempdir().unwrap();
    let db = Db::open(directory.path()).unwrap();
    (directory, db)
}

fn incoming(channel: &str, ts: &str, thread: Option<&str>) -> Incoming {
    let config: Config = serde_json::from_value(json!({
        "defaults": {"provider": "main"},
        "slack": {
            "dms": {"U1": "main"},
            "channels": {"C1": {"workspace": "main", "mention": "never"}}
        }
    }))
    .unwrap();
    let mut event = json!({"event_id":format!("Ev{ts}"),"event":{"type":"message","channel":channel,"user":"U1","ts":ts,"text":"hello"}});
    if let Some(thread) = thread {
        event["event"]["thread_ts"] = json!(thread);
    }
    match slack::normalize(&event, "UBOT", &config, true).unwrap() {
        Admission::Accept(incoming) => *incoming,
        other => panic!("{other:?}"),
    }
}

fn target(channel: &str, thread: Option<&str>) -> Destination {
    Destination {
        channel: channel.into(),
        thread: thread.map(str::to_owned),
    }
}

fn plain(text: &str) -> Vec<Value> {
    formatting::messages(text, true).unwrap()
}

fn sent(db: &Db, destination: &Destination, text: &str, background: bool) -> String {
    let ids = db
        .outgoing(destination, plain(text), &[], None, background)
        .unwrap();
    assert_eq!(ids.len(), 1);
    let delivery = db.claim_delivery().unwrap().unwrap();
    assert_eq!(delivery.id, ids[0]);
    db.record_delivery(&delivery.id, "sent", Some("900.001"), None, None)
        .unwrap();
    delivery.id
}

#[test]
fn same_slack_message_is_admitted_once_even_with_different_event_callbacks() {
    let (_directory, db) = database();
    let message = incoming("D1", "100.001", None);
    let accepted = db.accept(&message, false).unwrap().unwrap();
    let mut mention_callback = message.clone();
    mention_callback.event_id = "different-app-mention-event".into();
    assert!(db.accept(&mention_callback, false).unwrap().is_none());
    let reopened = Db::open(_directory.path()).unwrap();
    assert!(reopened.accept(&message, false).unwrap().is_none());
    let work = db.claim().unwrap().unwrap();
    assert_eq!(Some(work.id), accepted.run_id);
    assert_eq!(work.input["event_id"], message.event_id);
    assert!(db.claim().unwrap().is_none());
}

#[test]
fn concurrent_admission_is_atomic() {
    let (_directory, db) = database();
    let barrier = Arc::new(Barrier::new(6));
    let workers: Vec<_> = (0..6)
        .map(|_| {
            let db = db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let message = incoming("D1", "100.001", None);
                barrier.wait();
                db.accept(&message, false).unwrap().is_some()
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>(),
        1
    );
}

#[test]
fn dm_threads_share_session_but_runs_preserve_each_reply_destination() {
    let (_directory, db) = database();
    let first = db
        .accept(&incoming("D1", "100.001", None), false)
        .unwrap()
        .unwrap();
    let followup = db
        .accept(&incoming("D1", "101.001", Some("99.001")), false)
        .unwrap()
        .unwrap();
    assert_eq!(first.conversation_id, followup.conversation_id);
    assert!(!first.busy);
    assert!(followup.busy);
    let first_run = db.claim().unwrap().unwrap();
    assert!(
        db.claim().unwrap().is_none(),
        "same conversation must serialize"
    );
    assert_eq!(first_run.input["reply"]["thread"], Value::Null);
    db.finish(
        &first_run.id,
        "succeeded",
        "answer",
        None,
        Some("native-session"),
    )
    .unwrap();
    let second_run = db.claim().unwrap().unwrap();
    assert_eq!(second_run.session.as_deref(), Some("native-session"));
    assert_eq!(second_run.input["workspace"], "main");
    assert_eq!(second_run.input["reply"]["thread"], "99.001");
    assert_eq!(second_run.input["conversation_thread"], Value::Null);
}

#[test]
fn channel_threads_are_independent_and_clear_preserves_participation() {
    let (_directory, db) = database();
    let first = db
        .accept(&incoming("C1", "100.001", None), false)
        .unwrap()
        .unwrap();
    let reply = db
        .accept(&incoming("C1", "101.001", Some("100.001")), false)
        .unwrap()
        .unwrap();
    let other = db
        .accept(&incoming("C1", "200.001", None), false)
        .unwrap()
        .unwrap();
    assert_eq!(first.conversation_id, reply.conversation_id);
    assert_ne!(first.conversation_id, other.conversation_id);
    let first_run = db.claim().unwrap().unwrap();
    let other_run = db.claim().unwrap().unwrap();
    assert_eq!(
        other_run.conversation.as_deref(),
        Some(other.conversation_id.as_str())
    );
    assert!(db.clear(&first.conversation_id).is_err());
    db.stop(&first.conversation_id).unwrap();
    assert!(db.cancelled(&first_run.id).unwrap());
    assert_eq!(
        db.run(reply.run_id.as_deref().unwrap()).unwrap()["state"],
        "cancelled"
    );
    db.finish(&first_run.id, "cancelled", "", None, Some("old-session"))
        .unwrap();
    assert_eq!(
        db.conversation_status(&first.conversation_id).unwrap()["has_session"],
        true
    );
    db.clear(&first.conversation_id).unwrap();
    let status = db.conversation_status(&first.conversation_id).unwrap();
    assert_eq!(status["has_session"], false);
    assert!(db.participated("C1", "100.001").unwrap());
    assert_eq!(db.run(&other_run.id).unwrap()["state"], "running");
}

#[test]
fn jobs_use_one_queue_for_manual_and_cron_without_overlap_or_replay() {
    let (_directory, db) = database();
    let first = db
        .enqueue_job("report", "cron", Some("2026-10-06T09:00"))
        .unwrap()
        .unwrap();
    assert!(
        db.enqueue_job("report", "cron", Some("2026-10-06T09:00"))
            .unwrap()
            .is_none()
    );
    assert!(db.enqueue_job("report", "manual", None).is_err());
    let running = db.claim().unwrap().unwrap();
    assert_eq!(running.id, first);
    assert!(
        db.enqueue_job("report", "cron", Some("2026-10-06T09:01"))
            .unwrap()
            .is_none()
    );
    db.finish(&first, "succeeded", "done", None, None).unwrap();
    let manual = db.enqueue_job("report", "manual", None).unwrap().unwrap();
    assert_eq!(db.claim().unwrap().unwrap().id, manual);
    db.finish(&manual, "succeeded", "manual done", None, None)
        .unwrap();
    assert_eq!(db.last_job("report").unwrap()["id"], manual);
    // A repeated local clock value (e.g. DST) is a duplicate, not a SQL failure.
    assert!(
        db.enqueue_job("report", "cron", Some("2026-10-06T09:00"))
            .unwrap()
            .is_none()
    );
    assert!(db.claim().unwrap().is_none());
}

#[test]
fn outbox_splits_once_and_persists_stable_ids_and_confirmed_receipts() {
    let (_directory, db) = database();
    let destination = target("D1", Some("99.001"));
    let text = "**A long reply**\n\n".repeat(1500);
    let ids = db
        .outgoing(
            &destination,
            formatting::messages(&text, false).unwrap(),
            &[],
            None,
            true,
        )
        .unwrap();
    assert!(ids.len() > 1);
    for (index, expected) in ids.iter().enumerate() {
        assert_eq!(db.delivery(expected).unwrap()["state"], "pending");
        let delivery = db.claim_delivery().unwrap().unwrap();
        assert_eq!(&delivery.id, expected);
        assert_eq!(delivery.payload["client_msg_id"], *expected);
        assert_eq!(delivery.destination.thread.as_deref(), Some("99.001"));
        assert_eq!(db.delivery(expected).unwrap()["state"], "sending");
        let receipt = format!("900.{index:06}");
        db.record_delivery(expected, "sent", Some(&receipt), None, None)
            .unwrap();
        let delivered = db.delivery(expected).unwrap();
        assert_eq!(delivered["state"], "sent");
        assert_eq!(delivered["receipt"], receipt);
    }
    assert!(db.claim_delivery().unwrap().is_none());
}

#[test]
fn bad_attachment_prevents_partial_admission_and_valid_file_is_snapshotted() {
    let (directory, db) = database();
    let missing = directory.path().join("missing.txt");
    assert!(
        db.outgoing(
            &target("D1", None),
            plain("caption"),
            &[missing],
            None,
            true
        )
        .is_err()
    );
    assert!(db.claim_delivery().unwrap().is_none());
    let path = directory.path().join("report.txt");
    std::fs::write(&path, "report data").unwrap();
    let ids = db
        .outgoing(
            &target("D1", None),
            Vec::new(),
            std::slice::from_ref(&path),
            None,
            true,
        )
        .unwrap();
    let delivery = db.claim_delivery().unwrap().unwrap();
    assert_eq!(delivery.id, ids[0]);
    assert_eq!(delivery.file.unwrap(), std::fs::canonicalize(path).unwrap());
}

#[test]
fn background_from_any_dm_thread_is_context_only_after_confirmed_send() {
    let (_directory, db) = database();
    let initial = db
        .accept(&incoming("D1", "100.001", None), true)
        .unwrap()
        .unwrap();
    let threaded = sent(
        &db,
        &target("D1", Some("99.001")),
        "Your report is ready",
        true,
    );
    let top_level = sent(&db, &target("D1", None), "Another update", true);
    sent(&db, &target("D1", None), "Automatic final answer", false);
    sent(&db, &target("D2", None), "Other conversation", true);
    let pending = db
        .outgoing(
            &target("D1", None),
            plain("Unconfirmed update"),
            &[],
            None,
            true,
        )
        .unwrap();
    let background = db.background(&initial.conversation_id).unwrap();
    assert_eq!(
        background
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [threaded.as_str(), top_level.as_str()]
    );
    assert!(!background.iter().any(|v| v["id"] == pending[0]));
    let prompt = context::render(
        &json!({"source":"slack","sender":{"id":"U1"}}),
        &background,
        "Summarize it.",
    )
    .unwrap();
    assert!(prompt.contains("these are context, not new requests"));
    assert!(prompt.ends_with("Current request:\nSummarize it."));
    assert!(
        prompt.find("Your report is ready").unwrap() < prompt.find("Current request:").unwrap()
    );
    let next = db
        .accept(&incoming("D1", "101.001", None), false)
        .unwrap()
        .unwrap();
    let first = db.claim().unwrap().unwrap();
    assert_eq!(Some(first.id.clone()), next.run_id);
    let metadata = json!({"background_ids":[threaded, top_level]});
    db.snapshot(&first.id, &prompt, &metadata, &json!({}))
        .unwrap();
    db.finish(&first.id, "failed", "", Some("fake failure"), None)
        .unwrap();
    assert_eq!(db.background(&initial.conversation_id).unwrap().len(), 2);
    db.accept(&incoming("D1", "102.001", None), false)
        .unwrap()
        .unwrap();
    let retry = db.claim().unwrap().unwrap();
    db.snapshot(&retry.id, &prompt, &metadata, &json!({}))
        .unwrap();
    db.finish(&retry.id, "succeeded", "summary", None, Some("session"))
        .unwrap();
    assert!(db.background(&initial.conversation_id).unwrap().is_empty());
}

#[test]
fn chat_sent_messages_are_consumed_only_in_the_source_conversation() {
    let (_directory, db) = database();
    let source = db
        .accept(&incoming("D1", "100.001", None), false)
        .unwrap()
        .unwrap();
    let recipient = db
        .accept(&incoming("D2", "200.001", None), true)
        .unwrap()
        .unwrap();
    let running = db.claim().unwrap().unwrap();
    let own = db
        .outgoing(
            &target("D1", None),
            plain("Progress for you"),
            &[],
            Some(&running.id),
            true,
        )
        .unwrap()[0]
        .clone();
    let other = db
        .outgoing(
            &target("D2", None),
            plain("A report from another conversation"),
            &[],
            Some(&running.id),
            true,
        )
        .unwrap()[0]
        .clone();
    for expected in [&own, &other] {
        let delivery = db.claim_delivery().unwrap().unwrap();
        assert_eq!(&delivery.id, expected);
        db.record_delivery(&delivery.id, "sent", Some("900.001"), None, None)
            .unwrap();
    }
    db.finish(
        &running.id,
        "succeeded",
        "Finished",
        None,
        Some("native-session"),
    )
    .unwrap();
    assert!(db.background(&source.conversation_id).unwrap().is_empty());
    let recipient_background = db.background(&recipient.conversation_id).unwrap();
    assert_eq!(
        recipient_background.len(),
        1,
        "Messages sent elsewhere remain context for their recipient"
    );
    assert_eq!(recipient_background[0]["id"], other);
}

#[test]
fn channel_background_thread_follows_the_reply_root() {
    let (_directory, db) = database();
    let first = db
        .accept(&incoming("C1", "100.001", None), true)
        .unwrap()
        .unwrap();
    let other = db
        .accept(&incoming("C1", "200.001", None), true)
        .unwrap()
        .unwrap();
    let id = sent(
        &db,
        &target("C1", Some("100.001")),
        "Thread-only report",
        true,
    );
    assert_eq!(db.background(&first.conversation_id).unwrap()[0]["id"], id);
    assert!(db.background(&other.conversation_id).unwrap().is_empty());
}

#[test]
fn outgoing_thread_participation_requires_a_confirmed_agent_send() {
    let (directory, db) = database();
    let root = "100.001";
    let id = db
        .outgoing(&target("C1", Some(root)), plain("Report"), &[], None, true)
        .unwrap()
        .remove(0);
    assert!(!db.participated("C1", root).unwrap());
    assert_eq!(db.claim_delivery().unwrap().unwrap().id, id);
    assert!(!db.participated("C1", root).unwrap());
    for state in ["failed", "uncertain"] {
        db.record_delivery(&id, state, None, None, Some("Send failed"))
            .unwrap();
        assert!(!db.participated("C1", root).unwrap());
    }
    // File uploads in a known thread can succeed without a message timestamp.
    db.record_delivery(&id, "sent", None, Some("FREPORT"), None)
        .unwrap();
    let reopened = Db::open(directory.path()).unwrap();
    assert!(reopened.participated("C1", root).unwrap());
    assert!(!reopened.participated("C2", root).unwrap());
    assert!(!reopened.participated("C1", "200.001").unwrap());
    assert!(!reopened.participated("C1", "FREPORT").unwrap());

    // Setup notices for unconfigured channels must not join their threads.
    sent(
        &db,
        &target("C2", Some(root)),
        "Configure this channel",
        false,
    );
    assert!(!db.participated("C2", root).unwrap());
    // A top-level upload without a message timestamp cannot identify its thread.
    let id = db
        .outgoing(&target("C3", None), plain("Report"), &[], None, true)
        .unwrap()
        .remove(0);
    db.record_delivery(&id, "sent", None, Some("FREPORT"), None)
        .unwrap();
    assert!(!db.participated("C3", "FREPORT").unwrap());
    assert!(!db.participated("C3", "").unwrap());
}

#[test]
fn top_level_channel_background_belongs_to_its_own_reply_thread() {
    let (_directory, db) = database();
    let id = sent(&db, &target("C1", None), "Scheduled report", true);
    assert!(db.participated("C1", "900.001").unwrap());
    assert!(!db.participated("C1", "200.001").unwrap());
    let relevant = db
        .accept(&incoming("C1", "901.001", Some("900.001")), true)
        .unwrap()
        .unwrap();
    let unrelated = db
        .accept(&incoming("C1", "201.001", Some("200.001")), true)
        .unwrap()
        .unwrap();
    assert_eq!(
        db.background(&relevant.conversation_id).unwrap()[0]["id"],
        id
    );
    assert!(
        db.background(&unrelated.conversation_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn recovery_interrupts_work_retains_sessions_and_never_replays_uncertain_sends() {
    let (directory, db) = database();
    let accepted = db
        .accept(&incoming("D1", "100.001", None), false)
        .unwrap()
        .unwrap();
    let previous = db.claim().unwrap().unwrap();
    db.finish(
        &previous.id,
        "succeeded",
        "answer",
        None,
        Some("native-session"),
    )
    .unwrap();
    db.accept(&incoming("D1", "101.001", None), false)
        .unwrap()
        .unwrap();
    let running = db.claim().unwrap().unwrap();
    let queued = db
        .accept(&incoming("D1", "102.001", None), false)
        .unwrap()
        .unwrap()
        .run_id
        .unwrap();
    let job = db.enqueue_job("report", "manual", None).unwrap().unwrap();
    let unsent = db
        .outgoing(&target("D1", None), plain("maybe sent"), &[], None, true)
        .unwrap()[0]
        .clone();
    assert_eq!(db.claim_delivery().unwrap().unwrap().id, unsent);
    let pending = db
        .outgoing(
            &target("D1", None),
            plain("definitely unsent"),
            &[],
            None,
            true,
        )
        .unwrap()[0]
        .clone();
    let reopened = Db::open(directory.path()).unwrap();
    assert_eq!(reopened.recover().unwrap().len(), 2);
    for id in [&running.id, &queued, &job] {
        assert_eq!(reopened.run(id).unwrap()["state"], "interrupted");
    }
    assert!(reopened.claim().unwrap().is_none());
    assert_eq!(reopened.delivery(&unsent).unwrap()["state"], "uncertain");
    assert_eq!(reopened.claim_delivery().unwrap().unwrap().id, pending);
    assert!(reopened.claim_delivery().unwrap().is_none());
    assert_eq!(
        reopened
            .conversation_status(&accepted.conversation_id)
            .unwrap()["has_session"],
        true
    );
    assert!(reopened.recover().unwrap().is_empty());
    reopened
        .accept(&incoming("D1", "103.001", None), false)
        .unwrap()
        .unwrap();
    assert_eq!(
        reopened.claim().unwrap().unwrap().session.as_deref(),
        Some("native-session")
    );
}

#[test]
fn file_receipt_keeps_file_identity_and_routes_background_by_actual_share_message() {
    let (directory, db) = database();
    let path = directory.path().join("report.txt");
    std::fs::write(&path, "A scheduled report").unwrap();
    let id = db
        .outgoing(&target("C1", None), Vec::new(), &[path], None, true)
        .unwrap()[0]
        .clone();
    assert_eq!(db.claim_delivery().unwrap().unwrap().id, id);
    db.record_delivery(&id, "sent", Some("900.000001"), Some("FREPORT"), None)
        .unwrap();
    let receipt = db.delivery(&id).unwrap();
    assert_eq!(receipt["receipt"], "FREPORT");
    assert_eq!(receipt["message_ts"], "900.000001");
    assert!(db.participated("C1", "900.000001").unwrap());
    assert!(!db.participated("C1", "FREPORT").unwrap());
    let relevant = db
        .accept(&incoming("C1", "901.000001", Some("900.000001")), true)
        .unwrap()
        .unwrap();
    let unrelated = db
        .accept(&incoming("C1", "201.000001", Some("200.000001")), true)
        .unwrap()
        .unwrap();
    let background = db.background(&relevant.conversation_id).unwrap();
    assert_eq!(background.len(), 1);
    assert_eq!(background[0]["id"], id);
    assert!(
        background[0]["text"]
            .as_str()
            .unwrap()
            .contains("report.txt")
    );
    assert!(
        db.background(&unrelated.conversation_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn databases_from_other_schema_versions_are_rejected_without_migration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("enso.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE conversations(id TEXT PRIMARY KEY,provider TEXT); PRAGMA user_version=1;",
        )
        .unwrap();
    drop(connection);
    let error = Db::open(directory.path()).err().unwrap().to_string();
    assert_eq!(
        error,
        "enso.db uses schema 1; Enso 0.2.0 needs a new database. Move enso.db and its -wal/-shm files aside (see the 0.2.0 changelog)."
    );
    let fresh = tempfile::tempdir().unwrap();
    Db::open(fresh.path()).unwrap();
    Db::open(fresh.path()).unwrap();
    let version: i64 = rusqlite::Connection::open(fresh.path().join("enso.db"))
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
}

/// A 0.2.0 database keeps tables and columns Enso no longer uses; it must keep working.
#[test]
fn databases_created_by_0_2_0_keep_working() {
    let directory = tempfile::tempdir().unwrap();
    rusqlite::Connection::open(directory.path().join("enso.db"))
        .unwrap()
        .execute_batch(include_str!("fixtures/schema-0.2.0.sql"))
        .unwrap();
    let db = Db::open(directory.path()).unwrap();
    Db::check(directory.path()).unwrap();

    let accepted = db
        .accept(&incoming("D1", "100.001", None), false)
        .unwrap()
        .unwrap();
    let run = db.claim().unwrap().unwrap();
    db.finish(&run.id, "succeeded", "answer", None, Some("native-session"))
        .unwrap();
    db.accept(&incoming("D1", "101.001", None), false)
        .unwrap()
        .unwrap();
    let resumed = db.claim().unwrap().unwrap();
    assert_eq!(resumed.session.as_deref(), Some("native-session"));
    db.finish(&resumed.id, "failed", "", Some("error"), None)
        .unwrap();
    db.clear(&accepted.conversation_id).unwrap();
    assert_eq!(
        db.conversation_status(&accepted.conversation_id).unwrap()["has_session"],
        false
    );

    let job = db
        .enqueue_job("report", "cron", Some("2026-10-06T09:00"))
        .unwrap()
        .unwrap();
    assert_eq!(db.claim().unwrap().unwrap().id, job);
    db.finish(&job, "succeeded", "done", None, None).unwrap();
    assert!(
        db.enqueue_job("report", "cron", Some("2026-10-06T09:00"))
            .unwrap()
            .is_none()
    );

    sent(&db, &target("D1", None), "report ready", true);
    assert_eq!(
        db.background(&accepted.conversation_id).unwrap()[0]["text"],
        "report ready"
    );
}
