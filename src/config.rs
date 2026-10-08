use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
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
    fn problems(&self, name: &str, errors: &mut Vec<String>) {
        if self.cli.trim().is_empty() {
            errors.push(blank_cli(name));
        } else if !matches!(self.cli.as_str(), "claude" | "codex") {
            errors.push(format!(
                "providers.{name}.cli must be \"claude\" or \"codex\""
            ));
        }
        for (field, value) in [("model", &self.model), ("effort", &self.effort)] {
            if value.as_ref().is_some_and(|s| s.starts_with('-')) {
                errors.push(format!("providers.{name}.{field} must not start with -"));
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub provider: String,
    #[serde(default)]
    pub mention: Mention,
    #[serde(default = "timeout_seconds")]
    pub timeout_seconds: u64,
}

/// When channel messages must @mention the bot; DMs never need mentions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mention {
    /// At top level and in threads.
    #[default]
    Always,
    /// At top level; replies in a thread the bot joined need none.
    First,
    /// Never at top level; thread replies need one until the bot joins.
    Never,
    /// An unrecognized mode, kept so `config check` reports it with the
    /// other problems; validation rejects it before any routing.
    Invalid,
}

impl<'de> Deserialize<'de> for Mention {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "always" => Self::Always,
            "first" => Self::First,
            "never" => Self::Never,
            _ => Self::Invalid,
        })
    }
}

/// A named working directory, optionally with its own provider.
#[derive(Clone, Debug, Deserialize)]
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
        ensure!(
            slack_id(&self.channel, b"CDG"),
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
#[derive(Clone, Debug)]
pub enum ChannelRoute {
    Workspace(String),
    Settings(ChannelSettings),
}

/// Unlike an untagged derive, keeps the settings' field errors (such as an
/// old `top_level` key) instead of a generic "did not match any variant".
impl<'de> Deserialize<'de> for ChannelRoute {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Route;
        impl<'de> serde::de::Visitor<'de> for Route {
            type Value = ChannelRoute;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a workspace name or {\"workspace\": NAME, \"mention\": MODE}")
            }
            fn visit_str<E: serde::de::Error>(self, name: &str) -> Result<ChannelRoute, E> {
                Ok(ChannelRoute::Workspace(name.into()))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<ChannelRoute, A::Error> {
                ChannelSettings::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(ChannelRoute::Settings)
            }
        }
        deserializer.deserialize_any(Route)
    }
}

#[derive(Clone, Debug, Deserialize)]
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

#[derive(Clone, Debug, Deserialize)]
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

