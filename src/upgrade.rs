//! Public GitHub releases are the single source for binary upgrades.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use self_update::{VersionStatus, backends::github};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use crate::{app, db::Db, service};

const API: &str = "https://api.github.com";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        _ => anyhow::bail!("Binary upgrades support macOS and Linux on ARM64 and x86-64"),
    }
}

fn install(api: &str, current: &str, executable: &Path) -> Result<VersionStatus> {
    let target = target()?;
    let archive = format!("enso-{target}.tar.gz");
    let mut builder = github::Update::configure();
    builder
        .repo_owner("geekforbrains")
        .repo_name("enso-light")
        .api_base_url(api)
        .current_version(current)
        .target(target)
        .bin_name("enso")
        .bin_install_path(executable)
        .bin_path_in_archive("enso-{{ target }}/enso")
        .asset_matcher(move |assets| assets.iter().find(|asset| asset.name() == archive).cloned())
        // Requiring this asset also fails closed if the GitHub digest is absent.
        .checksum_from_asset("sha256.sum")
        .check_install_path_writable(true)
        .timeout(Duration::from_secs(120))
        .no_confirm(true)
        .show_output(false)
        .show_download_progress(false);
    // The unpinned library updater lists all releases. Explicitly use GitHub's
    // latest endpoint so prereleases and releases not marked latest are excluded.
    let releases = builder.build()?.get_latest_release()?;
    let release = releases
        .latest()
        .context("GitHub has no published Enso release")?;
    let version = release.version().to_owned();
    if !self_update::version::bump_is_greater(current, &version)? {
        return Ok(VersionStatus::UpToDate(current.to_owned()));
    }
    builder
        .release_tag(format!("v{version}"))
        .verify_binary(move |path| {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
            let output = Command::new(path).arg("--version").output()?;
            if !output.status.success()
                || String::from_utf8_lossy(&output.stdout).trim() != format!("enso {version}")
            {
                return Err(self_update::Error::verification_rejected(
                    "The downloaded Enso executable failed its version check",
                ));
            }
            Ok(())
        });
    Ok(builder.build()?.update()?)
}

pub fn run(home: &Path) -> Result<Value> {
    ensure!(
        std::env::var_os("ENSO_RUN_ID").is_none_or(|id| id.is_empty()),
        "Run enso upgrade from a terminal outside Enso; restarting stops Enso's agent and hook processes"
    );
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let parent = executable
        .parent()
        .context("Executable has no parent directory")?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(parent.join(".enso-upgrade.lock"))
        .context(
            "Cannot write the Enso install directory; install Enso in a user-writable directory",
        )?;
    lock.try_lock_exclusive()
        .context("Another Enso upgrade is in progress")?;
    let installed = service::upgrade_installed(home, &executable)?;
    ensure!(
        installed || !app::is_running(home),
        "Stop the foreground enso run before upgrading, then start it again"
    );
    let status = install(API, VERSION, &executable).context("Enso upgrade failed")?;
    let updated = status.is_updated();
    if updated && installed {
        restart(home).with_context(|| {
            format!(
                "Enso {} was installed, but service restart could not be verified. Inspect enso service status and enso service logs",
                status.version()
            )
        })?;
    }
    Ok(json!({
        "updated": updated,
        "version": status.version(),
        "executable": executable,
        "service_restarted": updated && installed,
    }))
}

