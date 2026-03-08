# Troubleshooting

## Common Issues

### "Permission denied" on `/dev/kvm`

```bash
# Check KVM access
ls -la /dev/kvm

# Fix: run daemon as root
sudo nova serve --config /etc/nova/nova.toml

# Or add user to kvm group (if exists)
sudo usermod -aG kvm $USER
```

### "Connection refused" when using nova CLI

The daemon isn't running or the socket path doesn't match.

```bash
# Check if daemon is running
ps aux | grep nova

# Check socket exists
ls -la /run/nova/nova.sock

# Check REST API
curl http://localhost:9800/health

# Start daemon
sudo RUST_LOG=info nova serve --config /etc/nova/nova.toml

# If using custom socket path:
nova --socket /path/to/nova.sock ps
```

### KVM not available on WSL2

```powershell
# In PowerShell (admin):
# 1. Check WSL version
wsl --version

# 2. Enable nested virtualization
Set-VMProcessor -VMName WSL -ExposeVirtualizationExtensions $true

# 3. Restart WSL
wsl --shutdown
```

Then inside WSL:
```bash
ls /dev/kvm  # should exist
```

### Cold boot hangs / takes too long

```bash
# Check kernel path in config
cat /etc/nova/nova.toml | grep kernel

# Verify kernel exists
ls -la /opt/nova/vmlinux

# Check daemon logs (RUST_LOG=info)
# Look for "KVM boot" messages

# Clear L3 cache and retry
sudo rm -rf /var/lib/nova/images/snapshots/*
```

### No network in guest VM

```bash
# 1. Check TAP device exists
ip link show tap0

# 2. Check config has tap_device
grep tap_device /etc/nova/nova.toml

# 3. Check daemon log for "TAP fd assigned"
# If missing, TAP open failed

# 4. Recreate TAP
sudo bash scripts/setup-network.sh

# 5. Check guest networking
nova exec <sandbox> ip addr show
nova exec <sandbox> ip route
```

### Guest networking works but no internet

```bash
# Check IP forwarding
cat /proc/sys/net/ipv4/ip_forward
# Must be 1

# Check NAT rules
sudo iptables -t nat -L POSTROUTING -v
# Must have MASQUERADE for 172.16.0.0/24

# Add missing rules
sudo iptables -t nat -A POSTROUTING -s 172.16.0.0/24 -o eth0 -j MASQUERADE
```

### eBPF programs fail to load

```bash
# Check bytecode exists
ls -la /opt/nova/ebpf/

# Check permissions (must be root)
sudo nova serve --config /etc/nova/nova.toml

# Check kernel eBPF support
cat /proc/config.gz | zcat | grep CONFIG_BPF

# Check RLIMIT_MEMLOCK (kernel < 5.11)
ulimit -l
# If 64, need to increase:
ulimit -l unlimited
```

### Snapshot restore fails

```bash
# Check snapshot exists
ls /var/lib/nova/images/snapshots/

# Check snapshot version (must be v3)
cat /var/lib/nova/images/snapshots/*/snapshot.json | head -1

# Clear stale snapshots
sudo rm -rf /var/lib/nova/images/snapshots/*

# Cold boot creates fresh snapshot
nova run nginx:alpine --name test
```

### "EADDRINUSE" on daemon start

Another daemon instance is running or the socket file is stale.

```bash
# Kill existing daemon
sudo pkill -f "nova serve"

# Remove stale socket
sudo rm -f /run/nova/nova.sock

# Restart
sudo RUST_LOG=info nova serve --config /etc/nova/nova.toml
```

### Exec command returns empty output

```bash
# Check sandbox is running
nova ps

# Try with explicit command
nova exec <sandbox> /bin/cat /etc/passwd

# Check daemon logs for serial console errors
# Exec uses serial protocol — high latency (~600ms) is expected
```

### Image pull fails

```bash
# Check internet connectivity
curl -I https://registry-1.docker.io/v2/

# Check image reference format
# Correct: nginx:alpine, ghcr.io/owner/image:tag
# Wrong:   nginx (no tag), http://registry/image

# Check disk space for image cache
df -h /var/lib/nova/images/
```

### Guest eBPF agent loads 0 probes

The guest agent found no eBPF bytecode to load. Ensure probes are defined in `nova.toml`:

```bash
# Check config has [[sensor.probes]] entries
grep -A3 'sensor.probes' /etc/nova/nova.toml

# Check eBPF bytecode exists
ls -la /opt/nova/ebpf/

# Probes must be defined for both host loading AND guest injection
# The agent injects bytecode files listed in [[sensor.probes]] into the guest
```

## Debug Logging

```bash
# Verbose logging
sudo RUST_LOG=debug nova serve --config /etc/nova/nova.toml

# Component-specific logging
sudo RUST_LOG=nova_vmm=debug,nova_runtime=info nova serve --config /etc/nova/nova.toml

# Log levels: error, warn, info, debug, trace
```

## Performance Tips

1. **Use snapshots** — First cold boot is slow (~2s), subsequent boots use L3 cache (~69ms)
2. **Pre-pull images** — `nova pull` before `nova run` avoids registry latency
3. **Bare metal > WSL2** — WSL2 adds 30-50% overhead on KVM operations
4. **Ext4 > NTFS** — WSL2's NTFS layer adds 200-500% I/O overhead. Store images on ext4 (WSL2's native fs) for better performance
5. **Disable unused probes** — Each eBPF probe adds minimal but non-zero overhead

## Windows-Specific Issues

### "connection failed (127.0.0.1:9800)" from Windows CLI

The daemon is not running in WSL or WSL port forwarding is broken.

```powershell
# Check status
nova status

# Start daemon
nova start

# If still failing, restart WSL entirely
wsl --shutdown
nova start
```

### "wsl exec failed"

WSL is not installed or not running.

```powershell
wsl --version         # Check WSL
wsl --install         # Install WSL
wsl --list --verbose  # List distributions
```

### IPv6 connection issues

Windows may resolve `localhost` to `::1` (IPv6) while the WSL daemon binds IPv4 only. The Windows CLI defaults to `127.0.0.1:9800`. If overriding `--api`, always use `127.0.0.1` instead of `localhost`.

### Git Bash expands paths in exec commands

Git Bash on Windows expands paths starting with `/` (e.g., `/etc/os-release` becomes `C:/Program Files/Git/etc/os-release`). Use PowerShell, cmd, or Windows Terminal instead of Git Bash for `nova exec` commands.

### "Access is denied" when rebuilding nova.exe

A running `nova.exe` process locks the binary. Kill it first:

```powershell
taskkill /F /IM nova.exe
cargo build --release
```

### sudo password prompt on start/setup

The Windows CLI runs `sudo -n` first (passwordless). If that fails, it prompts for your WSL password. To avoid prompts, configure passwordless sudo in WSL:

```bash
echo "$USER ALL=(ALL) NOPASSWD: ALL" | sudo tee /etc/sudoers.d/$USER
```

See [Windows guide](windows.md) for full Windows setup and troubleshooting.

## Getting Help

- Check daemon logs: `RUST_LOG=info` or `RUST_LOG=debug`
- Check event log: `tail -f /var/run/nova/events.jsonl`
- Check REST API: `curl http://localhost:9800/health`
- Run tests: `cargo test --test-threads=1` (serial, KVM tests conflict)
- Source code: each crate has module-level doc comments
