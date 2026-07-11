mod config;
mod driver;
mod runtime;
mod tui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ncd",
    about = "NCD Host — expose local devices to NCD device endpoints"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the interactive configuration TUI.
    Config,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Config) => match tui::run_tui() {
            Some(cfg) => {
                if let Err(e) = cfg.save() {
                    eprintln!("Failed to save configuration: {e}");
                    std::process::exit(1);
                }
                let path = config::config_path();
                println!("Configuration saved to {}", path.display());
                println!(
                    "{} device(s) configured. Run 'ncd' to start.",
                    cfg.device.len()
                );
            }
            None => {
                println!("Configuration cancelled.");
            }
        },
        None => {
            let cfg = config::HostConfig::load().unwrap_or_else(|| {
                let path = config::config_path();
                eprintln!("No configuration found at {}", path.display());
                eprintln!("Run 'ncd config' to create one.");
                std::process::exit(1);
            });

            if cfg.device.is_empty() {
                eprintln!("No devices configured. Run 'ncd config' to add devices.");
                std::process::exit(1);
            }

            eprintln!("Starting NCD host with {} device(s)...", cfg.device.len());

            if let Err(e) = runtime::run(cfg).await {
                eprintln!("Fatal error: {e}");
                std::process::exit(1);
            }
        }
    }
}
