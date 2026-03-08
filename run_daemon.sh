#!/bin/bash
# Run nova-daemon with proper output capture
exec nova-daemon --config /etc/nova/nova.toml > /tmp/nova-daemon.log 2>&1
