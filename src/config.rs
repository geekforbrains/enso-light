use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

/// A named agent CLI setup; blank optional fields use the CLI's native default.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub cli: String,
    #[serde(default, deserialize_with = "optional")]
    pub executable: Option<String>,
    #[serde(default, deserialize_with = "optional")]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "optional")]
    pub effort: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

fn optional<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.filter(|s| !s.trim().is_empty()))
}

impl Provider {
    pub fn validate(&self, name: &str) -> Result<()> {
        ensure!(
            !self.cli.trim().is_empty(),
            "providers.{name}.cli is blank; set \"claude\" or \"codex\""
        );
        ensure!(
            matches!(self.cli.as_str(), "claude" | "codex"),
            "providers.{name}.cli must be \"claude\" or \"codex\""
        );
        for (field, value) in [("model", &self.model), ("effort", &self.effort)] {
            ensure!(
                !value.as_ref().is_some_and(|s| s.starts_with('-')),
                "providers.{name}.{field} must not start with -"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub provider: String,
    #[serde(default)]
    pub mention: Mention,
    #[serde(default = "timeout_seconds")]
    pub timeout_seconds: u64,
}

/// When channel messages must @mention the bot; DMs never need mentions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mention {
    /// At top level and in threads.
    #[default]
    Always,
    /// At top level; replies in a thread the bot joined need none.
    First,
    /// Never at top level; thread replies need one until the bot joins.
    Never,
}

/// A named working directory, optionally with its own provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub path: PathBuf,
    #[serde(default, deserialize_with = "optional")]
    pub provider: Option<String>,
}

fn timeout_seconds() -> u64 {
    1800
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub channel: String,
    #[serde(default)]
    pub thread: Option<String>,
}

impl Destination {
    /// Requires a Slack conversation ID and, for a thread, its root message timestamp.
    pub fn validate(&self) -> Result<()> {
        let id = self.channel.as_bytes();
        ensure!(
            id.len() > 1
                && matches!(id[0], b'C' | b'D' | b'G')
                && id
                    .iter()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
            "channel must be a Slack channel or DM ID such as C012345 or D012345, not {:?}",
            self.channel
        );
        if let Some(thread) = &self.thread {
            ensure!(
                thread.split_once('.').is_some_and(|(seconds, fraction)| {
                    !seconds.is_empty()
                        && !fraction.is_empty()
                        && seconds
                            .bytes()
                            .chain(fraction.bytes())
                            .all(|b| b.is_ascii_digit())
                }),
                "thread must be a Slack message timestamp such as 1700000000.000001, not {thread:?}"
            );
        }
        Ok(())
    }
}

/// A channel's workspace name, or its workspace and mention mode.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChannelRoute {
    Workspace(String),
    Settings(ChannelSettings),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSettings {
    pub workspace: String,
    #[serde(default)]
    pub mention: Option<Mention>,
}

impl ChannelRoute {
    pub fn workspace(&self) -> &str {
        match self {
            Self::Workspace(name) => name,
            Self::Settings(settings) => &settings.workspace,
        }
    }

