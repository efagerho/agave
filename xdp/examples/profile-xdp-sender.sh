#!/usr/bin/env bash
# Profile the agave-xdp `xdp-sender` example under perf, replicating the
# methodology used to produce fast-socket-rs's `xdp-blast-baseline-3`:
#
#   - 60s blast at 64-byte payload to 213.239.141.11:41000 from
#     213.239.141.12:52000 over bond0
#   - ethtool -S snapshot of each bond slave before and after
#   - perf record at 999 Hz with frame-pointer call graphs
#   - perf script + perf report (children, self, dso)
#   - flamegraph via inferno or stackcollapse-perf.pl
#
# The agave-xdp example takes `--cpus N[,N..]` instead of `--all-queues`;
# the original benchmark coalesced 64 queues onto 32 CPUs, so the default
# CPUS list here covers every online CPU (0..nproc-1). Override with
# `CPUS=` to match a specific layout.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: xdp/examples/profile-xdp-sender.sh

Environment overrides (defaults match fast-socket-rs xdp-blast-baseline-3):
  IFACE=bond0
  LOCAL=213.239.141.12:52000
  TARGET=213.239.141.11:41000
  PAYLOAD_LEN=64
  DURATION_MS=60000      use PROFILE_SECONDS=60 to set in seconds
  CPUS=0,1,...,N-1       defaults to every online CPU; format is CSV ints
  ZERO_COPY=0            set to 1 to pass --zero-copy
  STATS_IFACES="enp1s0f0np0 enp1s0f1np1"
                         bond0 slaves; override per host
  ETHTOOL=ethtool
  PERF=perf
  PERF_SUDO=sudo         skipped when already root
  PERF_FREQ=999
  CALL_GRAPH=fp
  PERF_DELAY_MS=         optional perf record -D value
  OUT_DIR=bench-results/profiles/<UTC>-xdp-sender-blast
  FORCE_FRAME_POINTERS=1 add -C force-frame-pointers=yes to RUSTFLAGS

Requires sudo perf unless run as root. Flamegraph conversion uses
inferno-collapse-perf/inferno-flamegraph or stackcollapse-perf.pl/
flamegraph.pl if either is on PATH.
EOF
}

for arg in "$@"; do
  case "$arg" in
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; usage >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# --- benchmark parameters ----------------------------------------------------
PROFILE_SECONDS="${PROFILE_SECONDS:-60}"
DURATION_MS="${DURATION_MS:-$((PROFILE_SECONDS * 1000))}"
PAYLOAD_LEN="${PAYLOAD_LEN:-64}"
IFACE="${IFACE:-bond0}"
LOCAL="${LOCAL:-213.239.141.12:52000}"
TARGET="${TARGET:-213.239.141.11:41000}"
ZERO_COPY="${ZERO_COPY:-0}"

if [[ -z "${CPUS:-}" ]]; then
  if ! NPROC="$(getconf _NPROCESSORS_ONLN 2>/dev/null)" || [[ -z "$NPROC" ]]; then
    NPROC="$(nproc)"
  fi
  if [[ "$NPROC" -lt 1 ]]; then
    echo "could not determine online CPU count" >&2
    exit 1
  fi
  CPUS="$(seq -s, 0 "$((NPROC - 1))")"
fi

STATS_IFACES="${STATS_IFACES:-enp1s0f0np0 enp1s0f1np1}"
ETHTOOL="${ETHTOOL:-ethtool}"
PERF="${PERF:-perf}"
PERF_SUDO="${PERF_SUDO:-sudo}"
PERF_SUDO_ARGS=()
PERF_FREQ="${PERF_FREQ:-999}"
CALL_GRAPH="${CALL_GRAPH:-fp}"
PERF_DELAY_MS="${PERF_DELAY_MS:-}"
RUN_UID="$(id -u)"
RUN_GID="$(id -g)"

OUT_DIR_DEFAULT="$ROOT/bench-results/profiles/$(date -u +%Y%m%dT%H%M%SZ)-xdp-sender-blast"
OUT_DIR="${OUT_DIR:-$OUT_DIR_DEFAULT}"

EXAMPLE_BIN="$ROOT/target/release/examples/xdp-sender"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "required command not found: $1" >&2
    exit 1
  fi
}

require_command cargo
require_command "$PERF"
if [[ "$RUN_UID" != "0" ]]; then
  require_command "$PERF_SUDO"
  PERF_SUDO_ARGS=(-n)
  if ! "$PERF_SUDO" "${PERF_SUDO_ARGS[@]}" true >/dev/null 2>&1; then
    echo "$PERF_SUDO cannot run non-interactively; configure NOPASSWD or run as root" >&2
    exit 1
  fi
fi

mkdir -p "$OUT_DIR"

if [[ "${FORCE_FRAME_POINTERS:-1}" == "1" ]]; then
  if [[ -n "${RUSTFLAGS:-}" ]]; then
    if [[ "$RUSTFLAGS" != *force-frame-pointers* ]]; then
      export RUSTFLAGS="$RUSTFLAGS -C force-frame-pointers=yes"
    fi
  else
    export RUSTFLAGS="-C force-frame-pointers=yes"
  fi
fi

echo "building release xdp-sender example"
(
  cd "$ROOT"
  cargo build --release --example xdp-sender \
    -p agave-xdp --features agave-unstable-api
)

