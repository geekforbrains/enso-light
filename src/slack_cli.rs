//! Small, single-page Slack Web API commands; no daemon or Socket Mode required.
use anyhow::{Result, bail, ensure};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::slack::Slack;

#[derive(Debug, Args)]
pub struct ListPage {
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=200))]
    pub limit: u16,
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(Debug, Args)]
pub struct HistoryPage {
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=200))]
    pub limit: u16,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub oldest: Option<String>,
    #[arg(long)]
    pub latest: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum SlackCommand {
    /// List one page of conversations accessible to the bot.
    Channels {
        #[arg(long, default_value = "public_channel,private_channel,im,mpim")]
        types: String,
        #[command(flatten)]
        page: ListPage,
    },
    /// Read a conversation's metadata.
    Channel { channel: String },
    /// List one page of workspace users.
    Users {
        #[command(flatten)]
        page: ListPage,
    },
    /// Read a user's profile.
    User { user: String },
    /// Read one page of conversation history; thread replies need `thread`.
    History {
        channel: String,
        #[command(flatten)]
        page: HistoryPage,
    },
    /// Read one page of a thread, including its parent when returned by Slack.
    Thread {
        channel: String,
        ts: String,
        #[command(flatten)]
        page: HistoryPage,
    },
    /// Read an exact message; supply --thread ROOT for a thread reply.
    Message {
        channel: String,
        ts: String,
        #[arg(long)]
        thread: Option<String>,
    },
    /// Search one history page with --channel, or Slack's index with a user token.
    Search {
        query: String,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long, requires = "channel")]
        thread: Option<String>,
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Get the permanent URL for a message.
    Link { channel: String, ts: String },
    /// Add a reaction, or remove the bot's reaction with --remove.
    React {
        channel: String,
        ts: String,
        emoji: String,
        #[arg(long)]
        remove: bool,
    },
}

fn page(limit: u16, cursor: Option<String>, max: u16) -> Result<Value> {
    ensure!(
        (1..=max).contains(&limit),
        "limit must be between 1 and {max}"
    );
    let mut params = json!({"limit":limit});
    if let Some(cursor) = cursor.filter(|s| !s.is_empty()) {
        params["cursor"] = json!(cursor);
    }
    Ok(params)
}

fn timestamp(value: &str) -> Result<()> {
    let mut parts = value.split('.');
    let seconds = parts.next().unwrap_or("");
    let fraction = parts.next();
    ensure!(
        !seconds.is_empty()
            && seconds.bytes().all(|c| c.is_ascii_digit())
            && fraction.is_none_or(|s| !s.is_empty()
                && s.len() <= 6
                && s.bytes().all(|c| c.is_ascii_digit()))
            && parts.next().is_none(),
        "Timestamp must be Unix seconds, optionally followed by up to six decimal digits"
    );
    Ok(())
}

fn identifier(value: &str, prefixes: &str, label: &str) -> Result<()> {
    ensure!(
        value.len() >= 2
            && value.len() <= 80
            && value.chars().next().is_some_and(|c| prefixes.contains(c))
            && value.bytes().all(|c| c.is_ascii_alphanumeric()),
        "{label} must be a Slack ID"
    );
    Ok(())
}

fn history_params(channel: &str, thread: Option<&str>, options: HistoryPage) -> Result<Value> {
    identifier(channel, "CGD", "Channel")?;
    let mut params = page(options.limit, options.cursor, 200)?;
    params["channel"] = json!(channel);
    if let Some(thread) = thread {
        timestamp(thread)?;
        params["ts"] = json!(thread);
    }
    for (key, value) in [("oldest", options.oldest), ("latest", options.latest)] {
        if let Some(value) = value {
            timestamp(&value)?;
            params[key] = json!(value);
        }
    }
    Ok(params)
}

