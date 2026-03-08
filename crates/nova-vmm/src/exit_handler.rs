//! vCPU exit handler — the main run loop.

use std::sync::{Arc, Condvar, Mutex, Once};
use std::time::{Duration, Instant};

use nova_kvm::kvm_bindings::KvmRun;
use nova_kvm::vcpu::{VcpuFd, VmExit};

use crate::device_mgr::MmioBus;

// ---------------------------------------------------------------------------
// Signal-based vCPU timeout (Firecracker pattern)
// ---------------------------------------------------------------------------

/// Install a no-op handler for SIGUSR1 so it can interrupt `ioctl(KVM_RUN)`
/// without terminating the process.
fn install_vcpu_signal_handler() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        unsafe {
            libc::signal(libc::SIGUSR1, noop_signal_handler as libc::sighandler_t);
        }
    });
}

extern "C" fn noop_signal_handler(_sig: libc::c_int) {
    // Empty — the signal delivery itself interrupts the blocked ioctl.
}

/// RAII watchdog that interrupts a vCPU after a timeout.
///
/// Spawns a background thread that waits for the timeout duration, then sets
/// `kvm_run->immediate_exit = 1` and sends SIGUSR1 to the vCPU thread to
/// break out of the blocked `ioctl(KVM_RUN)`.
///
/// On drop, signals the watchdog thread to exit immediately and joins it.
struct VcpuWatchdog {
    done: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl VcpuWatchdog {
    /// Create a watchdog for `vcpu` that fires after `timeout`.
    ///
    /// Must be called from the thread that will run `vcpu.run()`.
    fn new(vcpu: &VcpuFd, timeout: Duration) -> Self {
        install_vcpu_signal_handler();

        let kvm_run_addr = unsafe { vcpu.kvm_run_ptr() } as usize;
        let vcpu_tid = unsafe { libc::pthread_self() };
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let done_clone = done.clone();

        let handle = std::thread::spawn(move || {
            let (lock, cvar) = &*done_clone;
            let mut finished = lock.lock().unwrap();
            let result = cvar.wait_timeout(finished, timeout).unwrap();
            finished = result.0;
            if !*finished {
                // Timeout expired — interrupt the vCPU.
                unsafe {
                    (*(kvm_run_addr as *mut KvmRun)).immediate_exit = 1;
                    libc::pthread_kill(vcpu_tid, libc::SIGUSR1);
                }
            }
        });

        Self {
            done,
            handle: Some(handle),
        }
    }
}

impl Drop for VcpuWatchdog {
    fn drop(&mut self) {
        // Signal the watchdog thread to exit.
        {
            let (lock, cvar) = &*self.done;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// RAII periodic timer that interrupts a vCPU every `interval` for I/O polling.
///
/// When the guest is idle, KVM handles HLT+PIT internally and never exits to
/// userspace. This timer forces KVM_RUN to return periodically so the vCPU
/// loop can poll TAP devices for incoming network packets.
struct VcpuPollTimer {
    done: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl VcpuPollTimer {
    /// Create a poll timer that interrupts `vcpu` every `interval`.
    ///
    /// Must be called from the thread that will run `vcpu.run()`.
    fn new(vcpu: &VcpuFd, interval: Duration) -> Self {
        install_vcpu_signal_handler();

        let kvm_run_addr = unsafe { vcpu.kvm_run_ptr() } as usize;
        let vcpu_tid = unsafe { libc::pthread_self() };
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let done_clone = done.clone();

        let handle = std::thread::spawn(move || {
            let (lock, cvar) = &*done_clone;
            loop {
                let mut finished = lock.lock().unwrap();
                let result = cvar.wait_timeout(finished, interval).unwrap();
                finished = result.0;
                if *finished {
                    break;
                }
                // Timer fired — interrupt the vCPU for I/O polling.
                unsafe {
                    (*(kvm_run_addr as *mut KvmRun)).immediate_exit = 1;
                    libc::pthread_kill(vcpu_tid, libc::SIGUSR1);
                }
            }
        });

        Self {
            done,
            handle: Some(handle),
        }
    }
}

impl Drop for VcpuPollTimer {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.done;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Exit action returned from the handler.
pub enum ExitAction {
    /// Continue running the vCPU.
    Continue,
    /// Shut down the VM.
    Shutdown,
    /// An error occurred.
    Error(String),
}

/// Handle a single vCPU exit.
///
/// This function is called in the hot path — no allocations, no logging
/// for common exits (MMIO, IO). Tracing only for unusual exits.
#[inline]
pub fn handle_exit(exit: &VmExit, mmio_bus: &mut MmioBus, vcpu: &VcpuFd) -> ExitAction {
    match exit {
        VmExit::MmioWrite { addr, data, len } => {
            // Extract value from data bytes based on len.
            let value = match *len {
                1 => data[0] as u32,
                2 => u16::from_le_bytes([data[0], data[1]]) as u32,
                4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
                _ => 0,
            };
            mmio_bus.write(*addr, *len, value);
            ExitAction::Continue
        }
        VmExit::MmioRead { addr, len } => {
            let value = mmio_bus.read(*addr, *len).unwrap_or(0);
            // Write the value back to the kvm_run MMIO data field.
            // SAFETY: We need to write to the kvm_run data area so the guest reads the value.
            unsafe {
                let run_ptr = vcpu.kvm_run_ptr();
                let exit_data = &mut (*run_ptr).exit_data;
                let mmio_data = exit_data.as_mut_ptr().add(8); // data field at offset 8 in mmio struct
                match *len {
                    1 => *mmio_data = value as u8,
                    2 => {
                        let bytes = (value as u16).to_le_bytes();
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mmio_data, 2);
                    }
                    4 => {
                        let bytes = value.to_le_bytes();
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mmio_data, 4);
                    }
                    _ => {}
                }
            }
            ExitAction::Continue
        }
        VmExit::IoOut { port, size, data } => {
            // Handle serial port output (COM1 = 0x3F8).
            if *port == 0x3F8 && *size == 1 {
                let ch = (*data & 0xFF) as u8;
                // Direct serial output — this is the early boot console.
                let _ = std::io::Write::write_all(&mut std::io::stdout(), &[ch]);
            }
            ExitAction::Continue
        }
        VmExit::IoIn { port, size: _ } => {
            // For serial port status register, return "transmitter empty".
            if *port == 0x3FD {
                // Line status register: transmitter holding register empty + transmitter empty.
                unsafe {
                    let run_ptr = vcpu.kvm_run_ptr();
                    let exit_data = &mut (*run_ptr).exit_data;
                    // IO data is at data_offset from kvm_run start.
                    let io_data = exit_data.as_ptr().cast::<KvmRunExitIoRef>();
                    let data_offset = (*io_data).data_offset as usize;
                    let data_ptr = (run_ptr as *mut u8).add(data_offset);
                    *data_ptr = 0x60; // THRE | TEMT
                }
            }
            ExitAction::Continue
        }
        VmExit::Hlt => ExitAction::Shutdown,
        VmExit::Shutdown => {
            tracing::info!("guest triggered shutdown");
            ExitAction::Shutdown
        }
        VmExit::Interrupted => ExitAction::Continue,
        VmExit::InternalError { suberror } => {
            ExitAction::Error(format!("KVM internal error: suberror={suberror}"))
        }
        VmExit::Unknown(reason) => ExitAction::Error(format!("unknown vmexit reason: {reason}")),
    }
}

/// Borrowed type to read IO exit data from kvm_run.
#[repr(C)]
struct KvmRunExitIoRef {
    pub direction: u8,
    pub size: u8,
    pub port: u16,
    pub count: u32,
    pub data_offset: u64,
}

/// Run a vCPU in a loop until shutdown or error.
pub fn run_vcpu_loop(vcpu: &VcpuFd, mmio_bus: &mut MmioBus) -> anyhow::Result<()> {
    loop {
        mmio_bus.poll_devices();
        let exit = vcpu.run().map_err(|e| anyhow::anyhow!("vCPU run: {e}"))?;

        // HLT = guest idle waiting for interrupts, not shutdown.
        // Poll for I/O, sleep briefly if nothing pending, then re-enter.
        if matches!(&exit, VmExit::Hlt) {
            if !mmio_bus.poll_devices() {
                std::thread::sleep(Duration::from_millis(1));
            }
            continue;
        }

        match handle_exit(&exit, mmio_bus, vcpu) {
            ExitAction::Continue => {}
            ExitAction::Shutdown => {
                tracing::info!("vCPU shutdown");
                return Ok(());
            }
            ExitAction::Error(msg) => {
                return Err(anyhow::anyhow!("vCPU error: {msg}"));
            }
        }
    }
}

/// The final reason the capture loop stopped.
#[derive(Debug)]
pub enum CaptureStopReason {
    /// Guest executed HLT.
    Hlt,
    /// Guest triggered shutdown.
    Shutdown,
    /// The timeout elapsed.
    Timeout,
    /// Output buffer reached max size.
    BufferFull,
    /// External shutdown signal (e.g., `novactl stop`).
    Stopped,
    /// A vCPU error occurred.
    Error(String),
}

/// Diagnostics from the capture run.
#[derive(Debug, Default, Clone)]
pub struct CaptureDiagnostics {
    pub total_exits: u64,
    pub io_in_count: u64,
    pub io_out_count: u64,
    pub io_out_serial_count: u64,
    pub mmio_count: u64,
    /// Wall-clock duration of the capture run.
    pub elapsed: Duration,
}

/// Minimal COM1 (0x3F8-0x3FF) register state for serial port emulation.
///
/// The Linux 8250 serial driver probes the UART by writing to registers
/// and reading them back. Without proper emulation, it concludes "no UART"
/// and never outputs to serial.
///
/// Crucially, the 8250 driver uses interrupt-driven TX: it enables the
/// THRE (Transmitter Holding Register Empty) interrupt in IER, then waits
/// for IRQ 4 before sending the next character. We must inject IRQ 4 and
/// report the correct IIR value for the driver to make progress.
struct SerialState {
    /// IER (0x3F9): Interrupt Enable Register
    ier: u8,
    /// LCR (0x3FB): Line Control Register
    lcr: u8,
    /// MCR (0x3FC): Modem Control Register
    mcr: u8,
    /// SCR (0x3FF): Scratch Register
    scr: u8,
    /// DLL (0x3F8 when DLAB=1): Divisor Latch Low
    dll: u8,
    /// DLM (0x3F9 when DLAB=1): Divisor Latch High
    dlm: u8,
    /// Whether a THRE interrupt is pending (IRQ 4 needs injection).
    thre_pending: bool,
    /// Input queue: bytes to deliver to guest when it reads RBR.
    input_queue: std::collections::VecDeque<u8>,
}

impl SerialState {
    fn new() -> Self {
        Self {
            ier: 0,
            lcr: 0,
            mcr: 0,
            scr: 0,
            dll: 0x01, // default divisor = 1
            dlm: 0,
            thre_pending: false,
            input_queue: std::collections::VecDeque::new(),
        }
    }

    /// DLAB (Divisor Latch Access Bit) is bit 7 of LCR.
    fn dlab(&self) -> bool {
        self.lcr & 0x80 != 0
    }

    /// Whether THRE (bit 1) interrupt is enabled in IER.
    fn thre_int_enabled(&self) -> bool {
        self.ier & 0x02 != 0
    }

    /// Whether Received Data Available (bit 0) interrupt is enabled in IER.
    fn rda_int_enabled(&self) -> bool {
        self.ier & 0x01 != 0
    }

    /// Handle an IoOut to a COM1 register. Returns true if data byte was written to TX.
    fn write(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x3F8 => {
                if self.dlab() {
                    self.dll = value;
                    false
                } else {
                    // TX data — this is the serial output byte.
                    // THR is now "empty" again (we consume instantly).
                    // If THRE interrupt enabled, schedule IRQ 4.
                    if self.thre_int_enabled() {
                        self.thre_pending = true;
                    }
                    true
                }
            }
            0x3F9 => {
                if self.dlab() {
                    self.dlm = value;
                } else {
                    self.ier = value;
                    // If driver just enabled THRE interrupt and THR is empty
                    // (always true for us), schedule IRQ 4.
                    if self.thre_int_enabled() {
                        self.thre_pending = true;
                    }
                }
                false
            }
            0x3FA => false,             // FCR: write-only, we ignore FIFO config
            0x3FB => { self.lcr = value; false }
            0x3FC => { self.mcr = value; false }
            0x3FF => { self.scr = value; false }
            _ => false,
        }
    }

    /// Queue input bytes to deliver to guest via RBR.
    fn queue_input(&mut self, data: &[u8]) {
        self.input_queue.extend(data);
    }

    /// Whether input data is available for the guest to read.
    fn has_input(&self) -> bool {
        !self.input_queue.is_empty()
    }

    /// Handle an IoIn from a COM1 register.
    fn read(&mut self, port: u16) -> u8 {
        match port {
            0x3F8 => {
                if self.dlab() {
                    self.dll
                } else {
                    // RBR: return next byte from input queue, or 0 if empty.
                    self.input_queue.pop_front().unwrap_or(0)
                }
            }
            0x3F9 => {
                if self.dlab() { self.dlm } else { self.ier }
            }
            0x3FA => {
                // IIR: Interrupt Identification Register.
                // Bit 0 = 0 means interrupt pending.
                // Bits 1-3 = interrupt ID.
                // 0x04 = RDA (Received Data Available) — highest priority.
                // 0x02 = THRE (transmitter holding register empty).
                if self.has_input() && self.rda_int_enabled() {
                    0x04 // RDA interrupt pending (data ready to read)
                } else if self.thre_pending {
                    self.thre_pending = false;
                    0x02 // THRE interrupt pending
                } else {
                    0x01 // no interrupt pending
                }
            }
            0x3FB => self.lcr,
            0x3FC => self.mcr,
            0x3FD => {
                // LSR: bit 0 = DR (Data Ready), bit 5 = THRE, bit 6 = TEMT
                let dr = if self.has_input() { 0x01 } else { 0x00 };
                0x60 | dr
            }
            0x3FE => 0x00,              // MSR: no modem signals
            0x3FF => self.scr,
            _ => 0xFF,
        }
    }

    /// Returns true if IRQ 4 should be injected into the guest.
    fn needs_irq(&self) -> bool {
        self.thre_pending || (self.has_input() && self.rda_int_enabled())
    }
}

/// Inject IRQ 4 (COM1) into the guest via KVM_IRQ_LINE.
fn inject_serial_irq(mmio_bus: &MmioBus) {
    if let Some(vm_fd) = mmio_bus.vm_fd() {
        let level_high = nova_kvm::kvm_bindings::KvmIrqLevel { irq: 4, level: 1 };
        let level_low = nova_kvm::kvm_bindings::KvmIrqLevel { irq: 4, level: 0 };
        unsafe {
            libc::ioctl(vm_fd, nova_kvm::kvm_bindings::KVM_IRQ_LINE, &level_high as *const nova_kvm::kvm_bindings::KvmIrqLevel);
            libc::ioctl(vm_fd, nova_kvm::kvm_bindings::KVM_IRQ_LINE, &level_low as *const nova_kvm::kvm_bindings::KvmIrqLevel);
        }
    }
}

/// Handle reads from non-serial IO ports.
///
/// Returns sensible defaults so the guest kernel doesn't hang in polling loops:
/// - Port 0x71 (CMOS data): 0x00 — clears the UIP (Update In Progress) bit
///   that `mach_get_cmos_time()` polls on register 0x0A.
/// - Port 0x61 (System Control Port B): 0x20 — PIT channel 2 output high,
///   so `pit_calibrate_tsc()` exits its wait loop immediately.
/// - Everything else: 0xFF (bus pull-up for unconnected ports).
#[inline]
fn default_io_read(port: u16) -> u8 {
    match port {
        0x71 => 0x00, // CMOS data — UIP=0, time fields=0
        0x61 => 0x20, // System control B — PIT OUT2 high
        _ => 0xFF,
    }
}

/// Shared serial input queue — allows sending bytes to the guest via serial console.
pub type SerialInput = Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>;

/// Run a vCPU loop that captures serial output instead of printing to stdout.
///
/// Returns the captured serial bytes, the reason the loop stopped, and diagnostics.
/// Uses a signal-based watchdog to enforce the timeout even when `vcpu.run()`
/// is blocked inside the KVM ioctl (e.g., during initramfs unpacking).
pub fn run_vcpu_with_capture(
    vcpu: &VcpuFd,
    mmio_bus: &mut MmioBus,
    timeout: Duration,
    max_output: usize,
) -> anyhow::Result<(Vec<u8>, CaptureStopReason, CaptureDiagnostics)> {
    run_vcpu_capture_inner(vcpu, mmio_bus, timeout, max_output, SerialState::new(), None)
}

/// Run a vCPU loop on a resumed/restored VM where the guest has already
/// configured the serial port during a previous boot phase.
///
/// Identical to `run_vcpu_with_capture` but initializes the host-side serial
/// state with IER=0x02 (THRE interrupt enabled) so the 8250 driver's
/// interrupt-driven TX path continues working after a snapshot restore
/// or a phase switch (e.g., after `run_vcpu_until_match`).
pub fn run_vcpu_resume_capture(
    vcpu: &VcpuFd,
    mmio_bus: &mut MmioBus,
    timeout: Duration,
    max_output: usize,
) -> anyhow::Result<(Vec<u8>, CaptureStopReason, CaptureDiagnostics)> {
    let mut serial = SerialState::new();
    serial.ier = 0x02; // Guest already enabled THRE interrupt during boot.
    run_vcpu_capture_inner(vcpu, mmio_bus, timeout, max_output, serial, None)
}

/// Shared console output buffer — allows real-time streaming of serial output.
pub type ConsoleOutput = Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>;

/// Shared shutdown flag — allows external stop signals to break the vCPU loop.
pub type ShutdownFlag = Arc<std::sync::atomic::AtomicBool>;

/// Run a vCPU loop with serial input/output for interactive exec and log streaming.
/// Output is written to `console_output` in real-time (not buffered until return).
/// If `shutdown` is provided, the loop checks it every ~10ms and exits with `Stopped`.
pub fn run_vcpu_resume_capture_interactive(
    vcpu: &VcpuFd,
    mmio_bus: &mut MmioBus,
    timeout: Duration,
    max_output: usize,
    console_input: SerialInput,
    console_output: ConsoleOutput,
    shutdown: Option<ShutdownFlag>,
) -> anyhow::Result<(Vec<u8>, CaptureStopReason, CaptureDiagnostics)> {
    let mut serial = SerialState::new();
    serial.ier = 0x02;
    run_vcpu_capture_inner_interactive(vcpu, mmio_bus, timeout, max_output, serial, console_input, console_output, shutdown)
}

fn run_vcpu_capture_inner_interactive(
    vcpu: &VcpuFd,
    mmio_bus: &mut MmioBus,
    timeout: Duration,
    max_output: usize,
    mut serial: SerialState,
    console_input: SerialInput,
    console_output: ConsoleOutput,
    shutdown: Option<ShutdownFlag>,
) -> anyhow::Result<(Vec<u8>, CaptureStopReason, CaptureDiagnostics)> {
    let mut output = Vec::new();
    let mut diag = CaptureDiagnostics::default();
    let start = Instant::now();

    let _watchdog = VcpuWatchdog::new(vcpu, timeout);
    let _poll_timer = VcpuPollTimer::new(vcpu, Duration::from_millis(10));

    loop {
        if output.len() >= max_output {
            diag.elapsed = start.elapsed();
            return Ok((output, CaptureStopReason::BufferFull, diag));
        }

        // Check external shutdown flag.
        if let Some(ref flag) = shutdown {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                diag.elapsed = start.elapsed();
                tracing::info!("vCPU loop: shutdown flag set, exiting");
                return Ok((output, CaptureStopReason::Stopped, diag));
            }
        }

        // Drain shared input queue into serial state.
        if let Ok(mut queue) = console_input.lock() {
            if !queue.is_empty() {
                let bytes: Vec<u8> = queue.drain(..).collect();
                serial.queue_input(&bytes);
                if serial.needs_irq() {
                    inject_serial_irq(mmio_bus);
                }
            }
        }

        mmio_bus.poll_devices();

        unsafe {
            let run_ptr = vcpu.kvm_run_ptr();
            (*run_ptr).immediate_exit = 0;
        }

        let exit = vcpu.run().map_err(|e| anyhow::anyhow!("vCPU run: {e}"))?;
        diag.total_exits += 1;

        match &exit {
            VmExit::Interrupted => {
                // Check shutdown flag on every timer interrupt (~10ms).
                if let Some(ref flag) = shutdown {
                    if flag.load(std::sync::atomic::Ordering::Relaxed) {
                        diag.elapsed = start.elapsed();
                        tracing::info!("vCPU loop: shutdown flag set (on interrupt), exiting");
                        return Ok((output, CaptureStopReason::Stopped, diag));
                    }
                }
                let elapsed = start.elapsed();
                if elapsed >= timeout {
                    diag.elapsed = elapsed;
                    return Ok((output, CaptureStopReason::Timeout, diag));
                }
            }
            VmExit::IoOut { port, size, data } => {
                diag.io_out_count += 1;
                if (0x3F8..=0x3FF).contains(port) && *size == 1 {
                    diag.io_out_serial_count += 1;
                    let byte = (*data & 0xFF) as u8;
                    if serial.write(*port, byte) {
                        output.push(byte);
                        // Write to shared console output in real-time.
                        if let Ok(mut buf) = console_output.lock() {
                            buf.push_back(byte);
                        }
                    }
                    if serial.needs_irq() {
                        inject_serial_irq(mmio_bus);
                    }
                }
            }
            VmExit::IoIn { port, size } => {
                diag.io_in_count += 1;
                let value: u8 = if (0x3F8..=0x3FF).contains(port) {
                    serial.read(*port)
                } else {
                    default_io_read(*port)
                };
                unsafe {
                    let run_ptr = vcpu.kvm_run_ptr();
                    let exit_data = &mut (*run_ptr).exit_data;
                    let io_data = exit_data.as_ptr().cast::<KvmRunExitIoRef>();
                    let data_offset = (*io_data).data_offset as usize;
                    let data_ptr = (run_ptr as *mut u8).add(data_offset);
                    for i in 0..(*size as usize) {
                        *data_ptr.add(i) = value;
                    }
                }
            }
            VmExit::MmioWrite { addr, data, len } => {
                diag.mmio_count += 1;
                let value = match *len {
                    1 => data[0] as u32,
                    2 => u16::from_le_bytes([data[0], data[1]]) as u32,
                    4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
                    _ => 0,
                };
                mmio_bus.write(*addr, *len, value);
            }
            VmExit::MmioRead { addr, len } => {
                diag.mmio_count += 1;
                let value = mmio_bus.read(*addr, *len).unwrap_or(0);
                unsafe {
                    let run_ptr = vcpu.kvm_run_ptr();
                    let exit_data = &mut (*run_ptr).exit_data;
                    let mmio_data = exit_data.as_mut_ptr().add(8);
                    match *len {
                        1 => *mmio_data = value as u8,
                        2 => {
                            let bytes = (value as u16).to_le_bytes();
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mmio_data, 2);
                        }
                        4 => {
                            let bytes = value.to_le_bytes();
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mmio_data, 4);
                        }
                        _ => {}
                    }
                }
            }
            VmExit::Hlt => {
                if !mmio_bus.poll_devices() {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            VmExit::Shutdown => {
                diag.elapsed = start.elapsed();
                return Ok((output, CaptureStopReason::Shutdown, diag));
            }
            VmExit::InternalError { suberror } => {
                diag.elapsed = start.elapsed();
                return Ok((
                    output,
                    CaptureStopReason::Error(format!("KVM internal error: suberror={suberror}")),
                    diag,
                ));
            }
            VmExit::Unknown(reason) => {
                diag.elapsed = start.elapsed();
                return Ok((
                    output,
                    CaptureStopReason::Error(format!("unknown vmexit reason: {reason}")),
                    diag,
                ));
            }
        }
    }
}

fn run_vcpu_capture_inner(
    vcpu: &VcpuFd,
    mmio_bus: &mut MmioBus,
    timeout: Duration,
    max_output: usize,
    mut serial: SerialState,
    console_input: Option<SerialInput>,
) -> anyhow::Result<(Vec<u8>, CaptureStopReason, CaptureDiagnostics)> {
    let mut output = Vec::new();
    let mut diag = CaptureDiagnostics::default();
    let start = Instant::now();

    // Watchdog will interrupt vcpu.run() via SIGUSR1 after timeout.
    let _watchdog = VcpuWatchdog::new(vcpu, timeout);
    // Poll timer interrupts vcpu.run() every 10ms for I/O polling.
    let _poll_timer = VcpuPollTimer::new(vcpu, Duration::from_millis(10));

    loop {
        if output.len() >= max_output {
            diag.elapsed = start.elapsed();
            return Ok((output, CaptureStopReason::BufferFull, diag));
        }

        // Drain shared input queue into serial state.
        if let Some(ref input) = console_input {
            if let Ok(mut queue) = input.lock() {
                if !queue.is_empty() {
                    let bytes: Vec<u8> = queue.drain(..).collect();
                    serial.queue_input(&bytes);
                    // Inject IRQ 4 to notify guest of available input.
                    if serial.needs_irq() {
                        inject_serial_irq(mmio_bus);
                    }
                }
            }
        }

        // Poll devices for async I/O (e.g., TAP RX packets) between VM exits.
        mmio_bus.poll_devices();

        // Clear immediate_exit before re-entering KVM_RUN.
        unsafe {
            let run_ptr = vcpu.kvm_run_ptr();
            (*run_ptr).immediate_exit = 0;
        }

        let exit = vcpu.run().map_err(|e| anyhow::anyhow!("vCPU run: {e}"))?;
        diag.total_exits += 1;

        match &exit {
            VmExit::Interrupted => {
                // Poll timer or watchdog fired — check if timeout has actually elapsed.
                let elapsed = start.elapsed();
                if elapsed >= timeout {
                    diag.elapsed = elapsed;
                    return Ok((output, CaptureStopReason::Timeout, diag));
                }
                // Timer interrupt for I/O polling — continue to poll_devices() at top.
            }
            VmExit::IoOut { port, size, data } => {
                diag.io_out_count += 1;
                if (0x3F8..=0x3FF).contains(port) && *size == 1 {
                    diag.io_out_serial_count += 1;
                    let byte = (*data & 0xFF) as u8;
                    if serial.write(*port, byte) {
                        output.push(byte);
                    }
                    if serial.needs_irq() {
                        inject_serial_irq(mmio_bus);
                    }
                }
            }
            VmExit::IoIn { port, size } => {
                diag.io_in_count += 1;
                let value: u8 = if (0x3F8..=0x3FF).contains(port) {
                    serial.read(*port)
                } else {
                    default_io_read(*port)
                };
                unsafe {
                    let run_ptr = vcpu.kvm_run_ptr();
                    let exit_data = &mut (*run_ptr).exit_data;
                    let io_data = exit_data.as_ptr().cast::<KvmRunExitIoRef>();
                    let data_offset = (*io_data).data_offset as usize;
                    let data_ptr = (run_ptr as *mut u8).add(data_offset);
                    for i in 0..(*size as usize) {
                        *data_ptr.add(i) = value;
                    }
                }
            }
            VmExit::MmioWrite { addr, data, len } => {
                diag.mmio_count += 1;
                let value = match *len {
                    1 => data[0] as u32,
                    2 => u16::from_le_bytes([data[0], data[1]]) as u32,
                    4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
                    _ => 0,
                };
                mmio_bus.write(*addr, *len, value);
            }
            VmExit::MmioRead { addr, len } => {
                diag.mmio_count += 1;
                let value = mmio_bus.read(*addr, *len).unwrap_or(0);
                unsafe {
                    let run_ptr = vcpu.kvm_run_ptr();
                    let exit_data = &mut (*run_ptr).exit_data;
                    let mmio_data = exit_data.as_mut_ptr().add(8);
                    match *len {
                        1 => *mmio_data = value as u8,
                        2 => {
                            let bytes = (value as u16).to_le_bytes();
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mmio_data, 2);
                        }
                        4 => {
                            let bytes = value.to_le_bytes();
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mmio_data, 4);
                        }
                        _ => {}
                    }
                }
            }
            VmExit::Hlt => {
                // Guest is idle waiting for interrupts — poll for I/O.
                if !mmio_bus.poll_devices() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                // Continue loop; watchdog will enforce timeout.
            }
            VmExit::Shutdown => {
                diag.elapsed = start.elapsed();
                return Ok((output, CaptureStopReason::Shutdown, diag));
            }
            VmExit::InternalError { suberror } => {
                diag.elapsed = start.elapsed();
                return Ok((
                    output,
                    CaptureStopReason::Error(format!("KVM internal error: suberror={suberror}")),
                    diag,
                ));
            }
            VmExit::Unknown(reason) => {
                diag.elapsed = start.elapsed();
                return Ok((
                    output,
                    CaptureStopReason::Error(format!("unknown vmexit reason: {reason}")),
                    diag,
                ));
            }
        }
    }
}

