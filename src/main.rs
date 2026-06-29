use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod config;
mod edit;
mod hyprland;
mod lock;
mod restore;
mod save;
mod session;
mod status;

#[derive(Parser)]
#[command(
    name = "hypr-recall",
    about = "Save and restore Hyprland window sessions"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Save {
        #[arg(short, long, help = "Path to session file")]
        file: Option<PathBuf>,
    },
    Restore {
        #[arg(short, long, help = "Path to session file")]
        file: Option<PathBuf>,
        #[arg(long, help = "Print what would be restored without launching anything")]
        dry_run: bool,
        #[arg(
            long,
            value_name = "CLASS",
            help = "Treat CLASS as a session-restore app (repeatable)",
            action = clap::ArgAction::Append,
        )]
        session_restore_app: Vec<String>,
    },
    Status {
        #[arg(short, long, help = "Path to session file")]
        file: Option<PathBuf>,
    },
    Edit {
        #[arg(short, long, help = "Path to session file")]
        file: Option<PathBuf>,
    },
}

fn default_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(home).join(".local/share/hypr-recall/session.json")
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let cfg = config::Config::load()?;

    match cli.command {
        Commands::Save { file } => {
            let path = file.unwrap_or_else(default_path);
            save::run(&path)?;
        }
        Commands::Restore {
            file,
            dry_run,
            session_restore_app,
        } => {
            let path = file.unwrap_or_else(default_path);
            if dry_run {
                restore::run_dry(&path, &session_restore_app, &cfg)?;
            } else {
                restore::run(&path, &session_restore_app, &cfg).await?;
            }
        }
        Commands::Status { file } => {
            let path = file.unwrap_or_else(default_path);
            status::run(&path)?;
        }
        Commands::Edit { file } => {
            let path = file.unwrap_or_else(default_path);
            edit::run(&path)?;
        }
    }

    Ok(())
}
