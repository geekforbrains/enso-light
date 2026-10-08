//! SQLite is the shared runtime state and local command queue.
use crate::{config::Destination, slack::Incoming};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub struct Db {
    path: PathBuf,
}
#[derive(Debug)]
pub struct Accepted {
    pub conversation_id: String,
    pub run_id: Option<String>,
    pub busy: bool,
}
#[derive(Clone, Debug)]
pub struct Run {
    pub id: String,
    pub kind: String,
    pub conversation: Option<String>,
    pub job: Option<String>,
    pub trigger: String,
    pub occurrence: Option<String>,
    pub request: String,
    pub created_at: i64,
    pub input: Value,
    pub session: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Delivery {
    pub id: String,
    pub destination: Destination,
    pub payload: Value,
    pub file: Option<PathBuf>,
}
pub fn now() -> i64 {
    Utc::now().timestamp_millis()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn event_key(channel: &str, ts: &str) -> String {
    format!("{channel}:{ts}")
}
fn queue(
    tx: &Transaction,
    destination: &Destination,
    payloads: Vec<Value>,
    files: &[PathBuf],
    run: Option<&str>,
    background: bool,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for mut payload in payloads {
        let id = id();
        payload["client_msg_id"] = json!(id);
        let body = payload["text"].as_str().unwrap_or_default();
        tx.execute("INSERT INTO messages(id,direction,run_id,channel,thread,body,payload,state,background,created_at) VALUES(?1,'out',?2,?3,?4,?5,?6,'pending',?7,?8)",params![id,run,destination.channel,destination.thread.as_deref().unwrap_or(""),body,payload.to_string(),background,now()])?;
        ids.push(id);
    }
    for file in files {
        let id = id();
        let path = fs::canonicalize(file)?;
        let body = format!(
            "Attachment: {} ({})",
            file.file_name().unwrap_or_default().to_string_lossy(),
            path.display()
        );
        tx.execute("INSERT INTO messages(id,direction,run_id,channel,thread,body,payload,file_path,state,background,created_at) VALUES(?1,'out',?2,?3,?4,?5,'{}',?6,'pending',?7,?8)",params![id,run,destination.channel,destination.thread.as_deref().unwrap_or(""),body,path.to_string_lossy(),background,now()])?;
        ids.push(id);
    }
    Ok(ids)
}

/// Whether the database has no schema yet; any schema but version 2 is an error.
fn empty(c: &Connection) -> Result<bool> {
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let count: i64 = c.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if version == 0 && count == 0 {
        return Ok(true);
    }
    ensure!(
        version == 2,
        "enso.db uses schema {version}; Enso 0.2.0 needs a new database. Move enso.db and its -wal/-shm files aside (see the 0.2.0 changelog)."
    );
    Ok(false)
}

impl Db {
    pub fn open(home: &Path) -> Result<Self> {
        let db = Self {
            path: home.join("enso.db"),
        };
        let c = db.connect()?;
        if empty(&c)? {
            c.execute_batch(include_str!("schema.sql"))?;
        }
        fs::set_permissions(&db.path, fs::Permissions::from_mode(0o600))?;
        Ok(db)
    }

    /// Checks an existing enso.db's schema read-only, creating nothing.
    pub fn check(home: &Path) -> Result<()> {
        let path = home.join("enso.db");
        if !path.exists() {
            return Ok(());
        }
        let c = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("cannot read enso.db")?;
        c.busy_timeout(Duration::from_secs(5))?;
        empty(&c).map(drop)
    }
    fn connect(&self) -> Result<Connection> {
        let c = Connection::open(&self.path)?;
        c.busy_timeout(Duration::from_secs(5))?;
        c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        Ok(c)
    }
    pub fn runtime(&self, connected: &str, error: Option<&str>) -> Result<()> {
        self.connect()?.execute("INSERT INTO runtime(singleton,pid,heartbeat,slack_state,error) VALUES(1,?1,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET pid=excluded.pid,heartbeat=excluded.heartbeat,slack_state=excluded.slack_state,error=excluded.error",params![std::process::id(),now(),connected,error])?;
        Ok(())
    }
    pub fn heartbeat(&self) -> Result<()> {
        self.connect()?
            .execute("UPDATE runtime SET heartbeat=?1 WHERE singleton=1", [now()])?;
        Ok(())
    }
    pub fn health(&self) -> Result<Value> {
        let c = self.connect()?;
        let value=c.query_row("SELECT pid,heartbeat,slack_state,error FROM runtime WHERE singleton=1",[],|r|Ok(json!({"pid":r.get::<_,i64>(0)?,"heartbeat":r.get::<_,i64>(1)?,"slack":r.get::<_,String>(2)?,"error":r.get::<_,Option<String>>(3)?}))).optional()?;
        Ok(value.unwrap_or(json!({"slack":"stopped"})))
    }
    pub fn participated(&self, channel: &str, thread: &str) -> Result<bool> {
        Ok(self
            .connect()?
            .query_row(
                "SELECT 1 FROM conversations WHERE channel=?1 AND thread=?2",
                params![channel, thread],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
    pub fn accept(&self, incoming: &Incoming, command: bool) -> Result<Option<Accepted>> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = event_key(&incoming.channel, &incoming.message_ts);
        if tx
            .query_row("SELECT 1 FROM messages WHERE event_key=?1", [&key], |_| {
                Ok(())
            })
            .optional()?
            .is_some()
        {
            return Ok(None);
        }
        let thread = incoming.conversation_thread.as_deref().unwrap_or("");
        let conversation = tx
            .query_row(
                "SELECT id FROM conversations WHERE channel=?1 AND thread=?2",
                params![incoming.channel, thread],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(id);
        tx.execute(
            "INSERT OR IGNORE INTO conversations(id,channel,thread,kind) VALUES(?1,?2,?3,?4)",
            params![
                conversation,
                incoming.channel,
                thread,
                incoming.channel_kind
            ],
        )?;
        let busy = tx.query_row(
            "SELECT count(*) FROM runs WHERE conversation_id=?1 AND state IN ('queued','running')",
            [&conversation],
            |r| r.get::<_, i64>(0),
        )? > 0;
        let message = id();
        let run = (!command).then(id);
        if let Some(run) = &run {
            tx.execute("INSERT INTO runs(id,kind,conversation_id,trigger,request,state,created_at) VALUES(?1,'chat',?2,'slack',?3,'queued',?4)",params![run,conversation,incoming.text,now()])?;
        }
        tx.execute("INSERT INTO messages(id,direction,conversation_id,run_id,channel,thread,slack_ts,event_key,body,payload,state,created_at) VALUES(?1,'in',?2,?3,?4,?5,?6,?7,?8,?9,'received',?10)",params![message,conversation,run,incoming.channel,incoming.thread_ts.as_deref().unwrap_or(""),incoming.message_ts,key,incoming.text,serde_json::to_string(incoming)?,now()])?;
        tx.commit()?;
        Ok(Some(Accepted {
            conversation_id: conversation,
            run_id: run,
            busy,
        }))
    }
    pub fn enqueue_job(
        &self,
        name: &str,
        trigger: &str,
        tick: Option<&str>,
    ) -> Result<Option<String>> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // An occurrence runs at most once, even if the local clock repeats a minute.
        if let Some(tick) = tick
            && tx
                .query_row(
                    "SELECT 1 FROM runs WHERE job_name=?1 AND occurrence=?2",
                    params![name, tick],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
        {
            return Ok(None);
        }
        let busy = tx.query_row(
            "SELECT count(*) FROM runs WHERE job_name=?1 AND state IN ('queued','running')",
            [name],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if busy {
            if trigger == "manual" {
                bail!("Job {name} is already queued or running")
            }
            tx.commit()?;
            return Ok(None);
        }
        let run = id();
        tx.execute("INSERT INTO runs(id,kind,job_name,trigger,occurrence,request,state,created_at) VALUES(?1,'job',?2,?3,?4,'','queued',?5)",params![run,name,trigger,tick,now()])?;
        tx.commit()?;
        Ok(Some(run))
    }
    pub fn claim(&self) -> Result<Option<Run>> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run=tx.query_row("SELECT r.id,r.kind,r.conversation_id,r.job_name,r.trigger,r.request,r.created_at,coalesce(m.payload,'{}'),c.session_id,r.occurrence FROM runs r LEFT JOIN conversations c ON c.id=r.conversation_id LEFT JOIN messages m ON m.run_id=r.id AND m.direction='in' WHERE r.state='queued' AND NOT EXISTS(SELECT 1 FROM runs active WHERE active.state='running' AND ((active.conversation_id IS NOT NULL AND active.conversation_id=r.conversation_id) OR (active.job_name IS NOT NULL AND active.job_name=r.job_name))) ORDER BY r.created_at,r.rowid LIMIT 1",[],|r|Ok(Run{id:r.get(0)?,kind:r.get(1)?,conversation:r.get(2)?,job:r.get(3)?,trigger:r.get(4)?,request:r.get(5)?,created_at:r.get(6)?,input:serde_json::from_str(&r.get::<_,String>(7)?).unwrap_or(Value::Null),session:r.get(8)?,occurrence:r.get(9)?})).optional()?;
        if let Some(r) = &run {
            tx.execute(
                "UPDATE runs SET state='running',started_at=?2 WHERE id=?1",
                params![r.id, now()],
            )?;
        }
        tx.commit()?;
        Ok(run)
    }
    pub fn snapshot(
        &self,
        run: &str,
        prompt: &str,
        context: &Value,
        settings: &Value,
    ) -> Result<()> {
        self.connect()?.execute(
            "UPDATE runs SET resolved_prompt=?2,context=?3,settings=?4 WHERE id=?1",
            params![run, prompt, context.to_string(), settings.to_string()],
        )?;
        Ok(())
    }
    /// Keeps postrun retry history with the run's stored context.
    pub fn record_attempts(&self, run: &str, attempts: &[Value]) -> Result<()> {
        self.connect()?.execute(
            "UPDATE runs SET context=json_set(coalesce(context,'{}'),'$.attempts',json(?2)) WHERE id=?1",
            params![run, Value::from(attempts.to_vec()).to_string()],
        )?;
        Ok(())
    }
    pub fn finish(
        &self,
        run: &str,
        state: &str,
        result: &str,
        error: Option<&str>,
        session: Option<&str>,
    ) -> Result<()> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE runs SET state=?2,result=?3,error=?4,finished_at=?5 WHERE id=?1",
            params![run, state, result, error, now()],
        )?;
        if let Some(session) = session {
            tx.execute("UPDATE conversations SET session_id=?2 WHERE id=(SELECT conversation_id FROM runs WHERE id=?1)",params![run,session])?;
        }
        tx.execute("UPDATE messages SET context_run_id=?1 WHERE run_id=?1 AND direction='out' AND EXISTS(SELECT 1 FROM runs r JOIN conversations c ON c.id=r.conversation_id WHERE r.id=?1 AND r.kind='chat' AND c.channel=messages.channel AND (c.kind IN ('dm','im') OR coalesce(nullif(messages.thread,''),messages.slack_ts)=c.thread))",[run])?;
        if state == "succeeded" {
            tx.execute("UPDATE messages SET context_run_id=?1 WHERE id IN (SELECT value FROM json_each((SELECT coalesce(json_extract(context,'$.background_ids'),'[]') FROM runs WHERE id=?1)))",[run])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn cancelled(&self, run: &str) -> Result<bool> {
        Ok(self.connect()?.query_row(
            "SELECT cancel_requested FROM runs WHERE id=?1",
            [run],
            |r| r.get::<_, bool>(0),
        )?)
    }
    pub fn stop(&self, conversation: &str) -> Result<()> {
        let c = self.connect()?;
        c.execute(
            "UPDATE runs SET cancel_requested=1 WHERE conversation_id=?1 AND state='running'",
            [conversation],
        )?;
        c.execute("UPDATE runs SET state='cancelled',finished_at=?2 WHERE conversation_id=?1 AND state='queued'",params![conversation,now()])?;
        Ok(())
    }
    pub fn clear(&self, conversation: &str) -> Result<()> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n: i64 = tx.query_row(
            "SELECT count(*) FROM runs WHERE conversation_id=?1 AND state IN ('queued','running')",
            [conversation],
            |r| r.get(0),
        )?;
        ensure!(
            n == 0,
            "Conversation is busy. Use !stop or wait before !clear."
        );
        tx.execute(
            "UPDATE conversations SET session_id=NULL WHERE id=?1",
            [conversation],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn conversation_status(&self, conversation: &str) -> Result<Value> {
        let c = self.connect()?;
        let session: Option<String> = c.query_row(
            "SELECT session_id FROM conversations WHERE id=?1",
            [conversation],
            |r| r.get(0),
        )?;
        let mut stmt=c.prepare("SELECT state,count(*) FROM runs WHERE conversation_id=?1 AND state IN ('queued','running') GROUP BY state")?;
        let counts = stmt
            .query_map([conversation], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()?;
        Ok(
            json!({"conversation":conversation,"has_session":session.is_some(),"running":counts.get("running").unwrap_or(&0),"queued":counts.get("queued").unwrap_or(&0)}),
        )
    }
    pub fn run(&self, run: &str) -> Result<Value> {
        self.connect()?.query_row("SELECT id,kind,job_name,state,created_at,started_at,finished_at,result,error,json_extract(context,'$.attempts') FROM runs WHERE id=?1",[run],|r|Ok(json!({"id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"job":r.get::<_,Option<String>>(2)?,"state":r.get::<_,String>(3)?,"created_at":r.get::<_,i64>(4)?,"started_at":r.get::<_,Option<i64>>(5)?,"finished_at":r.get::<_,Option<i64>>(6)?,"result":r.get::<_,Option<String>>(7)?,"error":r.get::<_,Option<String>>(8)?,"attempts":r.get::<_,Option<String>>(9)?.and_then(|a|serde_json::from_str::<Value>(&a).ok())}))).context("Unknown run")
    }
    pub fn last_job(&self, name: &str) -> Result<Value> {
        let id = self
            .connect()?
            .query_row(
                "SELECT id FROM runs WHERE job_name=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                [name],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        id.map(|id| self.run(&id)).unwrap_or(Ok(Value::Null))
    }
    pub fn outgoing(
        &self,
        destination: &Destination,
        payloads: Vec<Value>,
        files: &[PathBuf],
        run: Option<&str>,
        background: bool,
    ) -> Result<Vec<String>> {
        for file in files {
            ensure!(
                file.is_file(),
                "Attachment is not a file: {}",
                file.display()
            );
        }
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = queue(&tx, destination, payloads, files, run, background)?;
        tx.commit()?;
        Ok(ids)
    }
    /// Records an event no route accepts and queues its reply, once per event key.
    pub fn unconfigured(
        &self,
        channel: &str,
        ts: &str,
        thread: Option<&str>,
        text: &str,
        reply: &Destination,
        payloads: Vec<Value>,
    ) -> Result<()> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute("INSERT OR IGNORE INTO messages(id,direction,channel,thread,slack_ts,event_key,body,payload,state,created_at) VALUES(?1,'in',?2,?3,?4,?5,?6,'{}','ignored',?7)",params![id(),channel,thread.unwrap_or(""),ts,event_key(channel,ts),text,now()])? == 1 {
            queue(&tx, reply, payloads, &[], None, false)?;
            tx.commit()?;
        }
        Ok(())
    }
    pub fn claim_delivery(&self) -> Result<Option<Delivery>> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let delivery=tx.query_row("SELECT id,channel,thread,payload,file_path FROM messages WHERE direction='out' AND state='pending' ORDER BY created_at,rowid LIMIT 1",[],|r|{let thread:String=r.get(2)?;Ok(Delivery{id:r.get(0)?,destination:Destination{channel:r.get(1)?,thread:(!thread.is_empty()).then_some(thread)},payload:serde_json::from_str(&r.get::<_,String>(3)?).unwrap_or(Value::Null),file:r.get::<_,Option<String>>(4)?.map(PathBuf::from)})}).optional()?;
        if let Some(d) = &delivery {
            tx.execute("UPDATE messages SET state='sending' WHERE id=?1", [&d.id])?;
        }
        tx.commit()?;
        Ok(delivery)
    }
    pub fn record_delivery(
        &self,
        id: &str,
        state: &str,
        message_ts: Option<&str>,
        remote_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        self.connect()?.execute(
            "UPDATE messages SET state=?2,slack_ts=?3,remote_id=?4,error=?5,sent_at=?6 WHERE id=?1",
            params![id, state, message_ts, remote_id, error, now()],
        )?;
        Ok(())
    }
    pub fn delivery(&self, id: &str) -> Result<Value> {
        self.connect()?.query_row("SELECT id,state,channel,thread,coalesce(remote_id,slack_ts),error,slack_ts FROM messages WHERE id=?1",[id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"channel":r.get::<_,String>(2)?,"thread":r.get::<_,String>(3)?,"receipt":r.get::<_,Option<String>>(4)?,"error":r.get::<_,Option<String>>(5)?,"message_ts":r.get::<_,Option<String>>(6)?}))).context("Unknown delivery")
    }
    pub fn background(&self, conversation: &str) -> Result<Vec<Value>> {
        let c = self.connect()?;
        let mut s=c.prepare("SELECT m.id,m.body,m.sent_at,m.run_id FROM messages m JOIN conversations c ON c.id=?1 WHERE m.direction='out' AND m.background=1 AND m.state='sent' AND m.context_run_id IS NULL AND m.channel=c.channel AND (c.kind IN ('dm','im') OR coalesce(nullif(m.thread,''),m.slack_ts)=c.thread) ORDER BY m.sent_at,m.rowid LIMIT 30")?;
        Ok(s.query_map([conversation],|r|Ok(json!({"id":r.get::<_,String>(0)?,"text":r.get::<_,String>(1)?,"sent_at":r.get::<_,Option<i64>>(2)?,"run_id":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    /// Work is never replayed. Pending outbound sends are safe to keep; in-flight sends are uncertain.
    pub fn recover(&self) -> Result<Vec<Value>> {
        let mut c = self.connect()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let messages = {
            let mut s=tx.prepare("SELECT m.payload FROM messages m JOIN runs r ON r.id=m.run_id WHERE m.direction='in' AND r.state IN ('queued','running')")?;
            s.query_map([], |r| {
                Ok(serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute("UPDATE runs SET state='interrupted',error='Service stopped before completion; run again explicitly.',finished_at=?1 WHERE state IN ('queued','running')",[now()])?;
        tx.execute("UPDATE messages SET state='uncertain',error='Service stopped during delivery; inspect Slack before resending.' WHERE state='sending'",[])?;
        tx.commit()?;
        Ok(messages)
    }
}
