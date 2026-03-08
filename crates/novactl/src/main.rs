//! nova — unified CLI for NovaVM.
//!
//! Single binary that combines:
//! - Daemon server (`nova serve`)
//! - Sandbox management (`nova run`, `nova ps`, `nova exec`, ...)
//! - Image management (`nova pull`, `nova images`)
//! - Policy management (`nova policy ...`)
//! - Embedded asset extraction (`nova setup`)
//!
//! Replaces the separate `nova-daemon` and `novactl` binaries.

mod embedded;
mod style;

use clap::{Parser, Subcommand};
use tonic::transport::Endpoint;
use tower::service_fn;

use nova_api::config::DaemonConfig;
use nova_api::policy::policy_service_client::PolicyServiceClient;
use nova_api::policy::*;
use nova_api::runtime::runtime_service_client::RuntimeServiceClient;
use nova_api::runtime::*;
use nova_api::sandbox::sandbox_image_service_client::SandboxImageServiceClient;
use nova_api::sandbox::*;
use nova_api::server::RuntimeDaemon;

/// NovaVM — secure microVM sandbox runtime.
#[derive(Parser)]
#[command(name = "nova", version, about = "NovaVM — run code in secure microVM sandboxes")]
struct Cli {
    /// Runtime socket path.
    #[arg(long, default_value = "/run/nova/nova.sock")]
    socket: String,

    /// Output format.
    #[arg(long, default_value = "table")]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

/// Output format for CLI commands.
#[derive(Debug, Clone, clap::ValueEnum, PartialEq)]
enum OutputFormat {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the NovaVM daemon (gRPC + REST API).
    Serve {
        /// Path to config file (default: /etc/nova/nova.toml).
        #[arg(long)]
        config: Option<String>,
    },

    /// Extract embedded assets (kernel, eBPF, agent) to /opt/nova/.
    Setup {
        /// Overwrite existing files.
        #[arg(long)]
        force: bool,

        /// List embedded assets without extracting.
        #[arg(long)]
        list: bool,
    },

    /// Create and start a new sandbox from an image.
    Run {
        /// OCI image reference.
        image: String,
        /// Number of vCPUs.
        #[arg(long, default_value = "1")]
        vcpus: u32,
        /// Memory in MiB.
        #[arg(long, default_value = "128")]
        memory: u32,
        /// Sandbox name (auto-generated if not provided).
        #[arg(long)]
        name: Option<String>,
        /// Command to run inside the sandbox.
        #[arg(long)]
        cmd: Option<String>,
    },

    /// List running sandboxes.
    Ps {
        /// Show all sandboxes (including stopped).
        #[arg(short, long)]
        all: bool,
    },

    /// Stop a running sandbox.
    Stop {
        /// Sandbox ID or name.
        sandbox: String,
        /// Timeout in seconds before force-kill.
        #[arg(short, long, default_value = "10")]
        timeout: u32,
    },

    /// Force-kill a sandbox.
    Kill {
        /// Sandbox ID or name.
        sandbox: String,
    },

    /// Remove a stopped sandbox.
    Rm {
        /// Sandbox ID or name.
        sandbox: String,
        /// Force remove (stop if running).
        #[arg(short, long)]
        force: bool,
    },

    /// Execute a command in a running sandbox.
    Exec {
        /// Sandbox ID or name.
        sandbox: String,
        /// Command to execute.
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },

    /// Fetch logs from a sandbox.
    Logs {
        /// Sandbox ID or name.
        sandbox: String,
        /// Follow log output.
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show.
        #[arg(short = 'n', long, default_value = "100")]
        lines: u32,
    },

    /// List locally available images.
    Images,

    /// Pull an OCI image.
    Pull {
        /// OCI image reference.
        image: String,
    },

    /// Inspect a sandbox or image.
    Inspect {
        /// Sandbox or image ID.
        target: String,
    },

    /// Policy management commands.
    Policy {
        #[command(subcommand)]
        action: PolicyCommands,
    },