/// One of `prefixes` followed by uppercase letters or digits.
fn slack_id(id: &str, prefixes: &[u8]) -> bool {
    id.len() > 1
        && prefixes.contains(&id.as_bytes()[0])
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// `*` or a Slack ID with one of `prefixes`.
fn route_key(key: &str, prefixes: &[u8]) -> bool {
    key == "*" || slack_id(key, prefixes)
}

#[derive(Clone, Debug, Deserialize)]
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

/// Fails with every problem, joined, when there is any.
fn fail(problems: Vec<String>) -> Result<()> {
    ensure!(problems.is_empty(), "{}", problems.join("; "));
    Ok(())
}

impl Config {
    #[cfg(test)]
    pub fn validate(&self) -> Result<()> {
        fail(self.problems())
    }

    /// Every problem in the configuration itself, without inspecting the filesystem.
    fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.providers.is_empty() {
            errors.push("providers must define at least one provider".into());
        }
        for (name, provider) in &self.providers {
            provider.problems(name, &mut errors);
        }
        if self.defaults.provider.trim().is_empty() {
            errors.push("defaults.provider is blank; name one of providers".into());
        } else if !self.providers.contains_key(&self.defaults.provider) {
            errors.push("defaults.provider is not defined in providers".into());
        }
        const MENTION: &str = "must be \"always\", \"first\", or \"never\"";
        if self.defaults.mention == Mention::Invalid {
            errors.push(format!("defaults.mention {MENTION}"));
        }
        if self.defaults.timeout_seconds == 0 {
            errors.push("defaults.timeout_seconds must be greater than zero".into());
        }
        if self.workspaces.is_empty() {
            errors.push("workspaces must define at least one workspace".into());
        }
        for (name, workspace) in &self.workspaces {
            if !workspace.path.is_absolute() {
                errors.push(format!(
                    "workspaces.{name}.path must be absolute; use ${{ENSO_HOME}}/... for paths inside the Enso home"
                ));
            }
            if let Some(provider) = &workspace.provider
                && !self.providers.contains_key(provider)
            {
                errors.push(format!(
                    "workspaces.{name}.provider is not defined in providers"
                ));
            }
        }
        for (key, workspace) in &self.slack.dms {
            if !route_key(key, b"UW") {
                errors.push(format!(
                    "slack.dms key {key:?} must be \"*\" or a Slack user ID such as U012345"
                ));
            }
            if !self.workspaces.contains_key(workspace) {
                errors.push(format!(
                    "slack.dms.{key} names a workspace that is not defined in workspaces"
                ));
            }
        }
        for (key, route) in &self.slack.channels {
            if !route_key(key, b"CG") {
                errors.push(format!(
                    "slack.channels key {key:?} must be \"*\" or a channel ID such as C012345"
                ));
            }
            if !self.workspaces.contains_key(route.workspace()) {
                errors.push(format!(
                    "slack.channels.{key} names a workspace that is not defined in workspaces"
                ));
            }
            if route.mention() == Some(Mention::Invalid) {
                errors.push(format!("slack.channels.{key}.mention {MENTION}"));
            }
        }
        if self.slack.working_reaction.is_empty() {
            errors.push("slack.working_reaction must not be empty".into());
        }
        errors
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
    fn problems(&self) -> Vec<String> {
        [
            ("SLACK_BOT_TOKEN", &self.bot),
            ("SLACK_APP_TOKEN", &self.app),
        ]
        .into_iter()
        .filter(|(_, value)| value.trim().is_empty())
        .map(|(name, _)| blank_token(name))
        .collect()
    }

    pub fn secrets(&self) -> impl Iterator<Item = &String> {
        [&self.bot, &self.app].into_iter().chain(self.user.iter())
    }
}

fn blank_cli(provider: &str) -> String {
    format!("providers.{provider}.cli is blank; set \"claude\" or \"codex\"")
}

fn blank_token(name: &str) -> String {
    format!("{name} is blank; set it in .env")
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
        fail(self.problems())
    }

    fn problems(&self) -> Vec<String> {
        let mut errors = self.config.problems();
        errors.extend(self.tokens.problems());
        errors
    }
}

/// The result of `enso config check`.
#[derive(Debug, Default, Serialize)]
pub struct Check {
    pub valid: bool,
    pub errors: Vec<String>,
    pub notes: Vec<String>,
    pub providers: usize,
    pub workspaces: usize,
    pub jobs: usize,
}

/// Collects every configuration and job problem, never echoing substituted values.
pub fn check(home: &Path) -> Check {
    let mut check = Check::default();
    match load(home) {
        Err(error) => {
            check.errors.push(format!("{error:#}"));
            check.jobs = crate::jobs::count(home);
        }
        Ok(loaded) => {
            let config = &loaded.config;
            check.errors = loaded.problems();
            check.providers = config.providers.len();
            check.workspaces = config.workspaces.len();
            if config.slack.dms.is_empty() && config.slack.channels.is_empty() {
                check.notes.push(if config.slack.unconfigured_message.is_empty() {
                    "no dms or channels are configured; Enso will ignore every message".into()
                } else {
                    "no dms or channels are configured; Enso will reply \"not configured\" to every message".into()
                });
            }
            // A relative path is already an error.
            for (name, workspace) in &config.workspaces {
                if workspace.path.is_absolute() && !workspace.path.is_dir() {
                    check.errors.push(format!(
                        "workspaces.{name}.path is not a directory; create it or fix the path"
                    ));
                }
            }
            if config.providers.values().any(|p| p.cli == "codex")
                && fs::symlink_metadata(home.join(".git")).is_err()
            {
                check.notes.push("the Enso home is not a git repository, so Codex will not run in its workspaces unless they are trusted in your Codex configuration, and will not load the shared AGENTS.md and .agents/skills; run enso init or git init in the home".into());
            }
            match crate::jobs::list(home, config) {
                Ok(jobs) => {
                    check.jobs = jobs.len();
                    check.errors.extend(
                        jobs.into_iter()
                            .filter_map(|(_, job)| job.err().map(|e| format!("{e:#}"))),
                    );
                }
                Err(error) => check.errors.push(format!("{error:#}")),
            }
        }
    }
    if let Err(error) = crate::db::Db::check(home) {
        check.errors.push(format!("{error:#}"));
    }
    check.valid = check.errors.is_empty();
    check
}

