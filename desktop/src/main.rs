use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

const DEFAULT_API: &str = "http://127.0.0.1:9800";
const DEFAULT_HOST: &str = "127.0.0.1:9800";
const DEFAULT_CONFIG: &str = "/etc/nova/nova.toml";

// ── CLI ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "nova", about = "NovaVM Desktop — manage NovaVM sandboxes via WSL")]
struct Cli {
    /// REST API base URL
    #[arg(long, default_value = DEFAULT_API, global = true)]
    api: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the NovaVM daemon inside WSL
    Start {
        /// Config file path (inside WSL)
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: String,
    },
    /// Stop the NovaVM daemon
    Stop,
    /// Show daemon + WSL status
    Status,
    /// Create and start a sandbox
    Run {
        /// OCI image reference
        image: String,
        /// Sandbox name
        #[arg(long)]
        name: Option<String>,
        /// Number of vCPUs
        #[arg(long, default_value = "1")]
        vcpus: u32,
        /// Memory in MiB
        #[arg(long, default_value = "256")]
        memory: u32,
        /// Override entrypoint command
        #[arg(long)]
        cmd: Option<String>,
    },
    /// List running sandboxes
    Ps,
    /// Execute a command inside a sandbox
    Exec {
        /// Sandbox ID
        sandbox: String,
        /// Command to run
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Stop a sandbox
    #[command(name = "stop-sandbox")]
    StopSandbox {
        /// Sandbox ID
        sandbox: String,
    },
    /// Remove a sandbox
    Rm {
        /// Sandbox ID
        sandbox: String,
    },
    /// Pull an OCI image
    Pull {
        /// Image reference
        image: String,
    },
    /// List cached images
    Images,
    /// View sandbox console output
    Logs {
        /// Sandbox ID
        sandbox: String,
    },
    /// Tail eBPF events from events.jsonl (via WSL)
    Events {
        /// Number of lines
        #[arg(short, default_value = "20")]
        n: usize,
        /// Follow (tail -f)
        #[arg(short)]
        f: bool,
    },
    /// Show sandbox or image details
    Inspect {
        /// Sandbox ID or image name
        target: String,
    },
    /// Check prerequisites and set up WSL environment
    Setup,
}

// ── API types ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct CreateSandbox {
    sandbox_id: String,
    image: String,
    vcpus: u32,
    memory: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
}

#[derive(Deserialize)]
struct ExecResult {
    #[serde(default)]
    exit_code: i32,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
}

#[derive(Serialize)]
struct ExecRequest {
    command: String,
}

// ── Minimal HTTP client (no TLS needed — localhost only) ───────────

fn parse_host(api: &str) -> &str {
    let s = api.strip_prefix("http://").unwrap_or(api);
    s.split('/').next().unwrap_or(DEFAULT_HOST)
}

fn http_request(
    host: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    let mut stream = TcpStream::connect(host)
        .map_err(|e| format!("connection failed ({}): {}", host, e))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok();

    let hostname = host.split(':').next().unwrap_or("localhost");

    let req = if let Some(b) = body {
        format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            method, path, hostname, b.len(), b
        )
    } else {
        format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            method, path, hostname
        )
    };

    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write failed: {}", e))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {}", e))?;

    let response = String::from_utf8_lossy(&buf);

    // Parse status code
    let status_line = response.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Find body (after \r\n\r\n)
    let body_str = if let Some(pos) = response.find("\r\n\r\n") {
        let raw = &response[pos + 4..];
        // Handle chunked transfer encoding
        if response.contains("Transfer-Encoding: chunked") {
            decode_chunked(raw)
        } else {
            raw.to_string()
        }
    } else {
        String::new()
    };

    Ok((status, body_str))
}

