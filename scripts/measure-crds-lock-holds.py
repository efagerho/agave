#!/usr/bin/env python3

import argparse
import csv
import json
import math
import os
import pwd
import shutil
import statistics
import subprocess
import time
from collections import defaultdict
from pathlib import Path

from bcc import BPF
from crds_lock_profile import caller_lookup_bpf


BPF_PROGRAM = r"""
#include <uapi/linux/ptrace.h>
PROFILE_CALLER_HELPER

struct lock_key {
    u64 pid_tgid;
    u64 lock;
};

struct lock_start {
    u64 timestamp_ns;
    u64 caller_ip;
};

struct lock_event {
    u64 duration_ns;
    u64 lock;
    u64 caller_ip;
    u32 tid;
    u8 mode;
};

BPF_HASH(read_starts, struct lock_key, struct lock_start, 65536);
BPF_HASH(write_starts, struct lock_key, struct lock_start, 65536);
BPF_ARRAY(enabled, u32, 1);
BPF_PERCPU_ARRAY(diagnostics, u64, 4);
BPF_PERF_OUTPUT(events);

static __always_inline void increment_diagnostic(u32 index) {
    u64 *value = diagnostics.lookup(&index);
    if (value) {
        (*value)++;
    }
}

static __always_inline int tracing_enabled(void) {
    u32 index = 0;
    u32 *value = enabled.lookup(&index);
    return value && *value;
}

int trace_read_acquired(struct pt_regs *ctx) {
    if (!tracing_enabled()) {
        return 0;
    }
    void *lock = (void *)PT_REGS_PARM1(ctx);
    struct lock_key key = {
        .pid_tgid = bpf_get_current_pid_tgid(),
        .lock = (u64)lock,
    };
    if (read_starts.lookup(&key)) {
        increment_diagnostic(0);
    }
    struct lock_start start = {};
    start.caller_ip = profile_caller(ctx);
    // Take this timestamp last so reading the call site is excluded. Only the
    // hash update below occurs inside the measured interval.
    start.timestamp_ns = bpf_ktime_get_ns();
    read_starts.update(&key, &start);
    return 0;
}

int trace_read_releasing(struct pt_regs *ctx) {
    // Capture the end before map lookup and perf-buffer submission. Probe work
    // after this point is excluded from the reported hold duration.
    u64 end_ns = bpf_ktime_get_ns();
    if (!tracing_enabled()) {
        return 0;
    }
    void *lock = (void *)PT_REGS_PARM1(ctx);
    struct lock_key key = {
        .pid_tgid = bpf_get_current_pid_tgid(),
        .lock = (u64)lock,
    };
    struct lock_start *start = read_starts.lookup(&key);
    if (!start) {
        increment_diagnostic(2);
        return 0;
    }
    struct lock_event event = {
        .duration_ns = end_ns - start->timestamp_ns,
        .lock = (u64)lock,
        .caller_ip = start->caller_ip,
        .tid = (u32)key.pid_tgid,
        .mode = 0,
    };
    events.perf_submit(ctx, &event, sizeof(event));
    read_starts.delete(&key);
    return 0;
}

int trace_write_acquired(struct pt_regs *ctx) {
    if (!tracing_enabled()) {
        return 0;
    }
    void *lock = (void *)PT_REGS_PARM1(ctx);
    struct lock_key key = {
        .pid_tgid = bpf_get_current_pid_tgid(),
        .lock = (u64)lock,
    };
    if (write_starts.lookup(&key)) {
        increment_diagnostic(1);
    }
    struct lock_start start = {};
    start.caller_ip = profile_caller(ctx);
    start.timestamp_ns = bpf_ktime_get_ns();
    write_starts.update(&key, &start);
    return 0;
}

int trace_write_releasing(struct pt_regs *ctx) {
    u64 end_ns = bpf_ktime_get_ns();
    if (!tracing_enabled()) {
        return 0;
    }
    void *lock = (void *)PT_REGS_PARM1(ctx);
    struct lock_key key = {
        .pid_tgid = bpf_get_current_pid_tgid(),
        .lock = (u64)lock,
    };
    struct lock_start *start = write_starts.lookup(&key);
    if (!start) {
        increment_diagnostic(3);
        return 0;
    }
    struct lock_event event = {
        .duration_ns = end_ns - start->timestamp_ns,
        .lock = (u64)lock,
        .caller_ip = start->caller_ip,
        .tid = (u32)key.pid_tgid,
        .mode = 1,
    };
    events.perf_submit(ctx, &event, sizeof(event));
    write_starts.delete(&key);
    return 0;
}
"""


