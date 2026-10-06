use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use enso::{
    app,
    config::{self, Destination},
    db::Db,
    jobs, service,
    slack::Slack,
    slack_cli,
};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    version,
    about = "Slack conversations and scheduled prompts through native agent CLIs"
)]
struct Cli {
    #[arg(long, env = "ENSO_HOME", global = true)]
    home: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Create missing configuration, workspace guidance and job/upload directories.
    Init,
    /// Run the foreground service.
    Run,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
    },
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },
    /// Read Slack conversations and users, search messages, and manage reactions.
    Slack {
        #[command(subcommand)]
        command: slack_cli::SlackCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
}
#[derive(Subcommand)]
enum ConfigCommand {
    Check,
}
#[derive(Subcommand)]
enum JobsCommand {
    List,
    Run {
        name: String,
        #[arg(long)]
        wait: bool,
    },
}
#[derive(Subcommand)]
enum MessageCommand {
    Send {
        #[arg(conflicts_with = "text_file")]
        text: Option<String>,
        #[arg(long)]
        text_file: Option<PathBuf>,
        /// Attach a file; repeat for multiple files.
        #[arg(long = "file")]
        files: Vec<PathBuf>,
        /// Slack channel or DM ID. Defaults to the current run or configured notification target.
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        thread: Option<String>,
        #[arg(long)]
        plain: bool,
    },
}
#[derive(Subcommand)]
enum ServiceCommand {
    Install,
    Start,
    Stop,
    Restart,
    Uninstall,
    Status,
    Logs {
        #[arg(long)]
        follow: bool,
    },
}
#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(error) = execute(cli).await {
        if json {
            eprintln!("{}", json!({"ok":false,"error":format!("{error:#}")}));
        } else {
            eprintln!("Error: {error:#}");
        }
        std::process::exit(1)
    }
}
fn print(value: &Value, compact: bool) {
    if compact {
        println!("{value}");
    } else {
        println!("{}", serde_json::to_string_pretty(value).unwrap());
    }
}
fn active(home: &std::path::Path) -> Result<()> {
    ensure!(
        app::is_running(home),
        "Enso is not running. Use enso service start (or enso run)."
    );
    Ok(())
}
async fn execute(cli: Cli) -> Result<()> {
    let home = cli.home.unwrap_or(
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?
            .join(".enso"),
    );
    ensure!(home.is_absolute(), "--home must be an absolute path");
    match cli.command {
        Command::Init => {
            // Never seed a mini configuration over an old Enso installation.
            if home.join("enso.db").exists() {
                Db::open(&home)?;
            }
            if home.join("config.json").exists() {
                config::load(&home)?;
            }
            config::init(&home)?;
            Db::open(&home)?;
            print(
                &json!({"initialized":home,"next":"Set Slack credentials and allowed destinations, then run enso config check."}),
                cli.json,
            );
        }
        Command::Run => app::run(home).await?,
        Command::Slack { command } => {
            let loaded = config::load(&home)?;
            let slack = Slack::new(&loaded.config.slack)?;
            print(&slack_cli::execute(command, slack).await?, cli.json);
        }
        Command::Config {
            command: ConfigCommand::Check,
        } => {
            let loaded = config::load(&home)?;
            loaded.config.validate()?;
            let jobs = jobs::list(&home, &loaded.config.execution)?;
            print(
                &json!({"valid":true,"cli":loaded.config.execution.cli,"jobs":jobs.len()}),
                cli.json,
            );
        }
        Command::Jobs {
            command: JobsCommand::List,
        } => {
            let loaded = config::load(&home)?;
            let db = Db::open(&home)?;
            let mut rows = Vec::new();
            for job in jobs::list(&home, &loaded.config.execution)? {
                let next = if job.enabled {
                    job.cron
                        .as_ref()
                        .map(|cron| jobs::next_run(cron, chrono::Local::now()))
                        .transpose()?
                        .flatten()
                } else {
                    None
                };
                rows.push(json!({"name":job.name,"enabled":job.enabled,"cron":job.cron,"next_run":next,"last_run":db.last_job(&job.name)?}));
            }
            print(&json!(rows), cli.json);
        }
        Command::Jobs {
            command: JobsCommand::Run { name, wait },
        } => {
            active(&home)?;
            let loaded = config::load(&home)?;
            jobs::load(&home, &name, &loaded.config.execution)?;
            let db = Db::open(&home)?;
            let id = db
                .enqueue_job(&name, "manual", None)?
                .context("Job was not admitted")?;
            if wait {
                loop {
                    let run = db.run(&id)?;
                    let state = run["state"].as_str().unwrap_or("");
                    if !matches!(state, "queued" | "running") {
                        print(&run, cli.json);
                        ensure!(
                            matches!(state, "succeeded" | "skipped"),
                            "Job ended with {state}"
                        );
                        break;
                    }
                    ensure!(
                        app::is_running(&home),
                        "Enso stopped; inspect the job after restarting"
                    );
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            } else {
                print(&json!({"run_id":id,"state":"queued"}), cli.json);
            }
        }
        Command::Message {
            command:
                MessageCommand::Send {
                    text,
                    text_file,
                    files,
                    to,
                    thread,
                    plain,
                },
        } => {
            active(&home)?;
            let loaded = config::load(&home)?;
            let mut body = match (text, text_file) {
                (Some(text), _) => text,
                (_, Some(path)) => {
                    std::fs::read_to_string(path).context("Cannot read message text file")?
                }
                _ => String::new(),
            };
            if body == "-" {
                body.clear();
                std::io::stdin().read_to_string(&mut body)?;
            }
            ensure!(
                !body.trim().is_empty() || !files.is_empty(),
                "Supply message text, --text-file, stdin (-), or --file attachments"
            );
            let destination = if let Some(channel) = to {
                Destination { channel, thread }
            } else if let Ok(channel) = std::env::var("ENSO_CHANNEL")
                && !channel.is_empty()
            {
                Destination {
                    channel,
                    thread: thread.or_else(|| {
                        std::env::var("ENSO_THREAD_TS")
                            .ok()
                            .filter(|s| !s.is_empty())
                    }),
                }
            } else {
                let mut d = loaded.config.slack.notify.context(
                    "No destination: supply --to CHANNEL or configure slack.notify / job.notify",
                )?;
                if thread.is_some() {
                    d.thread = thread;
                }
                d
            };
            ensure!(
                !destination.channel.is_empty(),
                "Destination channel must not be empty"
            );
            let db = Db::open(&home)?;
            let run = std::env::var("ENSO_RUN_ID")
                .ok()
                .filter(|s| !s.is_empty() && db.run(s).is_ok());
            let ids = db.outgoing(&destination, &body, plain, &files, run.as_deref(), true)?;
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                let receipts = ids
                    .iter()
                    .map(|id| db.delivery(id))
                    .collect::<Result<Vec<_>>>()?;
                if receipts
                    .iter()
                    .all(|v| !matches!(v["state"].as_str(), Some("pending" | "sending")))
                {
                    let success = receipts.iter().all(|v| v["state"] == "sent");
                    print(&json!({"ok":success,"deliveries":receipts}), cli.json);
                    ensure!(
                        success,
                        "One or more messages failed or have uncertain delivery; inspect receipts before resending"
                    );
                    break;
                }
                if Instant::now() >= deadline || !app::is_running(&home) {
                    print(&json!({"ok":false,"deliveries":receipts}), cli.json);
                    bail!(
                        "Delivery remains pending; do not resend without checking Slack and service logs"
                    )
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        Command::Service { command } => match command {
            ServiceCommand::Install => print(&service::install(&home)?, cli.json),
            ServiceCommand::Logs { follow } => service::logs(&home, follow)?,
            ServiceCommand::Status => {
                let mut status = service::action(&home, "status")?;
                status["daemon_running"] = json!(app::is_running(&home));
                if home.join("enso.db").exists() {
                    status["runtime"] = Db::open(&home)?.health()?;
                }
                print(&status, cli.json);
            }
            action => {
                let name = match action {
                    ServiceCommand::Start => "start",
                    ServiceCommand::Stop => "stop",
                    ServiceCommand::Restart => "restart",
                    ServiceCommand::Uninstall => "uninstall",
                    _ => unreachable!(),
                };
                print(&service::action(&home, name)?, cli.json);
            }
        },
    }
    Ok(())
}
