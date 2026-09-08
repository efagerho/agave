#!/usr/bin/env python3

"""Measure contended CrdsGossip::crds RwLock acquisition latency.

Instrumented parking_lot builds are measured from a failed optimistic try-lock
until acquisition; this includes both userspace spinning and any subsequent
kernel parking. Uninstrumented std builds fall back to the std RwLock
read_contended/write_contended paths. Stable CRDS guard probes record holders
that overlap each wait. Only calls whose RwLock address matches --lock are
recorded. Overlap is not proof of causality: a queued writer can also delay new
readers behind existing readers. Those possible dependencies are reported
separately, not charged as causal wait time to the last overlapping holder.
"""

import argparse
import bisect
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
from crds_lock_profile import caller_lookup_bpf

BPF_PROGRAM = r"""
#include <uapi/linux/ptrace.h>
PROFILE_CALLER_HELPER

#define TARGET_LOCK TARGET_LOCK_VALUE

struct lock_start {
    u64 timestamp_ns;
    u64 caller_ip;
};

struct trace_event {
    u64 start_ns;
    u64 end_ns;
    u64 caller_ip;
    u32 tid;
    u8 kind;
    u8 mode;
};

BPF_HASH(read_starts, u64, struct lock_start, 65536);
BPF_HASH(write_starts, u64, struct lock_start, 65536);
BPF_HASH(read_holders, u64, struct lock_start, 65536);
BPF_HASH(write_holders, u64, struct lock_start, 65536);
BPF_ARRAY(wait_enabled, u32, 1);
BPF_ARRAY(holder_enabled, u32, 1);
BPF_PERCPU_ARRAY(diagnostics, u64, 8);
BPF_PERF_OUTPUT(events);

static __always_inline void increment_diagnostic(u32 index) {
    u64 *value = diagnostics.lookup(&index);
    if (value) {
        (*value)++;
    }
}

static __always_inline int wait_tracing_enabled(void) {
    u32 index = 0;
    u32 *value = wait_enabled.lookup(&index);
    return value && *value;
}

static __always_inline int holder_tracing_enabled(void) {
    u32 index = 0;
    u32 *value = holder_enabled.lookup(&index);
    return value && *value;
}

static __always_inline int record_start(struct pt_regs *ctx, u8 mode) {
    if (!wait_tracing_enabled() || (u64)PT_REGS_PARM1(ctx) != TARGET_LOCK) {
        return 0;
    }

    u64 pid_tgid = bpf_get_current_pid_tgid();
    struct lock_start start = {};
    start.caller_ip = profile_caller(ctx);
    // Exclude the caller lookup from the measured slow-path interval.
    start.timestamp_ns = bpf_ktime_get_ns();

    if (mode == 0) {
        if (read_starts.lookup(&pid_tgid)) {
            increment_diagnostic(0);
        }
        read_starts.update(&pid_tgid, &start);
    } else {
        if (write_starts.lookup(&pid_tgid)) {
            increment_diagnostic(1);
        }
        write_starts.update(&pid_tgid, &start);
    }
    return 0;
}

static __always_inline int record_end(struct pt_regs *ctx, u8 mode) {
    // Capture the endpoint before map/perf-buffer work.
    u64 end_ns = bpf_ktime_get_ns();
    u64 pid_tgid = bpf_get_current_pid_tgid();
    struct lock_start *start;
    struct lock_start copy;

    if (mode == 0) {
        start = read_starts.lookup(&pid_tgid);
    } else {
        start = write_starts.lookup(&pid_tgid);
    }
    // Most slow-path calls in the process are for other locks and therefore
    // intentionally have no entry in these maps.
    if (!start) {
        return 0;
    }
    copy = *start;
    if (mode == 0) {
        read_starts.delete(&pid_tgid);
    } else {
        write_starts.delete(&pid_tgid);
    }

    if (!wait_tracing_enabled()) {
        increment_diagnostic(mode == 0 ? 2 : 3);
    }

    struct trace_event event = {
        .start_ns = copy.timestamp_ns,
        .end_ns = end_ns,
        .caller_ip = copy.caller_ip,
        .tid = (u32)pid_tgid,
        .kind = 0,
        .mode = mode,
    };
    events.perf_submit(ctx, &event, sizeof(event));
    return 0;
}

static __always_inline int record_holder_start(struct pt_regs *ctx, u8 mode) {
    if (!holder_tracing_enabled() || (u64)PT_REGS_PARM1(ctx) != TARGET_LOCK) {
        return 0;
    }

    u64 pid_tgid = bpf_get_current_pid_tgid();
    struct lock_start start = {};
    start.caller_ip = profile_caller(ctx);
    start.timestamp_ns = bpf_ktime_get_ns();

    if (mode == 0) {
        if (read_holders.lookup(&pid_tgid)) {
            increment_diagnostic(4);
        }
        read_holders.update(&pid_tgid, &start);
    } else {
        if (write_holders.lookup(&pid_tgid)) {
            increment_diagnostic(5);
        }
        write_holders.update(&pid_tgid, &start);
    }
    return 0;
}

static __always_inline int record_holder_end(struct pt_regs *ctx, u8 mode) {
    u64 end_ns = bpf_ktime_get_ns();
    if (!holder_tracing_enabled() || (u64)PT_REGS_PARM1(ctx) != TARGET_LOCK) {
        return 0;
    }

    u64 pid_tgid = bpf_get_current_pid_tgid();
    struct lock_start *start;
    struct lock_start copy;
    if (mode == 0) {
        start = read_holders.lookup(&pid_tgid);
    } else {
        start = write_holders.lookup(&pid_tgid);
    }
    if (!start) {
        increment_diagnostic(mode == 0 ? 6 : 7);
        return 0;
    }
    copy = *start;
    if (mode == 0) {
        read_holders.delete(&pid_tgid);
    } else {
        write_holders.delete(&pid_tgid);
    }

    struct trace_event event = {
        .start_ns = copy.timestamp_ns,
        .end_ns = end_ns,
        .caller_ip = copy.caller_ip,
        .tid = (u32)pid_tgid,
        .kind = 1,
        .mode = mode,
    };
    events.perf_submit(ctx, &event, sizeof(event));
    return 0;
}

int trace_read_start(struct pt_regs *ctx) {
    return record_start(ctx, 0);
}

int trace_read_end(struct pt_regs *ctx) {
    return record_end(ctx, 0);
}

int trace_write_start(struct pt_regs *ctx) {
    return record_start(ctx, 1);
}

int trace_write_end(struct pt_regs *ctx) {
    return record_end(ctx, 1);
}

int trace_read_acquired_and_wait_end(struct pt_regs *ctx) {
    record_end(ctx, 0);
    return record_holder_start(ctx, 0);
}

int trace_read_acquired(struct pt_regs *ctx) {
    return record_holder_start(ctx, 0);
}

int trace_read_releasing(struct pt_regs *ctx) {
    return record_holder_end(ctx, 0);
}

int trace_write_acquired_and_wait_end(struct pt_regs *ctx) {
    record_end(ctx, 1);
    return record_holder_start(ctx, 1);
}

int trace_write_acquired(struct pt_regs *ctx) {
    return record_holder_start(ctx, 1);
}

int trace_write_releasing(struct pt_regs *ctx) {
    return record_holder_end(ctx, 1);
}
"""


