"""Frame-pointer caller lookup shared by the CRDS lock tracers (x86-64)."""

from pathlib import Path
import subprocess


def caller_lookup_bpf(binary, pid):
    # Resolve the ELF load bias, including executables whose executable segment
    # has a different file offset and virtual address. Do not use the text
    # mapping start as the base of symbol-table addresses.
    segments = subprocess.check_output(["readelf", "-lW", binary], text=True)
    first_load = next(
        line.split() for line in segments.splitlines()
        if line.strip().startswith("LOAD") and int(line.split()[1], 16) == 0
    )
    mappings = Path(f"/proc/{pid}/maps").read_text().splitlines()
    mapping = next(
        fields for line in mappings
        if len(fields := line.split(maxsplit=5)) == 6
        and fields[5] == str(binary) and int(fields[2], 16) == 0
    )
    bias = int(mapping[0].split("-")[0], 16) - int(first_load[2], 16)
    symbols = subprocess.check_output(
        ["nm", "-S", "--defined-only", "--demangle", binary], text=True
    )
    wrappers = []
    for line in symbols.splitlines():
        fields = line.split(maxsplit=3)
        if len(fields) != 4 or fields[2].lower() != "t":
            continue
        name = fields[3]
        if not (
            name.startswith("<solana_gossip::crds_rwlock::")
            or name == "<solana_gossip::cluster_info::ClusterInfo>::time_gossip_read_lock"
        ):
            continue
        start, size = int(fields[0], 16) + bias, int(fields[1], 16)
        if size:
            wrappers.append((start, start + size, name))
    condition = " || ".join(
        f"(ip >= {start}ULL && ip < {end}ULL)" for start, end, _ in wrappers
    ) or "0"
    code = r"""
static __always_inline int is_profile_wrapper(u64 ip) {
    return WRAPPER_CONDITION;
}

static __always_inline u64 profile_caller(struct pt_regs *ctx) {
    u64 ip = 0;
    bpf_probe_read_user(&ip, sizeof(ip), (void *)PT_REGS_SP(ctx));
    u64 frame = PT_REGS_FP(ctx);
    // At marker entry, rbp belongs to its caller. Skip only known wrappers;
    // retain the actual acquisition path and its offset, not an arbitrary frame.
    #pragma unroll
    for (int depth = 0; depth < 4; depth++) {
        if (!is_profile_wrapper(ip)) break;
        u64 parent_ip = 0, parent_frame = 0;
        if (bpf_probe_read_user(&parent_ip, sizeof(parent_ip), (void *)(frame + 8)) < 0
            || !parent_ip) break;
        bpf_probe_read_user(&parent_frame, sizeof(parent_frame), (void *)frame);
        ip = parent_ip;
        if (parent_frame <= frame) break;
        frame = parent_frame;
    }
    return ip;
}
""".replace("WRAPPER_CONDITION", condition)
    return code, [name for _, _, name in wrappers]
