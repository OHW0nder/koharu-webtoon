use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use koharu_translator::capture;

#[derive(Debug, Parser)]
#[command(about = "Inspect captured translation exchanges")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the capture directory and whether capturing is enabled.
    Path,
    /// List captured exchanges, newest last.
    List,
    /// Print a captured exchange, newest by default.
    Show {
        /// How many exchanges to step back from the newest. Defaults to 0.
        #[arg(default_value_t = 0)]
        index: usize,
    },
    /// Delete every captured exchange.
    Clear,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let root = capture::root()?;
    match args.command {
        Command::Path => {
            println!("{}", root.display());
            println!("enabled: {}", capture::enabled());
        }
        Command::List => {
            let exchanges = capture::exchanges()?;
            if exchanges.is_empty() {
                println!("no captured exchanges in {}", root.display());
            }
            for (index, directory) in exchanges.iter().enumerate() {
                println!("[{index}] {}", directory.display());
            }
        }
        Command::Show { index } => {
            let exchanges = capture::exchanges()?;
            let fallback = capture::latest()?;
            let directory = match exchanges.get(exchanges.len().saturating_sub(index + 1)) {
                Some(directory) => directory.as_path(),
                None => fallback.as_deref().context("no captured exchange")?,
            };
            for file in ["request.json", "response.json"] {
                let path = directory.join(file);
                if !path.is_file() {
                    continue;
                }
                println!("=== {file} ===");
                println!("{}", std::fs::read_to_string(&path)?.trim_end());
            }
        }
        Command::Clear => capture::clear()?,
    }
    Ok(())
}
