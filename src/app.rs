//! The daemon coordinates the database queue, native processes and Slack delivery.
use crate::{
    config::{self, Config},
    context,
    db::{Db, Run},
    formatting, jobs, runner,
    slack::{Incoming, Slack},
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Local, Utc};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

pub fn lock(home: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(home.join("daemon.lock"))?;
    file.try_lock_exclusive()
        .context("Enso is already running")?;
    Ok(file)
}
pub fn is_running(home: &Path) -> bool {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join("daemon.lock"))
    else {
        return false;
    };
    file.try_lock_exclusive().is_err()
}
fn time(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(Utc::now)
        .with_timezone(&Local)
        .to_rfc3339()
}
fn redact(error: &str, env: &BTreeMap<String, String>, config: &Config) -> String {
    let mut result = error.to_owned();
    for secret in env
        .values()
        .chain([&config.slack.bot_token, &config.slack.app_token])
        .chain(config.slack.user_token.iter())
    {
        if secret.len() >= 4 {
            result = result.replace(secret, "[redacted]");
        }
    }
    result.chars().take(2000).collect()
}

struct Event {
    payload: Value,
    accepted: oneshot::Sender<()>,
}
pub async fn run(home: PathBuf) -> Result<()> {
    let loaded = config::load(&home)?;
    loaded.config.validate()?;
    jobs::list(&home, &loaded.config.execution)?;
    let _lock = lock(&home)?;
    let db = Db::open(&home)?;
    db.runtime("starting", None)?;
    let config = Arc::new(loaded.config);
    let env = Arc::new(loaded.env);
    let slack = Slack::new(&config.slack)?;
    let identity = slack
        .identity()
        .await
        .context("Slack authentication failed")?;
    let interrupted = db.recover()?;
    for value in interrupted {
        if let Ok(input) = serde_json::from_value::<Incoming>(value) {
            let _ = slack
                .reaction(
                    &input.channel,
                    &input.message_ts,
                    &config.slack.working_reaction,
                    false,
                )
                .await;
            db.outgoing(
                &input.reply,
                formatting::messages(
                    "Enso restarted before this request completed. Please send it again.",
                    true,
                )?,
                &[],
                None,
                false,
            )?;
        }
    }
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel::<Event>(32);
    let socket_task = tokio::spawn(socket_loop(slack.clone(), db.clone(), tx, cancel.clone()));
    let mut delivery_task = tokio::spawn(delivery_loop(slack.clone(), db.clone(), cancel.clone()));
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut tasks = JoinSet::new();
    let mut running: HashMap<String, CancellationToken> = HashMap::new();
    let mut minute = Local::now().format("%Y-%m-%dT%H:%M").to_string();
    eprintln!(
        "Enso running in {} (Slack bot {})",
        home.display(),
        identity.bot_user_id
    );
    let result:Result<()>=async {
      loop {tokio::select! {
        _=tokio::signal::ctrl_c()=>break,
        _=signal.recv()=>break,
        Some(event)=rx.recv()=>{let outcome=accept_event(&db,&slack,&config,&identity.bot_user_id,&event.payload);match outcome{Ok(())=>{let _=event.accepted.send(());},Err(error)=>eprintln!("Slack admission: {}",redact(&format!("{error:#}"),&env,&config))}},
        Some(result)=tasks.join_next()=>{match result {Ok(id)=>{running.remove(&id);},Err(error)=>{return Err(anyhow::anyhow!("Execution worker stopped unexpectedly: {error}"))}}},
        _=tick.tick()=>{
            db.heartbeat()?;
            for(id,token)in &running{if db.cancelled(id)?{token.cancel();}}
            let now=Local::now();let key=now.format("%Y-%m-%dT%H:%M").to_string();
            if key!=minute {minute=key.clone();match jobs::list(&home,&config.execution){Ok(jobs)=>for job in jobs {if job.enabled && let Some(cron)=&job.cron && jobs::due(cron,now)?{db.enqueue_job(&job.name,"cron",Some(&key))?;}},Err(error)=>eprintln!("Job schedule: {error:#}")}}
            dispatch_ready(&db, |work| {let token=cancel.child_token();running.insert(work.id.clone(),token.clone());let (home,db,slack,config,env)=(home.clone(),db.clone(),slack.clone(),config.clone(),env.clone());tasks.spawn(async move{let id=work.id.clone();if let Err(error)=execute(&home,&db,&slack,&config,&env,&work,token).await{let error=redact(&format!("{error:#}"),&env,&config);eprintln!("Run {id}: {error}");let _=db.finish(&id,"failed","",Some(&error),None,&config.execution.cli);}id});})?;
        }
      }}Ok(())
    }.await;
    cancel.cancel();
    while tasks.join_next().await.is_some() {}
    socket_task.abort();
    let _ = socket_task.await;
    if tokio::time::timeout(Duration::from_secs(10), &mut delivery_task)
        .await
        .is_err()
    {
        delivery_task.abort();
        let _ = delivery_task.await;
    }
    // Leave any queued turns for startup recovery to report as interrupted.
    db.runtime("stopped", None)?;
    result
}

