use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

struct Service {
    name: String,
    file: PathBuf,
    home: PathBuf,
}

fn service(home: &Path) -> Result<Service> {
    let home = fs::canonicalize(home).context("Enso home does not exist; run enso init")?;
    let account_home = PathBuf::from(std::env::var_os("HOME").context("HOME must be set")?);
    // Stable, non-cryptographic path identifier permits separate test homes without
    // needing a registry file or another dependency.
    let hash = home
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    let name = format!("enso-{hash:016x}");
    let file = if cfg!(target_os = "macos") {
        account_home
            .join("Library/LaunchAgents")
            .join(format!("dev.enso.{name}.plist"))
    } else if cfg!(target_os = "linux") {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| account_home.join(".config"))
            .join("systemd/user")
            .join(format!("{name}.service"))
    } else {
        bail!("service management supports macOS launchd and Linux systemd");
    };
    Ok(Service { name, file, home })
}

/// An upgrade must replace the executable registered with this home's service.
/// An uninitialized home or an installation without a service is also supported.
pub fn upgrade_installed(home: &Path, executable: &Path) -> Result<bool> {
    if !home.exists() {
        return Ok(false);
    }
    let service = service(home)?;
    if !service.file.exists() {
        return Ok(false);
    }
    let definition = fs::read_to_string(&service.file)?;
    let expected = if cfg!(target_os = "macos") {
        format!(
            "<key>ProgramArguments</key><array><string>{}</string>",
            xml(&executable.to_string_lossy())?
        )
    } else {
        format!(
            "ExecStart={} --home ",
            systemd_quote(&executable.to_string_lossy())?.replace('$', "$$")
        )
    };
    ensure!(
        definition.contains(&expected),
        "The service uses a different Enso executable. Run upgrade using that executable, or reinstall the service with enso service install."
    );
    Ok(true)
}

pub fn install(home: &Path) -> Result<Value> {
    let service = service(home)?;
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let mut environment = BTreeMap::new();
    for key in [
        "HOME",
        "PATH",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
    ] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_owned(), value);
        }
    }
    if environment.get("PATH").is_none_or(String::is_empty) {
        bail!("PATH must contain your authenticated agent CLI when installing");
    }
    // Check what the service will load at startup: these variables and .env only,
    // not the installing shell. Invalid jobs and missing workspaces fail only their runs.
    crate::config::load_with(&service.home, environment.clone())
        .and_then(|loaded| loaded.validate())
        .map_err(|error| {
            anyhow::anyhow!(
                "{error:#}; the installed service reads only .env, not your shell environment"
            )
        })?;
    fs::create_dir_all(service.home.join("logs"))?;
    fs::create_dir_all(service.file.parent().context("invalid service file path")?)?;
    let rendered = if cfg!(target_os = "macos") {
        render_launchd(&service, &executable, &environment)?
    } else {
        render_systemd(&service, &executable, &environment)?
    };
    fs::write(&service.file, rendered)?;
    fs::set_permissions(&service.file, fs::Permissions::from_mode(0o600))?;
    if cfg!(target_os = "linux") {
        manager("systemctl", &["--user", "daemon-reload"])?;
        manager(
            "systemctl",
            &["--user", "enable", &format!("{}.service", service.name)],
        )?;
    }
    Ok(
        json!({"installed": true, "service": service.name, "file": service.file,
        "message": "Installed for login startup; run enso service start to start now."}),
    )
}

pub fn action(home: &Path, action: &str) -> Result<Value> {
    let service = service(home)?;
    if !matches!(
        action,
        "start" | "stop" | "restart" | "uninstall" | "status"
    ) {
        bail!("unknown service action {action}");
    }
    if cfg!(target_os = "macos") {
        launchd_action(&service, action)
    } else {
        systemd_action(&service, action)
    }
}

