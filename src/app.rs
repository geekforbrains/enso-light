//! The daemon coordinates the database queue, native processes and Slack delivery.
use crate::{
    config::{self, Config, Loaded},
    context,
    db::{Db, Run, Session},
    formatting, jobs, runner,
    slack::{self, Admission, Incoming, Slack},
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
fn redact(error: &str, loaded: &Loaded) -> String {
    let mut result = error.to_owned();
    for secret in loaded.env.values().chain(loaded.tokens.secrets()) {
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
/// Service startup before Slack connects. The database is checked before any
/// workspace is created, so a start that will fail changes nothing. A workspace
/// or job that cannot be used is returned as a log line, never fatal.
fn prepare(
    home: &Path,
    job_errors: &mut HashMap<String, String>,
) -> Result<(Loaded, File, Db, Vec<String>)> {
    let loaded = config::load(home)?;
    loaded.validate()?;
    let lock = lock(home)?;
    let db = Db::open(home)?;
    let mut log = Vec::new();
    for error in scheduled_jobs(home, &loaded.config, job_errors).1 {
        log.push(format!("Job schedule: {}", redact(&error, &loaded)));
    }
    let (created, errors) = config::scaffold(&loaded.config);
    log.extend(
        created
            .iter()
            .map(|path| format!("Created workspace {}", path.display())),
    );
    log.extend(
        errors
            .iter()
            .map(|error| format!("Workspace: {}", redact(error, &loaded))),
    );
    Ok((loaded, lock, db, log))
}

pub async fn run(home: PathBuf) -> Result<()> {
    let mut job_errors = HashMap::new();
    let (loaded, _lock, db, log) = prepare(&home, &mut job_errors)?;
    for line in log {
        eprintln!("{line}");
    }
    db.runtime("starting", None)?;
    let slack = Slack::new(&loaded.tokens)?;
    let loaded = Arc::new(loaded);
    let config = &loaded.config;
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
        Some(event)=rx.recv()=>{let outcome=accept_event(&db,config,&identity.bot_user_id,&event.payload);match outcome{Ok(())=>{let _=event.accepted.send(());},Err(error)=>eprintln!("Slack admission: {}",redact(&format!("{error:#}"),&loaded))}},
        Some(result)=tasks.join_next()=>{match result {Ok(id)=>{running.remove(&id);},Err(error)=>{return Err(anyhow::anyhow!("Execution worker stopped unexpectedly: {error}"))}}},
        _=tick.tick()=>{
            db.heartbeat()?;
            for(id,token)in &running{if db.cancelled(id)?{token.cancel();}}
            let now=Local::now();let key=now.format("%Y-%m-%dT%H:%M").to_string();
            if key!=minute {minute=key.clone();let(jobs,errors)=scheduled_jobs(&home,config,&mut job_errors);for error in errors{eprintln!("Job schedule: {}",redact(&error,&loaded));}for job in jobs {if job.enabled && let Some(cron)=&job.cron && jobs::due(cron,now)?{db.enqueue_job(&job.name,"cron",Some(&key))?;}}}
            dispatch_ready(&db, |work| {let token=cancel.child_token();running.insert(work.id.clone(),token.clone());let (home,db,slack,loaded)=(home.clone(),db.clone(),slack.clone(),loaded.clone());tasks.spawn(async move{let id=work.id.clone();if let Err(error)=execute(&home,&db,&slack,&loaded,&work,token).await{let error=redact(&format!("{error:#}"),&loaded);eprintln!("Run {id}: {error}");let _=db.finish(&id,"failed","",Some(&error),None);}id});})?;
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

/// Valid jobs, plus errors to log: an invalid job is skipped and its error is
/// reported only when it first appears or changes.
fn scheduled_jobs(
    home: &Path,
    config: &Config,
    logged: &mut HashMap<String, String>,
) -> (Vec<jobs::Job>, Vec<String>) {
    let mut valid = Vec::new();
    let mut errors = BTreeMap::new();
    match jobs::list(home, config) {
        Ok(entries) => {
            for (name, job) in entries {
                match job {
                    Ok(job) => valid.push(job),
                    Err(error) => {
                        errors.insert(name, format!("{error:#}"));
                    }
                }
            }
        }
        // Job names are never empty, so this key cannot collide.
        Err(error) => {
            errors.insert(String::new(), format!("{error:#}"));
        }
    }
    (valid, changed_errors(logged, errors))
}

/// Remembers the current errors and returns those not already logged.
fn changed_errors(
    logged: &mut HashMap<String, String>,
    current: BTreeMap<String, String>,
) -> Vec<String> {
    logged.retain(|name, _| current.contains_key(name));
    current
        .into_iter()
        .filter(|(name, error)| logged.insert(name.clone(), error.clone()).as_ref() != Some(error))
        .map(|(_, error)| error)
        .collect()
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
fn accept_event(db: &Db, config: &Config, bot: &str, payload: &Value) -> Result<()> {
    let event = &payload["event"];
    let channel = event["channel"].as_str().unwrap_or("");
    let thread = event["thread_ts"]
        .as_str()
        .or(event["ts"].as_str())
        .unwrap_or("");
    let participated = db.participated(channel, thread)?;
    let input = match slack::normalize(payload, bot, config, participated)? {
        Admission::Accept(input) => input,
        Admission::Unconfigured { reply, id, setting } => {
            let message = &config.slack.unconfigured_message;
            if !message.is_empty() {
                let ts = event["ts"].as_str().unwrap_or("");
                let text =
                    format!("{message}\n\nTo enable it, add `{id}` to `{setting}` in config.json.");
                db.unconfigured(
                    channel,
                    ts,
                    event["thread_ts"].as_str().filter(|t| *t != ts),
                    event["text"].as_str().unwrap_or(""),
                    &reply,
                    formatting::messages(&text, false)?,
                )?;
            }
            return Ok(());
        }
        Admission::Ignore => return Ok(()),
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
        "!status"=>{let state=db.conversation_status(&accepted.conversation_id)?;let workspace=config.workspace(&input.workspace)?;let(name,provider)=config.provider(workspace.provider.as_deref())?;format!("Enso: {name} ({} / {} / {})\nWorkspace: {}\nRunning: {} · queued: {}\nSession: {}",provider.cli,provider.model.as_deref().unwrap_or("native default"),provider.effort.as_deref().unwrap_or("native default"),input.workspace,state["running"],state["queued"],if state["has_session"]!=true{"not started"}else if state["cli"]==provider.cli.as_str()&&state["workspace"]==workspace.path.to_string_lossy().as_ref(){"active"}else{"active from another CLI or workspace; use !clear"})},
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
    loaded: &Loaded,
    work: &Run,
    cancel: CancellationToken,
) -> Result<()> {
    let config = &loaded.config;
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
        loaded,
        work,
        incoming.as_ref(),
        cancel.clone(),
    )
    .await;
    let (mut state, text, error, session) = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let message = redact(&format!("{error:#}"), loaded);
            let state = if cancel.is_cancelled() || message.contains("cancelled") {
                "cancelled"
            } else if message.contains("timed_out") {
                "timed_out"
            } else {
                "failed"
            };
            (state.to_owned(), String::new(), Some(message), None)
        }
    };
    if cancel.is_cancelled() || db.cancelled(&work.id)? {
        state = "cancelled".into();
    }
    db.finish(&work.id, &state, &text, error.as_deref(), session.as_ref())?;
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
type Completion = (String, String, Option<String>, Option<Session>);
async fn execute_inner(
    home: &Path,
    db: &Db,
    slack: &Slack,
    loaded: &Loaded,
    work: &Run,
    incoming: Option<&Incoming>,
    cancel: CancellationToken,
) -> Result<Completion> {
    let (config, base_env) = (&loaded.config, &loaded.env);
    let job = work
        .job
        .as_ref()
        .map(|name| jobs::load(home, name, config))
        .transpose()?;
    let workspace_name = job
        .as_ref()
        .map(|j| j.workspace.clone())
        .or(incoming.map(|i| i.workspace.clone()))
        .context("run has no workspace")?;
    let workspace_config = config.workspace(&workspace_name)?;
    let (provider, settings) = config.provider(
        job.as_ref()
            .and_then(|j| j.provider.as_deref())
            .or(workspace_config.provider.as_deref()),
    )?;
    let workspace = workspace_config.path.clone();
    let workspace_path = workspace.to_string_lossy().into_owned();
    let timeout_seconds = job
        .as_ref()
        .and_then(|j| j.timeout_seconds)
        .unwrap_or(config.defaults.timeout_seconds);
    if work.session.is_some() {
        ensure!(
            work.cli.as_deref() == Some(settings.cli.as_str()),
            "Configured CLI changed. Use !clear before starting a session with {}.",
            settings.cli
        );
        ensure!(
            work.workspace.as_deref() == Some(workspace_path.as_str()),
            "This conversation's workspace changed. Use !clear to start a fresh session in {workspace_name}."
        );
    }
    ensure!(
        workspace.is_dir(),
        "workspace {workspace_name} directory does not exist: {workspace_path}"
    );
    let pin = |session: Option<String>| {
        session.map(|id| Session {
            id,
            cli: settings.cli.clone(),
            workspace: workspace_path.clone(),
        })
    };
    let mut env = base_env.clone();
    for name in [
        "ENSO_HOME",
        "ENSO_WORKSPACE",
        "ENSO_RUN_ID",
        "ENSO_SOURCE",
        "ENSO_JOB",
        "ENSO_CHANNEL",
        "ENSO_THREAD_TS",
    ] {
        env.insert(name.into(), String::new());
    }
    env.insert("ENSO_HOME".into(), home.to_string_lossy().into());
    env.insert("ENSO_WORKSPACE".into(), workspace_path.clone());
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
    let mut header = json!({"source":if incoming.is_some(){"slack"}else{"job"},"run_id":work.id,"provider":provider,"workspace":{"name":workspace_name,"path":workspace_path},"received_at":time(work.created_at),"started_at":Local::now().to_rfc3339()});
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
            result=tokio::time::timeout(Duration::from_secs(timeout_seconds.min(120)),slack.download(&input.files,&directory))=>result.context("timed_out: attachment download")??,
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
                timeout_seconds,
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
                ));
            }
            variables = pre.vars;
        }
        request = jobs::interpolate(&job.prompt, &variables)?;
    }
    let source = if incoming.is_some() { "slack" } else { "job" };
    let mut prompt = context::render(
        source,
        work.session.is_none(),
        &header,
        &background,
        &request,
    )?;
    let mut snapshot = serde_json::to_value(settings)?;
    snapshot["provider"] = json!(provider);
    snapshot["timeout_seconds"] = json!(timeout_seconds);
    snapshot["workspace"] = json!({"name":workspace_name,"path":workspace_path});
    db.snapshot(&work.id, &prompt, &header, &snapshot)?;
    let max_attempts = job.as_ref().map_or(0, |job| job.retries) + 1;
    let mut session = work.session.clone();
    let mut attempts = Vec::new();
    let mut attempt = 1;
    loop {
        let outcome = runner::execute(
            runner::Request {
                workspace: workspace.clone(),
                settings: settings.clone(),
                timeout_seconds,
                prompt,
                session_id: session,
                env: env.clone(),
                images: images.clone(),
            },
            cancel.clone(),
        )
        .await;
        let (mut state, text, mut error, next_session) = match outcome {
            Ok(result) => ("succeeded".to_owned(), result.text, None, result.session_id),
            Err(error) => {
                if let Some(failure) = error.downcast_ref::<runner::Failure>()
                    && !failure.detail.is_empty()
                {
                    eprintln!(
                        "Run {} {} output:\n{}",
                        work.id,
                        settings.cli,
                        redact(&failure.detail, loaded)
                    );
                }
                let error = redact(&format!("{error:#}"), loaded);
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
        session = next_session;
        let Some(job) = job
            .as_ref()
            .filter(|job| job.postrun && !cancel.is_cancelled())
        else {
            return Ok((state, text, error, pin(session)));
        };
        let mut input = header.clone();
        input["variables"] = json!(variables);
        input["result"] = json!(text);
        input["status"] = json!(state);
        input["error"] = json!(error);
        input["attempt"] = json!(attempt);
        input["max_attempts"] = json!(max_attempts);
        let retry = match runner::hook(
            &job.directory.join("postrun.sh"),
            &input,
            &env,
            timeout_seconds,
            cancel.clone(),
        )
        .await
        .and_then(|output| jobs::postrun_result(&output))
        {
            Ok(retry) => retry.map(|message| redact(&message, loaded)),
            Err(post_error) => {
                state = "failed".into();
                error = Some(format!(
                    "postrun: {}",
                    redact(&format!("{post_error:#}"), loaded)
                ));
                None
            }
        };
        if retry.is_some() || attempt > 1 {
            attempts.push(json!({"attempt":attempt,"status":state,"error":error,"retry":retry}));
            db.record_attempts(&work.id, &attempts)?;
        }
        let Some(message) = retry else {
            return Ok((state, text, error, pin(session)));
        };
        if attempt == max_attempts {
            error = Some(format!(
                "postrun requested a retry with no retries left (retries: {}): {message}",
                max_attempts - 1
            ));
            return Ok(("failed".into(), text, error, pin(session)));
        }
        attempt += 1;
        let note = format!(
            "postrun.sh asked for a retry (attempt {attempt} of {max_attempts}):\n{message}"
        );
        prompt = if session.is_some() {
            context::render(source, false, &header, &background, &note)?
        } else {
            // A failed attempt leaves no session to resume, so start over with the original request.
            context::render(
                source,
                true,
                &header,
                &background,
                &format!("{request}\n\n{note}"),
            )?
        };
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