pub async fn execute(command: SlackCommand, slack: Slack) -> Result<Value> {
    match command {
        SlackCommand::Channels {
            types,
            page: options,
        } => {
            let types: Vec<_> = types.split(',').map(str::trim).collect();
            ensure!(
                types
                    .iter()
                    .all(|s| matches!(*s, "public_channel" | "private_channel" | "im" | "mpim")),
                "types must be a comma-separated list of public_channel,private_channel,im,mpim"
            );
            let mut params = page(options.limit, options.cursor, 200)?;
            params["types"] = json!(types.join(","));
            slack.read("conversations.list", &params).await
        }
        SlackCommand::Channel { channel } => {
            identifier(&channel, "CGD", "Channel")?;
            slack
                .read("conversations.info", &json!({"channel":channel}))
                .await
        }
        SlackCommand::Users { page: options } => {
            slack
                .read("users.list", &page(options.limit, options.cursor, 200)?)
                .await
        }
        SlackCommand::User { user } => {
            identifier(&user, "UW", "User")?;
            slack.read("users.info", &json!({"user":user})).await
        }
        SlackCommand::History { channel, page } => {
            slack
                .read(
                    "conversations.history",
                    &history_params(&channel, None, page)?,
                )
                .await
        }
        SlackCommand::Thread { channel, ts, page } => {
            slack
                .read(
                    "conversations.replies",
                    &history_params(&channel, Some(&ts), page)?,
                )
                .await
        }
        SlackCommand::Message {
            channel,
            ts,
            thread,
        } => {
            timestamp(&ts)?;
            let mut params = history_params(
                &channel,
                thread.as_deref(),
                HistoryPage {
                    limit: if thread.is_some() { 2 } else { 1 },
                    cursor: None,
                    oldest: thread.as_ref().map(|_| ts.clone()),
                    latest: Some(ts.clone()),
                },
            )?;
            params["inclusive"] = json!(true);
            let method = if thread.is_some() {
                "conversations.replies"
            } else {
                "conversations.history"
            };
            let mut response = slack.read(method, &params).await?;
            let message = response["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|m| m["ts"].as_str() == Some(&ts))
                .cloned();
            let Some(message) = message else {
                bail!(
                    "Exact Slack message was not returned. Check its timestamp and channel; use --thread ROOT for a thread reply."
                )
            };
            response.as_object_mut().unwrap().remove("messages");
            response["message"] = message;
            response["channel"] = json!(channel);
            Ok(response)
        }
        SlackCommand::Search {
            query,
            channel,
            thread,
            limit,
            cursor,
        } => {
            ensure!(!query.trim().is_empty(), "Search query must not be empty");
            ensure!(
                (1..=100).contains(&limit),
                "Search limit must be between 1 and 100"
            );
            if let Some(channel) = channel {
                let params = history_params(
                    &channel,
                    thread.as_deref(),
                    HistoryPage {
                        limit,
                        cursor,
                        oldest: None,
                        latest: None,
                    },
                )?;
                let method = if thread.is_some() {
                    "conversations.replies"
                } else {
                    "conversations.history"
                };
                let response = slack.read(method, &params).await?;
                let messages = response["messages"].as_array().cloned().unwrap_or_default();
                let needle = query.to_lowercase();
                let matches: Vec<_> = messages
                    .iter()
                    .filter(|m| {
                        m["text"]
                            .as_str()
                            .is_some_and(|text| text.to_lowercase().contains(&needle))
                    })
                    .cloned()
                    .collect();
                let next_cursor = response
                    .pointer("/response_metadata/next_cursor")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Ok(
                    json!({"ok":true,"mode":if thread.is_some(){"thread_history"}else{"channel_history"},"query":query,"channel":channel,"thread":thread,"scanned":messages.len(),"matches":matches,"has_more":response["has_more"].as_bool().unwrap_or(false)||!next_cursor.is_empty(),"next_cursor":next_cursor,"response_metadata":response["response_metadata"],"scope":"Case-insensitive literal text matching in this one returned history page only. Thread replies and other messages not returned by this page are excluded."}),
                )
            } else {
                ensure!(thread.is_none(), "--thread requires --channel");
                let params = json!({"query":query,"count":limit,"cursor":cursor.filter(|s| !s.is_empty()).unwrap_or_else(||"*".into())});
                let mut response = slack.search_messages(&params).await?;
                response["mode"] = json!("workspace_search");
                Ok(response)
            }
        }
        SlackCommand::Link { channel, ts } => {
            identifier(&channel, "CGD", "Channel")?;
            timestamp(&ts)?;
            slack
                .read(
                    "chat.getPermalink",
                    &json!({"channel":channel,"message_ts":ts}),
                )
                .await
        }
        SlackCommand::React {
            channel,
            ts,
            emoji,
            remove,
        } => {
            identifier(&channel, "CGD", "Channel")?;
            timestamp(&ts)?;
            slack.reaction(&channel, &ts, &emoji, !remove).await?;
            Ok(
                json!({"ok":true,"channel":channel,"ts":ts,"emoji":emoji.trim_matches(':'),"present":!remove}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tokens;
    use clap::Parser;
    use std::collections::BTreeMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: SlackCommand,
    }
    fn command(args: &[&str]) -> SlackCommand {
        Cli::try_parse_from(std::iter::once("slack").chain(args.iter().copied()))
            .unwrap()
            .command
    }
    fn tokens() -> Tokens {
        Tokens {
            bot: "fake-bot".into(),
            user: Some("fake-user".into()),
            ..Tokens::default()
        }
    }
    async fn fixture(
        tokens: Tokens,
        responses: Vec<Value>,
    ) -> (Slack, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                if key.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                let body = response.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
            requests
        });
        (
            Slack::with_test_endpoint(&tokens, format!("http://{address}")),
            task,
        )
    }
    fn request_url(request: &str) -> url::Url {
        let target = request
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap();
        url::Url::parse(&format!("http://fixture{target}")).unwrap()
    }
    fn query(request: &str) -> BTreeMap<String, String> {
        request_url(request)
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    #[test]
    fn clap_rejects_invalid_page_limits_and_thread_without_channel() {
        for args in [
            vec!["slack", "channels", "--limit", "201"],
            vec!["slack", "users", "--limit", "0"],
            vec!["slack", "history", "C1", "--limit", "0"],
            vec!["slack", "search", "term", "--limit", "101"],
            vec!["slack", "search", "term", "--thread", "123.000001"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[tokio::test]
    async fn reads_preserve_one_page_and_cursor_even_when_page_is_empty() {
        let response = json!({"ok":true,"channels":[],"members":[],"messages":[],"has_more":true,"response_metadata":{"next_cursor":"next-page","warnings":["fixture-warning"]}});
        for (args, method) in [
            (
                vec!["channels", "--limit", "7", "--cursor", "cursor +&"],
                "conversations.list",
            ),
            (
                vec!["users", "--limit", "7", "--cursor", "cursor +&"],
                "users.list",
            ),
            (
                vec!["history", "C1", "--limit", "7", "--cursor", "cursor +&"],
                "conversations.history",
            ),
            (
                vec![
                    "thread",
                    "C1",
                    "100.000001",
                    "--limit",
                    "7",
                    "--cursor",
                    "cursor +&",
                ],
                "conversations.replies",
            ),
        ] {
            let (slack, server) = fixture(tokens(), vec![response.clone()]).await;
            assert_eq!(execute(command(&args), slack).await.unwrap(), response);
            let requests = server.await.unwrap();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("GET "));
            assert_eq!(request_url(&requests[0]).path(), format!("/{method}"));
            assert_eq!(query(&requests[0])["limit"], "7");
            assert_eq!(query(&requests[0])["cursor"], "cursor +&");
            assert!(requests[0].contains("authorization: Bearer fake-bot"));
        }
    }

    #[tokio::test]
    async fn history_bounds_and_permalink_are_sent_as_exact_string_parameters() {
        let (slack, server) = fixture(
            tokens(),
            vec![
                json!({"ok":true,"messages":[]}),
                json!({"ok":true,"permalink":"https://example.invalid/permalink"}),
            ],
        )
        .await;
        execute(
            command(&[
                "history",
                "C1",
                "--oldest",
                "100.000001",
                "--latest",
                "200.000002",
            ]),
            slack.clone(),
        )
        .await
        .unwrap();
        execute(command(&["link", "C1", "150.000003"]), slack)
            .await
            .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(query(&requests[0])["oldest"], "100.000001");
        assert_eq!(query(&requests[0])["latest"], "200.000002");
        assert_eq!(query(&requests[1])["message_ts"], "150.000003");
        assert_eq!(request_url(&requests[1]).path(), "/chat.getPermalink");
    }

    #[tokio::test]
    async fn exact_message_never_substitutes_an_adjacent_message() {
        let (slack, server) = fixture(tokens(), vec![json!({"ok":true,"messages":[{"ts":"99.000001","text":"Wrong earlier message"}],"has_more":false})]).await;
        let error = execute(command(&["message", "C1", "100.000001"]), slack)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Exact Slack message was not returned"));
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 1);
        let params = query(&requests[0]);
        assert_eq!(params["latest"], "100.000001");
        assert_eq!(params["inclusive"], "true");
        assert_eq!(params["limit"], "1");
    }

    #[tokio::test]
    async fn exact_thread_message_selects_the_requested_reply_and_keeps_metadata() {
        let reply = json!({"ts":"105.000001","thread_ts":"100.000001","text":"Requested reply"});
        let (slack, server) = fixture(tokens(), vec![json!({"ok":true,"messages":[{"ts":"100.000001","text":"Parent"},reply],"response_metadata":{"next_cursor":"more"}})]).await;
        let result = execute(
            command(&["message", "C1", "105.000001", "--thread", "100.000001"]),
            slack,
        )
        .await
        .unwrap();
        assert_eq!(result["message"], reply);
        assert!(result.get("messages").is_none());
        assert_eq!(result["response_metadata"]["next_cursor"], "more");
        let requests = server.await.unwrap();
        assert_eq!(request_url(&requests[0]).path(), "/conversations.replies");
        let params = query(&requests[0]);
        assert_eq!(params["ts"], "100.000001");
        assert_eq!(params["oldest"], "105.000001");
        assert_eq!(params["latest"], "105.000001");
        assert_eq!(params["inclusive"], "true");
    }

    #[tokio::test]
    async fn scoped_search_is_literal_case_insensitive_and_explicitly_one_page() {
        let response = json!({"ok":true,"messages":[
            {"ts":"100.000001","text":"Report [READY]"},
            {"ts":"101.000001","text":"Report ready","reply_count":42},
            {"ts":"102.000001","text":"Another report [ready]"}
        ],"has_more":true,"response_metadata":{"next_cursor":"next"}});
        for (args, method, mode) in [
            (
                vec!["search", "[ready]", "--channel", "C1"],
                "conversations.history",
                "channel_history",
            ),
            (
                vec![
                    "search",
                    "[ready]",
                    "--channel",
                    "C1",
                    "--thread",
                    "99.000001",
                ],
                "conversations.replies",
                "thread_history",
            ),
        ] {
            let (slack, server) = fixture(tokens(), vec![response.clone()]).await;
            let result = execute(command(&args), slack).await.unwrap();
            assert_eq!(result["mode"], mode);
            assert_eq!(result["scanned"], 3);
            assert_eq!(result["matches"].as_array().unwrap().len(), 2);
            assert_eq!(result["next_cursor"], "next");
            assert_eq!(result["has_more"], true);
            assert!(
                result["scope"]
                    .as_str()
                    .unwrap()
                    .contains("not returned by this page are excluded")
            );
            let requests = server.await.unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(request_url(&requests[0]).path(), format!("/{method}"));
            assert!(!query(&requests[0]).contains_key("query"));
            assert!(requests[0].contains("authorization: Bearer fake-bot"));
        }
    }

    #[tokio::test]
    async fn scoped_search_does_not_mistake_an_empty_page_for_end_of_history() {
        let (slack, server) = fixture(tokens(), vec![json!({"ok":true,"messages":[],"response_metadata":{"next_cursor":"next-empty-page"}})]).await;
        let result = execute(
            command(&["search", "term", "--channel", "C1", "--cursor", "previous"]),
            slack,
        )
        .await
        .unwrap();
        assert_eq!(result["scanned"], 0);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["next_cursor"], "next-empty-page");
        let requests = server.await.unwrap();
        assert_eq!(query(&requests[0])["cursor"], "previous");
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn native_search_uses_only_user_token_and_native_cursor_parameters() {
        let response = json!({"ok":true,"query":"in:general report","messages":{"matches":[],"pagination":{"total_count":12},"next_cursor":"next-native"}});
        let mut tokens = tokens();
        tokens.bot.clear();
        let (slack, server) = fixture(tokens, vec![response.clone(), response.clone()]).await;
        for args in [
            vec!["search", "in:general report"],
            vec![
                "search",
                "in:general report",
                "--cursor",
                "next-native",
                "--limit",
                "100",
            ],
        ] {
            let result = execute(command(&args), slack.clone()).await.unwrap();
            assert_eq!(result["mode"], "workspace_search");
            assert_eq!(result["messages"], response["messages"]);
        }
        let requests = server.await.unwrap();
        for request in &requests {
            assert_eq!(request_url(request).path(), "/search.messages");
            assert!(request.starts_with("GET "));
            assert!(request.contains("authorization: Bearer fake-user"));
            assert!(!request.contains("fake-bot"));
            assert_eq!(query(request)["query"], "in:general report");
        }
        assert_eq!(query(&requests[0])["cursor"], "*");
        assert_eq!(query(&requests[0])["count"], "50");
        assert_eq!(query(&requests[1])["cursor"], "next-native");
        assert_eq!(query(&requests[1])["count"], "100");
    }

    #[tokio::test]
    async fn missing_tokens_and_bad_identifiers_fail_without_a_network_request() {
        let slack = Slack::with_test_endpoint(&Tokens::default(), "http://127.0.0.1:1".into());
        let error = execute(command(&["search", "term"]), slack.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SLACK_USER_TOKEN") && error.contains("search:read"));
        let error = execute(command(&["users"]), slack.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SLACK_BOT_TOKEN is not set"));
        assert!(
            execute(command(&["history", "not-a-channel-id"]), slack.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains("Slack ID")
        );
        assert!(
            execute(command(&["message", "C1", "1e100"]), slack)
                .await
                .unwrap_err()
                .to_string()
                .contains("Timestamp")
        );
    }

    #[tokio::test]
    async fn scope_errors_are_actionable_and_credentials_and_query_are_never_echoed() {
        let (slack, server) = fixture(
            tokens(),
            vec![
                json!({"ok":false,"error":"missing_scope","needed":"fake-user"}),
                json!({"ok":false,"error":"invalid_auth"}),
            ],
        )
        .await;
        let scope = execute(command(&["users"]), slack.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(scope.contains("users:read") && scope.contains("reauthorize"));
        assert!(!scope.contains("fake-user"));
        let auth = execute(command(&["search", "sensitive-search-term"]), slack)
            .await
            .unwrap_err()
            .to_string();
        assert!(auth.contains("invalid_auth"));
        assert!(!auth.contains("fake-user") && !auth.contains("sensitive-search-term"));
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn reaction_commands_reuse_idempotent_add_and_remove() {
        let (slack, server) = fixture(
            tokens(),
            vec![
                json!({"ok":false,"error":"already_reacted"}),
                json!({"ok":false,"error":"no_reaction"}),
            ],
        )
        .await;
        let added = execute(
            command(&["react", "C1", "100.000001", ":eyes:"]),
            slack.clone(),
        )
        .await
        .unwrap();
        let removed = execute(
            command(&["react", "C1", "100.000001", "eyes", "--remove"]),
            slack,
        )
        .await
        .unwrap();
        assert_eq!(added["present"], true);
        assert_eq!(added["emoji"], "eyes");
        assert_eq!(removed["present"], false);
        let requests = server.await.unwrap();
        assert_eq!(request_url(&requests[0]).path(), "/reactions.add");
        assert_eq!(request_url(&requests[1]).path(), "/reactions.remove");
        for request in requests {
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["channel"], "C1");
            assert_eq!(body["timestamp"], "100.000001");
            assert_eq!(body["name"], "eyes");
        }
    }
}
