# Snapshots & Fast Boot

NovaVM uses L3 snapshots for sub-100ms warm boot times.

## How It Works

```
Cold Boot (first run):
  OCI image -> rootfs -> initramfs -> KVM boot -> running VM
                                                    |
                                              [save snapshot]
                                                    |
                                                    v
                                        snapshot.json + memory.bin
                                        (saved to L3 cache)

Warm Boot (subsequent runs):
  L3 cache hit -> mmap(MAP_PRIVATE) memory -> restore registers
                -> force-activate virtio -> running VM
                                            (69ms avg)
```

## What's Saved

A snapshot captures the complete VM state:

| Component | Details |
|---|---|
| **vCPU registers** | General purpose, RIP, RSP, RFLAGS |
| **Segment registers** | CS, DS, SS, ES, FS, GS, LDT, TR, GDT, IDT |
| **MSRs** | EFER, STAR, LSTAR, kernel GS base, TSC, PAT, etc. |
| **XSAVE state** | FPU, SSE, AVX |
| **XCR0** | Extended control register |
| **KVM clock** | TSC offset, flags |
| **IRQ chip** | PIC, IOAPIC state |
| **PIT** | Programmable Interval Timer |
| **Guest memory** | Full RAM dump (demand-paged on restore) |
| **Virtio queues** | Queue addresses, sizes, avail/used indices |

## Snapshot Storage

```
/var/lib/nova/images/snapshots/<snapshot_id>/
    snapshot.json    # VM state (registers, MSRs, virtio queues, metadata)
    memory.bin       # Raw guest memory dump
```

## Demand-Paged Restore

Memory is restored using `mmap(MAP_PRIVATE)` instead of reading the entire file:

```
Traditional:  read(memory.bin) -> copy all 128MB into guest RAM     (~200ms)
NovaVM:       mmap(MAP_PRIVATE, memory.bin) -> pages loaded on demand (~20ms)
```

Only pages the guest actually touches get loaded from disk. This is similar to Firecracker's UFFD (userfaultfd) approach but simpler.

## Virtio State Restore

After L3 snapshot restore, virtio devices need their queue state restored. Without this, the guest thinks the device is active but the host has fresh (empty) device state.

NovaVM saves per-device queue state in the snapshot:
- Descriptor table address
- Available ring address
- Used ring address
- Queue size
- Next available/used indices

On restore, `force_activate()` reconstructs the queues and activates the device without any MMIO writes from the guest.

## Performance

| Scenario | Time |
|---|---|
| Cold boot (no cache) | ~2,005 ms |
| Snapshot save | ~429 ms |
| **Snapshot restore (L3)** | **~69 ms** |
| Snapshot restore (L3 + L4 pool) | ~50 ms |

Compared to:
- Docker warm start: ~2,045 ms (29x slower)
- Firecracker snapshot (WSL2): ~86 ms (1.2x slower)

## Usage

Snapshots are managed automatically by the runtime. No manual commands needed.

```bash
# First run: cold boot, snapshot saved automatically
nova run nginx:alpine --name web1

# Second run: warm boot from L3 snapshot
nova run nginx:alpine --name web2
# ^ This boots in ~69ms instead of ~2s
```

### Manual Snapshot (Advanced)

The snapshot save/restore is handled internally by `nova-runtime`. The L3 cache key is based on the image digest, so different images get separate snapshots.

Cache location: `/var/lib/nova/images/snapshots/`

To clear the snapshot cache (force cold boot):
```bash
sudo rm -rf /var/lib/nova/images/snapshots/*
```

## L4: VM Pool

On top of L3 snapshots, NovaVM has an L4 pre-warmed pool:

```
L4 Pool:
    Factory creates VMs from L3 snapshots in background
    Pool holds N ready-to-use VM instances
    On `run`, grab a pre-warmed VM instead of restoring
    Reduces warm boot from ~69ms to ~50ms
```

The pool uses type-erased factory payloads (`Box<dyn Any + Send>`) so it works with any VM configuration.
