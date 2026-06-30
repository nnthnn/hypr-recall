use anyhow::Result;
use clap::builder::styling::{Color, Effects, RgbColor, Style, Styles};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::path::PathBuf;

mod color;
mod config;
mod edit;
mod hyprland;
mod list;
mod lock;
mod restore;
mod save;
mod session;
mod status;

fn rgb(r: u8, g: u8, b: u8) -> Style {
    Style::new().fg_color(Some(Color::Rgb(RgbColor(r, g, b))))
}

fn styles() -> Styles {
    let orange = rgb(249, 115, 22) | Effects::BOLD; // #f97316
    let purple = rgb(168, 85, 247) | Effects::BOLD; // #a855f7
    Styles::styled()
        .header(orange)
        .usage(orange)
        .literal(purple)
        .placeholder(rgb(56, 189, 248)) // #38bdf8
        .error(rgb(239, 68, 68) | Effects::BOLD) // #ef4444
        .valid(rgb(34, 197, 94) | Effects::BOLD) // #22c55e
        .invalid(rgb(234, 179, 8) | Effects::BOLD) // #eab308
}

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
    /// Snapshot the current Hyprland session
    Save {
        /// Session name (default: "session")
        name: Option<String>,
        #[arg(short, long, help = "Explicit path to session file (overrides name)")]
        file: Option<PathBuf>,
    },
    /// Restore a saved session
    Restore {
        /// Session name (default: "session")
        name: Option<String>,
        #[arg(short, long, help = "Explicit path to session file (overrides name)")]
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
    /// List all saved sessions
    List,
    /// Show a summary of a saved session
    Status {
        /// Session name (default: "session")
        name: Option<String>,
        #[arg(short, long, help = "Explicit path to session file (overrides name)")]
        file: Option<PathBuf>,
    },
    /// Open a session file in $EDITOR
    Edit {
        /// Session name (default: "session")
        name: Option<String>,
        #[arg(short, long, help = "Explicit path to session file (overrides name)")]
        file: Option<PathBuf>,
    },
}

fn session_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(home).join(".local/share/hypr-recall")
}

fn session_path(name: Option<String>, file: Option<PathBuf>) -> PathBuf {
    if let Some(p) = file {
        return p;
    }
    let name = name.unwrap_or_else(|| "session".to_owned());
    session_dir().join(format!("{name}.json"))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Intentional leak: clap's bin_name needs a &'static str and this runs
    // once per short-lived process, so the gradient string lives for the run.
    let g: &'static str = color::gradient("hypr-recall").leak();
    let cmd = Cli::command().styles(styles()).bin_name(g);
    let mut matches = cmd.get_matches();
    let cli = Cli::from_arg_matches_mut(&mut matches).unwrap_or_else(|e| e.exit());

    let cfg = config::Config::load()?;

    match cli.command {
        Commands::Save { name, file } => {
            let path = session_path(name, file);
            save::run(&path)?;
        }
        Commands::Restore {
            name,
            file,
            dry_run,
            session_restore_app,
        } => {
            let path = session_path(name, file);
            if dry_run {
                restore::run_dry(&path, &session_restore_app, &cfg)?;
            } else {
                restore::run(&path, &session_restore_app, &cfg).await?;
            }
        }
        Commands::Status { name, file } => {
            let path = session_path(name, file);
            status::run(&path)?;
        }
        Commands::Edit { name, file } => {
            let path = session_path(name, file);
            edit::run(&path)?;
        }
        Commands::List => {
            list::run(&session_dir())?;
        }
    }

    Ok(())
}
