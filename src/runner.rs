use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use crate::config::Provider;

const MAX_STDOUT: usize = 16 * 1024 * 1024;
const MAX_STDERR: usize = 1024 * 1024;

pub struct Request {
    pub workspace: PathBuf,
    pub settings: Provider,
    pub timeout_seconds: u64,
    pub prompt: String,
    pub session_id: Option<String>,
    pub env: BTreeMap<String, String>,
    pub images: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Outcome {
    pub text: String,
    pub session_id: Option<String>,
}

pub async fn execute(request: Request, cancel: CancellationToken) -> Result<Outcome> {
    ensure!(
        request.workspace.is_absolute(),
        "workspace must be an absolute path"
    );
    if let Some(id) = &request.session_id {
        validate_session(id)?;
    }
    let args = arguments(&request)?;
    let program = request
        .settings
        .executable
        .as_deref()
        .unwrap_or(&request.settings.cli);
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&request.workspace)
        .envs(&request.env);
    // A service launched from a Claude terminal is still an independent client.
    if request.settings.cli == "claude" && !request.env.contains_key("CLAUDECODE") {
        command.env_remove("CLAUDECODE");
    }
    let output = process(
        command,
        request.prompt.into_bytes(),
        request.timeout_seconds,
        cancel,
    )
    .await?;
    if !output.status.success() {
        bail!(
            "{} ({} exited {})",
            failure_hint(&output.stderr),
            request.settings.cli,
            output.status
        );
    }
    parse_output(
        &request.settings.cli,
        &output.stdout,
        request.session_id.as_deref(),
    )
}

fn arguments(request: &Request) -> Result<Vec<String>> {
    let settings = &request.settings;
    let mut args: Vec<String> = match settings.cli.as_str() {
        "codex" => vec![
            "exec".into(),
            "--json".into(),
            "--skip-git-repo-check".into(),
        ],
        "claude" => [
            "--print",
            "--verbose",
            "--output-format",
            "stream-json",
            "--permission-prompts",
            "none",
        ]
        .map(str::to_owned)
        .to_vec(),
        _ => bail!("provider cli must be codex or claude"),
    };
    args.extend(settings.args.clone());
    if let Some(model) = &settings.model {
        ensure!(
            !model.is_empty() && !model.starts_with('-'),
            "invalid model setting"
        );
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &settings.effort {
        ensure!(
            !effort.is_empty() && !effort.starts_with('-'),
            "invalid effort setting"
        );
        if settings.cli == "codex" {
            args.extend([
                "--config".into(),
                format!("model_reasoning_effort={}", serde_json::to_string(effort)?),
            ]);
        } else {
            args.extend(["--effort".into(), effort.clone()]);
        }
    }
    if let Some(id) = &request.session_id {
        args.extend([
            if settings.cli == "codex" {
                "resume"
            } else {
                "--resume"
            }
            .into(),
            id.clone(),
        ]);
    }
    if settings.cli == "codex" {
        for path in &request.images {
            let path = path.to_str().context("image paths must be UTF-8")?;
            ensure!(
                Path::new(path).is_absolute() && !path.contains(','),
                "Codex image paths must be absolute and contain no commas"
            );
            args.extend(["--image".into(), path.into()]);
        }
        if !request.images.is_empty() {
            args.push("--".into());
        }
        args.push("-".into());
    }
    Ok(args)
}

fn validate_session(id: &str) -> Result<()> {
    ensure!(
        id.len() == 36
            && id.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            }),
        "session_unavailable: expected an exact native session UUID; use !clear to start fresh"
    );
    Ok(())
}

fn parse_output(cli: &str, stdout: &str, expected_session: Option<&str>) -> Result<Outcome> {
    let mut text = String::new();
    let mut session_id: Option<String> = None;
    let mut completed = false;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value = serde_json::from_str(line)
            .map_err(|_| anyhow!("invalid_output: CLI emitted invalid JSON"))?;
        let kind = event["type"]
            .as_str()
            .context("invalid_output: CLI event has no type")?;
        let id = if cli == "codex" && kind == "thread.started" {
            event["thread_id"].as_str()
        } else if cli == "claude" {
            event["session_id"].as_str()
        } else {
            None
        };
        if let Some(id) = id {
            validate_session(id)?;
            ensure!(
                expected_session.is_none_or(|expected| expected.eq_ignore_ascii_case(id))
                    && session_id
                        .as_ref()
                        .is_none_or(|old| old.eq_ignore_ascii_case(id)),
                "session_unavailable: CLI returned a different session; use !clear to start fresh"
            );
            session_id = Some(id.into());
        }
        match (cli, kind) {
            ("codex", "item.completed") if event["item"]["type"] == "agent_message" => {
                text = event["item"]["text"]
                    .as_str()
                    .context("invalid_output: missing agent text")?
                    .into();
            }
            ("codex", "turn.completed") => {
                ensure!(!completed, "invalid_output: CLI emitted multiple results");
                completed = true;
            }
            ("codex", "turn.failed" | "error") => bail!("{}", failure_hint(&event.to_string())),
            ("claude", "result") => {
                if event["is_error"].as_bool().unwrap_or(false) || event["subtype"] != "success" {
                    bail!("{}", failure_hint(&event.to_string()));
                }
                ensure!(!completed, "invalid_output: CLI emitted multiple results");
                text = event["result"]
                    .as_str()
                    .context("invalid_output: missing result text")?
                    .into();
                completed = true;
            }
            ("claude", "error") => bail!("{}", failure_hint(&event.to_string())),
            _ => {}
        }
    }
    ensure!(
        completed,
        "invalid_output: CLI exited without a completed turn"
    );
    ensure!(
        session_id.is_some(),
        "invalid_output: CLI returned no resumable session ID"
    );
    Ok(Outcome { text, session_id })
}

