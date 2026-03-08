#!/usr/bin/env python3
"""
Apply all binary patches to the NovaVM eBPF kernel (vmlinux).

Patches:
  1. 8x ktime_get UD2 (WARN_ON(timekeeping_suspended)) -> NOP
  2. trace_event_eval_update -> RET (prevents format string overflow)
  3. ftrace_free_init_mem -> RET (prevents console deadlock during init cleanup)

Usage: python3 patch_kernel.py <vmlinux-path>
"""
import subprocess
import struct
import sys
import os

def get_symbol_addr(vmlinux, name):
    """Get virtual address of a symbol from System.map or nm."""
    map_path = os.path.join(os.path.dirname(vmlinux), "System.map")
    if os.path.exists(map_path):
        with open(map_path) as f:
            for line in f:
                parts = line.strip().split()
                if len(parts) >= 3 and parts[2] == name:
                    return int(parts[0], 16)
    # Fallback: use nm
    result = subprocess.run(["nm", vmlinux], capture_output=True, text=True)
    for line in result.stdout.split("\n"):
        parts = line.strip().split()
        if len(parts) >= 3 and parts[2] == name:
            return int(parts[0], 16)
    return None

def get_section_info(vmlinux, section_name):
    """Get (vaddr, file_offset) for a section using readelf."""
    result = subprocess.run(["readelf", "-S", vmlinux], capture_output=True, text=True)
    for line in result.stdout.split("\n"):
        if section_name in line:
            parts = line.split()
            # Find the hex values for Address and Offset
            for i, part in enumerate(parts):
                if part == section_name:
                    # Next non-type field should be Address, then Offset
                    # Format: [Nr] Name Type Address Offset
                    # The type might be on same or next line
                    remaining = parts[i+1:]
                    hex_vals = [p for p in remaining if len(p) >= 8 and all(c in '0123456789abcdef' for c in p)]
                    if len(hex_vals) >= 2:
                        return int(hex_vals[0], 16), int(hex_vals[1], 16)
    return None, None

def vaddr_to_fileoff(vmlinux, vaddr):
    """Convert virtual address to file offset."""
    result = subprocess.run(["readelf", "-S", vmlinux], capture_output=True, text=True)
    sections = []
    lines = result.stdout.split("\n")
    for i, line in enumerate(lines):
        if "PROGBITS" in line or "NOBITS" in line:
            # Parse section header
            parts = line.split()
            hex_vals = [p for p in parts if len(p) >= 6 and all(c in '0123456789abcdef' for c in p.lower())]
            if len(hex_vals) >= 3:
                sec_vaddr = int(hex_vals[0], 16)
                sec_offset = int(hex_vals[1], 16)
                sec_size = int(hex_vals[2], 16)
                if sec_vaddr <= vaddr < sec_vaddr + sec_size:
                    return sec_offset + (vaddr - sec_vaddr)
    # Fallback: assume .text at standard offset
    text_vaddr = 0xffffffff81000000
    text_offset = 0x00200000
    return text_offset + (vaddr - text_vaddr)

def patch_bytes(f, offset, expected, replacement, name):
    """Patch bytes at file offset, verifying expected value."""
    f.seek(offset)
    actual = f.read(len(expected))
    if actual == expected:
        f.seek(offset)
        f.write(replacement)
        print(f"  [OK] {name} at offset 0x{offset:x}")
        return True
    elif actual == replacement:
        print(f"  [SKIP] {name} already patched")
        return True
    else:
        print(f"  [FAIL] {name} at offset 0x{offset:x}: expected {expected.hex()}, got {actual.hex()}")
        return False

def main():
    if len(sys.argv) < 2:
        print("Usage: python3 patch_kernel.py <vmlinux-path>")
        sys.exit(1)

    vmlinux = sys.argv[1]
    if not os.path.exists(vmlinux):
        print(f"Error: {vmlinux} not found")
        sys.exit(1)

    print(f"Patching {vmlinux}")
    total = 0
    ok = 0

    # --- Patch 1: ktime_get family UD2 -> NOP ---
    print("\n=== ktime_get UD2 patches (WARN_ON -> NOP) ===")
    ktime_symbols = [
        "ktime_get_coarse_real_ts64",
        "ktime_get_coarse_with_offset",
        "ktime_get_ts64",
        "ktime_get_seconds",
        "ktime_get_snapshot",
        "ktime_get_with_offset",
        "ktime_get_real_ts64",
        "ktime_get",
    ]

    # Known offsets within each function where the UD2 instruction is
    ktime_ud2_offsets = {
        "ktime_get_coarse_real_ts64": 0x90,
        "ktime_get_coarse_with_offset": 0x5c,
        "ktime_get_ts64": 0xe6,
        "ktime_get_seconds": 0x20,
        "ktime_get_snapshot": 0x13d,
        "ktime_get_with_offset": 0xb6,
        "ktime_get_real_ts64": 0xd6,
        "ktime_get": 0x97,
    }

    with open(vmlinux, "r+b") as f:
        for sym in ktime_symbols:
            addr = get_symbol_addr(vmlinux, sym)
            if addr is None:
                print(f"  [SKIP] {sym} not found in symbol table")
                continue

            offset_in_func = ktime_ud2_offsets.get(sym)
            if offset_in_func is None:
                continue

            file_offset = vaddr_to_fileoff(vmlinux, addr + offset_in_func)
            total += 1
            if patch_bytes(f, file_offset, b'\x0f\x0b', b'\x90\x90', f"{sym}+0x{offset_in_func:x}"):
                ok += 1

        # --- Patch 2: trace_event_eval_update -> RET ---
        print("\n=== trace_event_eval_update RET patch ===")
        addr = get_symbol_addr(vmlinux, "trace_event_eval_update")
        if addr:
            file_offset = vaddr_to_fileoff(vmlinux, addr)
            total += 1
            if patch_bytes(f, file_offset, b'\x55', b'\xc3', "trace_event_eval_update"):
                ok += 1
        else:
            print("  [SKIP] trace_event_eval_update not found")

        # --- Patch 3: ftrace_free_init_mem -> RET ---
        print("\n=== ftrace_free_init_mem RET patch ===")
        addr = get_symbol_addr(vmlinux, "ftrace_free_init_mem")
        if addr:
            file_offset = vaddr_to_fileoff(vmlinux, addr)
            total += 1
            if patch_bytes(f, file_offset, b'\x55', b'\xc3', "ftrace_free_init_mem"):
                ok += 1
        else:
            print("  [SKIP] ftrace_free_init_mem not found")

    print(f"\n=== Summary: {ok}/{total} patches applied ===")
    if ok < total:
        sys.exit(1)

if __name__ == "__main__":
    main()
