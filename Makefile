TARGET = x86_64-unknown-linux-gnu
RELEASE_DIR = target/$(TARGET)/release
VERSION = 0.1.0

.PHONY: build ebpf agent install deb clean all

# Build daemon + CLI
build:
	cargo build --release --target $(TARGET)

# Build eBPF programs (requires nightly + bpf target)
ebpf:
	cd crates/nova-eye-ebpf && \
	cargo +nightly build -Z build-std=core --target bpfel-unknown-none --release

# Build guest agent (static musl binary)
agent:
	cd crates/nova-eye-agent && \
	cargo build --target x86_64-unknown-linux-musl --release

# Build everything
all: build ebpf agent

# Install to system
install: build
	install -Dm755 $(RELEASE_DIR)/nova-daemon /usr/bin/nova-daemon
	install -Dm755 $(RELEASE_DIR)/novactl /usr/bin/novactl
	install -Dm644 config/nova.toml /etc/nova/nova.toml
	install -Dm644 novavm.service /lib/systemd/system/novavm.service
	mkdir -p /opt/nova /var/lib/nova/images/snapshots /var/run/nova
	@test -f tests/fixtures/vmlinux-5.10 && \
		install -Dm644 tests/fixtures/vmlinux-5.10 /opt/nova/vmlinux-5.10 || \
		echo "WARNING: kernel not found, set daemon.kernel in /etc/nova/nova.toml"
	@command -v systemctl >/dev/null && systemctl daemon-reload || true
	@echo ""
	@echo "Installed. Run: sudo systemctl start novavm"

# Build .deb package
deb:
	bash scripts/build-deb.sh

# Clean build artifacts
clean:
	cargo clean
