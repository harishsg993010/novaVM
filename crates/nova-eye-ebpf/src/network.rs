//! eBPF program for network connection monitoring.

#![no_std]
#![no_main]

use core::mem::MaybeUninit;

use aya_ebpf::{
    helpers::{bpf_get_current_pid_tgid, bpf_get_current_uid_gid, bpf_ktime_get_ns},
    macros::{kprobe, map},
    maps::PerfEventArray,
    programs::ProbeContext,
};
use nova_eye_common::{EventHeader, EventType};

#[map]
static EVENTS: PerfEventArray<EventHeader> = PerfEventArray::new(0);

#[inline(always)]
unsafe fn get_comm_no_memset() -> [u8; 16] {
    let mut comm = MaybeUninit::<[u8; 16]>::uninit();
    let _ = aya_ebpf::helpers::gen::bpf_get_current_comm(
        comm.as_mut_ptr() as *mut core::ffi::c_void,
        16u32,
    );
    comm.assume_init()
}

#[kprobe(function = "tcp_v4_connect")]
pub fn handle_tcp_v4_connect(ctx: ProbeContext) -> u32 {
    unsafe {
        let pid_tgid = bpf_get_current_pid_tgid();
        let uid_gid = bpf_get_current_uid_gid();
        let timestamp_ns = bpf_ktime_get_ns();
        let comm = get_comm_no_memset();

        let mut header = MaybeUninit::<EventHeader>::uninit();
        let p = header.as_mut_ptr();
        (*p).event_type = EventType::NetConnect as u32;
        (*p).timestamp_ns = timestamp_ns;
        (*p).pid = (pid_tgid >> 32) as u32;
        (*p).tid = pid_tgid as u32;
        (*p).uid = uid_gid as u32;
        (*p).gid = (uid_gid >> 32) as u32;
        (*p).comm = comm;
        let header = header.assume_init();
        EVENTS.output(&ctx, &header, 0);
    }
    0
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
