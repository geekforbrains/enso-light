use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Execution {
    pub cli: String,
    pub executable: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub args: Vec<String>,
    pub timeout_seconds: u64,
}

impl Default for Execution {
    fn default() -> Self {
        Self {
            cli: "claude".into(),
            executable: None,
            model: Some("sonnet".into()),
            effort: Some("high".into()),
            args: Vec::new(),
            timeout_seconds: 1800,
        }
    }
}

impl Execution {
    pub fn validate(&self) -> Result<()> {
        if !matches!(self.cli.as_str(), "claude" | "codex") {
            bail!("execution.cli must be claude or codex");
        }
        if self.timeout_seconds == 0 {
            bail!("execution.timeout_seconds must be greater than zero");
        }
        if self.executable.as_ref().is_some_and(|s| s.is_empty()) {
            bail!("execution.executable must not be empty");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub channel: String,
    #[serde(default)]
    pub thread: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mentions {
    pub top_level: bool,
    pub thread: bool,
}

impl Default for Mentions {
    fn default() -> Self {
        Self {
            top_level: true,
            thread: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlackConfig {
    pub bot_token: String,
    pub app_token: String,
    pub dm_users: Vec<String>,
    pub channels: BTreeMap<String, Mentions>,
    pub mentions: Mentions,
    pub working_reaction: String,
    pub queued_message: String,
    pub timeout_message: String,
    pub notify: Option<Destination>,
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            app_token: String::new(),
            dm_users: Vec::new(),
            channels: BTreeMap::new(),
            mentions: Mentions::default(),
            working_reaction: "thinking_face".into(),
            queued_message: "Queued — I’ll get to this after the current turn.".into(),
            timeout_message: "This turn timed out. You can send another message to continue."
                .into(),
            notify: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub execution: Execution,
    pub slack: SlackConfig,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        self.execution.validate()?;
        if self.slack.bot_token.is_empty() || self.slack.app_token.is_empty() {
            bail!("Slack bot_token and app_token are required; set them in config.json or .env");
        }
        if self.slack.dm_users.iter().any(String::is_empty)
            || self.slack.channels.keys().any(String::is_empty)
        {
            bail!("Slack user and channel IDs must not be empty");
        }
        if self.slack.working_reaction.is_empty() {
            bail!("slack.working_reaction must not be empty");
        }
        if self
            .slack
            .notify
            .as_ref()
            .is_some_and(|d| d.channel.is_empty())
        {
            bail!("slack.notify.channel must not be empty");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    /// Values from .env; child processes inherit the host environment first.
    pub env: BTreeMap<String, String>,
}

pub fn load(home: &Path) -> Result<Loaded> {
    let mut env = BTreeMap::new();
    let dotenv = home.join(".env");
    if dotenv.exists() {
        let entries = dotenvy::from_path_iter(&dotenv).context("cannot read Enso .env")?;
        for entry in entries {
            // dotenv parse errors can contain the original line, including secrets.
            let (key, value) = entry
                .map_err(|_| anyhow::anyhow!("invalid Enso .env; use KEY=value assignments"))?;
            env.insert(key, value);
        }
    }
    let mut variables: BTreeMap<String, String> = std::env::vars().collect();
    variables.extend(env.clone());
    let config = fs::read(home.join("config.json"))
        .context("cannot read Enso config.json; run enso init first")?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&config).context("invalid Enso config.json")?;
    expand_value(&mut value, &variables)?;
    native_defaults(&mut value);
    inherit_mentions(&mut value);
    // Avoid serde's invalid-value diagnostics echoing a substituted credential.
    let config: Config = serde_json::from_value(value).map_err(|_| {
        anyhow::anyhow!("invalid config.json fields or value types; see docs/configuration.md")
    })?;
    Ok(Loaded { config, env })
}

fn native_defaults(value: &mut serde_json::Value) {
    if let Some(execution) = value
        .get_mut("execution")
        .and_then(serde_json::Value::as_object_mut)
        && execution.get("cli").and_then(serde_json::Value::as_str) == Some("codex")
    {
        for name in ["model", "effort"] {
            execution.entry(name).or_insert(serde_json::Value::Null);
        }
    }
}

fn inherit_mentions(value: &mut serde_json::Value) {
    let Some(slack) = value
        .get_mut("slack")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let global = slack
        .get("mentions")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(channels) = slack
        .get_mut("channels")
        .and_then(serde_json::Value::as_object_mut)
    {
        for channel in channels
            .values_mut()
            .filter_map(serde_json::Value::as_object_mut)
        {
            for name in ["top_level", "thread"] {
                if !channel.contains_key(name)
                    && let Some(rule) = global.get(name)
                {
                    channel.insert(name.into(), rule.clone());
                }
            }
        }
    }
}

fn expand_value(value: &mut serde_json::Value, variables: &BTreeMap<String, String>) -> Result<()> {
    match value {
        serde_json::Value::String(text) => *text = expand(text, variables)?,
        serde_json::Value::Array(items) => {
            for item in items {
                expand_value(item, variables)?;
            }
        }
        serde_json::Value::Object(items) => {
            for item in items.values_mut() {
                expand_value(item, variables)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Substitute only the original input, never shell-evaluate or expand a value again.
pub fn expand(input: &str, variables: &BTreeMap<String, String>) -> Result<String> {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let end = tail
            .find('}')
            .context("unclosed ${NAME} in configuration")?;
        let name = &tail[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("invalid environment variable placeholder in configuration");
        }
        output.push_str(
            variables
                .get(name)
                .with_context(|| format!("missing environment variable {name}"))?,
        );
        rest = &tail[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

pub fn init(home: &Path) -> Result<()> {
    for dir in [
        "",
        "workspace",
        "workspace/uploads",
        "workspace/.skills",
        "workspace/.skills/enso",
        "jobs",
        "logs",
    ] {
        let path = home.join(dir);
        if !path.exists() {
            fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    create_file(
        &home.join("config.json"),
        include_str!("../bundled/config.json"),
    )?;
    create_file(
        &home.join(".env"),
        "# Slack app credentials. Restart Enso after changes.\nSLACK_BOT_TOKEN=\nSLACK_APP_TOKEN=\n",
    )?;
    create_file(
        &home.join("workspace/AGENTS.md"),
        include_str!("../bundled/AGENTS.md"),
    )?;
    create_file(
        &home.join("workspace/.skills/enso/SKILL.md"),
        include_str!("../bundled/SKILL.md"),
    )?;
    let claude = home.join("workspace/CLAUDE.md");
    if fs::symlink_metadata(&claude).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        symlink("AGENTS.md", claude)?;
    }
    Ok(())
}

fn create_file(path: &Path, content: &str) -> Result<()> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => file
            .write_all(content.as_bytes())
            .with_context(|| format!("write {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("create {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_is_literal_once_and_requires_variables() {
        let vars = BTreeMap::from([("TOKEN".into(), "a\"$()${MISSING}".into())]);
        assert_eq!(
            expand("before ${TOKEN} after", &vars).unwrap(),
            "before a\"$()${MISSING} after"
        );
        assert!(expand("${MISSING}", &vars).is_err());
        assert!(expand("${TOKEN", &vars).is_err());
    }

    #[test]
    fn init_preserves_existing_files_and_builds_workspace() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path()).unwrap();
        let config = temp.path().join("config.json");
        fs::write(&config, "existing").unwrap();
        init(temp.path()).unwrap();
        assert_eq!(fs::read_to_string(config).unwrap(), "existing");
        assert_eq!(
            fs::read_link(temp.path().join("workspace/CLAUDE.md")).unwrap(),
            Path::new("AGENTS.md")
        );
        assert!(
            temp.path()
                .join("workspace/.skills/enso/SKILL.md")
                .is_file()
        );
        assert_eq!(
            fs::metadata(temp.path().join(".env"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn load_resolves_dotenv_without_interpreting_json() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path()).unwrap();
        fs::write(
            temp.path().join(".env"),
            "SLACK_BOT_TOKEN='token\"with-quote'\nSLACK_APP_TOKEN=app\n",
        )
        .unwrap();
        let loaded = load(temp.path()).unwrap();
        assert_eq!(loaded.config.slack.bot_token, "token\"with-quote");
        assert_eq!(loaded.config.slack.app_token, "app");
        assert_eq!(loaded.env["SLACK_APP_TOKEN"], "app");
        assert!(loaded.config.slack.dm_users.is_empty());
    }

    #[test]
    fn channel_mentions_inherit_global_then_override() {
        let mut value = serde_json::json!({"slack": {
            "mentions": {"top_level":false,"thread":false},
            "channels": {"C1":{},"C2":{"top_level":true}}
        }});
        inherit_mentions(&mut value);
        let config: Config = serde_json::from_value(value).unwrap();
        assert!(!config.slack.channels["C1"].top_level);
        assert!(!config.slack.channels["C1"].thread);
        assert!(config.slack.channels["C2"].top_level);
        assert!(!config.slack.channels["C2"].thread);
    }

    #[test]
    fn codex_uses_native_defaults_when_model_and_effort_are_omitted() {
        let mut value = serde_json::json!({"execution":{"cli":"codex"}});
        native_defaults(&mut value);
        let config: Config = serde_json::from_value(value).unwrap();
        assert!(config.execution.model.is_none());
        assert!(config.execution.effort.is_none());
        let mut value =
            serde_json::json!({"execution":{"cli":"codex","model":"chosen","effort":"low"}});
        native_defaults(&mut value);
        let config: Config = serde_json::from_value(value).unwrap();
        assert_eq!(config.execution.model.as_deref(), Some("chosen"));
        assert_eq!(config.execution.effort.as_deref(), Some("low"));
    }
}