fn decode_chunked(raw: &str) -> String {
    let mut result = String::new();
    let mut remaining = raw;
    loop {
        let remaining_trimmed = remaining.trim_start_matches("\r\n");
        if remaining_trimmed.is_empty() {
            break;
        }
        let line_end = remaining_trimmed.find("\r\n").unwrap_or(remaining_trimmed.len());
        let size_str = &remaining_trimmed[..line_end];
        let size = usize::from_str_radix(size_str.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let data_start = line_end + 2;
        if data_start + size <= remaining_trimmed.len() {
            result.push_str(&remaining_trimmed[data_start..data_start + size]);
            remaining = &remaining_trimmed[data_start + size..];
        } else {
            // Partial chunk — take what we have
            result.push_str(&remaining_trimmed[data_start..]);
            break;
        }
    }
    result
}

fn api_get(api: &str, path: &str) -> Result<serde_json::Value, String> {
    let host = parse_host(api);
    let (status, body) = http_request(host, "GET", path, None)?;
    if status >= 400 {
        return Err(format!("GET {} -> {} {}", path, status, body.trim()));
    }
    if body.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {} (body: {})", e, &body[..body.len().min(200)]))
}

fn api_post_json(api: &str, path: &str, body: &impl Serialize) -> Result<serde_json::Value, String> {
    let host = parse_host(api);
    let json = serde_json::to_string(body).map_err(|e| format!("serialize: {}", e))?;
    let (status, resp) = http_request(host, "POST", path, Some(&json))?;
    if status >= 400 {
        return Err(format!("POST {} -> {} {}", path, status, resp.trim()));
    }
    if resp.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&resp).map_err(|e| format!("JSON parse error: {}", e))
}

fn api_post_empty(api: &str, path: &str) -> Result<serde_json::Value, String> {
    let host = parse_host(api);
    let (status, body) = http_request(host, "POST", path, None)?;
    if status >= 400 {
        return Err(format!("POST {} -> {} {}", path, status, body.trim()));
    }
    if body.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))
}

fn api_delete(api: &str, path: &str) -> Result<serde_json::Value, String> {
    let host = parse_host(api);
    let (status, body) = http_request(host, "DELETE", path, None)?;
    if status >= 400 {
        return Err(format!("DELETE {} -> {} {}", path, status, body.trim()));
    }
    if body.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {}", e))
}

fn daemon_healthy(api: &str) -> bool {
    let host = parse_host(api);
    // Use connect (not connect_timeout with SocketAddr) to support hostname resolution
    let stream = TcpStream::connect(host);
    match stream {
        Ok(mut s) => {
            s.set_read_timeout(Some(Duration::from_secs(2))).ok();
            s.set_write_timeout(Some(Duration::from_secs(2))).ok();
            let req = "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
            if s.write_all(req.as_bytes()).is_err() {
                return false;
            }
            let mut buf = [0u8; 512];
            match s.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let resp = String::from_utf8_lossy(&buf[..n]);
                    resp.contains("200") || resp.contains("ok")
                }
                _ => false,
            }
        }
        Err(_) => false,
    }
}

// ── WSL helpers ────────────────────────────────────────────────────

fn wsl(cmd: &str) -> Result<String, String> {
    let out = Command::new("wsl")
        .args(["-e", "bash", "-c", cmd])
        .output()
        .map_err(|e| format!("wsl exec failed: {}", e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

fn wsl_sudo(cmd: &str) -> Result<String, String> {
    // Try passwordless sudo first, fall back to prompting for password
    let result = wsl(&format!("sudo -n bash -c '{}'", cmd.replace('\'', "'\\''")));
    if result.is_ok() {
        return result;
    }
    // If passwordless sudo fails, prompt user for password
    eprint!("[sudo] password: ");
    let mut password = String::new();
    std::io::stdin()
        .read_line(&mut password)
        .map_err(|e| format!("read password: {}", e))?;
    let password = password.trim();
    let full_cmd = format!(
        "echo '{}' | sudo -S bash -c '{}' 2>/dev/null",
        password.replace('\'', "'\\''"),
        cmd.replace('\'', "'\\''")
    );
    wsl(&full_cmd)
}

// ── Output helpers ─────────────────────────────────────────────────

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    if rows.is_empty() {
        println!("(none)");
        return;
    }
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < cols {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }
    let header: String = headers
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{:width$}", h, width = widths[i]))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{}", header);
    println!(
        "{}",
        widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("  ")
    );
    for row in rows {
        let line: String = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let w = if i < cols { widths[i] } else { cell.len() };
                format!("{:width$}", cell, width = w)
            })
            .collect::<Vec<_>>()
            .join("  ");
        println!("{}", line);
    }
}

fn short_id(id: &str) -> &str {
    if id.len() > 12 { &id[..12] } else { id }
}

fn rand_id() -> String {
    use std::time::SystemTime;
    let n = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(42);
    format!("sb-{:x}", n & 0xFFFF_FFFF)
}

// ── Commands ───────────────────────────────────────────────────────