// Native failures can contain prompts, tool output, or credentials. Return a useful
// category, never arbitrary provider stderr or JSON to Slack or service logs.
fn failure_hint(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if [
        "no rollout found",
        "no conversation found",
        "no conversation session",
        "session not found",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        "session_unavailable: native session could not be resumed; use !clear"
    } else if [
        "not logged in",
        "not authenticated",
        "authentication",
        "unauthorized",
        "api key",
        "api_key",
        "login required",
        "401",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        "authentication_failed: check the selected CLI's login on this machine"
    } else if ["rate limit", "quota", "usage limit", "429"]
        .iter()
        .any(|s| lower.contains(s))
    {
        "usage_limit: the selected CLI reported an account or rate limit"
    } else if lower.contains("effort")
        && ["invalid", "unsupported", "not supported"]
            .iter()
            .any(|s| lower.contains(s))
    {
        "invalid_effort: check the provider effort for the selected CLI and model"
    } else if lower.contains("model")
        && [
            "invalid",
            "unsupported",
            "not supported",
            "not found",
            "unavailable",
        ]
        .iter()
        .any(|s| lower.contains(s))
    {
        "invalid_model: check the provider model for the selected CLI"
    } else if ["approval", "permission denied", "permission prompt"]
        .iter()
        .any(|s| lower.contains(s))
    {
        "permission_required: check the native CLI's permissions and provider args"
    } else {
        "execution_failed: native CLI failed; inspect its local session for details"
    }
}

pub async fn hook(
    path: &Path,
    input: &Value,
    env: &BTreeMap<String, String>,
    timeout_seconds: u64,
    cancel: CancellationToken,
) -> Result<String> {
    let path = path
        .canonicalize()
        .context("hook file could not be resolved")?;
    let mut command = Command::new("/bin/bash");
    command
        .arg(&path)
        .current_dir(path.parent().context("hook has no parent directory")?)
        .envs(env);
    let output = process(command, serde_json::to_vec(input)?, timeout_seconds, cancel).await?;
    ensure!(
        output.status.success(),
        "hook_failed: {} exited {}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        output.status
    );
    Ok(output.stdout)
}

struct Captured {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

struct ProcessGroup(u32);
impl ProcessGroup {
    fn kill(&self) {
        // Every child gets its own group, so cancellation also kills its tools.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

async fn process(
    mut command: Command,
    input: Vec<u8>,
    timeout_seconds: u64,
    cancel: CancellationToken,
) -> Result<Captured> {
    ensure!(timeout_seconds > 0, "timeout_seconds must be positive");
    ensure!(!cancel.is_cancelled(), "cancelled: run was stopped");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .context("could not launch executable; check the provider executable and service PATH")?;
    let group = ProcessGroup(child.id().context("child process has no ID")?);
    let mut stdin = child.stdin.take().context("child stdin missing")?;
    let stdout = child.stdout.take().context("child stdout missing")?;
    let stderr = child.stderr.take().context("child stderr missing")?;
    let result = {
        let operation = async {
            let wait = async {
                let status = child.wait().await.context("could not wait for child")?;
                group.kill();
                Ok::<_, anyhow::Error>(status)
            };
            let write = async {
                if let Err(error) = stdin.write_all(&input).await
                    && error.kind() != std::io::ErrorKind::BrokenPipe
                {
                    return Err(anyhow!("could not write child input: {error}"));
                }
                drop(stdin);
                Ok::<_, anyhow::Error>(())
            };
            let (status, stdout, stderr, ()) = tokio::try_join!(
                wait,
                read_bounded(stdout, MAX_STDOUT),
                read_bounded(stderr, MAX_STDERR),
                write
            )?;
            Ok(Captured {
                status,
                stdout,
                stderr,
            })
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow!("cancelled: run was stopped")),
            result = tokio::time::timeout(Duration::from_secs(timeout_seconds), operation) => result.unwrap_or_else(|_| Err(anyhow!("timed_out: run exceeded {timeout_seconds} seconds"))),
        }
    };
    group.kill();
    if result.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    result
}

async fn read_bounded(mut reader: impl AsyncRead + Unpin, limit: usize) -> Result<String> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .context("could not read child output")?;
        if count == 0 {
            break;
        }
        ensure!(
            output.len() + count <= limit,
            "output_limit: child output exceeded the capture limit"
        );
        output.extend_from_slice(&buffer[..count]);
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    const SESSION: &str = "00000000-0000-0000-0000-000000000001";