    /// Pull image, boot VM, and attach to console.
    Shell {
        /// OCI image reference.
        image: String,
        /// Number of vCPUs.
        #[arg(long, default_value = "1")]
        vcpus: u32,
        /// Memory in MiB.
        #[arg(long, default_value = "256")]
        memory: u32,
        /// Command to run.
        #[arg(long)]
        cmd: Option<String>,
    },
}

#[derive(Subcommand)]
enum PolicyCommands {
    /// List loaded policies.
    List,
    /// Load a policy bundle.
    Load {
        /// Path to the policy bundle file.
        path: String,
        /// Bundle ID.
        #[arg(long)]
        id: Option<String>,
    },
    /// Remove a policy bundle.
    Remove {
        /// Bundle ID.
        bundle_id: String,
    },
    /// Evaluate a policy.
    Eval {
        /// Policy path.
        policy: String,
        /// Input JSON.
        input: String,
    },
    /// Show policy engine status.
    Status,
}

// ── gRPC connection helpers ──────────────────────────────────────────

async fn connect_unix(
    socket_path: &str,
) -> anyhow::Result<RuntimeServiceClient<tonic::transport::Channel>> {
    let socket_path = socket_path.to_string();
    let channel = Endpoint::try_from("http://[::]:50051")?
        .connect_with_connector(service_fn(move |_: tonic::transport::Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await?;
    Ok(RuntimeServiceClient::new(channel))
}

async fn connect_policy_unix(
    socket_path: &str,
) -> anyhow::Result<PolicyServiceClient<tonic::transport::Channel>> {
    let socket_path = socket_path.to_string();
    let channel = Endpoint::try_from("http://[::]:50051")?
        .connect_with_connector(service_fn(move |_: tonic::transport::Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await?;
    Ok(PolicyServiceClient::new(channel))
}

async fn connect_image_unix(
    socket_path: &str,
) -> anyhow::Result<SandboxImageServiceClient<tonic::transport::Channel>> {
    let socket_path = socket_path.to_string();
    let channel = Endpoint::try_from("http://[::]:50051")?
        .connect_with_connector(service_fn(move |_: tonic::transport::Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await?;
    Ok(SandboxImageServiceClient::new(channel))
}

fn state_name(state: i32) -> &'static str {
    match SandboxState::try_from(state) {
        Ok(SandboxState::Created) => "created",
        Ok(SandboxState::Running) => "running",
        Ok(SandboxState::Stopped) => "stopped",
        Ok(SandboxState::Error) => "error",
        _ => "unknown",
    }
}

// ── Main ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let json = cli.format == OutputFormat::Json;

    match &cli.command {
        // ── Daemon ───────────────────────────────────────────────
        Commands::Serve { config } => {
            cmd_serve(config.clone()).await?;
        }

        // ── Setup ────────────────────────────────────────────────
        Commands::Setup { force, list } => {
            cmd_setup(*force, *list)?;
        }

        // ── Sandbox lifecycle ────────────────────────────────────
        Commands::Run {
            image,
            vcpus,
            memory,
            name,
            cmd,
        } => {
            let mut client = connect_unix(&cli.socket).await?;

            let sandbox_id = name
                .clone()
                .unwrap_or_else(|| format!("nova-{}", &uuid_short()));

            let sp = if !json {
                Some(style::spinner(&format!(
                    "Creating sandbox '{}' with {}...",
                    sandbox_id, image
                )))
            } else {
                None
            };

            let create_req = CreateSandboxRequest {
                sandbox_id: sandbox_id.clone(),
                image: image.clone(),
                config: Some(SandboxConfig {
                    vcpus: *vcpus,
                    memory_mib: *memory,
                    kernel: String::new(),
                    rootfs: String::new(),
                    cmdline: String::new(),
                    network: None,
                    env: std::collections::HashMap::new(),
                }),
                command: cmd.clone().unwrap_or_default(),
            };

            let create_resp = client.create_sandbox(create_req).await?;
            let created_id = create_resp.into_inner().sandbox_id;

            if let Some(ref s) = sp {
                s.finish_and_clear();
            }
            if json {
                println!(
                    "{}",
                    serde_json::json!({"sandbox_id": created_id, "action": "created"})
                );
            } else {
                style::success(&format!("Sandbox '{}' created", created_id));
            }

            let sp = if !json {
                Some(style::spinner("Starting sandbox..."))
            } else {
                None
            };

            let start_req = StartSandboxRequest {
                sandbox_id: created_id.clone(),
            };
            client.start_sandbox(start_req).await?;

            if let Some(ref s) = sp {
                s.finish_and_clear();
            }
            if json {
                println!(
                    "{}",
                    serde_json::json!({"sandbox_id": created_id, "action": "started"})
                );
            } else {
                style::success(&format!("Sandbox '{}' started", created_id));
            }
        }

        Commands::Ps { all: _ } => {
            let mut client = connect_unix(&cli.socket).await?;

            let resp = client.list_sandboxes(ListSandboxesRequest {}).await?;
            let sandboxes = resp.into_inner().sandboxes;

            if json {
                let items: Vec<_> = sandboxes
                    .iter()
                    .map(|sb| {
                        serde_json::json!({
                            "sandbox_id": sb.sandbox_id,
                            "state": state_name(sb.state),
                            "pid": sb.pid,
                            "created_at": sb.created_at,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items).unwrap());
            } else {
                let mut table = style::nova_table(&["SANDBOX ID", "STATE", "PID", "CREATED"]);
                for sb in &sandboxes {
                    let sn = state_name(sb.state);
                    let pid_str = if sb.pid > 0 {
                        sb.pid.to_string()
                    } else {
                        "-".to_string()
                    };
                    table.add_row(vec![
                        sb.sandbox_id.clone(),
                        style::state_styled(sn),
                        pid_str,
                        sb.created_at.clone(),
                    ]);
                }
                println!("{table}");
                let count = sandboxes.len();
                println!(
                    "  {} sandbox{}",
                    count,
                    if count == 1 { "" } else { "es" }
                );
            }
        }

        Commands::Stop { sandbox, timeout } => {
            let mut client = connect_unix(&cli.socket).await?;

            let req = StopSandboxRequest {
                sandbox_id: sandbox.clone(),
                timeout_seconds: *timeout,
            };
            client.stop_sandbox(req).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"sandbox_id": sandbox, "action": "stopped"})
                );
            } else {
                style::success(&format!("Sandbox '{}' stopped.", sandbox));
            }
        }

        Commands::Kill { sandbox } => {
            let mut client = connect_unix(&cli.socket).await?;

            let req = StopSandboxRequest {
                sandbox_id: sandbox.clone(),
                timeout_seconds: 0,
            };
            client.stop_sandbox(req).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"sandbox_id": sandbox, "action": "killed"})
                );
            } else {
                style::success(&format!("Sandbox '{}' killed.", sandbox));
            }
        }

