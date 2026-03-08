//! nova-daemon — NovaVM runtime gRPC daemon.
//!
//! Starts the RuntimeDaemon on a Unix domain socket and serves gRPC
//! requests for sandbox lifecycle management.
//!
//! Configuration is loaded from a TOML file (`--config`).

use nova_api::config::DaemonConfig;
use nova_api::server::RuntimeDaemon;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Parse --config flag (positional or flag).
    let config_path = parse_config_arg();

    let config = if let Some(ref path) = config_path {
        let p = std::path::Path::new(path);
        if p.exists() {
            tracing::info!(config = %path, "loading config from file");
            DaemonConfig::from_file(p)?
        } else {
            tracing::warn!(config = %path, "config file not found, using defaults");
            DaemonConfig::defaults()
        }
    } else {
        // Try default path, fall back to defaults.
        let default_path = std::path::Path::new("/etc/nova/nova.toml");
        if default_path.exists() {
            tracing::info!("loading config from /etc/nova/nova.toml");
            DaemonConfig::from_file(default_path)?
        } else {
            tracing::info!("no config file found, using defaults");
            DaemonConfig::defaults()
        }
    };

    let daemon = RuntimeDaemon::with_config(config);
    tracing::info!(socket = %daemon.socket_path().display(), "starting nova-daemon");
    daemon.serve().await
}

/// Simple arg parser: looks for `--config <path>` in argv.
fn parse_config_arg() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if args[i] == "--config" {
            return args.get(i + 1).cloned();
        }
    }
    None
}