HISTOGRAM_BOUNDS_NS = [250, 500]
HISTOGRAM_BOUNDS_NS.extend(1_000 * (2**power) for power in range(22))


def parse_args():
    parser = argparse.ArgumentParser(
        description="Measure contended CrdsGossip::crds lock acquisition times"
    )
    parser.add_argument("pid", type=int)
    parser.add_argument("--lock", type=lambda value: int(value, 0), required=True)
    parser.add_argument("--duration", type=float, default=60.0)
    parser.add_argument("--output-prefix", type=Path)
    return parser.parse_args()


def find_symbol(binary, function_name):
    result = subprocess.run(
        ["nm", "-an", binary],
        check=True,
        capture_output=True,
        text=True,
    )
    suffix = f"RwLock{len(function_name)}{function_name}"
    matches = []
    for line in result.stdout.splitlines():
        fields = line.split(maxsplit=2)
        if len(fields) == 3 and fields[2].endswith(suffix):
            matches.append(fields[2])
    if len(matches) != 1:
        raise SystemExit(
            f"expected one std futex RwLock::{function_name} symbol, found "
            f"{len(matches)}"
        )
    return matches[0]


def has_symbol(binary, symbol):
    result = subprocess.run(
        ["nm", "-an", binary],
        check=True,
        capture_output=True,
        text=True,
    )
    return any(
        len(fields := line.split(maxsplit=2)) == 3 and fields[2] == symbol
        for line in result.stdout.splitlines()
    )


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