        Commands::Rm { sandbox, force } => {
            let mut client = connect_unix(&cli.socket).await?;

            if *force {
                let stop_req = StopSandboxRequest {
                    sandbox_id: sandbox.clone(),
                    timeout_seconds: 0,
                };
                let _ = client.stop_sandbox(stop_req).await;
            }

            let req = DestroySandboxRequest {
                sandbox_id: sandbox.clone(),
            };
            client.destroy_sandbox(req).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"sandbox_id": sandbox, "action": "removed"})
                );
            } else {
                style::success(&format!("Sandbox '{}' removed.", sandbox));
            }
        }

        Commands::Exec { sandbox, command } => {
            if command.is_empty() {
                style::error("no command specified");
                std::process::exit(1);
            }

            let mut client = connect_unix(&cli.socket).await?;

            let req = ExecInSandboxRequest {
                sandbox_id: sandbox.clone(),
                command: command.clone(),
                env: std::collections::HashMap::new(),
            };
            let resp = client.exec_in_sandbox(req).await?;
            let exec_resp = resp.into_inner();

            if !exec_resp.stdout.is_empty() {
                print!("{}", String::from_utf8_lossy(&exec_resp.stdout));
            }
            if !exec_resp.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&exec_resp.stderr));
            }

            if exec_resp.exit_code != 0 {
                std::process::exit(exec_resp.exit_code);
            }
        }

        Commands::Logs {
            sandbox,
            follow,
            lines: _,
        } => {
            let mut client = connect_unix(&cli.socket).await?;

            let stream_req = StreamConsoleRequest {
                sandbox_id: sandbox.clone(),
            };

            match client.stream_console(stream_req).await {
                Ok(resp) => {
                    let mut stream = resp.into_inner();

                    if *follow {
                        loop {
                            tokio::select! {
                                msg = stream.message() => {
                                    match msg {
                                        Ok(Some(output)) => {
                                            print!("{}", String::from_utf8_lossy(&output.data));
                                        }
                                        Ok(None) => break,
                                        Err(e) => {
                                            style::error(&format!("Stream error: {}", e));
                                            break;
                                        }
                                    }
                                }
                                _ = tokio::signal::ctrl_c() => {
                                    break;
                                }
                            }
                        }
                    } else {
                        let deadline = tokio::time::Instant::now()
                            + std::time::Duration::from_secs(1);
                        let mut collected = Vec::new();

                        loop {
                            tokio::select! {
                                msg = stream.message() => {
                                    match msg {
                                        Ok(Some(output)) => {
                                            collected.extend_from_slice(&output.data);
                                        }
                                        Ok(None) => break,
                                        Err(_) => break,
                                    }
                                }
                                _ = tokio::time::sleep_until(deadline) => {
                                    break;
                                }
                            }
                        }

                        if collected.is_empty() {
                            style::info("(no console output yet)");
                        } else {
                            print!("{}", String::from_utf8_lossy(&collected));
                        }
                    }
                }
                Err(e) => {
                    let req = SandboxStatusRequest {
                        sandbox_id: sandbox.clone(),
                    };
                    let resp = client.sandbox_status(req).await?;
                    let status = resp.into_inner();
                    let sn = state_name(status.state);

                    if json {
                        let output = serde_json::json!({
                            "sandbox_id": status.sandbox_id,
                            "state": sn,
                            "pid": status.pid,
                            "created_at": status.created_at,
                            "error": e.to_string(),
                        });
                        println!("{}", serde_json::to_string_pretty(&output).unwrap());
                    } else {
                        println!();
                        style::header(&format!("Sandbox: {}", status.sandbox_id));
                        println!();
                        style::kv("State", &style::state_styled(sn));
                        style::kv(
                            "PID",
                            &if status.pid > 0 {
                                status.pid.to_string()
                            } else {
                                "-".to_string()
                            },
                        );
                        style::kv("Created", &status.created_at);
                        println!();
                        style::error(&format!("Console stream unavailable: {}", e));
                    }
                }
            }
        }

        Commands::Images => {
            let mut client = connect_image_unix(&cli.socket).await?;
            let resp = client.list_images(ListImagesRequest {}).await?;
            let images = resp.into_inner().images;

            if json {
                let items: Vec<_> = images
                    .iter()
                    .map(|img| {
                        serde_json::json!({
                            "image_ref": img.image_ref,
                            "format": img.format,
                            "size_bytes": img.size_bytes,
                            "digest": img.digest,
                            "rootfs_path": img.rootfs_path,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items).unwrap());
            } else {
                let mut table = style::nova_table(&["IMAGE", "FORMAT", "SIZE", "DIGEST"]);
                for img in &images {
                    let short_digest = if img.digest.len() > 19 {
                        format!("{}...", &img.digest[..19])
                    } else {
                        img.digest.clone()
                    };
                    table.add_row(vec![
                        img.image_ref.clone(),
                        img.format.clone(),
                        style::format_size(img.size_bytes),
                        short_digest,
                    ]);
                }
                println!("{table}");
                let count = images.len();
                println!(
                    "  {} image{}",
                    count,
                    if count == 1 { "" } else { "s" }
                );
            }
        }

        Commands::Pull { image } => {
            let sp = if !json {
                Some(style::spinner(&format!("Pulling {}...", image)))
            } else {
                None
            };

            let mut client = connect_image_unix(&cli.socket).await?;
            let resp = client
                .pull_image(PullImageRequest {
                    image_ref: image.clone(),
                })
                .await?;
            let result = resp.into_inner();

            if let Some(ref s) = sp {
                s.finish_and_clear();
            }

            if json {
                let output = serde_json::json!({
                    "image": image,
                    "digest": result.digest,
                    "rootfs_path": result.rootfs_path,
                });
                println!("{}", serde_json::to_string_pretty(&output).unwrap());
            } else {
                style::success(&format!("Digest:  {}", result.digest));
                style::success(&format!("Rootfs:  {}", result.rootfs_path));
            }
        }

        Commands::Inspect { target } => {
            match connect_image_unix(&cli.socket).await {
                Ok(mut img_client) => {
                    match img_client
                        .inspect_image(InspectImageRequest {
                            image_ref: target.clone(),
                        })
                        .await
                    {
                        Ok(resp) => {
                            if let Some(info) = resp.into_inner().info {
                                if json {
                                    let output = serde_json::json!({
                                        "type": "image",
                                        "image_ref": info.image_ref,
                                        "digest": info.digest,
                                        "size_bytes": info.size_bytes,
                                        "format": info.format,
                                        "rootfs_path": info.rootfs_path,
                                    });
                                    println!(
                                        "{}",
                                        serde_json::to_string_pretty(&output).unwrap()
                                    );
                                } else {
                                    println!();
                                    style::header(&format!("Image: {}", info.image_ref));
                                    println!();
                                    style::kv("Image Ref", &info.image_ref);
                                    style::kv("Digest", &info.digest);
                                    style::kv("Format", &info.format);
                                    style::kv("Size", &style::format_size(info.size_bytes));
                                    style::kv("Rootfs", &info.rootfs_path);
                                }
                            } else {
                                style::error("No image info returned");
                            }
                        }
                        Err(_) => {
                            inspect_sandbox(&cli.socket, target, json).await?;
                        }
                    }
                }
                Err(_) => {
                    inspect_sandbox(&cli.socket, target, json).await?;
                }
            }
        }

        Commands::Policy { action } => match action {
            PolicyCommands::List => {
                let mut client = connect_policy_unix(&cli.socket).await?;
                let resp = client.list_bundles(ListBundlesRequest {}).await?;
                let bundles = resp.into_inner().bundles;

                if json {
                    let items: Vec<_> = bundles
                        .iter()
                        .map(|b| {
                            serde_json::json!({
                                "bundle_id": b.bundle_id,
                                "policy_count": b.policy_count,
                                "loaded_at": b.loaded_at,
                                "digest": b.digest,
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&items).unwrap());
                } else {
                    let mut table =
                        style::nova_table(&["BUNDLE ID", "POLICIES", "LOADED AT", "DIGEST"]);
                    for b in &bundles {
                        table.add_row(vec![
                            b.bundle_id.clone(),
                            b.policy_count.to_string(),
                            b.loaded_at.clone(),
                            b.digest.clone(),
                        ]);
                    }
                    println!("{table}");
                    if bundles.is_empty() {
                        style::info("(no bundles loaded)");
                    }
                }
            }
            PolicyCommands::Load { path, id } => {
                let bundle_id = id.clone().unwrap_or_else(|| "default".to_string());
                let data = std::fs::read(path)?;

                let mut client = connect_policy_unix(&cli.socket).await?;
                let resp = client
                    .load_bundle(LoadBundleRequest {
                        bundle_id: bundle_id.clone(),
                        bundle_data: data,
                    })
                    .await?;
                let result = resp.into_inner();
                if result.success {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({"bundle_id": bundle_id, "action": "loaded"})
                        );
                    } else {
                        style::success(&format!("Bundle '{}' loaded.", bundle_id));
                    }
                } else {
                    style::error(&format!("Bundle load failed: {}", result.error_message));
                    std::process::exit(1);
                }
            }
            PolicyCommands::Remove { bundle_id } => {
                let mut client = connect_policy_unix(&cli.socket).await?;
                client
                    .remove_bundle(RemoveBundleRequest {
                        bundle_id: bundle_id.clone(),
                    })
                    .await?;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"bundle_id": bundle_id, "action": "removed"})
                    );
                } else {
                    style::success(&format!("Bundle '{}' removed.", bundle_id));
                }
            }
            PolicyCommands::Eval { policy, input } => {
                let mut client = connect_policy_unix(&cli.socket).await?;
                let resp = client
                    .evaluate(EvaluateRequest {
                        policy_path: policy.clone(),
                        input: input.as_bytes().to_vec(),
                    })
                    .await?;
                let result = resp.into_inner();

                if json {
                    let output = serde_json::json!({
                        "allowed": result.allowed,
                        "reason": result.reason,
                        "eval_duration_us": result.eval_duration_us,
                    });
                    println!("{}", serde_json::to_string_pretty(&output).unwrap());
                } else if result.allowed {
                    style::success("allowed: true");
                } else {
                    style::error("allowed: false");
                    style::kv("Reason", &result.reason);
                }
                if !json && result.eval_duration_us > 0 {
                    style::kv("Eval (us)", &result.eval_duration_us.to_string());
                }
            }
            PolicyCommands::Status => {
                let mut client = connect_policy_unix(&cli.socket).await?;
                let resp = client.get_status(GetPolicyStatusRequest {}).await?;
                let status = resp.into_inner();

                if json {
                    let output = serde_json::json!({
                        "loaded_bundles": status.loaded_bundles,
                        "total_evaluations": status.total_evaluations,
                        "denied_evaluations": status.denied_evaluations,
                        "avg_eval_duration_us": status.avg_eval_duration_us,
                    });
                    println!("{}", serde_json::to_string_pretty(&output).unwrap());
                } else {
                    println!();
                    style::header("Policy Engine Status");
                    println!();
                    style::kv("Bundles", &status.loaded_bundles.to_string());
                    style::kv("Total evals", &status.total_evaluations.to_string());
                    style::kv("Denied", &status.denied_evaluations.to_string());
                    style::kv("Avg (us)", &status.avg_eval_duration_us.to_string());
                }
            }
        },

        Commands::Shell {
            image,
            vcpus,
            memory,
            cmd,
        } => {
            cmd_shell(&cli.socket, image, *vcpus, *memory, cmd.clone(), json).await?;
        }
    }

    Ok(())
}