HISTOGRAM_BOUNDS_NS = [250, 500]
HISTOGRAM_BOUNDS_NS.extend(1_000 * (2**power) for power in range(21))


def parse_args():
    parser = argparse.ArgumentParser(
        description="Measure CrdsGossip::crds read/write lock hold times"
    )
    parser.add_argument("pid", type=int)
    parser.add_argument("--duration", type=float, default=60.0)
    parser.add_argument("--output-prefix", type=Path)
    return parser.parse_args()


def get_build_id(binary):
    result = subprocess.run(
        ["readelf", "-n", binary],
        check=True,
        capture_output=True,
        text=True,
    )
    for line in result.stdout.splitlines():
        if "Build ID:" in line:
            return line.split("Build ID:", 1)[1].strip()
    raise SystemExit(f"could not read build ID from {binary}")


def demangle_symbols(symbols):
    if not symbols:
        return
    rustfilt = shutil.which("rustfilt")
    if rustfilt is None and (sudo_user := os.environ.get("SUDO_USER")):
        candidate = Path(pwd.getpwnam(sudo_user).pw_dir) / ".cargo/bin/rustfilt"
        if candidate.is_file():
            rustfilt = str(candidate)
    if rustfilt is None:
        return
    addresses, names = zip(*symbols.items())
    try:
        result = subprocess.run(
            [rustfilt],
            check=True,
            capture_output=True,
            input="\n".join(names) + "\n",
            text=True,
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        return
    demangled = result.stdout.splitlines()
    if len(demangled) == len(addresses):
        symbols.update(zip(addresses, demangled))


def percentile(values, percentile):
    if not values:
        return None
    index = (len(values) - 1) * percentile
    lower = math.floor(index)
    upper = math.ceil(index)
    if lower == upper:
        return values[lower]
    fraction = index - lower
    return values[lower] * (1 - fraction) + values[upper] * fraction


def histogram(values):
    counts = [0] * (len(HISTOGRAM_BOUNDS_NS) + 1)
    for value in values:
        for index, upper_bound in enumerate(HISTOGRAM_BOUNDS_NS):
            if value < upper_bound:
                counts[index] += 1
                break
        else:
            counts[-1] += 1

    buckets = []
    lower_bound = 0
    for upper_bound, count in zip(HISTOGRAM_BOUNDS_NS, counts):
        buckets.append(
            {
                "lower_us": lower_bound / 1_000,
                "upper_us": upper_bound / 1_000,
                "count": count,
            }
        )
        lower_bound = upper_bound
    buckets.append(
        {
            "lower_us": lower_bound / 1_000,
            "upper_us": None,
            "count": counts[-1],
        }
    )
    return buckets


def summarize(values):
    ordered = sorted(values)
    return {
        "count": len(ordered),
        "mean_us": statistics.fmean(ordered) / 1_000,
        "median_us": statistics.median(ordered) / 1_000,
        "p95_us": percentile(ordered, 0.95) / 1_000,
        "p99_us": percentile(ordered, 0.99) / 1_000,
        "max_us": ordered[-1] / 1_000,
        "histogram": histogram(ordered),
    }


def main():
    args = parse_args()
    binary = os.path.realpath(f"/proc/{args.pid}/exe")
    if not os.path.exists(binary):
        raise SystemExit(f"PID {args.pid} does not have a readable executable")
    build_id = get_build_id(binary)

    timestamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
    prefix = args.output_prefix or Path(
        f"/tmp/crds-lock-holds-{args.pid}-{timestamp}"
    )
    events = []
    lost_events = 0

    caller_helper, skipped_wrappers = caller_lookup_bpf(binary, args.pid)
    bpf = BPF(text=BPF_PROGRAM.replace("PROFILE_CALLER_HELPER", caller_helper))
    probes = (
        ("agave_crds_read_lock_acquired", "trace_read_acquired"),
        ("agave_crds_read_lock_releasing", "trace_read_releasing"),
        ("agave_crds_write_lock_acquired", "trace_write_acquired"),
        ("agave_crds_write_lock_releasing", "trace_write_releasing"),
    )
    for symbol, function in probes:
        bpf.attach_uprobe(name=binary, sym=symbol, fn_name=function, pid=args.pid)

    enabled = bpf["enabled"]
    enabled_key = enabled.Key(0)
    enabled[enabled_key] = enabled.Leaf(0)

    def receive_event(_cpu, data, _size):
        event = bpf["events"].event(data)
        events.append(
            (
                "read" if event.mode == 0 else "write",
                int(event.lock),
                int(event.caller_ip),
                int(event.tid),
                int(event.duration_ns),
            )
        )

    def receive_lost(_cpu, count):
        nonlocal lost_events
        lost_events += count

    bpf["events"].open_perf_buffer(
        receive_event, page_cnt=256, lost_cb=receive_lost
    )

    enabled[enabled_key] = enabled.Leaf(1)
    started = time.monotonic()
    deadline = started + args.duration
    while time.monotonic() < deadline:
        remaining_ms = max(1, int((deadline - time.monotonic()) * 1_000))
        bpf.perf_buffer_poll(timeout=min(remaining_ms, 250))
    enabled[enabled_key] = enabled.Leaf(0)
    elapsed = time.monotonic() - started
    for _ in range(4):
        bpf.perf_buffer_poll(timeout=10)

    symbols = {}
    for caller_ip in {event[2] for event in events}:
        symbol = bpf.sym(caller_ip, args.pid, show_module=True, show_offset=True)
        symbols[caller_ip] = symbol.decode("utf-8", errors="replace")
    demangle_symbols(symbols)

    raw_path = prefix.with_suffix(".csv")
    with raw_path.open("w", newline="") as output:
        writer = csv.writer(output)
        writer.writerow(
            ("mode", "lock", "caller_ip", "caller", "tid", "duration_ns")
        )
        for mode, lock, caller_ip, tid, duration_ns in events:
            writer.writerow(
                (
                    mode,
                    f"0x{lock:x}",
                    f"0x{caller_ip:x}",
                    symbols[caller_ip],
                    tid,
                    duration_ns,
                )
            )

    by_lock = defaultdict(list)
    by_callsite = defaultdict(list)
    for mode, lock, caller_ip, _tid, duration_ns in events:
        by_lock[(mode, lock)].append(duration_ns)
        by_callsite[(mode, lock, caller_ip)].append(duration_ns)

    diagnostics = []
    diagnostics_map = bpf["diagnostics"]
    for index in range(4):
        key = diagnostics_map.Key(index)
        diagnostics.append(diagnostics_map.sum(key).value)

    report = {
        "pid": args.pid,
        "binary": binary,
        "build_id": build_id,
        "caller_lookup": "frame_pointer_skip_known_wrappers",
        "skipped_wrapper_symbols": skipped_wrappers,
        "duration_seconds": elapsed,
        "lost_events": lost_events,
        "diagnostics": {
            "nested_read_acquisitions": diagnostics[0],
            "nested_write_acquisitions": diagnostics[1],
            "unmatched_read_releases": diagnostics[2],
            "unmatched_write_releases": diagnostics[3],
        },
        "locks": [],
        "callsites": [],
    }
    for (mode, lock), values in sorted(by_lock.items()):
        report["locks"].append(
            {"mode": mode, "lock": f"0x{lock:x}", **summarize(values)}
        )
    for (mode, lock, caller_ip), values in sorted(by_callsite.items()):
        report["callsites"].append(
            {
                "mode": mode,
                "lock": f"0x{lock:x}",
                "caller_ip": f"0x{caller_ip:x}",
                "caller": symbols[caller_ip],
                **summarize(values),
            }
        )

    json_path = prefix.with_suffix(".json")
    with json_path.open("w") as output:
        json.dump(report, output, indent=2)
        output.write("\n")

    print(json.dumps({
        "raw_csv": str(raw_path),
        "report_json": str(json_path),
        "build_id": build_id,
        "events": len(events),
        "lost_events": lost_events,
        "duration_seconds": elapsed,
        "diagnostics": report["diagnostics"],
        "locks": [
            {key: value for key, value in lock.items() if key != "histogram"}
            for lock in report["locks"]
        ],
    }, indent=2))


if __name__ == "__main__":
    main()