    fn request(root: &Path, cli: &str, script: &str) -> Request {
        let executable = root.join("fake-cli");
        std::fs::write(&executable, format!("#!/bin/bash\n{script}\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        Request {
            workspace: root.into(),
            settings: Provider {
                cli: cli.into(),
                executable: Some(executable.to_string_lossy().into_owned()),
                model: None,
                effort: None,
                args: vec![],
            },
            timeout_seconds: 5,
            prompt: "a literal `prompt` $(not a command)".into(),
            session_id: None,
            env: BTreeMap::new(),
            images: vec![],
        }
    }

    #[tokio::test]
    async fn executes_stdin_in_workspace_with_environment_and_exact_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let script = format!(
            "cat > input\nprintf '%s\\n' \"$@\" > args\nprintf '%s' \"$CUSTOM\" > env\nprintf '%s\\n' '{{\"type\":\"thread.started\",\"thread_id\":\"{SESSION}\"}}' '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"ready\"}}}}' '{{\"type\":\"turn.completed\"}}'"
        );
        let mut req = request(temp.path(), "codex", &script);
        req.settings.args = vec!["--config".into(), "a literal value".into()];
        req.settings.effort = Some("high".into());
        req.session_id = Some(SESSION.into());
        req.env.insert("CUSTOM".into(), "available".into());
        let prompt = req.prompt.clone();
        let result = execute(req, CancellationToken::new()).await.unwrap();
        assert_eq!(result.text, "ready");
        assert_eq!(result.session_id.as_deref(), Some(SESSION));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("input")).unwrap(),
            prompt
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("env")).unwrap(),
            "available"
        );
        let args = std::fs::read_to_string(temp.path().join("args")).unwrap();
        assert!(args.contains("--config\na literal value\n"));
        assert!(args.ends_with(&format!("resume\n{SESSION}\n-\n")));
    }

    #[tokio::test]
    async fn claude_result_and_failures_are_checked() {
        let temp = tempfile::tempdir().unwrap();
        let req = request(
            temp.path(),
            "claude",
            &format!(
                "cat >/dev/null\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"{SESSION}\",\"result\":\"done\"}}'"
            ),
        );
        assert_eq!(
            execute(req, CancellationToken::new()).await.unwrap().text,
            "done"
        );
        let bad = format!(
            r#"{{"type":"result","subtype":"error_during_execution","session_id":"{SESSION}","is_error":true,"result":"secret"}}"#
        );
        let error = parse_output("claude", &bad, None).unwrap_err().to_string();
        assert!(error.contains("execution_failed"));
        assert!(!error.contains("secret"));
        assert!(parse_output("codex", r#"{"type":"turn.completed"}"#, None).is_err());
        assert!(parse_output("codex", "not json", None).is_err());
    }

    #[tokio::test]
    async fn timeout_kills_child_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let mut req = request(
            temp.path(),
            "codex",
            "sleep 30 &\necho $! > child-pid\nwait",
        );
        // Allow the shell to start under parallel CI load before testing timeout cleanup.
        req.timeout_seconds = 3;
        let error = execute(req, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("timed_out:"));
        let pid: i32 = std::fs::read_to_string(temp.path().join("child-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..50 {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("child process survived timeout");
    }

    #[tokio::test]
    async fn cancellation_and_bounded_output() {
        let temp = tempfile::tempdir().unwrap();
        let req = request(temp.path(), "codex", "cat >/dev/null\nsleep 30");
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });
        assert!(
            execute(req, token)
                .await
                .unwrap_err()
                .to_string()
                .starts_with("cancelled:")
        );
        assert!(
            read_bounded(&b"12345"[..], 4)
                .await
                .unwrap_err()
                .to_string()
                .starts_with("output_limit:")
        );
    }

    #[tokio::test]
    async fn hooks_need_no_executable_bit_and_receive_json() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prerun.sh");
        std::fs::write(
            &path,
            "cat > input.json\nprintf '%s' '{\"vars\":{\"COUNT\":3}}'\n",
        )
        .unwrap();
        let input = serde_json::json!({"run_id":"run-1"});
        let result = hook(&path, &input, &BTreeMap::new(), 5, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&result).unwrap()["vars"]["COUNT"],
            3
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &std::fs::read(temp.path().join("input.json")).unwrap()
            )
            .unwrap(),
            input
        );
    }

    #[test]
    fn resumed_session_must_match_and_images_are_separate_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let mut req = request(temp.path(), "codex", "");
        req.images = vec![temp.path().join("one image.png")];
        let args = arguments(&req).unwrap();
        assert_eq!(&args[args.len() - 2..], ["--", "-"]);
        let output = format!(r#"{{"type":"thread.started","thread_id":"{SESSION}"}}"#);
        assert!(
            parse_output(
                "codex",
                &output,
                Some("00000000-0000-0000-0000-000000000002")
            )
            .unwrap_err()
            .to_string()
            .starts_with("session_unavailable:")
        );
    }
}
