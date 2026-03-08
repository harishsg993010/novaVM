# Networking

NovaVM uses virtio-net with a TAP device to provide guest networking.

## Architecture

```
Guest VM                    Host
  eth0 (virtio-net)          tap0 (TAP)
  IP: 172.16.0.2/24          IP: 172.16.0.1/24
       |                          |
       +--- virtio queues --------+
                                  |
                              iptables NAT
                                  |
                              eth0 (internet)
```

## Setup

### 1. Create TAP Device

```bash
sudo bash scripts/setup-network.sh
```

This script:
- Creates TAP device `tap0`
- Assigns IP `172.16.0.1/24`
- Enables IP forwarding (`/proc/sys/net/ipv4/ip_forward`)

### 2. Add NAT Rules (for internet access from guest)

```bash
# Find your outbound interface
ip route | grep default
# e.g., "default via 172.20.0.1 dev eth0"

# Add masquerade rule
sudo iptables -t nat -A POSTROUTING -s 172.16.0.0/24 -o eth0 -j MASQUERADE
sudo iptables -A FORWARD -i tap0 -o eth0 -j ACCEPT
sudo iptables -A FORWARD -i eth0 -o tap0 -m state --state RELATED,ESTABLISHED -j ACCEPT
```

### 3. Configure Daemon

Add `tap_device` to `/etc/nova/nova.toml`:

```toml
[daemon]
socket = "/run/nova/nova.sock"
image_dir = "/var/lib/nova/images"
kernel = "/opt/nova/vmlinux"
api_port = 9800
tap_device = "tap0"          # <-- enable networking
```

### 4. Restart Daemon

```bash
sudo RUST_LOG=info nova serve --config /etc/nova/nova.toml
```

## Guest Network Configuration

NovaVM's init script automatically configures the guest:

```bash
# Inside the guest (done automatically):
ip addr add 172.16.0.2/24 dev eth0
ip link set eth0 up
ip route add default via 172.16.0.1
```

The guest IP is always `172.16.0.2` and the gateway is `172.16.0.1` (host TAP).

## Accessing Guest Services

Once networking is configured:

```bash
# Run nginx
nova run nginx:alpine --name web

# Access from host
curl http://172.16.0.2:80

# Access from guest
nova exec web curl http://172.16.0.1  # reach host
nova exec web ping 8.8.8.8            # reach internet (if NAT configured)
```

## How It Works

1. **TAP device** — The daemon opens `/dev/net/tun` and creates a TAP interface
2. **Virtio-net** — Guest kernel sees a virtio NIC (`eth0`)
3. **TX path** — Guest writes to virtio TX queue -> daemon reads descriptor chain -> strips vnet header -> writes raw frame to TAP
4. **RX path** — Daemon polls TAP fd (non-blocking) -> reads frame -> prepends vnet header -> writes to virtio RX queue -> injects IRQ
5. **IRQ injection** — Uses `KVM_IRQFD` (eventfd registered with KVM) for efficient interrupt delivery

## Virtio-Net Features

| Feature | Status |
|---|---|
| MAC address | Configurable (default: `52:54:00:12:34:56`) |
| Link status | Always UP |
| TX/RX | Real packet I/O via TAP |
| Checksum offload | Not implemented (guest does checksums) |
| TSO/GSO | Not implemented |
| Mergeable RX buffers | Not implemented |
| Multi-queue | Not implemented (single RX + TX) |

## Troubleshooting

**No network in guest:**
```bash
# Check TAP exists
ip link show tap0

# Check daemon opened TAP
# Look for "TAP fd assigned to net device" in daemon logs

# Check guest eth0
nova exec <sandbox> ip addr show eth0
```

**Can't reach internet from guest:**
```bash
# Check IP forwarding
cat /proc/sys/net/ipv4/ip_forward  # should be 1

# Check NAT rule
sudo iptables -t nat -L POSTROUTING -v

# Check routing
nova exec <sandbox> ip route
```

**Packets sent but not received:**
```bash
# Check TAP is up
ip link show tap0  # should show UP

# Monitor TAP traffic
sudo tcpdump -i tap0 -nn
```
