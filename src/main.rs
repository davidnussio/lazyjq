use std::{
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::anyhow;
use clap::Parser;
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
};
use tokio::sync::mpsc;

mod app;
use app::App;
mod config;
use config::{Config, DEFAULT_CONFIG};
mod editor;
mod history;
mod jq_worker;
mod json;
mod json_tree;
mod stdout_redirect;
use stdout_redirect::StdoutRedirect;
mod suggestions;
mod ui;
mod value_completion;

/// A lazygit-style terminal UI to explore JSON with jq
#[derive(Parser)]
#[command(
    name = "lazyjq",
    version,
    help_template = "
{about}

Usage: {usage}

Examples:
- Read from a file:
        {bin} data.json

- Read from standard input:
        cat data.json | {bin}

Arguments:
{positionals}

Options:
{options}
"
)]
pub struct Args {
    /// Optional path to a JSON file.
    /// If not provided or if "-" is specified,
    /// reads from standard input.
    pub input: Option<PathBuf>,

    #[arg(short = 'c', long = "config", help = "Path to the configuration file.")]
    pub config_file: Option<PathBuf>,

    #[arg(
        long = "default-filter",
        help = "Default jq filter to apply to the input data",
        long_help = "
        Sets the default jq filter to apply to the input data.
        The filter is applied when the interface is first loaded.
        "
    )]
    default_filter: Option<String>,

    #[arg(
        long = "write-to-stdout",
        help = "Write the current JSON result to stdout when exiting"
    )]
    write_to_stdout: bool,
}

/// Read the input from the given file, or from standard input when it is
/// missing or `-`.
fn read_input(args: &Args) -> anyhow::Result<String> {
    let mut ret = String::new();
    match &args.input {
        Some(path) if path != &PathBuf::from("-") => {
            File::open(path)
                .map_err(|e| anyhow!("Failed to open {}: {e}", path.display()))?
                .read_to_string(&mut ret)?;
        }
        _ => {
            io::stdin().read_to_string(&mut ret)?;
        }
    }
    Ok(ret)
}

/// Write the default configuration to `path` if there is no file there yet.
fn ensure_file_exists(path: &PathBuf) -> anyhow::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| anyhow!("Failed to create directory: {e}"))?;
    }
    std::fs::File::create(path)?.write_all(DEFAULT_CONFIG.as_bytes())?;
    Ok(())
}

/// The configuration file: the one given on the command line, or
/// `<config dir>/lazyjq/config.toml`. It is created with the defaults if missing.
fn determine_config_file(config_path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let path = match config_path {
        Some(path) => path,
        None => dirs::config_dir()
            .ok_or_else(|| anyhow!("Failed to determine the configuration directory"))?
            .join("lazyjq")
            .join("config.toml"),
    };
    ensure_file_exists(&path)?;
    Ok(path)
}

/// Restores the terminal when dropped, also on errors.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        ratatui::restore();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let config_file = determine_config_file(args.config_file.clone())?;
    let config = std::fs::read_to_string(&config_file)
        .map_err(|e| anyhow!("Failed to read configuration file: {e}"))
        .and_then(|content| Config::load_from(&content))
        .map_err(|e| anyhow!("Invalid configuration file {}: {e}", config_file.display()))?;

    let input = read_input(&args)?;
    let input = Arc::new(json::deserialize(&input, config.json.max_streams)?);

    // With `--write-to-stdout | next`, the UI must go to the terminal, not to the pipe.
    let mut stdout_redirect = StdoutRedirect::try_new_for_tui(args.write_to_stdout)?;

    let (tx, rx) = mpsc::unbounded_channel();
    let app = App::new(config, input, args.default_filter, tx);

    let app = {
        let mut terminal = ratatui::init();
        let _guard = TerminalGuard;
        execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
        app.run(&mut terminal, rx).await?
    };

    stdout_redirect.restore()?;

    if args.write_to_stdout {
        let output = app.formatted_result();
        let mut stdout = io::stdout();
        stdout.write_all(output.as_bytes())?;
        if !output.ends_with('\n') {
            stdout.write_all(b"\n")?;
        }
    }

    Ok(())
}
