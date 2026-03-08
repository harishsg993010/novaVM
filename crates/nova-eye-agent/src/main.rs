//! nova-eye-agent — lightweight eBPF telemetry agent for guest VMs.
//!
//! Runs inside the guest, loads eBPF programs, polls perf event arrays,
//! and sends events to the host via UDP over the TAP interface.
//!
//! Uses PerfEventArray (available since kernel 4.3) instead of RingBuf
//! (which requires kernel 5.8+) for maximum kernel compatibility.
//!
//! Build: `cargo build -p nova-eye-agent --target x86_64-unknown-linux-musl --release`

use std::net::UdpSocket;
use std::path::Path;
use std::time::Duration;

use aya::maps::perf::{PerfEventArray, PerfEventArrayBuffer};
use aya::maps::MapData;
use aya::programs::{KProbe, TracePoint};
use aya::Ebpf;
use bytes::BytesMut;
use nova_eye_common::EventHeader;
use serde::Deserialize;

/// Probe configuration injected as /etc/nova/probes.json.
#[derive(Deserialize)]
struct ProbeConfig {
    hook_type: String,
    target: String,
    bytecode: Option<String>,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Host gateway address (standard NovaVM TAP config).
const HOST_GATEWAY: &str = "172.16.0.1";
/// Default UDP event port.
const DEFAULT_EVENT_PORT: u16 = 9876;
/// Poll interval between perf event drains.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// eBPF bytecode directory inside the guest.
const EBPF_DIR: &str = "/opt/nova/ebpf";
/// Probe config path inside the guest.
const PROBES_CONFIG: &str = "/etc/nova/probes.json";

/// Holds a loaded eBPF program + its perf event buffers.
struct LoadedProbe {
    _bpf: Ebpf,
    bufs: Vec<PerfEventArrayBuffer<MapData>>,
}

fn main() {
    eprintln!("[nova-eye-agent] ===== STARTING =====");

    // Raise RLIMIT_MEMLOCK to unlimited — required on kernels < 5.11 for
    // BPF map creation (maps are charged against RLIMIT_MEMLOCK).
    unsafe {
        let rlim = libc::rlimit {
            rlim_cur: libc::RLIM_INFINITY,
            rlim_max: libc::RLIM_INFINITY,
        };
        let ret = libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim);
        eprintln!("[nova-eye-agent] setrlimit(MEMLOCK, INFINITY) = {}", ret);
    }

    // Log kernel version for debugging.
    log_kernel_info();
    log_ebpf_caps();