// ── Serve command ────────────────────────────────────────────────────

async fn cmd_serve(config_path: Option<String>) -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Auto-extract embedded assets if not yet extracted.
    if embedded::has_embedded_assets() {
        let kernel_exists = embedded::nova_dir().join("vmlinux").exists();
        if !kernel_exists {
            tracing::info!("first run detected — extracting embedded assets");
            match embedded::extract_all(false) {
                Ok(n) => tracing::info!(count = n, "assets extracted"),
                Err(e) => tracing::warn!(err = %e, "asset extraction failed (run `nova setup` manually)"),
            }
        }
    }

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
        // Try default path, then auto-generated, then defaults.
        let default_path = std::path::Path::new("/etc/nova/nova.toml");
        if default_path.exists() {
            tracing::info!("loading config from /etc/nova/nova.toml");
            DaemonConfig::from_file(default_path)?
        } else if let Some(generated) = embedded::generate_config_if_needed() {
            tracing::info!(config = %generated.display(), "using auto-generated config");
            DaemonConfig::from_file(&generated)?
        } else {
            tracing::info!("no config file found, using defaults");
            DaemonConfig::defaults()
        }
    };

    let daemon = RuntimeDaemon::with_config(config);
    tracing::info!(socket = %daemon.socket_path().display(), "starting nova daemon");
    daemon.serve().await.map_err(|e| anyhow::anyhow!("{}", e))
}