fn dispatch_ready(db: &Db, mut start: impl FnMut(Run)) -> Result<()> {
    while let Some(work) = db.claim()? {
        start(work);
    }
    Ok(())
}

async fn socket_loop(slack: Slack, db: Db, tx: mpsc::Sender<Event>, cancel: CancellationToken) {
    let mut delay = 1;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let attempt:Result<()>=async{let mut socket=slack.connect().await?;db.runtime("connected",None)?;delay=1;loop{let event=tokio::select!{_=cancel.cancelled()=>return Ok(()),event=socket.next()=>event?};let Some(event)=event else{break};let(ack,wait)=oneshot::channel();tx.send(Event{payload:event.payload,accepted:ack}).await?;tokio::select!{_=cancel.cancelled()=>return Ok(()),result=wait=>{result?;socket.ack(&event.envelope_id).await?;}}}Ok(())}.await;
        if cancel.is_cancelled() {
            break;
        }
        if let Err(error) = attempt {
            eprintln!("Slack connection: {error:#}");
            let _ = db.runtime("disconnected", Some("Slack disconnected; reconnecting"));
        }
        tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(delay))=>{}}
        delay = (delay * 2).min(30);
    }
}
async fn delivery_loop(slack: Slack, db: Db, cancel: CancellationToken) {
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match db.claim_delivery() {
            Ok(Some(delivery)) => {
                let outcome = if let Some(path) = &delivery.file {
                    slack
                        .send_file(&delivery.destination, path)
                        .await
                        .map(|receipt| (receipt.message_ts, Some(receipt.file_id)))
                } else {
                    slack
                        .send_payload(&delivery.destination, &delivery.payload)
                        .await
                        .map(|ts| (Some(ts), None))
                };
                let (state, message_ts, remote_id, error) = match outcome {
                    Ok((ts, id)) => ("sent", ts, id, None),
                    Err(e) => {
                        let text = format!("{e:#}");
                        let state = if text.contains("uncertain") {
                            "uncertain"
                        } else {
                            "failed"
                        };
                        (state, None, None, Some(text))
                    }
                };
                // A receipt write failure must never cause another network send.
                loop {
                    match db.record_delivery(
                        &delivery.id,
                        state,
                        message_ts.as_deref(),
                        remote_id.as_deref(),
                        error.as_deref(),
                    ) {
                        Ok(()) => break,
                        Err(e) => {
                            eprintln!("Delivery receipt {}: {e}", delivery.id);
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            if cancel.is_cancelled() {
                                break;
                            }
                        }
                    }
                }
            }
            Ok(None) => {
                tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(200))=>{}}
            }
            Err(e) => {
                eprintln!("Outbox: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
fn accept_event(db: &Db, slack: &Slack, config: &Config, bot: &str, payload: &Value) -> Result<()> {
    let event = &payload["event"];
    let channel = event["channel"].as_str().unwrap_or("");
    let thread = event["thread_ts"]
        .as_str()
        .or(event["ts"].as_str())
        .unwrap_or("");
    let participated = db.participated(channel, thread)?;
    let Some(input) = slack.normalize(payload, bot, participated)? else {
        return Ok(());
    };
    let command = input.text.trim();
    let is_command = matches!(command, "!clear" | "!stop" | "!status" | "!help");
    let Some(accepted) = db.accept(&input, is_command)? else {
        return Ok(());
    };
    let response = if is_command {
        Some(match command{
        "!clear"=>match db.clear(&accepted.conversation_id){Ok(())=>"Conversation cleared. Your next message starts a fresh session.".into(),Err(e)=>e.to_string()},
        "!stop"=>{db.stop(&accepted.conversation_id)?;"Stopped active and queued work in this conversation.".into()},
        "!status"=>{let state=db.conversation_status(&accepted.conversation_id)?;format!("Enso: {} / {} / {}\nRunning: {} · queued: {}\nSession: {}",config.execution.cli,config.execution.model.as_deref().unwrap_or("native default"),config.execution.effort.as_deref().unwrap_or("native default"),state["running"],state["queued"],if state["has_session"]==true{"active"}else{"not started"})},
        _=>"!clear — start a fresh session when idle\n!stop — cancel active and queued work\n!status — show session and queue\n!help — show these commands".into()
    })
    } else if accepted.busy {
        Some(config.slack.queued_message.clone())
    } else {
        None
    };
    if let Some(response) = response.filter(|s| !s.is_empty()) {
        db.outgoing(
            &input.reply,
            formatting::messages(&response, true)?,
            &[],
            None,
            false,
        )?;
    }
    Ok(())
}

async fn execute(
    home: &Path,
    db: &Db,
    slack: &Slack,
    config: &Config,
    base_env: &BTreeMap<String, String>,
    work: &Run,
    cancel: CancellationToken,
) -> Result<()> {
    let incoming = if work.kind == "chat" {
        Some(serde_json::from_value::<Incoming>(work.input.clone())?)
    } else {
        None
    };
    if let Some(input) = &incoming {
        tokio::select! {
            _ = cancel.cancelled() => {},
            _ = tokio::time::timeout(Duration::from_secs(3), slack.reaction(&input.channel, &input.message_ts, &config.slack.working_reaction, true)) => {},
        }
    }
    let outcome = execute_inner(
        home,
        db,
        slack,
        config,
        base_env,
        work,
        incoming.as_ref(),
        cancel.clone(),
    )
    .await;
    let (mut state, text, error, session, provider) = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let message = redact(&format!("{error:#}"), base_env, config);
            let state = if cancel.is_cancelled() || message.contains("cancelled") {
                "cancelled"
            } else if message.contains("timed_out") {
                "timed_out"
            } else {
                "failed"
            };
            (
                state.to_owned(),
                String::new(),
                Some(message),
                None,
                config.execution.cli.clone(),
            )
        }
    };
    if cancel.is_cancelled() || db.cancelled(&work.id)? {
        state = "cancelled".into();
    }
    db.finish(
        &work.id,
        &state,
        &text,
        error.as_deref(),
        session.as_deref(),
        &provider,
    )?;
    if let Some(input) = &incoming {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            slack.reaction(
                &input.channel,
                &input.message_ts,
                &config.slack.working_reaction,
                false,
            ),
        )
        .await;
        let response = match state.as_str() {
            "succeeded" => {
                if text.trim().is_empty() {
                    "Completed without a text response.".to_owned()
                } else {
                    text
                }
            }
            "timed_out" => config.slack.timeout_message.clone(),
            "cancelled" => "Request stopped.".into(),
            _ => format!(
                "Request failed: {}",
                error.as_deref().unwrap_or("unknown error")
            ),
        };
        let payloads = formatting::messages(&response, state != "succeeded").or_else(|error| {
            eprintln!("Reply formatting {}: {error}", work.id);
            formatting::messages("The run finished, but its reply could not be formatted. Inspect the service logs with enso service logs.", true)
        })?;
        db.outgoing(&input.reply, payloads, &[], Some(&work.id), false)?;
    } else if let Some(error) = error {
        eprintln!("Job {}: {error}", work.job.as_deref().unwrap_or("unknown"));
    }
    Ok(())
}
type Completion = (String, String, Option<String>, Option<String>, String);
#[allow(clippy::too_many_arguments)]
async fn execute_inner(
    home: &Path,
    db: &Db,
    slack: &Slack,
    config: &Config,
    base_env: &BTreeMap<String, String>,
    work: &Run,
    incoming: Option<&Incoming>,
    cancel: CancellationToken,
) -> Result<Completion> {
    let job = work
        .job
        .as_ref()
        .map(|name| jobs::load(home, name, &config.execution))
        .transpose()?;
    let settings = job
        .as_ref()
        .map(|j| j.settings.clone())
        .unwrap_or_else(|| config.execution.clone());
    if work.session.is_some() {
        ensure!(
            work.provider.as_deref() == Some(settings.cli.as_str()),
            "Configured CLI changed. Use !clear before starting a session with {}.",
            settings.cli
        );
    }
    let workspace = home.join("workspace");
    let mut env = base_env.clone();
    for name in [
        "ENSO_HOME",
        "ENSO_RUN_ID",
        "ENSO_SOURCE",
        "ENSO_JOB",
        "ENSO_CHANNEL",
        "ENSO_THREAD_TS",
    ] {
        env.insert(name.into(), String::new());
    }
    env.insert("ENSO_HOME".into(), home.to_string_lossy().into());
    env.insert("ENSO_RUN_ID".into(), work.id.clone());
    env.insert(
        "ENSO_SOURCE".into(),
        if work.kind == "chat" {
            "slack".into()
        } else {
            "job".into()
        },
    );
    env.insert("ENSO_JOB".into(), work.job.clone().unwrap_or_default());
    let target = if let Some(input) = incoming {
        Some(input.reply.clone())
    } else {
        job.as_ref().and_then(|j| j.notify.clone())
    };
    if let Some(dest) = &target {
        env.insert("ENSO_CHANNEL".into(), dest.channel.clone());
        env.insert(
            "ENSO_THREAD_TS".into(),
            dest.thread.clone().unwrap_or_default(),
        );
    }
    let mut header = json!({"source":if incoming.is_some(){"slack"}else{"job"},"run_id":work.id,"workspace":workspace,"received_at":time(work.created_at),"started_at":Local::now().to_rfc3339()});
    let background = if let Some(conv) = &work.conversation {
        db.background(conv)?
    } else {
        Vec::new()
    };
    header["background_ids"] = json!(
        background
            .iter()
            .filter_map(|x| x["id"].as_str())
            .collect::<Vec<_>>()
    );
    let mut images = Vec::new();
    if let Some(input) = incoming {
        header["conversation_id"] = json!(work.conversation);
        let user_name = if input.user_name.is_some() {
            input.user_name.clone()
        } else {
            tokio::select! { _=cancel.cancelled()=>anyhow::bail!("cancelled"), name=tokio::time::timeout(Duration::from_secs(3),slack.user_name(&input.user_id))=>name.ok().and_then(Result::ok).flatten() }
        };
        let channel_name = if input.channel_kind == "im" {
            Some("Direct message".to_owned())
        } else {
            tokio::select! { _=cancel.cancelled()=>anyhow::bail!("cancelled"), name=tokio::time::timeout(Duration::from_secs(3),slack.channel_name(&input.channel))=>name.ok().and_then(Result::ok).flatten() }
        };
        header["sender"] = json!({"id":input.user_id,"name":user_name});
        header["channel"] = json!({"id":input.channel,"name":channel_name,"type":if input.channel_kind=="im"{"dm"}else{&input.channel_kind}});
        header["message_ts"] = json!(input.message_ts);
        header["thread_ts"] = json!(input.thread_ts);
        header["reply"] =
            json!({"mode":"automatic","channel":input.reply.channel,"thread":input.reply.thread});
        let directory = workspace.join("uploads").join(&work.id);
        let files = tokio::select! {
            _=cancel.cancelled()=>anyhow::bail!("cancelled"),
            result=tokio::time::timeout(Duration::from_secs(settings.timeout_seconds.min(120)),slack.download(&input.files,&directory))=>result.context("timed_out: attachment download")??,
        };
        db.record_attachments(&work.id, &files)?;
        images = files
            .iter()
            .filter(|f| {
                f.media_type
                    .as_deref()
                    .is_some_and(|s| matches!(s, "image/png" | "image/jpeg" | "image/webp"))
            })
            .map(|f| f.path.clone())
            .collect();
        header["attachments"] = serde_json::to_value(files)?;
    }
    let mut variables = BTreeMap::new();
    let mut request = work.request.clone();
    if let Some(job) = &job {
        header["job"] = json!({"name":job.name,"directory":job.directory,"trigger":work.trigger});
        if let Some(occurrence) = &work.occurrence {
            header["job"]["scheduled_for"] = json!(occurrence);
        }
        header["reply"] = json!({"mode":"none"});
        header["notification_target"] = json!(target);
        if job.prerun {
            let output = runner::hook(
                &job.directory.join("prerun.sh"),
                &header,
                &env,
                settings.timeout_seconds,
                cancel.clone(),
            )
            .await?;
            let pre = jobs::prerun_result(&output)?;
            if pre.skip {
                return Ok((
                    "skipped".into(),
                    pre.reason
                        .unwrap_or_else(|| "Prerun skipped this job".into()),
                    None,
                    None,
                    settings.cli,
                ));
            }
            variables = pre.vars;
        }
        request = jobs::interpolate(&job.prompt, &variables)?;
    }
    let prompt = context::render(
        if incoming.is_some() { "slack" } else { "job" },
        work.session.is_none(),
        &header,
        &background,
        &request,
    )?;
    db.snapshot(
        &work.id,
        &prompt,
        &header,
        &serde_json::to_value(&settings)?,
    )?;
    let outcome = runner::execute(
        runner::Request {
            workspace,
            settings: settings.clone(),
            prompt,
            session_id: work.session.clone(),
            env: env.clone(),
            images,
        },
        cancel.clone(),
    )
    .await;
    let (mut state, text, mut error, session) = match outcome {
        Ok(result) => ("succeeded".to_owned(), result.text, None, result.session_id),
        Err(error) => {
            let error = redact(&format!("{error:#}"), base_env, config);
            let state = if cancel.is_cancelled() || error.contains("cancelled") {
                "cancelled"
            } else if error.contains("timed_out") {
                "timed_out"
            } else {
                "failed"
            };
            (state.into(), String::new(), Some(error), None)
        }
    };
    if let Some(job) = &job
        && job.postrun
        && !cancel.is_cancelled()
    {
        let mut input = header;
        input["variables"] = json!(variables);
        input["result"] = json!(text);
        input["status"] = json!(state);
        input["error"] = json!(error);
        if let Err(post_error) = runner::hook(
            &job.directory.join("postrun.sh"),
            &input,
            &env,
            settings.timeout_seconds,
            cancel,
        )
        .await
        {
            state = "failed".into();
            error = Some(format!(
                "postrun: {}",
                redact(&format!("{post_error:#}"), base_env, config)
            ));
        }
    }
    Ok((state, text, error, session, settings.cli))
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