    // Read probe config.
    let probes: Vec<ProbeConfig> = match std::fs::read_to_string(PROBES_CONFIG) {
        Ok(json) => {
            eprintln!("[nova-eye-agent] loaded probe config from {}", PROBES_CONFIG);
            match serde_json::from_str(&json) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[nova-eye-agent] failed to parse {}: {}", PROBES_CONFIG, e);
                    Vec::new()
                }
            }
        }
        Err(e) => {
            eprintln!("[nova-eye-agent] no probe config at {}: {}", PROBES_CONFIG, e);
            Vec::new()
        }
    };

    eprintln!("[nova-eye-agent] {} probes configured", probes.len());

    // Open UDP socket to host.
    let dest = format!("{}:{}", HOST_GATEWAY, DEFAULT_EVENT_PORT);
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[nova-eye-agent] failed to bind UDP socket: {}", e);
            return;
        }
    };
    eprintln!("[nova-eye-agent] UDP socket bound, target={}", dest);

    // Send hello message.
    let hello = b"NOVA-EYE-HELLO";
    let _ = socket.send_to(hello, &dest);
    eprintln!("[nova-eye-agent] sent hello packet to host");

    // Load and attach each probe.
    let mut loaded_probes: Vec<LoadedProbe> = Vec::new();
    let mut loaded_count = 0u32;
    let mut failed_count = 0u32;

    for probe in &probes {
        if !probe.enabled {
            eprintln!("[nova-eye-agent] probe {} {} SKIPPED (disabled)", probe.hook_type, probe.target);
            continue;
        }
        let bytecode_name = match &probe.bytecode {
            Some(name) => name,
            None => {
                eprintln!("[nova-eye-agent] probe {} {} SKIPPED (no bytecode)", probe.hook_type, probe.target);
                continue;
            }
        };
        let bytecode_path = format!("{}/{}", EBPF_DIR, bytecode_name);

        if !Path::new(&bytecode_path).exists() {
            eprintln!("[nova-eye-agent] MISS bytecode not found: {}", bytecode_path);
            failed_count += 1;
            continue;
        }

        eprintln!("[nova-eye-agent] loading {} {} from {}", probe.hook_type, probe.target, bytecode_path);

        match load_probe(&probe.hook_type, &probe.target, &bytecode_path) {
            Ok(loaded) => {
                eprintln!(
                    "[nova-eye-agent] OK loaded {} {} -> {} (cpus={})",
                    probe.hook_type, probe.target, bytecode_name, loaded.bufs.len()
                );
                loaded_probes.push(loaded);
                loaded_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "[nova-eye-agent] FAIL {} {} : {}",
                    probe.hook_type, probe.target, e
                );
                failed_count += 1;
            }
        }
    }

    eprintln!("[nova-eye-agent] ===== SUMMARY =====");
    eprintln!("[nova-eye-agent] loaded={} failed={}", loaded_count, failed_count);

    if loaded_probes.is_empty() {
        eprintln!("[nova-eye-agent] NO eBPF probes active — entering idle");
        let status = b"NOVA-EYE-NO-PROBES";
        let _ = socket.send_to(status, &dest);
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    eprintln!("[nova-eye-agent] eBPF ACTIVE — entering poll loop");
    let status = format!("NOVA-EYE-ACTIVE:{}", loaded_count);
    let _ = socket.send_to(status.as_bytes(), &dest);

    // Poll loop: drain perf event buffers, send events via UDP.
    // NOTE: No eprintln! in the loop — serial console writes block the agent.
    let header_size = core::mem::size_of::<EventHeader>();
    let mut total_events: u64 = 0;
    let mut total_polls: u64 = 0;
    let mut total_errors: u64 = 0;
    let mut last_heartbeat = std::time::Instant::now();

    // Pre-allocate output buffers (reused each poll).
    let mut out_bufs: Vec<BytesMut> = (0..32)
        .map(|_| BytesMut::with_capacity(512))
        .collect();

    loop {
        for loaded in &mut loaded_probes {
            for buf in &mut loaded.bufs {
                // Reset buffers for reuse.
                for b in out_bufs.iter_mut() {
                    b.clear();
                }

                match buf.read_events(&mut out_bufs) {
                    Ok(ev) => {
                        for i in 0..ev.read {
                            let data = &out_bufs[i];
                            if data.len() >= header_size {
                                let _ = socket.send_to(data, &dest);
                                total_events += 1;
                            }
                        }
                    }
                    Err(_) => {
                        total_errors += 1;
                    }
                }
            }
        }
        total_polls += 1;

        // Unconditional heartbeat every 5 seconds via UDP.
        if last_heartbeat.elapsed() >= Duration::from_secs(5) {
            let heartbeat = format!(
                "NOVA-EYE-HB:polls={},events={},errors={}",
                total_polls, total_events, total_errors
            );
            let _ = socket.send_to(heartbeat.as_bytes(), &dest);
            last_heartbeat = std::time::Instant::now();
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Log kernel version.
fn log_kernel_info() {
    if let Ok(ver) = std::fs::read_to_string("/proc/version") {
        eprintln!("[nova-eye-agent] kernel: {}", ver.trim());
    } else {
        eprintln!("[nova-eye-agent] kernel: unknown (no /proc/version)");
    }
    if let Ok(rel) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        eprintln!("[nova-eye-agent] release: {}", rel.trim());
    }
}

/// Log eBPF-related kernel capabilities.
fn log_ebpf_caps() {
    let tracefs = Path::new("/sys/kernel/tracing");
    let debugfs_tracing = Path::new("/sys/kernel/debug/tracing");
    eprintln!(
        "[nova-eye-agent] tracefs: {} | debugfs/tracing: {}",
        if tracefs.exists() { "YES" } else { "NO" },
        if debugfs_tracing.exists() { "YES" } else { "NO" },
    );

    let bpffs = Path::new("/sys/fs/bpf");
    eprintln!("[nova-eye-agent] bpffs: {}", if bpffs.exists() { "YES" } else { "NO" });

    let kprobes = Path::new("/sys/kernel/debug/kprobes");
    eprintln!("[nova-eye-agent] kprobes dir: {}", if kprobes.exists() { "YES" } else { "NO" });

    if let Ok(entries) = std::fs::read_dir(EBPF_DIR) {
        let files: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        eprintln!("[nova-eye-agent] bytecode files in {}: {:?}", EBPF_DIR, files);
    } else {
        eprintln!("[nova-eye-agent] bytecode dir {} not found", EBPF_DIR);
    }
}

/// Load an eBPF program and return the loaded probe with perf event buffers.
fn load_probe(
    hook_type: &str,
    target: &str,
    bytecode_path: &str,
) -> Result<LoadedProbe, String> {
    let data = std::fs::read(bytecode_path).map_err(|e| format!("read: {}", e))?;
    eprintln!("[nova-eye-agent]   bytecode size: {} bytes", data.len());

    let mut bpf = Ebpf::load(&data).map_err(|e| format!("bpf_load: {}", e))?;
    eprintln!("[nova-eye-agent]   bpf loaded OK");

    match hook_type {
        "tracepoint" => {
            let parts: Vec<&str> = target.splitn(2, '/').collect();
            if parts.len() != 2 {
                return Err(format!("invalid tracepoint target: {}", target));
            }
            let prog_name = find_program_name(&bpf, "tracepoint");
            eprintln!("[nova-eye-agent]   using program name: {}", prog_name);
            let program: &mut TracePoint = bpf
                .program_mut(&prog_name)
                .ok_or_else(|| format!("program '{}' not found", prog_name))?
                .try_into()
                .map_err(|e| format!("not a tracepoint: {}", e))?;
            program.load().map_err(|e| format!("prog_load: {:?}", e))?;
            eprintln!("[nova-eye-agent]   program loaded into kernel");
            program
                .attach(parts[0], parts[1])
                .map_err(|e| format!("attach({}/{}): {:?}", parts[0], parts[1], e))?;
            eprintln!("[nova-eye-agent]   attached to {}/{}", parts[0], parts[1]);
        }
        "kprobe" => {
            let prog_name = find_program_name(&bpf, "kprobe");
            eprintln!("[nova-eye-agent]   using program name: {}", prog_name);
            let program: &mut KProbe = bpf
                .program_mut(&prog_name)
                .ok_or_else(|| format!("program '{}' not found", prog_name))?
                .try_into()
                .map_err(|e| format!("not a kprobe: {:?}", e))?;
            program.load().map_err(|e| format!("prog_load: {:?}", e))?;
            eprintln!("[nova-eye-agent]   program loaded into kernel");
            program
                .attach(target, 0)
                .map_err(|e| format!("attach({}): {}", target, e))?;
            eprintln!("[nova-eye-agent]   attached to {}", target);
        }
        _ => return Err(format!("unsupported hook type: {}", hook_type)),
    }

    // Take the EVENTS perf event array and open per-CPU buffers.
    let map = bpf
        .take_map("EVENTS")
        .ok_or_else(|| "EVENTS map not found".to_string())?;
    let mut perf_array =
        PerfEventArray::try_from(map).map_err(|e| format!("perf_array: {}", e))?;
    eprintln!("[nova-eye-agent]   perf event array OK");

    let online_cpus = aya::util::online_cpus().map_err(|e| format!("online_cpus: {:?}", e))?;
    let mut bufs = Vec::new();
    for cpu_id in online_cpus {
        let buf = perf_array
            .open(cpu_id, Some(32))
            .map_err(|e| format!("open perf buf CPU {}: {}", cpu_id, e))?;
        bufs.push(buf);
    }
    eprintln!("[nova-eye-agent]   opened {} perf buffers", bufs.len());

    Ok(LoadedProbe { _bpf: bpf, bufs })
}

/// Find the first program name matching a given type hint.
fn find_program_name(bpf: &Ebpf, _type_hint: &str) -> String {
    for name in &[
        "handle_exec",
        "handle_vfs_open",
        "handle_tcp_v4_connect",
        "handle_ssl_write",
        "handle_ssl_read",
    ] {
        if bpf.program(name).is_some() {
            return name.to_string();
        }
    }
    "unknown".to_string()
}
