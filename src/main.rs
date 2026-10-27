//! staged-runner: self-hosted GitHub Actions runners across hosts that spill
//! over in a fixed order. See README.md.

mod agent;
mod config;
mod local;
mod service;
mod stages;

use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The fleet file, the same on every host.
    #[arg(short, long, default_value = "/etc/staged-runner.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run this host's runners in their turn and serve their states.
    Run {
        /// This host's name in the fleet file.
        host: String,
    },
    /// Print the spill-over order.
    Plan,
    /// Ask every host for its runners' states, in spill-over order.
    Status,
    /// Print a systemd unit (Linux) or a launchd daemon (macOS) that runs `run`.
    Service {
        /// This host's name in the fleet file.
        host: String,
        /// The account the service runs as. Default: root for Docker runners
        /// on Linux, otherwise the account running this command.
        #[arg(long)]
        user: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("staged-runner: {e}");
            return ExitCode::from(78);
        }
    };
    let result = match cli.command {
        Cmd::Run { host } => config
            .host(&host)
            .and_then(|me| agent::run(&config, me, local::for_kind(me.kind, me.root.clone()))),
        Cmd::Plan => {
            for (i, r) in stages::order(&config.hosts).iter().enumerate() {
                println!("{:>3}  {r}", i + 1);
            }
            Ok(())
        }
        Cmd::Status => {
            status(&config);
            Ok(())
        }
        Cmd::Service { host, user } => config.host(&host).and_then(|me| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let path = std::path::absolute(&cli.config).map_err(|e| e.to_string())?;
            let user = user.or_else(|| default_user(me.kind));
            if me.kind == config::Kind::Native && user.as_deref().is_none_or(|u| u == "root") {
                return Err("native runners cannot run as root: pass --user".into());
            }
            service::render(&exe, &path, &host, user.as_deref()).map(|s| print!("{s}"))
        }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("staged-runner: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Under sudo, the account that ran sudo.
fn default_user(kind: config::Kind) -> Option<String> {
    if cfg!(target_os = "linux") && kind == config::Kind::Docker {
        return None;
    }
    std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|u| u != "root")
}

fn status(config: &Config) {
    let mut states = std::collections::HashMap::new();
    for h in &config.hosts {
        match agent::fetch(&h.endpoint) {
            Ok(s) => states.extend(s.runners),
            Err(e) => eprintln!("{}: {e}", h.endpoint),
        }
    }
    for r in stages::order(&config.hosts) {
        let state = states
            .get(&r)
            .map_or("no answer".to_string(), |s| format!("{s:?}").to_lowercase());
        println!("{r:<24} {state}");
    }
}