def percentile(values, fraction):
    index = (len(values) - 1) * fraction
    lower = math.floor(index)
    upper = math.ceil(index)
    if lower == upper:
        return values[lower]
    weight = index - lower
    return values[lower] * (1 - weight) + values[upper] * weight


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


def find_blockers(wait_events, hold_events):
    """Find direct conflicting overlaps, not complete causal blocker chains."""
    holds_by_mode = {}
    for mode in ("read", "write"):
        holds = sorted(
            (
                (hold_id, event)
                for hold_id, event in enumerate(hold_events)
                if event[0] == mode
            ),
            key=lambda item: item[1][3],
        )
        holds_by_mode[mode] = {
            "holds": holds,
            "starts": [event[3] for _, event in holds],
            "max_duration": max(
                (event[4] - event[3] for _, event in holds), default=0
            ),
        }

    blockers_by_wait = []
    last_holder_by_wait = []
    for wait_id, wait in enumerate(wait_events):
        wait_mode, _waiter_ip, waiter_tid, wait_start, wait_end = wait
        conflicting_modes = ("write",) if wait_mode == "read" else ("read", "write")
        blockers = []
        for blocker_mode in conflicting_modes:
            index = holds_by_mode[blocker_mode]
            lower = bisect.bisect_left(
                index["starts"], wait_start - index["max_duration"]
            )
            upper = bisect.bisect_left(index["starts"], wait_end)
            for hold_id, hold in index["holds"][lower:upper]:
                _mode, blocker_ip, blocker_tid, hold_start, hold_end = hold
                if blocker_tid == waiter_tid or hold_end <= wait_start:
                    continue
                overlap = min(wait_end, hold_end) - max(wait_start, hold_start)
                if overlap <= 0:
                    continue
                blockers.append(
                    {
                        "wait_id": wait_id,
                        "hold_id": hold_id,
                        "mode": blocker_mode,
                        "caller_ip": blocker_ip,
                        "tid": blocker_tid,
                        "start_ns": hold_start,
                        "end_ns": hold_end,
                        "overlap_ns": overlap,
                        "active_at_start": hold_start <= wait_start < hold_end,
                    }
                )
        last_holder = max(blockers, key=lambda blocker: blocker["end_ns"], default=None)
        blockers_by_wait.append(blockers)
        last_holder_by_wait.append(last_holder)
    return blockers_by_wait, last_holder_by_wait


def find_reader_queue_dependencies(wait_events, blockers_by_wait):
    """Infer reader -> waiting writer -> reader chains from overlapping intervals.

    A failed try-lock precedes actual queuing, so these are possible dependencies,
    not proof that the writer was already queued. Keep them separate from direct
    overlaps. Multiple chains can cover the same interval; never sum their
    durations as time explained.
    """
    writers = sorted(
        ((index, wait) for index, wait in enumerate(wait_events) if wait[0] == "write"),
        key=lambda item: item[1][3],
    )
    starts = [wait[3] for _, wait in writers]
    max_duration = max((wait[4] - wait[3] for _, wait in writers), default=0)
    dependencies = []
    for wait_id, wait in enumerate(wait_events):
        mode, _, tid, start, end = wait
        if mode != "read":
            continue
        lower = bisect.bisect_left(starts, start - max_duration)
        upper = bisect.bisect_right(starts, start)
        for writer_wait_id, writer in writers[lower:upper]:
            if writer[2] == tid or writer[4] <= start:
                continue
            for holder in blockers_by_wait[writer_wait_id]:
                if holder["mode"] != "read" or holder["tid"] == tid:
                    continue
                overlap_start = max(start, writer[3], holder["start_ns"])
                overlap_end = min(end, writer[4], holder["end_ns"])
                if overlap_start < overlap_end:
                    dependencies.append({
                        "wait_id": wait_id,
                        "writer_wait_id": writer_wait_id,
                        "holder_id": holder["hold_id"],
                        "holder_ip": holder["caller_ip"],
                        "holder_tid": holder["tid"],
                        "overlap_start_ns": overlap_start,
                        "overlap_end_ns": overlap_end,
                    })
    return dependencies