fn launchd_action(service: &Service, action: &str) -> Result<Value> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let target = format!("{domain}/dev.enso.{}", service.name);
    let output = Command::new("launchctl")
        .args(["print", &target])
        .output()
        .context("launchctl is unavailable")?;
    let loaded = output.status.success();
    if action == "status" {
        let running = loaded && String::from_utf8_lossy(&output.stdout).contains("state = running");
        return Ok(
            json!({"service":service.name,"installed":service.file.exists(),"running":running,"loaded":loaded,"manager":"launchd"}),
        );
    }
    if matches!(action, "stop" | "restart" | "uninstall") && loaded {
        manager("launchctl", &["bootout", &target])?;
    }
    if action == "uninstall" {
        if service.file.exists() {
            fs::remove_file(&service.file)?;
        }
    } else if matches!(action, "start" | "restart") {
        if !service.file.exists() {
            bail!("service is not installed; run enso service install");
        }
        if !loaded || action == "restart" {
            let file = service.file.to_str().context("service path is not UTF-8")?;
            // bootout returns before launchd finishes removing the job, so an
            // immediate bootstrap can fail with an I/O error until it has.
            let deadline = Instant::now() + Duration::from_secs(10);
            while let Err(error) = manager("launchctl", &["bootstrap", &domain, file]) {
                if Instant::now() >= deadline {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        manager("launchctl", &["kickstart", &target])?;
    }
    Ok(json!({"service":service.name,"action":action,"ok":true}))
}

fn systemd_action(service: &Service, action: &str) -> Result<Value> {
    let unit = format!("{}.service", service.name);
    if action == "status" {
        let output = Command::new("systemctl")
            .args(["--user", "is-active", &unit])
            .output()
            .context("systemctl is unavailable")?;
        return Ok(
            json!({"service":service.name,"installed":service.file.exists(),"running":output.status.success(),"manager":"systemd"}),
        );
    }
    if action == "uninstall" {
        if service.file.exists() {
            manager("systemctl", &["--user", "disable", "--now", &unit])?;
            fs::remove_file(&service.file)?;
            manager("systemctl", &["--user", "daemon-reload"])?;
        }
    } else {
        if !service.file.exists() {
            bail!("service is not installed; run enso service install");
        }
        manager("systemctl", &["--user", action, &unit])?;
    }
    Ok(json!({"service":service.name,"action":action,"ok":true}))
}

pub fn logs(home: &Path, follow: bool) -> Result<()> {
    let service = service(home)?;
    let status = if cfg!(target_os = "macos") {
        let log = service.home.join("logs/enso.log");
        if !log.exists() {
            bail!("no service log yet; start the service first");
        }
        let mut command = Command::new("tail");
        command.args(["-n", "100"]);
        if follow {
            command.arg("-F");
        }
        command.arg(log).status()?
    } else {
        let mut command = Command::new("journalctl");
        command.args([
            "--user",
            "--unit",
            &format!("{}.service", service.name),
            "--no-pager",
            "-n",
            "100",
        ]);
        if follow {
            command.arg("--follow");
        }
        command.status()?
    };
    if !status.success() {
        bail!("log reader exited with {status}");
    }
    Ok(())
}

fn manager(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("run {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn xml(value: &str) -> Result<String> {
    if value.chars().any(|c| c.is_control()) {
        bail!("service values cannot contain control characters");
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn render_launchd(
    service: &Service,
    executable: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    let env = environment
        .iter()
        .map(|(key, value)| {
            Ok(format!(
                "<key>{}</key><string>{}</string>",
                xml(key)?,
                xml(value)?
            ))
        })
        .collect::<Result<Vec<_>>>()?
        .join("\n");
    let home = xml(&service.home.to_string_lossy())?;
    let executable = xml(&executable.to_string_lossy())?;
    let log = xml(&service.home.join("logs/enso.log").to_string_lossy())?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>dev.enso.{name}</string>
<key>ProgramArguments</key><array><string>{executable}</string><string>--home</string><string>{home}</string><string>run</string></array>
<key>WorkingDirectory</key><string>{home}</string>
<key>EnvironmentVariables</key><dict>{env}</dict>
<key>MaterializeDatalessFiles</key><true/>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>5</integer>
<key>ExitTimeOut</key><integer>30</integer>
<key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#,
        name = service.name
    ))
}

fn systemd_quote(value: &str) -> Result<String> {
    if value.chars().any(|c| c.is_control()) {
        bail!("service values cannot contain control characters");
    }
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    ))
}

/// Path settings such as `WorkingDirectory=` take the value literally: systemd
/// does not remove quotes there, and it trims surrounding whitespace.
fn systemd_path(value: &str) -> Result<String> {
    if value.chars().any(|c| c.is_control()) || value.trim() != value {
        bail!("service paths cannot contain control characters or surrounding whitespace");
    }
    Ok(value.replace('%', "%%"))
}

fn render_systemd(
    service: &Service,
    executable: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    let env = environment
        .iter()
        .map(|(key, value)| {
            Ok(format!(
                "Environment={}\n",
                systemd_quote(&format!("{key}={value}"))?
            ))
        })
        .collect::<Result<Vec<_>>>()?
        .join("");
    // systemd expands $variables in ExecStart even inside quoted arguments.
    let executable = systemd_quote(&executable.to_string_lossy())?.replace('$', "$$");
    let home_arg = systemd_quote(&service.home.to_string_lossy())?.replace('$', "$$");
    let home = systemd_path(&service.home.to_string_lossy())?;
    Ok(format!(
        "[Unit]\nDescription=Enso Slack assistant\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={executable} --home {home_arg} run\nWorkingDirectory={home}\n{env}Restart=on-failure\nRestartSec=5\nTimeoutStopSec=30\nKillMode=mixed\nUMask=0077\n\n[Install]\nWantedBy=default.target\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_rendering_escapes_paths_without_shell_evaluation() {
        let service = Service {
            name: "enso-test".into(),
            file: PathBuf::from("unused"),
            home: PathBuf::from("/tmp/a & b/$home%"),
        };
        let env = BTreeMap::from([("PATH".into(), "/bin:/path with space".into())]);
        let launchd = render_launchd(&service, Path::new("/tmp/bin&enso"), &env).unwrap();
        assert!(launchd.contains("/tmp/bin&amp;enso"));
        assert!(launchd.contains("/tmp/a &amp; b/$home%"));
        let systemd = render_systemd(&service, Path::new("/tmp/bin$enso"), &env).unwrap();
        assert!(
            systemd.contains("ExecStart=\"/tmp/bin$$enso\" --home \"/tmp/a & b/$$home%%\" run")
        );
        assert!(systemd.contains("\nWorkingDirectory=/tmp/a & b/$home%%\n"));
        assert!(systemd.contains("Environment=\"PATH=/bin:/path with space\""));
        assert!(systemd_path("/tmp/trailing ").is_err());
        assert!(xml("bad\nvalue").is_err());
    }
}
