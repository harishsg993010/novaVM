#!/bin/bash
# NovaVM install script
# Usage: curl -sSL https://raw.githubusercontent.com/novavm/novavm/main/install.sh | sudo bash
#
# Installs: nova-daemon, novactl, kernel, config, systemd service

set -e

VERSION="0.1.0"
REPO="novavm/novavm"
ARCH="amd64"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[+]${NC} $1"; }
warn()  { echo -e "${YELLOW}[!]${NC} $1"; }
error() { echo -e "${RED}[-]${NC} $1"; exit 1; }

echo ""
echo "  _   _                __     ____  __ "
echo " | \ | | _____   ____ _\ \   / /  \/  |"
echo " |  \| |/ _ \ \ / / _  |\ \ / /| |\/| |"
echo " | |\  | (_) \ V / (_| | \ V / | |  | |"
echo " |_| \_|\___/ \_/ \__,_|  \_/  |_|  |_|"
echo ""
echo " Lightweight microVM hypervisor v${VERSION}"
echo ""

# ---- Checks ----

if [ "$EUID" -ne 0 ]; then
    error "Please run as root: curl -sSL ... | sudo bash"
fi

if [ "$(uname -m)" != "x86_64" ]; then
    error "NovaVM only supports x86_64 (got: $(uname -m))"
fi

if [ "$(uname -s)" != "Linux" ]; then
    error "NovaVM only runs on Linux (got: $(uname -s))"
fi

# ---- Install Method ----

DEB_URL="https://github.com/${REPO}/releases/download/v${VERSION}/novavm_${VERSION}_${ARCH}.deb"

# Try .deb first (preferred)
if command -v dpkg &> /dev/null; then
    info "Downloading novavm_${VERSION}_${ARCH}.deb..."
    TMP=$(mktemp /tmp/novavm-XXXXXX.deb)
    if curl -fsSL -o "$TMP" "$DEB_URL" 2>/dev/null; then
        info "Installing .deb package..."
        dpkg -i "$TMP"
        rm -f "$TMP"
        echo ""
        info "NovaVM v${VERSION} installed successfully!"
        exit 0
    else
        warn ".deb not found on GitHub Releases. Falling back to binary install."
        rm -f "$TMP"
    fi
fi

# Fallback: download binaries directly
BINARY_BASE="https://github.com/${REPO}/releases/download/v${VERSION}"

info "Installing NovaVM v${VERSION} from binaries..."

# Binaries
info "Downloading nova-daemon..."
curl -fsSL -o /usr/bin/nova-daemon "${BINARY_BASE}/nova-daemon" || error "Failed to download nova-daemon"
chmod 755 /usr/bin/nova-daemon

info "Downloading novactl..."
curl -fsSL -o /usr/bin/novactl "${BINARY_BASE}/novactl" || error "Failed to download novactl"
chmod 755 /usr/bin/novactl

# Kernel
info "Downloading vmlinux-5.10..."
mkdir -p /opt/nova
curl -fsSL -o /opt/nova/vmlinux-5.10 "${BINARY_BASE}/vmlinux-5.10" || warn "Kernel download failed. Set daemon.kernel in nova.toml manually."

# Config
info "Creating default config..."
mkdir -p /etc/nova
if [ ! -f /etc/nova/nova.toml ]; then
    cat > /etc/nova/nova.toml << 'TOML'
[daemon]
socket = "/run/nova/nova.sock"
image_dir = "/var/lib/nova/images"
kernel = "/opt/nova/vmlinux-5.10"

[sensor]
events_log = "/var/run/nova/events.jsonl"

[policy]
admission_enabled = false
enforcement_enabled = false
TOML
else
    warn "/etc/nova/nova.toml already exists, skipping."
fi

# Directories
mkdir -p /var/lib/nova/images/snapshots
mkdir -p /var/run/nova
mkdir -p /var/lib/nova/policy/bundles

# Systemd service
if command -v systemctl &> /dev/null; then
    info "Installing systemd service..."
    cat > /lib/systemd/system/novavm.service << 'SERVICE'
[Unit]
Description=NovaVM Daemon
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/nova-daemon --config /etc/nova/nova.toml
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info
ProtectHome=yes
ReadWritePaths=/var/lib/nova /var/run/nova /run/nova

[Install]
WantedBy=multi-user.target
SERVICE
    systemctl daemon-reload
fi

# Check KVM
if [ ! -e /dev/kvm ]; then
    warn "/dev/kvm not found. NovaVM requires KVM support."
    warn "On WSL2: Set-VMProcessor -VMName WSL -ExposeVirtualizationExtensions \$true"
fi

echo ""
info "NovaVM v${VERSION} installed successfully!"
echo ""
echo "  Start:   sudo systemctl start novavm"
echo "  Run:     novactl run nginx:alpine"
echo "  Config:  /etc/nova/nova.toml"
echo "  Logs:    journalctl -u novavm -f"
echo ""