fn cmd_start(api: &str, config: &str) -> Result<(), String> {
    if daemon_healthy(api) {
        println!("[+] Daemon is already running");
        return Ok(());
    }

    println!("[*] Starting NovaVM daemon in WSL...");

    wsl_sudo("mkdir -p /run/nova /var/run/nova /var/lib/nova/images")?;

    // Create TAP if needed
    if wsl("ip link show tap0 2>/dev/null").is_err() {
        println!("[*] Creating TAP device...");
        wsl_sudo("ip tuntap add dev tap0 mode tap && ip addr add 172.16.0.1/24 dev tap0 && ip link set tap0 up")?;
        println!("    tap0 UP (172.16.0.1/24)");
    }

    let cmd = format!(
        "RUST_LOG=info nova serve --config {} > /tmp/nova-daemon.log 2>&1 &",
        config
    );
    wsl_sudo(&cmd)?;

    print!("[*] Waiting for daemon");
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(500));
        print!(".");
        if daemon_healthy(api) {
            println!(" ready!");
            println!("[+] NovaVM daemon running on {}", api);
            return Ok(());
        }
    }
    println!(" timeout!");

    if let Ok(log) = wsl("tail -10 /tmp/nova-daemon.log 2>/dev/null") {
        eprintln!("\nDaemon log:\n{}", log);
    }
    Err("Daemon failed to start within 10s".into())
}

fn cmd_stop(api: &str) -> Result<(), String> {
    println!("[*] Stopping NovaVM daemon...");
    wsl_sudo("pkill -f 'nova serve' 2>/dev/null || true")?;
    std::thread::sleep(Duration::from_millis(500));

    if daemon_healthy(api) {
        Err("Daemon is still running".into())
    } else {
        println!("[+] Daemon stopped");
        Ok(())
    }
}