    pub fn mention(&self) -> Option<Mention> {
        match self {
            Self::Workspace(_) => None,
            Self::Settings(settings) => settings.mention,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlackConfig {
    /// Slack user ID or `*` to workspace name.
    pub dms: BTreeMap<String, String>,
    /// Channel ID or `*` to route.
    pub channels: BTreeMap<String, ChannelRoute>,
    pub working_reaction: String,
    pub queued_message: String,
    pub timeout_message: String,
    pub unconfigured_message: String,
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            dms: BTreeMap::new(),
            channels: BTreeMap::new(),
            working_reaction: "thinking_face".into(),
            queued_message: "Queued — I’ll get to this after the current turn.".into(),
            timeout_message: "This turn timed out. You can send another message to continue."
                .into(),
            unconfigured_message: "Enso isn't set up for this conversation.".into(),
        }
    }
}

/// `*` or an ID made of one of `prefixes` and uppercase letters or digits.
fn route_key(key: &str, prefixes: &[u8]) -> bool {
    key == "*"
        || key.len() > 1
            && prefixes.contains(&key.as_bytes()[0])
            && key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Letters, digits, hyphens, and underscores, as for job names.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub defaults: Defaults,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
    #[serde(default)]
    pub slack: SlackConfig,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.providers.is_empty(),
            "providers must define at least one provider"
        );
        for (name, provider) in &self.providers {
            provider.validate(name)?;
        }
        ensure!(
            !self.defaults.provider.trim().is_empty(),
            "defaults.provider is blank; name one of providers"
        );
        ensure!(
            self.providers.contains_key(&self.defaults.provider),
            "defaults.provider {:?} is not defined in providers",
            self.defaults.provider
        );
        ensure!(
            self.defaults.timeout_seconds > 0,
            "defaults.timeout_seconds must be greater than zero"
        );
        ensure!(
            !self.workspaces.is_empty(),
            "workspaces must define at least one workspace"
        );
        for (name, workspace) in &self.workspaces {
            ensure!(
                valid_name(name),
                "workspace name {name:?} may contain only letters, numbers, hyphens, and underscores"
            );
            ensure!(
                workspace.path.is_absolute(),
                "workspaces.{name}.path must be absolute; use ${{ENSO_HOME}}/... for paths inside the Enso home"
            );
            if let Some(provider) = &workspace.provider {
                ensure!(
                    self.providers.contains_key(provider),
                    "workspaces.{name}.provider {provider:?} is not defined in providers"
                );
            }
        }
        for (key, workspace) in &self.slack.dms {
            ensure!(
                route_key(key, b"UW"),
                "slack.dms key {key:?} must be \"*\" or a Slack user ID such as U012345"
            );
            ensure!(
                self.workspaces.contains_key(workspace),
                "slack.dms.{key} names workspace {workspace:?}, which is not defined in workspaces"
            );
        }
        for (key, route) in &self.slack.channels {
            ensure!(
                route_key(key, b"CG"),
                "slack.channels key {key:?} must be \"*\" or a channel ID such as C012345"
            );
            ensure!(
                self.workspaces.contains_key(route.workspace()),
                "slack.channels.{key} names workspace {:?}, which is not defined in workspaces",
                route.workspace()
            );
        }
        if self.slack.working_reaction.is_empty() {
            bail!("slack.working_reaction must not be empty");
        }
        Ok(())
    }

    /// Resolves an explicit provider name, or `defaults.provider`.
    pub fn provider<'a>(&'a self, name: Option<&'a str>) -> Result<(&'a str, &'a Provider)> {
        let name = name.unwrap_or(&self.defaults.provider);
        let provider = self
            .providers
            .get(name)
            .with_context(|| format!("provider {name:?} is not defined in providers"))?;
        Ok((name, provider))
    }

    pub fn workspace(&self, name: &str) -> Result<&Workspace> {
        self.workspaces
            .get(name)
            .with_context(|| format!("workspace {name:?} is not defined in workspaces"))
    }
}

/// Slack credentials from the environment, never from config.json.
#[derive(Clone, Debug, Default)]
pub struct Tokens {
    pub bot: String,
    pub app: String,
    /// Optional user OAuth token for Slack's workspace search API only.
    pub user: Option<String>,
}

impl Tokens {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("SLACK_BOT_TOKEN", &self.bot),
            ("SLACK_APP_TOKEN", &self.app),
        ] {
            ensure!(!value.trim().is_empty(), "{name} is blank; set it in .env");
        }
        Ok(())
    }

    pub fn secrets(&self) -> impl Iterator<Item = &String> {
        [&self.bot, &self.app].into_iter().chain(self.user.iter())
    }
}

#[derive(Clone, Debug)]
pub struct Loaded {
    pub config: Config,
    /// Values from .env; child processes inherit the host environment first.
    pub env: BTreeMap<String, String>,
    pub tokens: Tokens,
}

impl Loaded {
    pub fn validate(&self) -> Result<()> {
        self.config.validate()?;
        self.tokens.validate()
    }
}

pub fn load(home: &Path) -> Result<Loaded> {
    load_with(home, std::env::vars().collect())
}