def symbolize_reader_dependencies(dependencies, wait_events, symbols):
    return [
        {
            **dependency,
            "holder_ip": f"0x{dependency['holder_ip']:x}",
            "holder": symbols[dependency["holder_ip"]],
            "waiter": symbols[wait_events[dependency["wait_id"]][1]],
            "via_writer": symbols[wait_events[dependency["writer_wait_id"]][1]],
            "via_writer_tid": wait_events[dependency["writer_wait_id"]][2],
            "overlap_us": (dependency["overlap_end_ns"] - dependency["overlap_start_ns"]) / 1_000,
        }
        for dependency in dependencies
    ]


def main():
    from bcc import BPF

    args = parse_args()
    binary = os.path.realpath(f"/proc/{args.pid}/exe")
    if not os.path.exists(binary):
        raise SystemExit(f"PID {args.pid} does not have a readable executable")
    build_id = get_build_id(binary)

    program = BPF_PROGRAM.replace("TARGET_LOCK_VALUE", f"0x{args.lock:x}ULL")
    caller_helper, skipped_wrappers = caller_lookup_bpf(binary, args.pid)
    program = program.replace("PROFILE_CALLER_HELPER", caller_helper)
    bpf = BPF(text=program)

    marker_wait_probes = has_symbol(binary, "agave_crds_read_lock_contended") and has_symbol(
        binary, "agave_crds_write_lock_contended"
    )
    if marker_wait_probes:
        wait_probe = "parking_lot_try_lock_fallback"
        bpf.attach_uprobe(
            name=binary,
            sym="agave_crds_read_lock_contended",
            fn_name="trace_read_start",
            pid=args.pid,
        )
        bpf.attach_uprobe(
            name=binary,
            sym="agave_crds_write_lock_contended",
            fn_name="trace_write_start",
            pid=args.pid,
        )
        read_acquired_probe = "trace_read_acquired_and_wait_end"
        write_acquired_probe = "trace_write_acquired_and_wait_end"
    else:
        wait_probe = "std_futex_rwlock_slow_path"
        read_symbol = find_symbol(binary, "read_contended")
        write_symbol = find_symbol(binary, "write_contended")
        bpf.attach_uprobe(
            name=binary, sym=read_symbol, fn_name="trace_read_start", pid=args.pid
        )
        bpf.attach_uretprobe(
            name=binary, sym=read_symbol, fn_name="trace_read_end", pid=args.pid
        )
        bpf.attach_uprobe(
            name=binary, sym=write_symbol, fn_name="trace_write_start", pid=args.pid
        )
        bpf.attach_uretprobe(
            name=binary, sym=write_symbol, fn_name="trace_write_end", pid=args.pid
        )
        read_acquired_probe = "trace_read_acquired"
        write_acquired_probe = "trace_write_acquired"
    bpf.attach_uprobe(
        name=binary,
        sym="agave_crds_read_lock_acquired",
        fn_name=read_acquired_probe,
        pid=args.pid,
    )
    bpf.attach_uprobe(
        name=binary,
        sym="agave_crds_read_lock_releasing",
        fn_name="trace_read_releasing",
        pid=args.pid,
    )
    bpf.attach_uprobe(
        name=binary,
        sym="agave_crds_write_lock_acquired",
        fn_name=write_acquired_probe,
        pid=args.pid,
    )
    bpf.attach_uprobe(
        name=binary,
        sym="agave_crds_write_lock_releasing",
        fn_name="trace_write_releasing",
        pid=args.pid,
    )

    wait_enabled = bpf["wait_enabled"]
    holder_enabled = bpf["holder_enabled"]
    enabled_key = wait_enabled.Key(0)
    wait_enabled[enabled_key] = wait_enabled.Leaf(0)
    holder_enabled[enabled_key] = holder_enabled.Leaf(0)
    wait_events = []
    hold_events = []
    lost_events = 0

    def receive_event(_cpu, data, _size):
        event = bpf["events"].event(data)
        output = wait_events if event.kind == 0 else hold_events
        output.append(
            (
                "read" if event.mode == 0 else "write",
                int(event.caller_ip),
                int(event.tid),
                int(event.start_ns),
                int(event.end_ns),
            )
        )

    def receive_lost(_cpu, count):
        nonlocal lost_events
        lost_events += count

    bpf["events"].open_perf_buffer(
        receive_event, page_cnt=256, lost_cb=receive_lost
    )

    # Populate holder state before enabling wait collection so a guard which
    # predates the measurement window cannot be mistaken for an unattributed
    # wait. Normal CRDS guards are orders of magnitude shorter than this.
    holder_enabled[enabled_key] = holder_enabled.Leaf(1)
    warmup_deadline = time.monotonic() + 0.05
    while time.monotonic() < warmup_deadline:
        bpf.perf_buffer_poll(timeout=10)
    diagnostics_map = bpf["diagnostics"]
    diagnostics_before = [
        diagnostics_map.sum(diagnostics_map.Key(index)).value for index in range(8)
    ]

    wait_enabled[enabled_key] = wait_enabled.Leaf(1)
    started = time.monotonic()
    deadline = started + args.duration
    while time.monotonic() < deadline:
        remaining_ms = max(1, int((deadline - time.monotonic()) * 1_000))
        bpf.perf_buffer_poll(timeout=min(remaining_ms, 250))
    wait_enabled[enabled_key] = wait_enabled.Leaf(0)
    elapsed = time.monotonic() - started

    # Complete waits and holder intervals which began inside the measurement
    # window before disabling holder tracking.
    drain_deadline = time.monotonic() + 0.1
    while time.monotonic() < drain_deadline:
        bpf.perf_buffer_poll(timeout=10)
    holder_enabled[enabled_key] = holder_enabled.Leaf(0)
    for _ in range(4):
        bpf.perf_buffer_poll(timeout=10)

    wait_events.sort(key=lambda event: event[3])
    hold_events.sort(key=lambda event: event[3])
    blockers_by_wait, last_holder_by_wait = find_blockers(wait_events, hold_events)
    reader_dependencies = find_reader_queue_dependencies(wait_events, blockers_by_wait)

    symbols = {}
    caller_ips = {event[1] for event in wait_events}
    caller_ips.update(event[1] for event in hold_events)
    for caller_ip in caller_ips:
        value = bpf.sym(caller_ip, args.pid, show_module=True, show_offset=True)
        symbols[caller_ip] = value.decode("utf-8", errors="replace")
    demangle_symbols(symbols)

    timestamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
    prefix = args.output_prefix or Path(
        f"/tmp/crds-lock-waits-{args.pid}-{timestamp}"
    )
    raw_path = prefix.with_suffix(".csv")
    with raw_path.open("w", newline="") as output:
        writer = csv.writer(output)
        writer.writerow(
            (
                "wait_id",
                "mode",
                "caller_ip",
                "caller",
                "tid",
                "start_ns",
                "end_ns",
                "duration_ns",
                "blocker_count",
                "last_overlapping_holder_mode",
                "last_overlapping_holder_ip",
                "last_overlapping_holder",
                "last_holder_release_to_acquire_ns",
            )
        )
        for wait_id, (mode, caller_ip, tid, start_ns, end_ns) in enumerate(
            wait_events
        ):
            last_holder = last_holder_by_wait[wait_id]
            writer.writerow(
                (
                    wait_id,
                    mode,
                    f"0x{caller_ip:x}",
                    symbols[caller_ip],
                    tid,
                    start_ns,
                    end_ns,
                    end_ns - start_ns,
                    len(blockers_by_wait[wait_id]),
                    last_holder["mode"] if last_holder else "",
                    f"0x{last_holder['caller_ip']:x}" if last_holder else "",
                    symbols[last_holder["caller_ip"]] if last_holder else "",
                    end_ns - last_holder["end_ns"] if last_holder else "",
                )
            )

    blockers_path = prefix.with_suffix(".blockers.csv")
    with blockers_path.open("w", newline="") as output:
        writer = csv.writer(output)
        writer.writerow(
            (
                "wait_id",
                "wait_mode",
                "waiter_ip",
                "waiter",
                "waiter_tid",
                "wait_duration_ns",
                "blocker_mode",
                "blocker_ip",
                "blocker",
                "blocker_tid",
                "blocker_start_ns",
                "blocker_end_ns",
                "overlap_ns",
                "active_at_wait_start",
                "last_overlapping_holder",
            )
        )
        for wait_id, wait in enumerate(wait_events):
            wait_mode, waiter_ip, waiter_tid, wait_start, wait_end = wait
            last_holder = last_holder_by_wait[wait_id]
            for blocker in blockers_by_wait[wait_id]:
                writer.writerow(
                    (
                        wait_id,
                        wait_mode,
                        f"0x{waiter_ip:x}",
                        symbols[waiter_ip],
                        waiter_tid,
                        wait_end - wait_start,
                        blocker["mode"],
                        f"0x{blocker['caller_ip']:x}",
                        symbols[blocker["caller_ip"]],
                        blocker["tid"],
                        blocker["start_ns"],
                        blocker["end_ns"],
                        blocker["overlap_ns"],
                        blocker["active_at_start"],
                        blocker is last_holder,
                    )
                )

    by_mode = defaultdict(list)
    by_callsite = defaultdict(list)
    all_values = []
    for mode, caller_ip, _tid, start_ns, end_ns in wait_events:
        duration_ns = end_ns - start_ns
        all_values.append(duration_ns)
        by_mode[mode].append(duration_ns)
        by_callsite[(mode, caller_ip)].append(duration_ns)

    blocker_groups = {}
    last_holder_groups = defaultdict(lambda: {"waits": [], "release_gaps": []})
    unattributed_by_mode = defaultdict(list)
    for wait_id, wait in enumerate(wait_events):
        wait_mode, _waiter_ip, _waiter_tid, wait_start, wait_end = wait
        duration_ns = wait_end - wait_start
        blockers = blockers_by_wait[wait_id]
        last_holder = last_holder_by_wait[wait_id]
        if not blockers:
            unattributed_by_mode[wait_mode].append(duration_ns)
        for blocker in blockers:
            key = (wait_mode, blocker["mode"], blocker["caller_ip"])
            group = blocker_groups.setdefault(
                key,
                {
                    "holder_instances": 0,
                    "waits": {},
                    "overlaps": {},
                    "initial_waits": set(),
                    "last_holder_waits": set(),
                },
            )
            group["holder_instances"] += 1
            group["waits"][wait_id] = duration_ns
            group["overlaps"][wait_id] = max(
                group["overlaps"].get(wait_id, 0), blocker["overlap_ns"]
            )
            if blocker["active_at_start"]:
                group["initial_waits"].add(wait_id)
            if blocker is last_holder:
                group["last_holder_waits"].add(wait_id)
        if last_holder:
            key = (wait_mode, last_holder["mode"], last_holder["caller_ip"])
            last_holder_groups[key]["waits"].append(duration_ns)
            last_holder_groups[key]["release_gaps"].append(wait_end - last_holder["end_ns"])

    diagnostics = []
    for index in range(8):
        value = diagnostics_map.sum(diagnostics_map.Key(index)).value
        diagnostics.append(max(0, value - diagnostics_before[index]))

    report = {
        "schema_version": 2,
        "attribution_notes": [
            "Holder rankings summarize directly conflicting overlaps, not causal delay.",
            "last_overlapping_holder replaces the misleading terminal_blocker fields in schema 1.",
            "Inferred reader dependencies require a potentially queued writer; queue state is not traced.",
            "Inferred dependency intervals may overlap; their durations must not be summed.",
            "Guard markers run while locked and perturb timing; marker end precedes actual unlock.",
        ],
        "pid": args.pid,
        "binary": binary,
        "build_id": build_id,
        "caller_lookup": "frame_pointer_skip_known_wrappers",
        "skipped_wrapper_symbols": skipped_wrappers,
        "wait_probe": wait_probe,
        "lock": f"0x{args.lock:x}",
        "duration_seconds": elapsed,
        "lost_events": lost_events,
        "diagnostics": {
            "nested_read_slow_paths": diagnostics[0],
            "nested_write_slow_paths": diagnostics[1],
            "read_completions_after_stop": diagnostics[2],
            "write_completions_after_stop": diagnostics[3],
            "nested_read_holds": diagnostics[4],
            "nested_write_holds": diagnostics[5],
            "unmatched_read_releases": diagnostics[6],
            "unmatched_write_releases": diagnostics[7],
        },
        "holder_events": len(hold_events),
        "attributed_waits": sum(bool(blockers) for blockers in blockers_by_wait),
        "unattributed_waits": sum(not blockers for blockers in blockers_by_wait),
        "groups": [],
        "callsites": [],
        "blockers": [],
        "last_overlapping_holders": [],
        "inferred_reader_dependencies": symbolize_reader_dependencies(
            reader_dependencies, wait_events, symbols
        ),
        "unattributed": [],
        "longest_waits": [],
    }
    if all_values:
        report["groups"].append({"mode": "combined", **summarize(all_values)})
    for mode, values in sorted(by_mode.items()):
        report["groups"].append({"mode": mode, **summarize(values)})
    for (mode, caller_ip), values in sorted(by_callsite.items()):
        report["callsites"].append(
            {
                "mode": mode,
                "caller_ip": f"0x{caller_ip:x}",
                "caller": symbols[caller_ip],
                **summarize(values),
            }
        )
    for (wait_mode, blocker_mode, blocker_ip), group in blocker_groups.items():
        report["blockers"].append(
            {
                "wait_mode": wait_mode,
                "blocker_mode": blocker_mode,
                "blocker_ip": f"0x{blocker_ip:x}",
                "blocker": symbols[blocker_ip],
                "holder_instances": group["holder_instances"],
                "distinct_waits": len(group["waits"]),
                "initial_waits": len(group["initial_waits"]),
                "last_holder_waits": len(group["last_holder_waits"]),
                "waits": summarize(list(group["waits"].values())),
                "overlaps": summarize(list(group["overlaps"].values())),
            }
        )
    report["blockers"].sort(
        key=lambda group: (
            -group["last_holder_waits"],
            -group["distinct_waits"],
            group["blocker"],
        )
    )
    for (wait_mode, blocker_mode, blocker_ip), group in last_holder_groups.items():
        report["last_overlapping_holders"].append(
            {
                "wait_mode": wait_mode,
                "blocker_mode": blocker_mode,
                "blocker_ip": f"0x{blocker_ip:x}",
                "blocker": symbols[blocker_ip],
                "waits": summarize(group["waits"]),
                "release_to_acquire": summarize(group["release_gaps"]),
            }
        )
    report["last_overlapping_holders"].sort(
        key=lambda group: (-group["waits"]["count"], group["blocker"])
    )
    for mode, values in sorted(unattributed_by_mode.items()):
        report["unattributed"].append({"mode": mode, **summarize(values)})

    longest_wait_ids = sorted(
        range(len(wait_events)),
        key=lambda wait_id: wait_events[wait_id][4] - wait_events[wait_id][3],
        reverse=True,
    )[:20]
    for wait_id in longest_wait_ids:
        mode, waiter_ip, waiter_tid, wait_start, wait_end = wait_events[wait_id]
        last_holder = last_holder_by_wait[wait_id]
        report["longest_waits"].append(
            {
                "wait_id": wait_id,
                "mode": mode,
                "waiter_ip": f"0x{waiter_ip:x}",
                "waiter": symbols[waiter_ip],
                "waiter_tid": waiter_tid,
                "duration_us": (wait_end - wait_start) / 1_000,
                "blocker_count": len(blockers_by_wait[wait_id]),
                "last_overlapping_holder_mode": last_holder["mode"] if last_holder else None,
                "last_overlapping_holder_ip": (
                    f"0x{last_holder['caller_ip']:x}" if last_holder else None
                ),
                "last_overlapping_holder": (
                    symbols[last_holder["caller_ip"]] if last_holder else None
                ),
                "last_holder_release_to_acquire_us": (
                    (wait_end - last_holder["end_ns"]) / 1_000 if last_holder else None
                ),
            }
        )

    json_path = prefix.with_suffix(".json")
    with json_path.open("w") as output:
        json.dump(report, output, indent=2)
        output.write("\n")

    print(
        json.dumps(
            {
                "raw_csv": str(raw_path),
                "blockers_csv": str(blockers_path),
                "report_json": str(json_path),
                "schema_version": report["schema_version"],
                "attribution_notes": report["attribution_notes"],
                "inferred_reader_dependency_count": len(reader_dependencies),
                "build_id": build_id,
                "wait_events": len(wait_events),
                "holder_events": len(hold_events),
                "lost_events": lost_events,
                "duration_seconds": elapsed,
                "diagnostics": report["diagnostics"],
                "attributed_waits": report["attributed_waits"],
                "unattributed_waits": report["unattributed_waits"],
                "groups": [
                    {key: value for key, value in group.items() if key != "histogram"}
                    for group in report["groups"]
                ],
                "last_overlapping_holders": [
                    {
                        "wait_mode": group["wait_mode"],
                        "blocker_mode": group["blocker_mode"],
                        "blocker": group["blocker"],
                        "waits": {
                            key: value
                            for key, value in group["waits"].items()
                            if key != "histogram"
                        },
                    }
                    for group in report["last_overlapping_holders"]
                ],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