/// Serde's message only when it names fields and cannot echo a value.
pub fn safe_detail(error: &serde_json::Error) -> Option<String> {
    let text = error.to_string();
    (text.starts_with("unknown field `") || text.starts_with("missing field `")).then_some(text)
}

pub fn load(home: &Path) -> Result<Loaded> {
    load_with(home, std::env::vars().collect())
}

/// `.env` overrides the inherited environment; `ENSO_HOME` is always the home in use.
pub fn load_with(home: &Path, mut variables: BTreeMap<String, String>) -> Result<Loaded> {
    let env = read_dotenv(home)?;
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
    let mut config: Config =
        serde_json::from_value(value).map_err(|error| match safe_detail(&error) {
            Some(detail) => {
                anyhow::anyhow!("invalid config.json: {detail}; see docs/configuration.md")
            }
            None => {
                anyhow::anyhow!(
                    "invalid config.json fields or value types; see docs/configuration.md"
                )
            }
        })?;
    // One spelling per directory, so equal paths compare equal.
    for workspace in config.workspaces.values_mut() {
        workspace.path = workspace.path.components().collect();
    }
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

/// Reads the home's `.env`, if any, without echoing a malformed line.
pub fn read_dotenv(home: &Path) -> Result<BTreeMap<String, String>> {
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
    Ok(env)
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

/// Creates the Enso home's missing files and directories; never overwrites.
/// The starter `workspaces/main` comes only with the starter config that names it.
pub fn init(home: &Path) -> Result<()> {
    let starter = !home.join("config.json").exists();
    for dir in [
        "",
        ".agents",
        ".agents/skills",
        ".agents/skills/enso",
        ".claude",
        "jobs",
        "logs",
    ] {
        create_dir(&home.join(dir))?;
    }
    for (path, content) in [
        (".gitignore", include_str!("../bundled/gitignore")),
        ("config.json", include_str!("../bundled/config.json")),
        (
            ".env",
            "# Slack app credentials. Restart Enso after changes.\nSLACK_BOT_TOKEN=\nSLACK_APP_TOKEN=\n# Optional: user token with search:read for workspace-wide search\nSLACK_USER_TOKEN=\n",
        ),
        ("AGENTS.md", include_str!("../bundled/AGENTS.md")),
        (
            ".agents/skills/enso/SKILL.md",
            include_str!("../bundled/SKILL.md"),
        ),
    ] {
        create_file(&home.join(path), content)?;
    }
    link_guidance(home)?;
    if starter {
        let main = home.join("workspaces/main");
        for dir in ["", ".agents", ".agents/skills", ".claude"] {
            create_dir(&main.join(dir))?;
        }
        create_file(
            &main.join("AGENTS.md"),
            include_str!("../bundled/WORKSPACE.md"),
        )?;
        link_guidance(&main)?;
    }
    Ok(())
}

/// Claude Code reads the same instructions and skills as Codex through links.
fn link_guidance(root: &Path) -> Result<()> {
    for (target, link) in [
        ("AGENTS.md", "CLAUDE.md"),
        ("../.agents/skills", ".claude/skills"),
    ] {
        let link = root.join(link);
        if fs::symlink_metadata(&link).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            symlink(target, &link).with_context(|| format!("link {}", link.display()))?;
        }
    }
    Ok(())
}

fn create_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("create {}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Codex finds shared instructions and skills by walking up to a git root.
/// Returns a warning instead of failing when the home cannot become a repository.
pub fn git_init(home: &Path) -> Option<String> {
    if fs::symlink_metadata(home.join(".git")).is_ok() {
        return None;
    }
    let mut git = std::process::Command::new("git");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        git.env_remove(name);
    }
    let problem = match git
        .args(["init", "-q"])
        .current_dir(home)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() && home.join(".git").exists() => return None,
        Ok(output) if output.status.success() => {
            "git init did not create .git in the home".to_string()
        }
        Ok(output) => format!(
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => format!("cannot run git: {error}"),
    };
    Some(format!(
        "{problem}. Codex needs the Enso home to be a git repository to run in its workspaces without trusting each one and to load its shared AGENTS.md and .agents/skills; install git and run git init in {}.",
        home.display()
    ))
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

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn assert_guidance_links(root: &Path) {
        assert_eq!(
            fs::read_link(root.join("CLAUDE.md")).unwrap(),
            Path::new("AGENTS.md")
        );
        assert_eq!(
            fs::read_link(root.join(".claude/skills")).unwrap(),
            Path::new("../.agents/skills")
        );
        assert!(
            fs::symlink_metadata(root.join(".agents/skills"))
                .unwrap()
                .is_dir()
        );
        assert!(root.join(".claude/skills").is_dir());
    }

    #[test]
    fn init_builds_the_home_layout_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        init(&home).unwrap();
        for dir in [
            "",
            ".agents",
            ".agents/skills",
            ".agents/skills/enso",
            ".claude",
            "jobs",
            "logs",
        ] {
            assert_eq!(mode(&home.join(dir)), 0o700, "{dir}");
        }
        for file in [
            ".gitignore",
            "config.json",
            ".env",
            "AGENTS.md",
            ".agents/skills/enso/SKILL.md",
        ] {
            assert_eq!(mode(&home.join(file)), 0o600, "{file}");
        }
        let main = home.join("workspaces/main");
        for dir in [
            "workspaces",
            "workspaces/main",
            "workspaces/main/.agents/skills",
        ] {
            assert_eq!(mode(&home.join(dir)), 0o700, "{dir}");
        }
        assert_eq!(mode(&main.join("AGENTS.md")), 0o600);
        assert_eq!(
            fs::read_to_string(main.join("AGENTS.md")).unwrap(),
            include_str!("../bundled/WORKSPACE.md")
        );
        for root in [&home, &main] {
            assert_guidance_links(root);
        }
        assert!(home.join(".claude/skills/enso/SKILL.md").is_file());
        assert_eq!(
            fs::read_to_string(home.join(".gitignore")).unwrap(),
            "# Enso secrets and runtime state\n.env\nenso.db\nenso.db-*\ndaemon.lock\nlogs/\nworkspaces/*/uploads/\n"
        );
        assert_eq!(
            fs::read_to_string(home.join("AGENTS.md")).unwrap(),
            include_str!("../bundled/AGENTS.md")
        );
        for old in ["skills", "workspace", "workspaces/main/uploads"] {
            assert!(!home.join(old).exists(), "{old}");
        }
        for file in [
            "config.json",
            ".gitignore",
            "AGENTS.md",
            ".agents/skills/enso/SKILL.md",
        ] {
            fs::write(home.join(file), "custom").unwrap();
        }
        init(&home).unwrap();
        for file in [
            "config.json",
            ".gitignore",
            "AGENTS.md",
            ".agents/skills/enso/SKILL.md",
        ] {
            assert_eq!(
                fs::read_to_string(home.join(file)).unwrap(),
                "custom",
                "{file}"
            );
        }
        // The existing config may not name `main`, so init never recreates it.
        fs::remove_dir_all(home.join("workspaces")).unwrap();
        init(&home).unwrap();
        assert!(!home.join("workspaces").exists());
    }

    #[test]
    fn workspace_paths_are_normalized_once_at_load() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join("config.json"),
            r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude"}},"workspaces":{"a":{"path":"/x//acme/"},"b":{"path":"/x/./acme"},"main":{"path":"${ENSO_HOME}/workspaces/main"}}}"#,
        )
        .unwrap();
        let home_slash = PathBuf::from(format!("{}/", home.display()));
        let config = load_with(&home_slash, BTreeMap::new()).unwrap().config;
        for name in ["a", "b"] {
            assert_eq!(
                config.workspaces[name].path.to_string_lossy(),
                "/x/acme",
                "{name}"
            );
        }
        assert_eq!(
            config.workspaces["main"].path.to_string_lossy(),
            home.join("workspaces/main").to_string_lossy()
        );
    }

    const VALID: &str = r#"{"defaults":{"provider":"main"},"providers":{"main":{"cli":"claude"}},"workspaces":{"main":{"path":"${ENSO_HOME}/workspaces/main"}}}"#;

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
            temp.path().join("workspaces/main")
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
        assert!(
            error.starts_with("invalid config.json: unknown field `execution`, expected one of"),
            "{error}"
        );
        let missing = format!(
            "{:#}",
            load_config(r#"{"providers":{}}"#, "", &[]).1.unwrap_err()
        );
        assert!(missing.contains("missing field `defaults`"), "{missing}");
        let typed = r#"{"defaults":{"provider":"main","timeout_seconds":"${SLACK_BOT_TOKEN}"},"providers":{"main":{"cli":"claude"}}}"#;
        let error = format!(
            "{:#}",
            load_config(typed, "SLACK_BOT_TOKEN=fake-secret-bot-token\n", &[])
                .1
                .unwrap_err()
        );
        assert_eq!(
            error,
            "invalid config.json fields or value types; see docs/configuration.md"
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let config = r#"{
            "defaults": {"provider": "", "timeout_seconds": 0},
            "providers": {"main": {"cli": ""}, "other": {"cli": "gemini", "model": "-x"}},
            "workspaces": {"bad name": {"path": "relative", "provider": "missing"}},
            "slack": {"dms": {"D1": "nowhere"}, "channels": {"general": {"workspace": "nowhere"}}}
        }"#;
        let (_temp, loaded) = load_config(config, "", &[]);
        let loaded = loaded.unwrap();
        let problems = loaded.problems();
        for message in [
            "providers.main.cli is blank",
            "providers.other.cli must be",
            "providers.other.model must not start with -",
            "defaults.provider is blank",
            "defaults.timeout_seconds must be greater than zero",
            "workspaces.bad name.path must be absolute",
            "workspaces.bad name.provider is not defined",
            "slack.dms key \"D1\"",
            "slack.dms.D1 names a workspace that is not defined",
            "slack.channels key \"general\"",
            "slack.channels.general names a workspace that is not defined",
            "SLACK_BOT_TOKEN is blank",
            "SLACK_APP_TOKEN is blank",
        ] {
            assert_eq!(
                problems.iter().filter(|p| p.contains(message)).count(),
                1,
                "{message}: {problems:#?}"
            );
        }
        assert_eq!(problems.len(), 13, "{problems:#?}");
        let error = loaded.validate().unwrap_err().to_string();
        assert!(
            error.starts_with("providers.main.cli is blank")
                && error.contains("; SLACK_APP_TOKEN is blank"),
            "{error}"
        );
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
                "defaults.provider is not defined in providers",
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
            r#"{"main":{"path":"${ENSO_HOME}/workspaces/main"},"acme":{"path":"/srv/acme","provider":"codex"},"acme-opus":{"path":"/srv/acme"}}"#,
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
                r#"{"main":{"path":"/srv","provider":"opus"}}"#,
                "{}",
                "workspaces.main.provider is not defined in providers",
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
                "slack.dms.U1 names a workspace that is not defined",
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
                "slack.channels.* names a workspace that is not defined",
            ),
        ] {
            let error = format!("{:#}", routed(workspaces, slack).unwrap_err());
            assert!(error.contains(message), "{workspaces} {slack}: {error}");
        }
        for (slack, message) in [
            (r#"{"dm_users":["U1"]}"#, "unknown field `dm_users`"),
            (
                r#"{"mentions":{"top_level":true,"thread":true}}"#,
                "unknown field `mentions`",
            ),
            (
                r#"{"channels":{"C1":{"top_level":true,"thread":false}}}"#,
                "invalid config.json: unknown field `thread`, expected `workspace` or `mention`",
            ),
            (
                r#"{"channels":{"C1":{"workspace":"main","mention":1}}}"#,
                "invalid config.json fields",
            ),
            (
                r#"{"channels":{"C1":{"workspace":"main","thread":false}}}"#,
                "unknown field `thread`",
            ),
            (r#"{"dms":["U1"]}"#, "invalid config.json fields"),
        ] {
            let error = format!("{:#}", routed(main, slack).unwrap_err());
            assert!(error.contains(message), "{slack}: {error}");
        }
        // An unknown mode is a validation problem that never repeats the value.
        let error = format!(
            "{:#}",
            routed(
                main,
                r#"{"channels":{"C1":{"workspace":"main","mention":"sometimes"}}}"#
            )
            .unwrap_err()
        );
        assert_eq!(
            error,
            r#"slack.channels.C1.mention must be "always", "first", or "never""#
        );
        let (_temp, loaded) = load_config(
            r#"{"defaults":{"provider":"main","mention":"top_level"},"providers":{"main":{"cli":"claude"}}}"#,
            "",
            &[],
        );
        let error = loaded.unwrap().config.validate().unwrap_err().to_string();
        assert!(
            error.contains(r#"defaults.mention must be "always", "first", or "never""#)
                && !error.contains("top_level"),
            "{error}"
        );
    }
}
