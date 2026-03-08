//! Real eBPF event source using the aya library.
//!
//! This module is gated behind the `ebpf` feature flag because it requires
//! the `aya` crate and Linux with root privileges to function.
//!
//! It provides [`AyaBpfSource`] which loads compiled eBPF bytecode,
//! attaches it to kernel hooks (tracepoints, kprobes), and polls
//! the perf event array for events to feed into the sensor pipeline.

#[cfg(feature = "ebpf")]
mod inner {
    use std::path::Path;

    use aya::maps::perf::{PerfEventArray, PerfEventArrayBuffer};
    use aya::maps::MapData;
    use aya::programs::{KProbe, TracePoint, UProbe};
    use aya::Ebpf;
    use bytes::BytesMut;

    use nova_eye_common::EventHeader;

    use crate::error::{EyeError, Result};
    use crate::source::SensorSource;

    /// A live eBPF event source backed by the aya loader.
    ///
    /// Each instance owns an [`Ebpf`] handle and perf event buffers
    /// that collect events from the kernel.
    pub struct AyaBpfSource {
        #[allow(dead_code)]
        bpf: Ebpf,
        /// Perf event buffers (one per online CPU).
        bufs: Vec<PerfEventArrayBuffer<MapData>>,
        /// Human-readable name for this source.
        source_name: String,
    }

    /// Helper: load bytecode, set up perf array, return (Ebpf, Vec<PerfEventArrayBuffer>).
    fn load_and_take_perf_array(name: &str, bytecode_path: &str) -> Result<(Ebpf, Vec<PerfEventArrayBuffer<MapData>>)> {
        let path = Path::new(bytecode_path);
        if !path.exists() {
            return Err(EyeError::LoadError {
                name: name.into(),
                reason: format!("bytecode not found: {}", bytecode_path),
            });
        }

        let data = std::fs::read(path).map_err(|e| EyeError::LoadError {
            name: name.into(),
            reason: format!("failed to read bytecode: {}", e),
        })?;

        let mut bpf = Ebpf::load(&data).map_err(|e| EyeError::LoadError {
            name: name.into(),
            reason: format!("aya load failed: {}", e),
        })?;

        let map = bpf.take_map("EVENTS").ok_or_else(|| EyeError::MapError {
            map_name: "EVENTS".into(),
            reason: "perf event array map not found".into(),
        })?;

        let mut perf_array = PerfEventArray::try_from(map).map_err(|e| EyeError::MapError {
            map_name: "EVENTS".into(),
            reason: format!("failed to create PerfEventArray: {}", e),
        })?;

        // Open perf buffers for each online CPU.
        let online_cpus = aya::util::online_cpus().map_err(|e| EyeError::MapError {
            map_name: "EVENTS".into(),
            reason: format!("failed to get online CPUs: {:?}", e),
        })?;
        let mut bufs = Vec::new();
        for cpu_id in online_cpus {
            let buf = perf_array.open(cpu_id, Some(64)).map_err(|e| EyeError::MapError {
                map_name: "EVENTS".into(),
                reason: format!("failed to open perf buffer for CPU {}: {}", cpu_id, e),
            })?;
            bufs.push(buf);
        }

        Ok((bpf, bufs))
    }

    impl AyaBpfSource {
        /// Load the process-exec eBPF program from compiled bytecode on disk
        /// and attach it to the `sched/sched_process_exec` tracepoint.
        pub fn load_process_exec(bytecode_path: &str) -> Result<Self> {
            let (mut bpf, bufs) = load_and_take_perf_array("process_exec", bytecode_path)?;

            let program: &mut TracePoint = bpf
                .program_mut("handle_exec")
                .ok_or_else(|| EyeError::LoadError {
                    name: "process_exec".into(),
                    reason: "program 'handle_exec' not found in ELF".into(),
                })?
                .try_into()
                .map_err(|e| EyeError::LoadError {
                    name: "process_exec".into(),
                    reason: format!("not a tracepoint program: {}", e),
                })?;

            program.load().map_err(|e| EyeError::LoadError {
                name: "process_exec".into(),
                reason: format!("program load failed: {}", e),
            })?;

            program
                .attach("sched", "sched_process_exec")
                .map_err(|e| EyeError::LoadError {
                    name: "process_exec".into(),
                    reason: format!("attach tracepoint failed: {}", e),
                })?;

            Ok(Self {
                bpf,
                bufs,
                source_name: format!("ebpf:process_exec:{}", bytecode_path),
            })
        }