// ── Setup command ────────────────────────────────────────────────────

fn cmd_setup(force: bool, list: bool) -> anyhow::Result<()> {
    if list {
        embedded::list_assets();
        return Ok(());
    }

    if !embedded::has_embedded_assets() {
        style::info("No embedded assets in this build.");
        style::info("Build with assets in crates/novactl/assets/ to embed them.");
        style::info("Run: scripts/package-assets.sh");
        return Ok(());
    }

    println!();
    style::header("NovaVM Setup");
    println!();

    let count = embedded::extract_all(force)?;

    // Generate config if needed.
    if let Some(path) = embedded::generate_config_if_needed() {
        println!("  config → {}", path.display());
    }

    println!();
    if count > 0 {
        style::success(&format!("{} assets extracted. Ready to run: nova serve", count));
    } else {
        style::info("All assets already installed. Use --force to overwrite.");
    }

    Ok(())
}

// ── Inspect sandbox ──────────────────────────────────────────────────

async fn inspect_sandbox(socket: &str, target: &str, json: bool) -> anyhow::Result<()> {
    let mut client = connect_unix(socket).await?;
    let req = SandboxStatusRequest {
        sandbox_id: target.to_string(),
    };
    let resp = client.sandbox_status(req).await?;
    let status = resp.into_inner();
    let sn = state_name(status.state);

    if json {
        let output = serde_json::json!({
            "type": "sandbox",
            "sandbox_id": status.sandbox_id,
            "state": sn,
            "pid": status.pid,
            "created_at": status.created_at,
        });
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        println!();
        style::header(&format!("Sandbox: {}", status.sandbox_id));
        println!();
        style::kv("State", &style::state_styled(sn));
        style::kv(
            "PID",
            &if status.pid > 0 {
                status.pid.to_string()
            } else {
                "-".to_string()
            },
        );
        style::kv("Created", &status.created_at);
    }
    Ok(())
}