fn cmd_status(api: &str) -> Result<(), String> {
    let wsl_ok = Command::new("wsl")
        .args(["-e", "echo", "ok"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    println!("WSL:        {}", if wsl_ok { "running" } else { "not running" });

    let healthy = daemon_healthy(api);
    println!("Daemon:     {}", if healthy { "running" } else { "stopped" });
    println!("API:        {}", api);

    if healthy {
        if let Ok(val) = api_get(api, "/api/v1/sandboxes") {
            if let Some(arr) = val.as_array() {
                let running = arr
                    .iter()
                    .filter(|s| s["state"].as_str() == Some("running"))
                    .count();
                println!("Sandboxes:  {} total, {} running", arr.len(), running);
            }
        }
    }

    if let Ok(uname) = wsl("uname -r 2>/dev/null") {
        println!("WSL kernel: {}", uname);
    }
    if let Ok(du) = wsl("du -sh /var/lib/nova/images 2>/dev/null | cut -f1") {
        println!("Cache:      {}", du);
    }

    Ok(())
}

fn cmd_run(
    api: &str,
    image: &str,
    name: Option<&str>,
    vcpus: u32,
    memory: u32,
    cmd: Option<&str>,
) -> Result<(), String> {
    if !daemon_healthy(api) {
        return Err("Daemon is not running. Start it with: nova start".into());
    }

    let sandbox_id = name.map(|s| s.to_string()).unwrap_or_else(rand_id);
    println!("[*] Creating sandbox '{}' with {}...", sandbox_id, image);

    let body = CreateSandbox {
        sandbox_id: sandbox_id.clone(),
        image: image.to_string(),
        vcpus,
        memory,
        command: cmd.map(|s| s.to_string()),
    };

    api_post_json(api, "/api/v1/sandboxes", &body)?;
    println!("[+] Sandbox '{}' running", sandbox_id);
    Ok(())
}

fn cmd_ps(api: &str) -> Result<(), String> {
    let val = api_get(api, "/api/v1/sandboxes")?;
    let arr = val.as_array().ok_or("expected array")?;

    let rows: Vec<Vec<String>> = arr
        .iter()
        .map(|s| {
            vec![
                s["sandbox_id"].as_str().unwrap_or("-").to_string(),
                s["image"].as_str().unwrap_or("-").to_string(),
                s["state"].as_str().unwrap_or("-").to_string(),
                s["created_at"].as_str().unwrap_or("-").to_string(),
            ]
        })
        .collect();

    print_table(&["ID", "IMAGE", "STATE", "CREATED"], &rows);
    Ok(())
}

fn cmd_exec(api: &str, sandbox: &str, command: &[String]) -> Result<(), String> {
    let cmd_str = command.join(" ");
    let body = ExecRequest { command: cmd_str };
    let path = format!("/api/v1/sandboxes/{}/exec", sandbox);
    let val = api_post_json(api, &path, &body)?;

    let result: ExecResult =
        serde_json::from_value(val).map_err(|e| format!("parse error: {}", e))?;

    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
        if !result.stdout.ends_with('\n') {
            println!();
        }
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
    }
    if result.exit_code != 0 {
        std::process::exit(result.exit_code);
    }
    Ok(())
}

fn cmd_stop_sandbox(api: &str, sandbox: &str) -> Result<(), String> {
    let path = format!("/api/v1/sandboxes/{}/stop", sandbox);
    api_post_empty(api, &path)?;
    println!("[+] Sandbox '{}' stopped", sandbox);
    Ok(())
}

fn cmd_rm(api: &str, sandbox: &str) -> Result<(), String> {
    let path = format!("/api/v1/sandboxes/{}", sandbox);
    api_delete(api, &path)?;
    println!("[+] Sandbox '{}' removed", sandbox);
    Ok(())
}

fn cmd_pull(api: &str, image: &str) -> Result<(), String> {
    println!("[*] Pulling {}...", image);

    #[derive(Serialize)]
    struct PullReq { image: String }

    let body = PullReq { image: image.to_string() };
    let val = api_post_json(api, "/api/v1/images/pull", &body)?;
    let digest = val["digest"].as_str().unwrap_or("unknown");
    println!("[+] Pulled {} ({})", image, short_id(digest));
    Ok(())
}

fn cmd_images(api: &str) -> Result<(), String> {
    let val = api_get(api, "/api/v1/images")?;
    let arr = val.as_array().ok_or("expected array")?;

    let rows: Vec<Vec<String>> = arr
        .iter()
        .map(|img| {
            vec![
                img["name"].as_str().unwrap_or("-").to_string(),
                short_id(img["digest"].as_str().unwrap_or("-")).to_string(),
                img["size"].as_str().unwrap_or("-").to_string(),
            ]
        })
        .collect();

    print_table(&["IMAGE", "DIGEST", "SIZE"], &rows);
    Ok(())
}

fn cmd_logs(api: &str, sandbox: &str) -> Result<(), String> {
    let path = format!("/api/v1/sandboxes/{}/logs", sandbox);
    let val = api_get(api, &path)?;
    if let Some(logs) = val["logs"].as_str() {
        print!("{}", logs);
    } else {
        println!("{}", serde_json::to_string_pretty(&val).unwrap_or_default());
    }
    Ok(())
}

fn cmd_events(n: usize, follow: bool) -> Result<(), String> {
    // No sudo — events.jsonl is readable. Use tail directly.
    let cmd = if follow {
        "tail -f /var/run/nova/events.jsonl 2>/dev/null".to_string()
    } else {
        format!("tail -n {} /var/run/nova/events.jsonl 2>/dev/null", n)
    };

    if follow {
        println!("[*] Streaming events (Ctrl+C to stop)...\n");
        let mut child = Command::new("wsl")
            .args(["-e", "bash", "-c", &cmd])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("wsl exec failed: {}", e))?;

        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => print_event_line(&l),
                    Err(_) => break,
                }
            }
        }
        let _ = child.kill();
    } else {
        let output = wsl(&cmd)?;
        for line in output.lines() {
            print_event_line(line);
        }
    }
    Ok(())
}

fn print_event_line(line: &str) {
    if line.trim().is_empty() {
        return;
    }
    if let Ok(ev) = serde_json::from_str::<serde_json::Value>(line) {
        let etype = ev["event_type"].as_str().unwrap_or("?");
        let comm = ev["comm"].as_str().unwrap_or("?");
        let pid = ev["pid"].as_u64().unwrap_or(0);
        let sandbox = ev["sandbox_id"].as_str();

        let prefix = match sandbox {
            Some(id) => format!("[guest:{}]", short_id(id)),
            None => "[host]".to_string(),
        };

        println!("{} {:16} pid={:<6} comm={}", prefix, etype, pid, comm);
    } else {
        println!("{}", line);
    }
}

