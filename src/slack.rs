//! Slack admission, Socket Mode, and Web API. Secrets never enter diagnostics.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt, net::TcpStream};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use crate::{
    config::{Destination, SlackConfig},
    formatting,
};

const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_FILES: usize = 20;

#[derive(Clone)]
pub struct Slack {
    config: SlackConfig,
    http: Client,
    base: String,
}

pub struct Identity {
    pub bot_user_id: String,
    pub team_id: String,
}

pub struct Socket {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

pub struct Envelope {
    pub envelope_id: String,
    pub payload: Value,
}

/// A confirmed upload and its message location, when Slack exposes that share.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileReceipt {
    pub file_id: String,
    pub message_ts: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Incoming {
    pub event_id: String,
    pub channel: String,
    pub channel_kind: String,
    pub user_id: String,
    pub user_name: Option<String>,
    pub text: String,
    pub message_ts: String,
    pub thread_ts: Option<String>,
    pub conversation_thread: Option<String>,
    pub reply: Destination,
    pub files: Vec<RemoteFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteFile {
    pub id: String,
    pub name: String,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    pub download_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub path: PathBuf,
    pub media_type: Option<String>,
}

impl Slack {
    pub fn new(config: &SlackConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            http: Client::builder()
                .timeout(Duration::from_secs(90))
                .connect_timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("Cannot create Slack HTTP client")?,
            base: "https://slack.com/api".into(),
        })
    }

    /// Only explicit rate-limit rejections are retried. A transport error may
    /// follow remote acceptance, so never blindly repeat a message or file share.
    async fn request(&self, method: &str, body: &Value, app_token: bool) -> Result<Value> {
        let token = if app_token {
            &self.config.app_token
        } else {
            &self.config.bot_token
        };
        for attempt in 0..3 {
            let address = format!("{}/{method}", self.base);
            let request = if matches!(method, "files.info" | "users.info" | "conversations.info") {
                self.http.get(&address).query(body)
            } else {
                self.http.post(&address).json(body)
            };
            let response =
                request.bearer_auth(token).send().await.map_err(|_| {
                    anyhow!("Slack {method} request failed; delivery may be uncertain")
                })?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS && attempt < 2 {
                let seconds = response
                    .headers()
                    .get("retry-after")
                    .and_then(|s| s.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1);
                ensure!(
                    seconds <= 30,
                    "Slack {method} is rate limited; try again later"
                );
                tokio::time::sleep(Duration::from_secs(seconds.max(1))).await;
                continue;
            }
            if response.status().is_server_error() {
                bail!(
                    "Slack {method} returned HTTP {}; delivery may be uncertain",
                    response.status().as_u16()
                );
            }
            ensure!(
                response.status().is_success(),
                "Slack {method} returned HTTP {}",
                response.status().as_u16()
            );
            let value: Value = response.json().await.map_err(|_| {
                anyhow!("Slack {method} returned an unreadable response; delivery may be uncertain")
            })?;
            return Ok(value);
        }
        unreachable!()
    }

    async fn api(&self, method: &str, body: &Value) -> Result<Value> {
        let value = self.request(method, body, false).await?;
        check_api(method, &value)?;
        Ok(value)
    }

    pub async fn identity(&self) -> Result<Identity> {
        let value = self.api("auth.test", &json!({})).await?;
        Ok(Identity {
            bot_user_id: required(&value, "user_id", "Slack auth.test")?.into(),
            team_id: required(&value, "team_id", "Slack auth.test")?.into(),
        })
    }

    pub async fn user_name(&self, user: &str) -> Result<Option<String>> {
        let value = self.api("users.info", &json!({"user":user})).await?;
        Ok([
            "/user/profile/display_name",
            "/user/profile/real_name",
            "/user/name",
        ]
        .iter()
        .find_map(|path| {
            value
                .pointer(path)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        }))
    }