        /// Load the TCP connect eBPF program and attach as kprobe to `tcp_v4_connect`.
        pub fn load_tcp_connect(bytecode_path: &str) -> Result<Self> {
            let (mut bpf, bufs) = load_and_take_perf_array("tcp_connect", bytecode_path)?;

            let program: &mut KProbe = bpf
                .program_mut("handle_tcp_v4_connect")
                .ok_or_else(|| EyeError::LoadError {
                    name: "tcp_connect".into(),
                    reason: "program 'handle_tcp_v4_connect' not found in ELF".into(),
                })?
                .try_into()
                .map_err(|e| EyeError::LoadError {
                    name: "tcp_connect".into(),
                    reason: format!("not a kprobe program: {}", e),
                })?;

            program.load().map_err(|e| EyeError::LoadError {
                name: "tcp_connect".into(),
                reason: format!("program load failed: {}", e),
            })?;

            program
                .attach("tcp_v4_connect", 0)
                .map_err(|e| EyeError::LoadError {
                    name: "tcp_connect".into(),
                    reason: format!("attach kprobe failed: {}", e),
                })?;

            Ok(Self {
                bpf,
                bufs,
                source_name: format!("ebpf:tcp_connect:{}", bytecode_path),
            })
        }

        /// Load the file monitor eBPF program and attach as kprobe to `vfs_open`.
        pub fn load_file_monitor(bytecode_path: &str) -> Result<Self> {
            let (mut bpf, bufs) = load_and_take_perf_array("file_monitor", bytecode_path)?;

            let program: &mut KProbe = bpf
                .program_mut("handle_vfs_open")
                .ok_or_else(|| EyeError::LoadError {
                    name: "file_monitor".into(),
                    reason: "program 'handle_vfs_open' not found in ELF".into(),
                })?
                .try_into()
                .map_err(|e| EyeError::LoadError {
                    name: "file_monitor".into(),
                    reason: format!("not a kprobe program: {}", e),
                })?;

            program.load().map_err(|e| EyeError::LoadError {
                name: "file_monitor".into(),
                reason: format!("program load failed: {}", e),
            })?;

            program
                .attach("vfs_open", 0)
                .map_err(|e| EyeError::LoadError {
                    name: "file_monitor".into(),
                    reason: format!("attach kprobe failed: {}", e),
                })?;

            Ok(Self {
                bpf,
                bufs,
                source_name: format!("ebpf:file_monitor:{}", bytecode_path),
            })
        }

        /// Load the SSL monitor eBPF program and attach as uprobe to `SSL_write`.
        pub fn load_ssl_monitor(bytecode_path: &str, libssl_path: &str) -> Result<Self> {
            let (mut bpf, bufs) = load_and_take_perf_array("ssl_monitor", bytecode_path)?;

            let program: &mut UProbe = bpf
                .program_mut("handle_ssl_write")
                .ok_or_else(|| EyeError::LoadError {
                    name: "ssl_monitor".into(),
                    reason: "program 'handle_ssl_write' not found in ELF".into(),
                })?
                .try_into()
                .map_err(|e| EyeError::LoadError {
                    name: "ssl_monitor".into(),
                    reason: format!("not a uprobe program: {}", e),
                })?;

            program.load().map_err(|e| EyeError::LoadError {
                name: "ssl_monitor".into(),
                reason: format!("program load failed: {}", e),
            })?;

            program
                .attach(Some("SSL_write"), 0, libssl_path, None)
                .map_err(|e| EyeError::LoadError {
                    name: "ssl_monitor".into(),
                    reason: format!("attach uprobe failed: {}", e),
                })?;

            Ok(Self {
                bpf,
                bufs,
                source_name: format!("ebpf:ssl_monitor:{}", bytecode_path),
            })
        }
    }

    impl SensorSource for AyaBpfSource {
        fn poll_events(&mut self) -> Result<Vec<(EventHeader, Vec<u8>)>> {
            let mut events = Vec::new();
            let header_size = core::mem::size_of::<EventHeader>();

            // Poll all perf event buffers (one per CPU).
            for buf in &mut self.bufs {
                // Allocate read buffers for this poll cycle.
                let mut out_bufs: Vec<BytesMut> = (0..64)
                    .map(|_| BytesMut::with_capacity(512))
                    .collect();

                match buf.read_events(&mut out_bufs) {
                    Ok(ev) => {
                        for i in 0..ev.read {
                            let data = &out_bufs[i];
                            if data.len() >= header_size {
                                let header: EventHeader = unsafe {
                                    core::ptr::read_unaligned(data.as_ptr() as *const EventHeader)
                                };
                                events.push((header, data.to_vec()));
                            }
                        }
                    }
                    Err(_) => {} // Buffer not ready or no events.
                }
            }

            Ok(events)
        }

        fn name(&self) -> &str {
            &self.source_name
        }
    }
}

#[cfg(feature = "ebpf")]
pub use inner::AyaBpfSource;