fn cmd_inspect(api: &str, target: &str) -> Result<(), String> {
    // Try sandbox first
    let path = format!("/api/v1/sandboxes/{}", target);
    if let Ok(val) = api_get(api, &path) {
        if val.get("sandbox_id").is_some() {
            println!("{}", serde_json::to_string_pretty(&val).unwrap());
            return Ok(());
        }
    }

    // Try image
    let path = format!("/api/v1/images/{}", target);
    match api_get(api, &path) {
        Ok(val) => {
            println!("{}", serde_json::to_string_pretty(&val).unwrap());
            Ok(())
        }
        Err(_) => Err(format!("'{}' not found as sandbox or image", target)),
    }
}

fn cmd_setup() -> Result<(), String> {
    println!("[*] Setting up NovaVM in WSL...\n");

    print!("  Checking WSL... ");
    wsl("echo ok").map_err(|_| "WSL is not available. Install WSL2 first.".to_string())?;
    println!("ok");

    print!("  Checking KVM... ");
    match wsl("ls /dev/kvm 2>/dev/null") {
        Ok(_) => println!("ok"),
        Err(_) => {
            println!("NOT AVAILABLE");
            println!("\n  Enable nested virtualization in PowerShell (admin):");
            println!("    Set-VMProcessor -VMName WSL -ExposeVirtualizationExtensions $true");
            println!("    wsl --shutdown");
            return Err("KVM not available".into());
        }
    }

    print!("  Checking nova binary... ");
    match wsl("which nova 2>/dev/null || ls /usr/local/bin/nova 2>/dev/null") {
        Ok(path) => println!("{}", path),
        Err(_) => {
            println!("NOT FOUND");
            println!("\n  Build and install the nova binary in WSL first.");
            return Err("nova binary not installed in WSL".into());
        }
    }

    print!("  Checking kernel... ");
    match wsl("ls /opt/nova/vmlinux 2>/dev/null") {
        Ok(_) => println!("/opt/nova/vmlinux"),
        Err(_) => {
            println!("not found — run: wsl -e sudo nova setup");
            return Err("kernel not installed".into());
        }
    }

    print!("  Creating directories... ");
    wsl_sudo("mkdir -p /run/nova /var/run/nova /var/lib/nova/images /etc/nova /var/lib/nova/policy/bundles")?;
    println!("ok");

    print!("  Checking config... ");
    match wsl("test -f /etc/nova/nova.toml && echo exists") {
        Ok(s) if s.contains("exists") => println!("/etc/nova/nova.toml"),
        _ => {
            print!("creating... ");
            wsl_sudo("nova setup 2>/dev/null || echo 'run: wsl -e sudo nova setup'")?;
            println!("ok");
        }
    }

    print!("  Checking TAP device... ");
    match wsl("ip link show tap0 2>/dev/null") {
        Ok(_) => println!("tap0 exists"),
        Err(_) => {
            print!("creating... ");
            wsl_sudo("ip tuntap add dev tap0 mode tap && ip addr add 172.16.0.1/24 dev tap0 && ip link set tap0 up")?;
            println!("tap0 UP");
        }
    }

    println!("\n[+] Setup complete!");
    println!("    Start daemon:  nova start");
    println!("    Run sandbox:   nova run nginx:alpine --name web");
    println!("    List running:  nova ps");
    Ok(())
}

// ── Main ───────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let api = &cli.api;

    let result = match &cli.cmd {
        Cmd::Start { config } => cmd_start(api, config),
        Cmd::Stop => cmd_stop(api),
        Cmd::Status => cmd_status(api),
        Cmd::Run { image, name, vcpus, memory, cmd } => {
            cmd_run(api, image, name.as_deref(), *vcpus, *memory, cmd.as_deref())
        }
        Cmd::Ps => cmd_ps(api),
        Cmd::Exec { sandbox, command } => cmd_exec(api, sandbox, command),
        Cmd::StopSandbox { sandbox } => cmd_stop_sandbox(api, sandbox),
        Cmd::Rm { sandbox } => cmd_rm(api, sandbox),
        Cmd::Pull { image } => cmd_pull(api, image),
        Cmd::Images => cmd_images(api),
        Cmd::Logs { sandbox } => cmd_logs(api, sandbox),
        Cmd::Events { n, f } => cmd_events(*n, *f),
        Cmd::Inspect { target } => cmd_inspect(api, target),
        Cmd::Setup => cmd_setup(),
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}
