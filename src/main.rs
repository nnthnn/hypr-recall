use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod hyprland;
mod lock;
mod restore;
mod save;
mod session;

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
    },
}

fn default_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(home).join(".local/share/hypr-recall/session.json")
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Save { file } => {
            let path = file.unwrap_or_else(default_path);
            save::run(&path)?;
        }
        Commands::Restore { file } => {
            let path = file.unwrap_or_else(default_path);
            restore::run(&path).await?;
        }
    }

    Ok(())
}