/// Run a vCPU loop that stops as soon as the serial output contains the target string.
///
/// Returns the captured output, time elapsed to match, and diagnostics.
/// This is designed for benchmarking boot time to a specific marker.
/// Uses a signal-based watchdog to enforce the timeout.
pub fn run_vcpu_until_match(
    vcpu: &VcpuFd,
    mmio_bus: &mut MmioBus,
    timeout: Duration,
    target: &str,
) -> anyhow::Result<(Vec<u8>, CaptureStopReason, CaptureDiagnostics)> {
    let mut output = Vec::new();
    let mut serial = SerialState::new();
    let mut diag = CaptureDiagnostics::default();
    let start = Instant::now();
    let target_bytes = target.as_bytes();

    // Watchdog will interrupt vcpu.run() via SIGUSR1 after timeout.
    let _watchdog = VcpuWatchdog::new(vcpu, timeout);

    loop {
        // Check if target string has been found in output.
        if output.len() >= target_bytes.len() {
            let search_start = output.len().saturating_sub(target_bytes.len() + 256);
            if output[search_start..]
                .windows(target_bytes.len())
                .any(|w| w == target_bytes)
            {
                diag.elapsed = start.elapsed();
                return Ok((output, CaptureStopReason::BufferFull, diag));
            }
        }

        // Poll devices for async I/O (e.g., TAP RX packets) between VM exits.
        mmio_bus.poll_devices();

        let exit = vcpu.run().map_err(|e| anyhow::anyhow!("vCPU run: {e}"))?;
        diag.total_exits += 1;

        match &exit {
            VmExit::Interrupted => {
                let elapsed = start.elapsed();
                if elapsed >= timeout {
                    diag.elapsed = elapsed;
                    return Ok((output, CaptureStopReason::Timeout, diag));
                }
            }
            VmExit::IoOut { port, size, data } => {
                diag.io_out_count += 1;
                if (0x3F8..=0x3FF).contains(port) && *size == 1 {
                    diag.io_out_serial_count += 1;
                    let byte = (*data & 0xFF) as u8;
                    if serial.write(*port, byte) {
                        output.push(byte);
                    }
                    if serial.needs_irq() {
                        inject_serial_irq(mmio_bus);
                    }
                }
            }
            VmExit::IoIn { port, size } => {
                diag.io_in_count += 1;
                let value: u8 = if (0x3F8..=0x3FF).contains(port) {
                    serial.read(*port)
                } else {
                    default_io_read(*port)
                };
                unsafe {
                    let run_ptr = vcpu.kvm_run_ptr();
                    let exit_data = &mut (*run_ptr).exit_data;
                    let io_data = exit_data.as_ptr().cast::<KvmRunExitIoRef>();
                    let data_offset = (*io_data).data_offset as usize;
                    let data_ptr = (run_ptr as *mut u8).add(data_offset);
                    for i in 0..(*size as usize) {
                        *data_ptr.add(i) = value;
                    }
                }
            }
            VmExit::MmioWrite { addr, data, len } => {
                diag.mmio_count += 1;
                let value = match *len {
                    1 => data[0] as u32,
                    2 => u16::from_le_bytes([data[0], data[1]]) as u32,
                    4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
                    _ => 0,
                };
                mmio_bus.write(*addr, *len, value);
            }
            VmExit::MmioRead { addr, len } => {
                diag.mmio_count += 1;
                let value = mmio_bus.read(*addr, *len).unwrap_or(0);
                unsafe {
                    let run_ptr = vcpu.kvm_run_ptr();
                    let exit_data = &mut (*run_ptr).exit_data;
                    let mmio_data = exit_data.as_mut_ptr().add(8);
                    match *len {
                        1 => *mmio_data = value as u8,
                        2 => {
                            let bytes = (value as u16).to_le_bytes();
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mmio_data, 2);
                        }
                        4 => {
                            let bytes = value.to_le_bytes();
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mmio_data, 4);
                        }
                        _ => {}
                    }
                }
            }
            VmExit::Hlt => {
                // Guest is idle waiting for interrupts — poll for I/O.
                if !mmio_bus.poll_devices() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                // Continue loop; watchdog will enforce timeout.
            }
            VmExit::Shutdown => {
                diag.elapsed = start.elapsed();
                return Ok((output, CaptureStopReason::Shutdown, diag));
            }
            VmExit::InternalError { suberror } => {
                diag.elapsed = start.elapsed();
                return Ok((
                    output,
                    CaptureStopReason::Error(format!("KVM internal error: suberror={suberror}")),
                    diag,
                ));
            }
            VmExit::Unknown(reason) => {
                diag.elapsed = start.elapsed();
                return Ok((
                    output,
                    CaptureStopReason::Error(format!("unknown vmexit reason: {reason}")),
                    diag,
                ));
            }
        }
    }
}