fn restart(home: &Path) -> Result<()> {
    let db = Db::open(home)?;
    let old_pid = db.health()?["pid"].as_i64();
    service::action(home, "restart")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let health = db.health()?;
        if health["pid"]
            .as_i64()
            .is_some_and(|pid| Some(pid) != old_pid)
            && health["slack"] == "connected"
            && app::is_running(home)
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Service did not connect to Slack within 30 seconds"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread::{self, JoinHandle},
    };

    struct ReleaseServer {
        address: String,
        stopped: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<String>>>,
        worker: Option<JoinHandle<()>>,
    }

    impl ReleaseServer {
        fn new(checksum: bool, corrupt: bool, binary_version: &str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let name = format!("enso-{}.tar.gz", target().unwrap());
            let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
            let script = format!("#!/bin/sh\nprintf 'enso {binary_version}\\n'\n");
            let mut header = tar::Header::new_gnu();
            header.set_size(script.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(
                    &mut header,
                    format!("enso-{}/enso", target().unwrap()),
                    script.as_bytes(),
                )
                .unwrap();
            let archive = archive.into_inner().unwrap().finish().unwrap();
            let digest = if corrupt {
                "00".repeat(32)
            } else {
                Sha256::digest(&archive)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            };
            let sums = format!("{digest}  {name}\n").into_bytes();
            let mut assets = vec![
                json!({"name":format!("{name}.sha256"),"url":format!("{address}/wrong")}),
                json!({"name":name,"url":format!("{address}/archive")}),
            ];
            if checksum {
                assets.push(json!({"name":"sha256.sum","url":format!("{address}/sums")}));
            }
            let release = serde_json::to_vec(&json!({
                "tag_name":"v0.2.0", "created_at":"2026-10-06T00:00:00Z",
                "assets":assets,
            }))
            .unwrap();
            let stopped = Arc::new(AtomicBool::new(false));
            let stop = stopped.clone();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            let worker = thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let (mut socket, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(error) => panic!("{error}"),
                    };
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut request = Vec::new();
                    let mut byte = [0];
                    while !request.ends_with(b"\r\n\r\n") {
                        if socket.read(&mut byte).unwrap_or(0) == 0 {
                            break;
                        }
                        request.push(byte[0]);
                        assert!(request.len() < 16_384);
                    }
                    let request = String::from_utf8(request).unwrap();
                    let path = request.split_whitespace().nth(1).unwrap().to_owned();
                    seen.lock().unwrap().push(path.clone());
                    let body = match path.as_str() {
                        "/repos/geekforbrains/enso-light/releases/latest"
                        | "/repos/geekforbrains/enso-light/releases/tags/v0.2.0" => &release,
                        "/sums" => &sums,
                        "/archive" => &archive,
                        _ => panic!("Unexpected update request: {path}"),
                    };
                    write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    socket.write_all(body).unwrap();
                }
            });
            Self {
                address,
                stopped,
                requests,
                worker: Some(worker),
            }
        }
    }

    impl Drop for ReleaseServer {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    #[test]
    fn verified_release_replaces_only_the_binary_and_can_run() {
        let server = ReleaseServer::new(true, false, "0.2.0");
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("enso");
        fs::write(&binary, "old binary").unwrap();
        fs::write(temp.path().join("enso.db"), "keep state").unwrap();
        let status = install(&server.address, "0.1.0", &binary).unwrap();
        assert!(status.is_updated());
        assert_eq!(status.version(), "0.2.0");
        let output = Command::new(binary).arg("--version").output().unwrap();
        assert_eq!(output.stdout, b"enso 0.2.0\n");
        assert_eq!(
            fs::read_to_string(temp.path().join("enso.db")).unwrap(),
            "keep state"
        );
    }

    #[test]
    fn self_replacement_works_for_a_running_executable() {
        if let Ok(api) = std::env::var("ENSO_UPGRADE_TEST_API") {
            let status = install(&api, "0.1.0", &std::env::current_exe().unwrap()).unwrap();
            assert!(status.is_updated());
            return;
        }
        let server = ReleaseServer::new(true, false, "0.2.0");
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("enso");
        fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
        let result = Command::new(&binary)
            .args([
                "--exact",
                "upgrade::tests::self_replacement_works_for_a_running_executable",
            ])
            .env("ENSO_UPGRADE_TEST_API", &server.address)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        assert_eq!(
            Command::new(binary)
                .arg("--version")
                .output()
                .unwrap()
                .stdout,
            b"enso 0.2.0\n"
        );
    }

    #[test]
    fn bad_checksum_missing_checksum_or_wrong_binary_preserves_installation() {
        for (checksum, corrupt, version) in [
            (true, true, "0.2.0"),
            (false, false, "0.2.0"),
            (true, false, "0.1.0"),
        ] {
            let server = ReleaseServer::new(checksum, corrupt, version);
            let temp = tempfile::tempdir().unwrap();
            let binary = temp.path().join("enso");
            fs::write(&binary, "old binary").unwrap();
            assert!(install(&server.address, "0.1.0", &binary).is_err());
            assert_eq!(fs::read_to_string(binary).unwrap(), "old binary");
        }
    }

    #[test]
    fn current_or_newer_installation_never_downloads_or_downgrades() {
        for current in ["0.2.0", "0.3.0"] {
            let server = ReleaseServer::new(true, false, "0.2.0");
            let temp = tempfile::tempdir().unwrap();
            let binary = temp.path().join("enso");
            fs::write(&binary, "keep binary").unwrap();
            assert!(
                install(&server.address, current, &binary)
                    .unwrap()
                    .is_up_to_date()
            );
            assert_eq!(fs::read_to_string(binary).unwrap(), "keep binary");
            assert_eq!(
                *server.requests.lock().unwrap(),
                vec!["/repos/geekforbrains/enso-light/releases/latest"]
            );
        }
    }
}