    pub async fn channel_name(&self, channel: &str) -> Result<Option<String>> {
        let value = self
            .api("conversations.info", &json!({"channel":channel}))
            .await?;
        Ok(value
            .pointer("/channel/name")
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    pub async fn connect(&self) -> Result<Socket> {
        let value = self
            .request("apps.connections.open", &json!({}), true)
            .await?;
        check_api("apps.connections.open", &value)?;
        let address = required(&value, "url", "Slack Socket Mode")?;
        let url = url::Url::parse(address).map_err(|_| anyhow!("Invalid Slack Socket Mode URL"))?;
        ensure!(
            url.scheme() == "wss"
                && slack_host(&url)
                && url.username().is_empty()
                && url.password().is_none(),
            "Refusing Socket Mode URL outside Slack WSS hosts"
        );
        let (socket, _) = tokio_tungstenite::connect_async(address)
            .await
            .map_err(|_| anyhow!("Slack Socket Mode handshake failed"))?;
        Ok(Socket { socket })
    }

    /// The caller looks up thread participation in SQLite before admission.
    /// DMs share one conversation but replies follow the incoming DM thread.
    pub fn normalize(
        &self,
        payload: &Value,
        bot_user_id: &str,
        participated: bool,
    ) -> Result<Option<Incoming>> {
        let event = payload.get("event").unwrap_or(payload);
        if !matches!(event["type"].as_str(), Some("message" | "app_mention"))
            || event.get("bot_id").is_some()
            || event["user"].as_str() == Some(bot_user_id)
            || event["hidden"] == true
        {
            return Ok(None);
        }
        if let Some(subtype) = event["subtype"].as_str()
            && !matches!(subtype, "file_share" | "thread_broadcast" | "me_message")
        {
            return Ok(None);
        }
        let (Some(user), Some(channel), Some(ts)) = (
            event["user"].as_str(),
            event["channel"].as_str(),
            event["ts"].as_str(),
        ) else {
            return Ok(None);
        };
        let dm = event["channel_type"] == "im" || channel.starts_with('D');
        if dm && !self.config.dm_users.iter().any(|id| id == user) {
            return Ok(None);
        }
        let rules = if dm {
            &self.config.mentions
        } else {
            let Some(rules) = self.config.channels.get(channel) else {
                return Ok(None);
            };
            rules
        };
        let original = event["text"].as_str().unwrap_or("");
        let mention = format!("<@{bot_user_id}>");
        let mentioned = original.contains(&mention);
        let thread = event["thread_ts"].as_str().filter(|thread| *thread != ts);
        if !dm
            && !mentioned
            && if thread.is_some() {
                rules.thread || !participated
            } else {
                rules.top_level
            }
        {
            return Ok(None);
        }
        let text = original.replace(&mention, "").trim().to_owned();
        let files: Vec<_> = event["files"]
            .as_array()
            .into_iter()
            .flatten()
            .map(remote_file)
            .collect();
        ensure!(
            files.len() <= MAX_FILES,
            "A Slack message may have at most {MAX_FILES} attachments"
        );
        if text.is_empty() && files.is_empty() {
            return Ok(None);
        }
        let conversation_thread = (!dm).then(|| thread.unwrap_or(ts).to_owned());
        Ok(Some(Incoming {
            event_id: payload["event_id"].as_str().unwrap_or(ts).to_owned(),
            channel: channel.into(),
            channel_kind: if dm {
                "im".into()
            } else {
                event["channel_type"].as_str().unwrap_or("channel").into()
            },
            user_id: user.into(),
            user_name: event
                .pointer("/user_profile/display_name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            text,
            message_ts: ts.into(),
            thread_ts: thread.map(str::to_owned),
            conversation_thread,
            reply: Destination {
                channel: channel.into(),
                thread: if dm {
                    thread.map(str::to_owned)
                } else {
                    Some(thread.unwrap_or(ts).into())
                },
            },
            files,
        }))
    }

    pub async fn send_text(
        &self,
        destination: &Destination,
        text: &str,
        plain: bool,
    ) -> Result<Vec<String>> {
        let mut timestamps = Vec::new();
        for message in formatting::messages(text, plain)? {
            timestamps.push(self.send_payload(destination, &message).await?);
        }
        Ok(timestamps)
    }

    /// Payloads can carry a persisted UUID client_msg_id supplied by the outbox.
    pub async fn send_payload(&self, destination: &Destination, payload: &Value) -> Result<String> {
        ensure!(
            payload.is_object(),
            "Slack message payload must be an object"
        );
        let mut body = payload.clone();
        body["channel"] = json!(destination.channel);
        if let Some(thread) = &destination.thread {
            body["thread_ts"] = json!(thread);
        }
        let value = self.api("chat.postMessage", &body).await?;
        Ok(required(
            &value,
            "ts",
            "Slack accepted message but omitted timestamp; delivery uncertain",
        )?
        .into())
    }

    pub async fn reaction(
        &self,
        channel: &str,
        ts: &str,
        emoji: &str,
        present: bool,
    ) -> Result<()> {
        let emoji = emoji.trim_matches(':');
        ensure!(
            !emoji.is_empty()
                && emoji.len() <= 100
                && emoji
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+')),
            "Reaction must be a Slack emoji shortcode"
        );
        let method = if present {
            "reactions.add"
        } else {
            "reactions.remove"
        };
        let value = self
            .request(
                method,
                &json!({"channel":channel,"timestamp":ts,"name":emoji}),
                false,
            )
            .await?;
        let error = value["error"].as_str().unwrap_or("");
        if value["ok"] == true
            || present && error == "already_reacted"
            || !present && matches!(error, "no_reaction" | "message_not_found")
        {
            return Ok(());
        }
        check_api(method, &value)
    }

    pub async fn download(
        &self,
        files: &[RemoteFile],
        directory: &Path,
    ) -> Result<Vec<Attachment>> {
        ensure!(
            files.len() <= MAX_FILES,
            "A Slack message may have at most {MAX_FILES} attachments"
        );
        tokio::fs::create_dir_all(directory)
            .await
            .context("Cannot create attachment directory")?;
        let mut attachments = Vec::new();
        for (index, source) in files.iter().enumerate() {
            let mut file = source.clone();
            if file.download_url.is_none() && !file.id.is_empty() {
                let value = self.api("files.info", &json!({"file":file.id})).await?;
                file = remote_file(&value["file"]);
            }
            ensure!(
                file.size.unwrap_or(0) <= MAX_FILE_BYTES,
                "Attachment exceeds 50 MiB"
            );
            let address = file
                .download_url
                .as_deref()
                .context("Slack attachment unavailable; check files:read permission")?;
            validate_file_url(address)?;
            let response = self
                .http
                .get(address)
                .bearer_auth(&self.config.bot_token)
                .send()
                .await
                .map_err(|_| anyhow!("Slack attachment download failed"))?;
            ensure!(
                response.status().is_success(),
                "Slack attachment download returned HTTP {}",
                response.status().as_u16()
            );
            ensure!(
                response.content_length().unwrap_or(0) <= MAX_FILE_BYTES,
                "Attachment exceeds 50 MiB"
            );
            let safe_name = safe_filename(&file.name);
            let path = directory.join(format!("{index:02}-{safe_name}"));
            // Exclusive creation rejects collisions and symlink replacement.
            let mut output = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .context("Cannot create attachment file")?;
            let result: Result<()> = async {
                let mut bytes = response.bytes_stream();
                let mut count: u64 = 0;
                while let Some(chunk) = bytes.next().await {
                    let chunk =
                        chunk.map_err(|_| anyhow!("Slack attachment download interrupted"))?;
                    count += chunk.len() as u64;
                    ensure!(count <= MAX_FILE_BYTES, "Attachment exceeds 50 MiB");
                    output
                        .write_all(&chunk)
                        .await
                        .context("Cannot save attachment")?;
                }
                output.flush().await.context("Cannot save attachment")?;
                Ok(())
            }
            .await;
            drop(output);
            if let Err(error) = result {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(error);
            }
            attachments.push(Attachment {
                name: file.name,
                path,
                media_type: file.media_type,
            });
        }
        Ok(attachments)
    }

    pub async fn send_file(&self, destination: &Destination, path: &Path) -> Result<FileReceipt> {
        let file = tokio::fs::File::open(path)
            .await
            .context("Cannot open outgoing attachment")?;
        let metadata = file
            .metadata()
            .await
            .context("Cannot inspect outgoing attachment")?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_FILE_BYTES,
            "Attachment must be a regular file no larger than 50 MiB"
        );
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment");
        let value = self
            .api(
                "files.getUploadURLExternal",
                &json!({"filename":name,"length":metadata.len()}),
            )
            .await?;
        let address = required(&value, "upload_url", "Slack file upload")?;
        validate_file_url(address)?;
        let id = required(&value, "file_id", "Slack file upload")?;
        let response = self
            .http
            .post(address)
            .header(reqwest::header::CONTENT_LENGTH, metadata.len())
            .body(reqwest::Body::wrap_stream(
                tokio_util::io::ReaderStream::new(file),
            ))
            .send()
            .await
            .map_err(|_| anyhow!("Slack file upload failed before sharing"))?;
        ensure!(
            response.status().is_success(),
            "Slack file upload returned HTTP {}",
            response.status().as_u16()
        );
        let mut body = json!({"files":[{"id":id,"title":name}],"channel_id":destination.channel});
        if let Some(thread) = &destination.thread {
            body["thread_ts"] = json!(thread);
        }
        self.api("files.completeUploadExternal", &body).await?;
        // File sharing is already confirmed. A missing/slow metadata lookup must
        // never turn this into a failed send or invite a duplicate upload.
        let message_ts = tokio::time::timeout(
            Duration::from_secs(3),
            self.file_message_ts(id, destination),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        Ok(FileReceipt {
            file_id: id.into(),
            message_ts,
        })
    }

    /// Resolve the actual share message, not the file creation timestamp or a
    /// share in some other conversation. No timestamp is guessed when absent.
    pub async fn file_message_ts(
        &self,
        file_id: &str,
        destination: &Destination,
    ) -> Result<Option<String>> {
        let value = self.api("files.info", &json!({"file":file_id})).await?;
        ensure!(
            value["file"]["id"].as_str() == Some(file_id),
            "Slack files.info returned a different file"
        );
        let shares = &value["file"]["shares"];
        Ok(["public", "private"]
            .into_iter()
            .flat_map(|visibility| {
                shares[visibility][&destination.channel]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .filter_map(|share| {
                let ts = share["ts"].as_str()?;
                let thread = share["thread_ts"]
                    .as_str()
                    .filter(|value| !value.is_empty());
                let matches = match destination.thread.as_deref() {
                    Some(expected) => thread == Some(expected),
                    None => thread.is_none() || thread == Some(ts),
                };
                matches.then_some(())?;
                Some((timestamp_key(ts)?, ts))
            })
            .max_by_key(|(key, _)| *key)
            .map(|(_, ts)| ts.to_owned()))
    }
}

fn timestamp_key(value: &str) -> Option<(u64, u64)> {
    let (seconds, micros) = value.split_once('.')?;
    if seconds.is_empty()
        || micros.is_empty()
        || micros.len() > 6
        || !seconds
            .bytes()
            .chain(micros.bytes())
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((
        seconds.parse().ok()?,
        micros.parse::<u64>().ok()? * 10_u64.pow(6 - micros.len() as u32),
    ))
}

fn remote_file(value: &Value) -> RemoteFile {
    RemoteFile {
        id: value["id"].as_str().unwrap_or("").into(),
        name: value["name"].as_str().unwrap_or("attachment").into(),
        media_type: value["mimetype"].as_str().map(str::to_owned),
        size: value["size"].as_u64(),
        download_url: value["url_private_download"]
            .as_str()
            .or_else(|| value["url_private"].as_str())
            .map(str::to_owned),
    }
}

fn required<'a>(value: &'a Value, field: &str, context: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("{context}: missing {field}"))
}

fn check_api(method: &str, value: &Value) -> Result<()> {
    if value["ok"] == true {
        return Ok(());
    }
    // Echo only a bounded API error identifier, never arbitrary remote text.
    let code = value["error"]
        .as_str()
        .filter(|s| s.len() < 80 && s.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .unwrap_or("unknown_error");
    if matches!(
        code,
        "internal_error" | "fatal_error" | "service_unavailable"
    ) {
        bail!("Slack {method} failed ({code}); delivery may be uncertain");
    }
    bail!("Slack {method} failed ({code})")
}

fn slack_host(url: &url::Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "slack.com" || host.ends_with(".slack.com"))
}

fn validate_file_url(address: &str) -> Result<()> {
    let url = url::Url::parse(address).map_err(|_| anyhow!("Invalid Slack attachment URL"))?;
    ensure!(
        url.scheme() == "https"
            && slack_host(&url)
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443),
        "Refusing attachment URL outside Slack HTTPS hosts"
    );
    Ok(())
}

fn safe_filename(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("attachment");
    let mut safe = String::new();
    for c in name.chars().filter(|c| !c.is_control()) {
        if safe.len() + c.len_utf8() > 180 {
            break;
        }
        safe.push(c);
    }
    let name = safe.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if name.is_empty() {
        "attachment".into()
    } else {
        name.into()
    }
}

impl Socket {
    pub async fn next(&mut self) -> Result<Option<Envelope>> {
        while let Some(message) = self.socket.next().await {
            match message.map_err(|_| anyhow!("Slack Socket Mode disconnected"))? {
                Message::Text(text) => {
                    let value: Value =
                        serde_json::from_str(&text).context("Invalid Slack Socket Mode payload")?;
                    if value["type"] == "disconnect" {
                        return Ok(None);
                    }
                    if let Some(id) = value["envelope_id"].as_str() {
                        return Ok(Some(Envelope {
                            envelope_id: id.into(),
                            payload: value["payload"].clone(),
                        }));
                    }
                }
                Message::Ping(data) => {
                    self.socket
                        .send(Message::Pong(data))
                        .await
                        .map_err(|_| anyhow!("Slack heartbeat failed"))?;
                }
                Message::Close(_) => return Ok(None),
                _ => {}
            }
        }
        Ok(None)
    }

    pub async fn ack(&mut self, id: &str) -> Result<()> {
        self.socket
            .send(Message::Text(json!({"envelope_id":id}).to_string().into()))
            .await
            .map_err(|_| anyhow!("Slack acknowledgement failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mentions;
    use tokio::io::AsyncReadExt;

    fn slack() -> Slack {
        let mut config = SlackConfig {
            dm_users: vec!["U1".into()],
            ..SlackConfig::default()
        };
        config.channels.insert("C1".into(), Mentions::default());
        Slack::new(&config).unwrap()
    }

    fn message(channel: &str, text: &str, thread: Option<&str>) -> Value {
        let mut value = json!({"event_id":"Ev1","event":{"type":"message","channel":channel,"user":"U1","ts":"123.002","text":text}});
        if let Some(thread) = thread {
            value["event"]["thread_ts"] = json!(thread);
        }
        value
    }

    #[test]
    fn dm_uses_one_session_and_preserves_reply_thread() {
        let slack = slack();
        for thread in [None, Some("122.001")] {
            let result = slack
                .normalize(&message("D1", "hello", thread), "UBOT", false)
                .unwrap()
                .unwrap();
            assert_eq!(result.conversation_thread, None);
            assert_eq!(result.reply.thread.as_deref(), thread);
            assert_eq!(result.channel_kind, "im");
        }
        let mut other_user = message("D1", "hello", None);
        other_user["event"]["user"] = json!("U2");
        assert!(
            slack
                .normalize(&other_user, "UBOT", false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn channels_need_permission_and_independent_mention_rules() {
        let mut slack = slack();
        assert!(
            slack
                .normalize(&message("C2", "<@UBOT> hello", None), "UBOT", false)
                .unwrap()
                .is_none()
        );
        assert!(
            slack
                .normalize(&message("C1", "hello", None), "UBOT", false)
                .unwrap()
                .is_none()
        );
        let incoming = slack
            .normalize(&message("C1", "<@UBOT> hello", None), "UBOT", false)
            .unwrap()
            .unwrap();
        assert_eq!(incoming.text, "hello");
        assert_eq!(incoming.conversation_thread.as_deref(), Some("123.002"));
        assert_eq!(incoming.reply.thread.as_deref(), Some("123.002"));
        slack.config.channels.insert(
            "C1".into(),
            Mentions {
                top_level: false,
                thread: true,
            },
        );
        assert!(
            slack
                .normalize(&message("C1", "hello", None), "UBOT", false)
                .unwrap()
                .is_some()
        );
        assert!(
            slack
                .normalize(&message("C1", "hello", Some("122.001")), "UBOT", true)
                .unwrap()
                .is_none()
        );
        slack.config.channels.insert(
            "C1".into(),
            Mentions {
                top_level: true,
                thread: false,
            },
        );
        let followup = message("C1", "hello", Some("122.001"));
        assert!(slack.normalize(&followup, "UBOT", false).unwrap().is_none());
        assert!(slack.normalize(&followup, "UBOT", true).unwrap().is_some());
        assert!(
            slack
                .normalize(
                    &message("C1", "<@UBOT> hello", Some("122.001")),
                    "UBOT",
                    false
                )
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn ignores_edits_bots_and_accepts_attachment_only() {
        let slack = slack();
        let mut input = message("D1", "hello", None);
        input["event"]["subtype"] = json!("message_changed");
        assert!(slack.normalize(&input, "UBOT", false).unwrap().is_none());
        input["event"]["subtype"] = json!("file_share");
        input["event"]["text"] = json!("");
        input["event"]["files"] = json!([{"id":"F1","name":"document.pdf","mimetype":"application/pdf","url_private":"https://files.slack.com/private"}]);
        let incoming = slack.normalize(&input, "UBOT", false).unwrap().unwrap();
        assert_eq!(incoming.files.len(), 1);
        input["event"]["bot_id"] = json!("B1");
        assert!(slack.normalize(&input, "UBOT", false).unwrap().is_none());
    }

    #[test]
    fn filenames_and_remote_addresses_cannot_escape() {
        for value in [
            "https://evil.example/file",
            "https://slack.com.evil.example/file",
            "http://files.slack.com/file",
            "https://user@files.slack.com/file",
            "https://files.slack.com:99/file",
        ] {
            assert!(validate_file_url(value).is_err(), "{value}");
        }
        assert!(validate_file_url("https://files.slack.com/files-pri/file").is_ok());
        assert_eq!(safe_filename("../../secret.txt"), "secret.txt");
        assert_eq!(safe_filename("..\\..\\secret.txt"), "secret.txt");
        assert_eq!(safe_filename("\u{0}..."), "attachment");
        assert!(safe_filename(&"界".repeat(200)).len() <= 180);
    }

    /// A tiny local HTTP fixture records complete requests without third-party
    /// mocking code. Connection-close makes each expected request deterministic.
    async fn fixture(responses: Vec<String>) -> (Slack, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut connection, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut part = [0u8; 4096];
                loop {
                    let read = connection.read(&mut part).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&part[..read]);
                    if let Some(offset) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..offset]);
                        let length = headers
                            .lines()
                            .find_map(|s| {
                                let (key, value) = s.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if request.len() >= offset + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(request).unwrap());
                if !response.is_empty() {
                    connection.write_all(response.as_bytes()).await.unwrap();
                }
            }
            requests
        });
        let mut slack = slack();
        slack.base = format!("http://{address}");
        slack.config.bot_token = "secret-bot-token".into();
        (slack, task)
    }

    fn response(value: Value) -> String {
        let body = value.to_string();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn messages_include_destination_and_acknowledged_timestamp() {
        let (slack, server) = fixture(vec![response(json!({"ok":true,"ts":"123.001"}))]).await;
        let target = Destination {
            channel: "D1".into(),
            thread: Some("122.001".into()),
        };
        let ts = slack
            .send_payload(
                &target,
                &json!({"text":"hello","client_msg_id":"stable-id"}),
            )
            .await
            .unwrap();
        assert_eq!(ts, "123.001");
        let requests = server.await.unwrap();
        let request = &requests[0];
        assert!(request.starts_with("POST /chat.postMessage "));
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["channel"], "D1");
        assert_eq!(body["thread_ts"], "122.001");
        assert_eq!(body["client_msg_id"], "stable-id");
    }

    #[tokio::test]
    async fn uncertain_delivery_is_not_retried_or_leaked() {
        let (slack, server) = fixture(vec![String::new()]).await;
        let target = Destination {
            channel: "D1".into(),
            thread: None,
        };
        let error = slack
            .send_payload(&target, &json!({"text":"hello"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("uncertain"));
        assert!(!error.contains("secret-bot-token"));
        assert!(!error.contains("127.0.0.1"));
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn server_errors_are_uncertain_and_never_retried() {
        for reply in [
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into(),
            response(json!({"ok":false,"error":"internal_error"})),
            response(json!({"ok":false,"error":"fatal_error"})),
            response(json!({"ok":false,"error":"service_unavailable"})),
        ] {
            let (slack, server) = fixture(vec![reply]).await;
            let target = Destination {
                channel: "D1".into(),
                thread: None,
            };
            let error = slack
                .send_payload(&target, &json!({"text":"hello"}))
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("uncertain"), "{error}");
            assert_eq!(server.await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn file_share_lookup_matches_channel_thread_and_latest_actual_message() {
        let info = json!({"ok":true,"file":{"id":"F1","timestamp":987654321,"shares":{
            "public": {
                "C1": [
                    {"ts":"99.999999"},
                    {"ts":"100.000001"},
                    {"ts":"500.000001","thread_ts":"42.000001"},
                    {"ts":"600.000001","thread_ts":"43.000001"},
                    {"ts":"1000.not-a-timestamp"}
                ],
                "C2": [{"ts":"999.000001"}]
            },
            "private": {
                "C1": [{"ts":"102.000001","thread_ts":"102.000001"}],
                "D1": [{"ts":"200.000002"},{"ts":"300.000001","thread_ts":"50.000001"}]
            }
        }}});
        for (channel, thread, expected) in [
            ("C1", None, Some("102.000001")),
            ("C1", Some("42.000001"), Some("500.000001")),
            ("C1", Some("41.000001"), None),
            ("D1", None, Some("200.000002")),
            ("D1", Some("50.000001"), Some("300.000001")),
            ("C3", None, None),
        ] {
            let (slack, server) = fixture(vec![response(info.clone())]).await;
            let destination = Destination {
                channel: channel.into(),
                thread: thread.map(str::to_owned),
            };
            assert_eq!(
                slack
                    .file_message_ts("F1", &destination)
                    .await
                    .unwrap()
                    .as_deref(),
                expected
            );
            assert!(server.await.unwrap()[0].starts_with("GET /files.info?file=F1 "));
        }
    }

    #[tokio::test]
    async fn missing_file_shares_never_invent_a_message_timestamp() {
        let (slack, server) = fixture(vec![response(
            json!({"ok":true,"file":{"id":"F1","timestamp":12345}}),
        )])
        .await;
        let destination = Destination {
            channel: "C1".into(),
            thread: None,
        };
        assert!(
            slack
                .file_message_ts("F1", &destination)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn file_completion_errors_preserve_unknown_remote_acceptance() {
        let (slack, server) =
            fixture(vec![response(json!({"ok":false,"error":"internal_error"}))]).await;
        let error = slack
            .api(
                "files.completeUploadExternal",
                &json!({"files":[{"id":"F1"}],"channel_id":"D1"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("uncertain"));
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn explicit_rate_limit_retries_and_reactions_are_idempotent() {
        let limited = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into();
        let (slack, server) = fixture(vec![
            limited,
            response(json!({"ok":false,"error":"already_reacted"})),
            response(json!({"ok":false,"error":"no_reaction"})),
        ])
        .await;
        slack
            .reaction("D1", "123.001", ":eyes:", true)
            .await
            .unwrap();
        slack
            .reaction("D1", "123.001", "eyes", false)
            .await
            .unwrap();
        assert_eq!(server.await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn missing_file_urls_resolve_with_get_then_reject_non_slack_url() {
        let (slack, server) = fixture(vec![response(json!({"ok":true,"file":{"id":"F1","name":"report.txt","url_private":"https://outside.example/secret"}}))]).await;
        let directory = tempfile::tempdir().unwrap();
        let file = RemoteFile {
            id: "F1".into(),
            name: "report.txt".into(),
            media_type: None,
            size: None,
            download_url: None,
        };
        let error = slack
            .download(&[file], directory.path())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside Slack"));
        assert!(server.await.unwrap()[0].starts_with("GET /files.info?file=F1 "));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn socket_handles_hello_ping_ack_and_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(json!({"type":"hello"}).to_string().into()))
                .await
                .unwrap();
            socket
                .send(Message::Ping(vec![1, 2, 3].into()))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({"envelope_id":"E1","payload":{"event":{"type":"message"}}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Pong(_)
            ));
            let ack = socket.next().await.unwrap().unwrap();
            let value: Value = serde_json::from_str(ack.to_text().unwrap()).unwrap();
            assert_eq!(value["envelope_id"], "E1");
            socket
                .send(Message::Text(
                    json!({"type":"disconnect"}).to_string().into(),
                ))
                .await
                .unwrap();
        });
        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}"))
            .await
            .unwrap();
        let mut socket = Socket { socket };
        let envelope = socket.next().await.unwrap().unwrap();
        assert_eq!(envelope.envelope_id, "E1");
        socket.ack(&envelope.envelope_id).await.unwrap();
        assert!(socket.next().await.unwrap().is_none());
        server.await.unwrap();
    }
}
