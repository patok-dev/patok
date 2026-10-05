//! `patok` CLI entry point. The only shipped executable.
//!
//! Engine and shell are two OS processes of this one binary: `patok` attaches to the
//! per-project engine or spawns it (`patok daemon`), then runs the shell.

mod daemon;
mod tasks;
mod update;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "patok", version, about)]
struct Cli {
    /// Project directory (default: the current directory).
    #[arg(short = 'd', long = "dir", global = true)]
    dir: Option<PathBuf>,
    /// Run the build without the terminal UI: force the build loop into sprint
    /// mode, stream the agent output to stdout line by line, exit 0 once every
    /// task is done, 1 when the build fails.
    #[arg(long)]
    headless: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Attach to (or start) the engine and open the terminal UI. The default.
    Run,
    /// Check for a newer release and report how to update.
    Update {
        /// Release channel to check.
        #[arg(long, value_enum, default_value_t = update::ChannelArg::Stable)]
        channel: update::ChannelArg,
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
    let project = project_dir(cli.dir)?;
    if cli.headless {
        match &cli.command {
            None | Some(Command::Run) => return daemon::run_headless(&project).await,
            Some(_) => bail!("--headless only applies to the default run mode"),
        }
    }
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => daemon::run_shell(&project).await,
        Command::Update { channel } => update::run(channel.into()),
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