// ── Shell command ────────────────────────────────────────────────────

async fn cmd_shell(
    socket: &str,
    image: &str,
    vcpus: u32,
    memory: u32,
    cmd: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let sp = if !json {
        Some(style::spinner(&format!("Pulling {}...", image)))
    } else {
        None
    };

    let mut img_client = connect_image_unix(socket).await?;
    let pull_resp = img_client
        .pull_image(PullImageRequest {
            image_ref: image.to_string(),
        })
        .await?;
    let pull_result = pull_resp.into_inner();

    if let Some(ref s) = sp {
        s.finish_and_clear();
    }
    if !json {
        style::success(&format!(
            "Image ready (digest: {}...)",
            &pull_result
                .digest
                .get(..19)
                .unwrap_or(&pull_result.digest)
        ));
    }

    let sandbox_id = format!("nova-{}", uuid_short());

    let sp = if !json {
        Some(style::spinner(&format!(
            "Booting VM '{}'...",
            sandbox_id
        )))
    } else {
        None
    };

    let mut client = connect_unix(socket).await?;
    let create_req = CreateSandboxRequest {
        sandbox_id: sandbox_id.clone(),
        image: image.to_string(),
        config: Some(SandboxConfig {
            vcpus,
            memory_mib: memory,
            kernel: String::new(),
            rootfs: String::new(),
            cmdline: String::new(),
            network: None,
            env: std::collections::HashMap::new(),
        }),
        command: cmd.unwrap_or_default(),
    };
    let create_resp = client.create_sandbox(create_req).await?;
    let created_id = create_resp.into_inner().sandbox_id;

    let start_req = StartSandboxRequest {
        sandbox_id: created_id.clone(),
    };
    client.start_sandbox(start_req).await?;

    if let Some(ref s) = sp {
        s.finish_and_clear();
    }
    if !json {
        style::success(&format!("VM '{}' started", created_id));
        println!();
        style::header("Console Output (Ctrl-C to stop)");
        println!();
    }

    let sandbox_for_cleanup = created_id.clone();
    let socket_for_cleanup = socket.to_string();

    let stream_result = async {
        let mut stream_client = connect_unix(socket).await?;
        let stream_req = StreamConsoleRequest {
            sandbox_id: created_id.clone(),
        };
        let mut stream = stream_client
            .stream_console(stream_req)
            .await?
            .into_inner();

        let input_socket = socket.to_string();
        let input_sandbox_id = created_id.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let stdin_handle = tokio::spawn(async move {
            let mut input_client = match connect_unix(&input_socket).await {
                Ok(c) => c,
                Err(_) => return,
            };
            let stdin = tokio::io::stdin();
            let mut reader = tokio::io::BufReader::new(stdin);
            let mut line = String::new();

            loop {
                line.clear();
                tokio::select! {
                    result = tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line) => {
                        match result {
                            Ok(0) => break,
                            Ok(_) => {
                                let _ = input_client
                                    .send_console_input(ConsoleInputRequest {
                                        sandbox_id: input_sandbox_id.clone(),
                                        data: line.as_bytes().to_vec(),
                                    })
                                    .await;
                            }
                            Err(_) => break,
                        }
                    }
                    _ = &mut stop_rx => break,
                }
            }
        });

        loop {
            tokio::select! {
                msg = stream.message() => {
                    match msg {
                        Ok(Some(output)) => {
                            use std::io::Write;
                            let _ = std::io::stdout().write_all(&output.data);
                            let _ = std::io::stdout().flush();
                        }
                        Ok(None) => break,
                        Err(e) => {
                            style::error(&format!("Stream error: {}", e));
                            break;
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
            }
        }

        let _ = stop_tx.send(());
        stdin_handle.abort();
        Ok::<_, anyhow::Error>(())
    }
    .await;

    if let Err(e) = stream_result {
        style::error(&format!("Console stream failed: {}", e));
    }

    println!();
    let sp = if !json {
        Some(style::spinner("Stopping sandbox..."))
    } else {
        None
    };

    let mut cleanup_client = connect_unix(&socket_for_cleanup).await?;
    let _ = cleanup_client
        .stop_sandbox(StopSandboxRequest {
            sandbox_id: sandbox_for_cleanup.clone(),
            timeout_seconds: 5,
        })
        .await;
    let _ = cleanup_client
        .destroy_sandbox(DestroySandboxRequest {
            sandbox_id: sandbox_for_cleanup.clone(),
        })
        .await;

    if let Some(ref s) = sp {
        s.finish_and_clear();
    }
    if !json {
        style::success(&format!(
            "Sandbox '{}' destroyed.",
            sandbox_for_cleanup
        ));
    }

    Ok(())
}

fn uuid_short() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", ts & 0xFFFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn test_cli_parses_serve() {
        let cli = Cli::try_parse_from(["nova", "serve"]).unwrap();
        match cli.command {
            Commands::Serve { config } => assert!(config.is_none()),
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_parses_serve_with_config() {
        let cli =
            Cli::try_parse_from(["nova", "serve", "--config", "/etc/nova/nova.toml"]).unwrap();
        match cli.command {
            Commands::Serve { config } => {
                assert_eq!(config.unwrap(), "/etc/nova/nova.toml");
            }
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn test_cli_parses_setup() {
        let cli = Cli::try_parse_from(["nova", "setup"]).unwrap();
        match cli.command {
            Commands::Setup { force, list } => {
                assert!(!force);
                assert!(!list);
            }
            _ => panic!("expected Setup command"),
        }
    }

    #[test]
    fn test_cli_parses_setup_force() {
        let cli = Cli::try_parse_from(["nova", "setup", "--force"]).unwrap();
        match cli.command {
            Commands::Setup { force, list } => {
                assert!(force);
                assert!(!list);
            }
            _ => panic!("expected Setup command"),
        }
    }

    #[test]
    fn test_cli_parses_setup_list() {
        let cli = Cli::try_parse_from(["nova", "setup", "--list"]).unwrap();
        match cli.command {
            Commands::Setup { force, list } => {
                assert!(!force);
                assert!(list);
            }
            _ => panic!("expected Setup command"),
        }
    }

    #[test]
    fn test_cli_parses_run() {
        let cli = Cli::try_parse_from(["nova", "run", "nginx:latest"]).unwrap();
        match cli.command {
            Commands::Run {
                image,
                vcpus,
                memory,
                ..
            } => {
                assert_eq!(image, "nginx:latest");
                assert_eq!(vcpus, 1);
                assert_eq!(memory, 128);
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn test_cli_parses_run_with_options() {
        let cli = Cli::try_parse_from([
            "nova",
            "run",
            "alpine:latest",
            "--vcpus",
            "4",
            "--memory",
            "512",
            "--name",
            "my-sandbox",
            "--cmd",
            "python3 -c 'print(1)'",
        ])
        .unwrap();
        match cli.command {
            Commands::Run {
                image,
                vcpus,
                memory,
                name,
                cmd,
            } => {
                assert_eq!(image, "alpine:latest");
                assert_eq!(vcpus, 4);
                assert_eq!(memory, 512);
                assert_eq!(name.unwrap(), "my-sandbox");
                assert!(cmd.is_some());
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn test_cli_parses_ps() {
        let cli = Cli::try_parse_from(["nova", "ps"]).unwrap();
        match cli.command {
            Commands::Ps { all } => assert!(!all),
            _ => panic!("expected Ps command"),
        }

        let cli = Cli::try_parse_from(["nova", "ps", "-a"]).unwrap();
        match cli.command {
            Commands::Ps { all } => assert!(all),
            _ => panic!("expected Ps command"),
        }
    }

    #[test]
    fn test_cli_parses_stop() {
        let cli = Cli::try_parse_from(["nova", "stop", "my-sandbox"]).unwrap();
        match cli.command {
            Commands::Stop { sandbox, timeout } => {
                assert_eq!(sandbox, "my-sandbox");
                assert_eq!(timeout, 10);
            }
            _ => panic!("expected Stop command"),
        }
    }

    #[test]
    fn test_cli_parses_exec() {
        let cli = Cli::try_parse_from(["nova", "exec", "my-sandbox", "ls", "-la"]).unwrap();
        match cli.command {
            Commands::Exec { sandbox, command } => {
                assert_eq!(sandbox, "my-sandbox");
                assert_eq!(command, vec!["ls", "-la"]);
            }
            _ => panic!("expected Exec command"),
        }
    }

    #[test]
    fn test_cli_parses_policy_eval() {
        let cli = Cli::try_parse_from([
            "nova",
            "policy",
            "eval",
            "nova/sandbox/allow",
            "{\"image\":\"nginx\"}",
        ])
        .unwrap();
        match cli.command {
            Commands::Policy {
                action: PolicyCommands::Eval { policy, input },
            } => {
                assert_eq!(policy, "nova/sandbox/allow");
                assert_eq!(input, "{\"image\":\"nginx\"}");
            }
            _ => panic!("expected Policy Eval command"),
        }
    }

    #[test]
    fn test_cli_parses_shell() {
        let cli = Cli::try_parse_from([
            "nova",
            "shell",
            "busybox:latest",
            "--vcpus",
            "2",
            "--memory",
            "512",
        ])
        .unwrap();
        match cli.command {
            Commands::Shell {
                image,
                vcpus,
                memory,
                cmd,
            } => {
                assert_eq!(image, "busybox:latest");
                assert_eq!(vcpus, 2);
                assert_eq!(memory, 512);
                assert!(cmd.is_none());
            }
            _ => panic!("expected Shell command"),
        }
    }

    #[test]
    fn test_cli_verify() {
        Cli::command().debug_assert();
    }

    #[test]
    fn test_uuid_short() {
        let id = uuid_short();
        assert!(!id.is_empty());
        assert!(id.len() <= 8);
    }

    #[test]
    fn test_format_size() {
        assert_eq!(style::format_size(0), "0 B");
        assert_eq!(style::format_size(512), "512 B");
        assert_eq!(style::format_size(1024), "1.0 KiB");
        assert_eq!(style::format_size(1024 * 1024), "1.0 MiB");
        assert_eq!(style::format_size(1024 * 1024 * 1024), "1.0 GiB");
    }

    #[test]
    fn test_has_embedded_assets() {
        // In development builds without assets/, this should be false.
        // In packaged builds, it should be true.
        let _has = embedded::has_embedded_assets();
    }
}