if [[ ! -x "$EXAMPLE_BIN" ]]; then
  echo "expected example binary was not built: $EXAMPLE_BIN" >&2
  exit 1
fi

write_run_env() {
  {
    echo "target=$TARGET"
    echo "mode=blast"
    echo "iface=$IFACE"
    echo "local=$LOCAL"
    echo "payload_len=$PAYLOAD_LEN"
    echo "duration_ms=$DURATION_MS"
    echo "cpus=$CPUS"
    echo "zero_copy=$ZERO_COPY"
    echo "perf_freq=$PERF_FREQ"
    echo "call_graph=$CALL_GRAPH"
    echo "stats_ifaces=$STATS_IFACES"
    if [[ -n "$PERF_DELAY_MS" ]]; then
      echo "perf_delay_ms=$PERF_DELAY_MS"
    fi
  } > "$OUT_DIR/run.env"
}

dump_nic_stats() {
  local phase="$1"
  local run_dir="$2"
  local out="$run_dir/nic-stats-$phase.txt"

  {
    echo "# captured_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "# command: $ETHTOOL -S <iface>"

    if [[ -z "$STATS_IFACES" ]]; then
      echo "# no interfaces configured in STATS_IFACES"
    elif ! command -v "$ETHTOOL" >/dev/null 2>&1; then
      echo "# ethtool command not found: $ETHTOOL"
    else
      local iface
      for iface in $STATS_IFACES; do
        echo
        echo "## $iface"
        if "$ETHTOOL" -S "$iface"; then
          :
        else
          local status=$?
          echo "# ethtool -S $iface failed with exit status $status"
        fi
      done
    fi
  } > "$out"
}

record_sender_profile() {
  local perf_data="$1"
  local log="$2"
  shift 2

  local perf_args=(
    record
    -F "$PERF_FREQ"
    --call-graph "$CALL_GRAPH"
    -o "$perf_data"
  )
  if [[ -n "$PERF_DELAY_MS" ]]; then
    perf_args+=(-D "$PERF_DELAY_MS")
  fi
  perf_args+=(-- "$@")

  if [[ "$RUN_UID" == "0" ]]; then
    "$PERF" "${perf_args[@]}" > "$log" 2>&1
    return
  fi

  "$PERF_SUDO" "${PERF_SUDO_ARGS[@]}" "$PERF" "${perf_args[@]}" > "$log" 2>&1
  "$PERF_SUDO" "${PERF_SUDO_ARGS[@]}" chown "$RUN_UID:$RUN_GID" "$perf_data"
}

write_reports() {
  local run_dir="$1"
  local perf_data="$run_dir/perf.data"

  "$PERF" report -i "$perf_data" --stdio > "$run_dir/report-children.txt" 2>/dev/null || true
  "$PERF" report -i "$perf_data" --stdio --no-children > "$run_dir/report-self.txt" 2>/dev/null || true
  "$PERF" report -i "$perf_data" --stdio --sort dso > "$run_dir/report-dso.txt" 2>/dev/null || true
}

convert_flamegraph() {
  local run_dir="$1"
  local perf_data="$run_dir/perf.data"
  local perf_script="$run_dir/perf.script"
  local collapsed="$run_dir/stacks.folded"
  local svg="$run_dir/flamegraph.svg"

  "$PERF" script -i "$perf_data" > "$perf_script"

  if command -v inferno-collapse-perf >/dev/null 2>&1 &&
     command -v inferno-flamegraph >/dev/null 2>&1; then
    inferno-collapse-perf "$perf_script" > "$collapsed"
    inferno-flamegraph "$collapsed" > "$svg"
    echo "wrote $svg"
    return
  fi

  if command -v stackcollapse-perf.pl >/dev/null 2>&1 &&
     command -v flamegraph.pl >/dev/null 2>&1; then
    stackcollapse-perf.pl "$perf_script" > "$collapsed"
    flamegraph.pl "$collapsed" > "$svg"
    echo "wrote $svg"
    return
  fi

  cat > "$run_dir/FLAMEGRAPH_MISSING.txt" <<'EOF'
Flamegraph tools were not found.

Install one of:
  cargo install inferno
  https://github.com/brendangregg/FlameGraph on PATH

Then rerun conversion manually, for example:
  perf script -i perf.data > perf.script
  inferno-collapse-perf perf.script > stacks.folded
  inferno-flamegraph stacks.folded > flamegraph.svg
EOF
  echo "flamegraph tools missing; wrote $perf_script and $run_dir/FLAMEGRAPH_MISSING.txt"
}

sender_args=(
  "$EXAMPLE_BIN"
  --interface "$IFACE"
  --cpus "$CPUS"
  --local "$LOCAL"
  --dest "$TARGET"
  --payload-len "$PAYLOAD_LEN"
  --duration-ms "$DURATION_MS"
)

if [[ "$ZERO_COPY" == "1" ]]; then
  sender_args+=(--zero-copy)
fi

write_run_env

echo "profiling xdp-sender example: ${sender_args[*]}"
dump_nic_stats before "$OUT_DIR"
record_sender_profile "$OUT_DIR/perf.data" "$OUT_DIR/sender.log" "${sender_args[@]}"
dump_nic_stats after "$OUT_DIR"
write_reports "$OUT_DIR"
convert_flamegraph "$OUT_DIR"

echo "results: $OUT_DIR"
