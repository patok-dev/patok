//! `patok` CLI entry point. The only shipped executable.
//!
//! Engine and shell are two OS processes of this one binary: `patok` attaches to the
//! per-project engine or spawns it (`patok daemon`), then runs the shell.

mod daemon;
mod ps;
mod tasks;
mod update;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "patok", version, about)]
struct Cli {
    /// Project directory (default: the current directory).
    #[arg(short = 'd', long = "dir", global = true)]
    dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Attach to (or start) the engine and open the terminal UI. The default.
    Run,
    /// Run the build without the terminal UI: force the build loop into sprint
    /// mode, stream the agent output to stdout line by line, exit 0 once every
    /// task is done, 1 when the build fails
    Headless {
        /// Color the streamed output with the shell's theme colors. Off by
        /// default: the headless output stays plain without the flag.
        #[arg(long)]
        color: bool,
    },
    /// Check for a newer release and report how to update.
    Update {
        /// Release channel to check; without the flag the tui.update_channel
        /// setting applies (stable by default).
        #[arg(long, value_enum)]
        channel: Option<update::ChannelArg>,
    },
    /// Start the engine for this project and exit once it is listening.
    Daemon {
        /// Run in the foreground instead of detaching (for debugging).
        #[arg(long)]
        foreground: bool,
        #[command(subcommand)]
        action: Option<DaemonAction>,
    },
    /// Manage tasks in the shared task store.
    Tasks {
        #[command(subcommand)]
        action: TasksAction,
    },
    /// List every running patok daemon on this machine with its project, run state, task
    /// progress, uptime, pid and engine version. Lists all daemons, so the global -d/--dir
    /// option is accepted but has no effect here.
    Ps {
        /// Print a JSON array instead of the table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TasksAction {
    /// Append a task with a patok-generated id to the shared task store.
    Add {
        /// The task text. One line; no id needed.
        text: String,
    },
    /// Remove a pending task from the shared task store by its id.
    Remove {
        /// The id of the task to remove, e.g. T3.1.
        id: String,
    },
    /// List every task in the shared task store, one per line, id first.
    List,
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Soft-stop the engine and wait until it has exited.
    Stop {
        /// Stop immediately, killing the running agent.
        #[arg(long)]
        now: bool,
    },
    /// Stop the engine, then start it again.
    Restart,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("patok: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let command = cli.command.unwrap_or(Command::Run);
    // `ps` lists every daemon on the machine; it never resolves a project directory.
    if let Command::Ps { json } = command {
        return ps::run(json).await;
    }
    let project = project_dir(cli.dir)?;
    match command {
        Command::Run => daemon::run_shell(&project).await,
        Command::Headless { color } => daemon::run_headless(&project, color).await,
        Command::Update { channel } => update::run(channel, &project),
        Command::Daemon {
            foreground: true,
            action: None,
        } => daemon::run_engine(&project).await,
        Command::Daemon {
            foreground: false,
            action: None,
        } => daemon::start(&project).await,
        Command::Daemon {
            action: Some(DaemonAction::Stop { now }),
            ..
        } => daemon::stop(&project, now).await,
        Command::Daemon {
            action: Some(DaemonAction::Restart),
            ..
        } => {
            daemon::stop(&project, false).await?;
            daemon::start(&project).await
        }
        Command::Tasks { action } => match action {
            TasksAction::Add { text } => tasks::add(&project, &text),
            TasksAction::Remove { id } => tasks::remove(&project, &id).await,
            TasksAction::List => tasks::list(&project),
        },
        Command::Ps { .. } => unreachable!("handled above"),
    }
}

/// The project directory with symlinks resolved: two spellings of one directory must map to
/// one engine.
fn project_dir(dir: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let dir = match dir {
        Some(dir) => dir,
        None => std::env::current_dir().context("the current directory cannot be determined")?,
    };
    Ok(dir.canonicalize().unwrap_or(dir))
}
