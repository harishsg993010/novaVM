#!/usr/bin/env python3
import json, sys, collections

events = [json.loads(l) for l in sys.stdin if l.strip()]
base_ts = events[0]["timestamp_ns"] if events else 0

types = collections.Counter(e["event_type"] for e in events)
comms = collections.Counter(e["comm"] for e in events)

print(f"=== nova-eye JSOND Event Log: {len(events)} real eBPF events ===")
print()

print("--- Event Type Distribution ---")
for t, c in types.most_common():
    print(f"  {t:<20} {c:>5}")
print()

print("--- Top Processes ---")
for comm, c in comms.most_common(15):
    print(f"  {comm:<20} {c:>5}")
print()

print("--- Process Exec Events (chronological) ---")
execs = [e for e in events if e["event_type"] == "process_exec"]
for i, e in enumerate(execs[:25]):
    rel_ms = (e["timestamp_ns"] - base_ts) / 1e6
    pid = e["pid"]
    uid = e["uid"]
    comm = e["comm"]
    print(f"  [{i+1:>2}] +{rel_ms:>10.1f}ms  pid={pid:>6}  uid={uid}  {comm}")
if len(execs) > 25:
    print(f"  ... ({len(execs)} total)")
print()

print("--- File Open Events (first 15) ---")
files = [e for e in events if e["event_type"] == "file_open"]
for i, e in enumerate(files[:15]):
    rel_ms = (e["timestamp_ns"] - base_ts) / 1e6
    pid = e["pid"]
    comm = e["comm"]
    print(f"  [{i+1:>2}] +{rel_ms:>10.1f}ms  pid={pid:>6}  {comm}")
if len(files) > 15:
    print(f"  ... ({len(files)} total)")
print()

net = [e for e in events if e["event_type"] == "net_connect"]
if net:
    print("--- Net Connect Events ---")
    for i, e in enumerate(net[:10]):
        rel_ms = (e["timestamp_ns"] - base_ts) / 1e6
        pid = e["pid"]
        comm = e["comm"]
        print(f"  [{i+1:>2}] +{rel_ms:>10.1f}ms  pid={pid:>6}  {comm}")
    print()