/// `.env` overrides the inherited environment; `ENSO_HOME` is always the home in use.
fn load_with(home: &Path, mut variables: BTreeMap<String, String>) -> Result<Loaded> {
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
    variables.extend(env.clone());
    let token = |name: &str| variables.get(name).cloned().unwrap_or_default();
    let tokens = Tokens {
        bot: token("SLACK_BOT_TOKEN"),
        app: token("SLACK_APP_TOKEN"),
        user: Some(token("SLACK_USER_TOKEN")).filter(|s| !s.trim().is_empty()),
    };
    variables.insert("ENSO_HOME".into(), home.to_string_lossy().into());
    let config = fs::read(home.join("config.json"))
        .context("cannot read Enso config.json; run enso init first")?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&config).context("invalid Enso config.json")?;
    expand_value(&mut value, &variables)?;
    // Avoid serde's invalid-value diagnostics echoing a substituted credential.
    let config: Config = serde_json::from_value(value).map_err(|_| {
        anyhow::anyhow!("invalid config.json fields or value types; see docs/configuration.md")
    })?;
    Ok(Loaded {
        config,
        env,
        tokens,
    })
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
        "workspace/.agents",
        "workspace/.claude",
        "skills",
        "skills/enso",
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
        "# Slack app credentials. Restart Enso after changes.\nSLACK_BOT_TOKEN=\nSLACK_APP_TOKEN=\n# Optional: user token with search:read for workspace-wide search\nSLACK_USER_TOKEN=\n",
    )?;
    create_file(
        &home.join("workspace/AGENTS.md"),
        include_str!("../bundled/AGENTS.md"),
    )?;
    create_file(
        &home.join("skills/enso/SKILL.md"),
        include_str!("../bundled/SKILL.md"),
    )?;
    // Claude Code and Codex share one instruction file and one skills directory.
    for (target, link) in [
        ("AGENTS.md", "workspace/CLAUDE.md"),
        ("../../skills", "workspace/.agents/skills"),
        ("../../skills", "workspace/.claude/skills"),
    ] {
        let link = home.join(link);
        if fs::symlink_metadata(&link).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            symlink(target, link)?;
        }
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
        for tool in [".agents", ".claude"] {
            let skills = temp.path().join("workspace").join(tool).join("skills");
            assert_eq!(fs::read_link(&skills).unwrap(), Path::new("../../skills"));
            assert!(skills.join("enso/SKILL.md").is_file());
        }
        assert_eq!(
            fs::metadata(temp.path().join(".env"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    const VALID: &str = r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude"}},"workspaces":{"main":{"path":"${ENSO_HOME}/workspace"}}}"#;

    fn load_config(
        config: &str,
        dotenv: &str,
        inherited: &[(&str, &str)],
    ) -> (tempfile::TempDir, Result<Loaded>) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.json"), config).unwrap();
        fs::write(temp.path().join(".env"), dotenv).unwrap();
        let inherited = inherited
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let loaded = load_with(temp.path(), inherited);
        (temp, loaded)
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
        let loaded = load_with(temp.path(), BTreeMap::new()).unwrap();
        assert_eq!(loaded.tokens.bot, "token\"with-quote");
        assert_eq!(loaded.tokens.app, "app");
        assert_eq!(loaded.env["SLACK_APP_TOKEN"], "app");
        assert!(loaded.config.slack.dms.is_empty() && loaded.config.slack.channels.is_empty());
        assert_eq!(
            loaded.config.workspaces["main"].path,
            temp.path().join("workspace")
        );
        assert!(loaded.config.workspaces["main"].provider.is_none());
        assert_eq!(loaded.config.defaults.mention, Mention::Always);
        assert!(loaded.tokens.user.is_none());
        let starter = &loaded.config.providers["main"];
        assert!(starter.model.is_none() && starter.effort.is_none());
        assert_eq!(loaded.config.defaults.timeout_seconds, 1800);
        assert!(
            loaded
                .validate()
                .unwrap_err()
                .to_string()
                .contains(r#"providers.main.cli is blank; set "claude" or "codex""#)
        );
    }

    #[test]
    fn tokens_come_from_the_environment_with_dotenv_taking_precedence() {
        let (_temp, loaded) = load_config(
            VALID,
            "SLACK_BOT_TOKEN=dotenv-bot\nSLACK_USER_TOKEN=\n",
            &[
                ("SLACK_BOT_TOKEN", "inherited-bot"),
                ("SLACK_APP_TOKEN", "inherited-app"),
                ("SLACK_USER_TOKEN", "inherited-user"),
            ],
        );
        let loaded = loaded.unwrap();
        assert_eq!(loaded.tokens.bot, "dotenv-bot");
        assert_eq!(loaded.tokens.app, "inherited-app");
        assert!(loaded.tokens.user.is_none());
        loaded.validate().unwrap();
        let (_temp, loaded) = load_config(
            VALID,
            "SLACK_BOT_TOKEN=bot\nSLACK_USER_TOKEN=fake-search-user-token\n",
            &[],
        );
        let loaded = loaded.unwrap();
        assert_eq!(
            loaded.tokens.user.as_deref(),
            Some("fake-search-user-token")
        );
        assert!(
            loaded
                .validate()
                .unwrap_err()
                .to_string()
                .contains("SLACK_APP_TOKEN is blank")
        );
        for old in [
            r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude"}},"slack":{"bot_token":"x"}}"#,
            r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude"}},"slack":{"user_token":"x"}}"#,
            r#"{"execution":{"cli":"claude"}}"#,
        ] {
            assert!(load_config(old, "", &[]).1.is_err(), "{old}");
        }
        let leftover = r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude"}},"execution":{"cli":"claude"}}"#;
        let error = format!("{:#}", load_config(leftover, "", &[]).1.unwrap_err());
        assert!(error.contains("invalid config.json fields"), "{error}");
    }

    #[test]
    fn enso_home_substitution_cannot_be_overridden() {
        let config = r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude","executable":"${ENSO_HOME}/bin/claude"}}}"#;
        let (temp, loaded) = load_config(
            config,
            "ENSO_HOME=/from/dotenv\n",
            &[("ENSO_HOME", "/from/inherited")],
        );
        assert_eq!(
            loaded.unwrap().config.providers["main"].executable,
            Some(format!("{}/bin/claude", temp.path().display()))
        );
    }

    #[test]
    fn providers_resolve_by_name_with_blank_fields_as_native_defaults() {
        let config = r#"{
            "defaults": {"provider": "main", "timeout_seconds": 60},
            "providers": {
                "main": {"cli": "claude", "model": "sonnet", "effort": "high", "args": ["--x"]},
                "codex": {"cli": "codex", "model": "", "effort": "", "executable": ""}
            },
            "workspaces": {"main": {"path": "/srv/main", "provider": ""}}
        }"#;
        let (_temp, loaded) = load_config(config, "", &[]);
        let config = loaded.unwrap().config;
        config.validate().unwrap();
        assert_eq!(config.defaults.timeout_seconds, 60);
        let (name, main) = config.provider(None).unwrap();
        assert_eq!(name, "main");
        assert_eq!(main.model.as_deref(), Some("sonnet"));
        assert_eq!(main.args, ["--x"]);
        let (name, codex) = config.provider(Some("codex")).unwrap();
        assert_eq!(name, "codex");
        assert!(codex.model.is_none() && codex.effort.is_none() && codex.executable.is_none());
        assert!(codex.args.is_empty());
        assert!(config.provider(Some("missing")).is_err());
    }

    #[test]
    fn invalid_providers_and_defaults_are_rejected() {
        for (config, message) in [
            (
                r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":""}}}"#,
                r#"providers.main.cli is blank; set "claude" or "codex""#,
            ),
            (
                r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"gemini"}}}"#,
                "providers.main.cli must be",
            ),
            (
                r#"{"defaults":{"provider":"other"},"providers":{"main":{"cli":"claude"}}}"#,
                "defaults.provider \"other\" is not defined in providers",
            ),
            (
                r#"{"defaults":{"provider":""},"providers":{"main":{"cli":"claude"}}}"#,
                "defaults.provider is blank",
            ),
            (
                r#"{"defaults":{"provider":"main"}}"#,
                "at least one provider",
            ),
            (
                r#"{"defaults":{"provider":"main","timeout_seconds":0},"providers":{"main":{"cli":"claude"}}}"#,
                "timeout_seconds must be greater than zero",
            ),
            (
                r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude","model":"--x"}}}"#,
                "providers.main.model must not start with -",
            ),
        ] {
            let (_temp, loaded) = load_config(config, "", &[]);
            let error = loaded.unwrap().config.validate().unwrap_err().to_string();
            assert!(error.contains(message), "{config}: {error}");
        }
        for config in [
            r#"{"providers":{"main":{"cli":"claude"}}}"#,
            r#"{"defaults":{"provider":"main"},"providers":{"main":{}}}"#,
            r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude","timeout_seconds":5}}}"#,
        ] {
            assert!(load_config(config, "", &[]).1.is_err(), "{config}");
        }
    }

    fn routed(workspaces: &str, slack: &str) -> Result<Config> {
        let config = format!(
            r#"{{"defaults":{{"provider":"main"}},"providers":{{"main":{{"cli":"claude"}},"codex":{{"cli":"codex"}}}},"workspaces":{workspaces},"slack":{slack}}}"#
        );
        let (_temp, loaded) = load_config(&config, "", &[]);
        let config = loaded?.config;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn workspaces_and_routes_resolve_names_paths_and_mention_modes() {
        let config = routed(
            r#"{"main":{"path":"${ENSO_HOME}/workspace"},"acme":{"path":"/srv/acme","provider":"codex"},"acme-opus":{"path":"/srv/acme"}}"#,
            r#"{"dms":{"U012345":"acme","W012345":"acme-opus","*":"main"},"channels":{"C012345":"acme","G067890":{"workspace":"acme-opus","mention":"never"},"C111":{"workspace":"main"},"*":"main"}}"#,
        )
        .unwrap();
        assert_eq!(
            config.workspace("acme").unwrap().provider.as_deref(),
            Some("codex")
        );
        assert_eq!(
            config.workspace("acme-opus").unwrap().path,
            Path::new("/srv/acme")
        );
        assert!(config.workspace("missing").is_err());
        assert_eq!(config.slack.dms["*"], "main");
        assert_eq!(config.slack.dms["W012345"], "acme-opus");
        let channels = &config.slack.channels;
        assert_eq!(channels["C012345"].workspace(), "acme");
        assert_eq!(channels["C012345"].mention(), None);
        assert_eq!(channels["G067890"].workspace(), "acme-opus");
        assert_eq!(channels["G067890"].mention(), Some(Mention::Never));
        assert_eq!(channels["C111"].mention(), None);
        assert_eq!(
            config.slack.unconfigured_message,
            "Enso isn't set up for this conversation."
        );
        let silent = routed(
            r#"{"main":{"path":"/srv/main"}}"#,
            r#"{"unconfigured_message":""}"#,
        )
        .unwrap();
        assert!(silent.slack.unconfigured_message.is_empty());
        let (_temp, loaded) = load_config(
            r#"{"defaults":{"provider":"main","mention":"first"},"providers":{"main":{"cli":"claude"}}}"#,
            "",
            &[],
        );
        assert_eq!(loaded.unwrap().config.defaults.mention, Mention::First);
    }

    #[test]
    fn invalid_workspaces_and_routes_are_rejected() {
        let main = r#"{"main":{"path":"/srv/main"}}"#;
        for (workspaces, slack, message) in [
            ("{}", "{}", "at least one workspace"),
            (
                r#"{"main":{"path":"workspace"}}"#,
                "{}",
                "workspaces.main.path must be absolute; use ${ENSO_HOME}/... for paths inside the Enso home",
            ),
            (r#"{"main":{"path":""}}"#, "{}", "must be absolute"),
            (
                r#"{"my space":{"path":"/srv"}}"#,
                "{}",
                "may contain only letters",
            ),
            (
                r#"{"main":{"path":"/srv","provider":"opus"}}"#,
                "{}",
                "workspaces.main.provider \"opus\" is not defined in providers",
            ),
            (
                main,
                r#"{"dms":{"D012345":"main"}}"#,
                "slack.dms key \"D012345\"",
            ),
            (main, r#"{"dms":{"u012345":"main"}}"#, "slack.dms key"),
            (main, r#"{"dms":{"":"main"}}"#, "slack.dms key"),
            (
                main,
                r#"{"dms":{"U1":"acme"}}"#,
                "slack.dms.U1 names workspace \"acme\"",
            ),
            (
                main,
                r#"{"channels":{"D1":"main"}}"#,
                "slack.channels key \"D1\"",
            ),
            (
                main,
                r#"{"channels":{"general":"main"}}"#,
                "slack.channels key",
            ),
            (
                main,
                r#"{"channels":{"*":{"workspace":"acme"}}}"#,
                "slack.channels.* names workspace \"acme\"",
            ),
        ] {
            let error = format!("{:#}", routed(workspaces, slack).unwrap_err());
            assert!(error.contains(message), "{workspaces} {slack}: {error}");
        }
        for slack in [
            r#"{"dm_users":["U1"]}"#,
            r#"{"mentions":{"top_level":true,"thread":true}}"#,
            r#"{"channels":{"C1":{"top_level":true,"thread":false}}}"#,
            r#"{"channels":{"C1":{"workspace":"main","mention":"sometimes"}}}"#,
            r#"{"channels":{"C1":{"workspace":"main","thread":false}}}"#,
            r#"{"dms":["U1"]}"#,
        ] {
            let error = format!("{:#}", routed(main, slack).unwrap_err());
            assert!(
                error.contains("invalid config.json fields"),
                "{slack}: {error}"
            );
        }
        let (_temp, loaded) = load_config(
            r#"{"defaults":{"provider":"main","mention":"top_level"},"providers":{"main":{"cli":"claude"}}}"#,
            "",
            &[],
        );
        assert!(loaded.is_err());
    }
}
