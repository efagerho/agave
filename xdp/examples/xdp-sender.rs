//! Replication of `fast-socket-rs`'s `xdp-sender blast` mode on top of the
//! `agave-xdp` transmitter.
//!
//! Single mode: blasts UDP datagrams to a destination as fast as the
//! transmitter accepts them. Each producer thread feeds one TxLoop channel
//! exposed by [`agave_xdp::transmitter::XdpSender`]; the TxLoops themselves
//! are pinned to the CPUs listed in `--cpus` and drive one NIC TX queue each.
//!
//! Differences vs `fast-socket-rs`'s blast:
//!   - No per-queue "all queues" coalescing logic — the agave-xdp transmitter
//!     already spawns one TxLoop per CPU you give it, so `--cpus 0,1,2,3`
//!     is the equivalent of `--all-queues` over four queues.
//!   - Payload is delivered via `bytes::Bytes` (the public API); the TxLoop
//!     copies into a UMEM frame on its side. fast-socket-rs allocates the
//!     UMEM frame directly in the producer — one fewer copy. That's an
//!     intrinsic API difference, not a configurable one.
//!
//! Usage (must run on Linux with CAP_NET_ADMIN + CAP_NET_RAW; CAP_BPF +
//! CAP_PERFMON additionally if --zero-copy is set):
//!
//!   xdp-sender --interface eth0 --cpus 0,1,2,3 \
//!              --local 10.0.0.5:50000 --dest 10.0.0.6:9000 \
//!              --payload-len 1200 --duration-ms 10000
//!
//!   xdp-sender --interface eth0 --cpus 0 \
//!              --local 10.0.0.5:50000 --dest 10.0.0.6:9000 \
//!              --count 1000000
//!
//! Either `--count N` or `--duration-ms N` (or both) bounds the run. SIGINT
//! / SIGTERM cleanly terminates and prints a final summary.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("xdp-sender example only builds on Linux");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::run()
}

#[cfg(target_os = "linux")]
mod linux {
    use {
        agave_xdp::transmitter::{
            BytesTxPacket, Transmitter, TransmitterBuilder, XdpConfig, XdpSender,
        },
        bytes::Bytes,
        crossbeam_channel::TrySendError,
        std::{
            error::Error,
            net::{SocketAddr, SocketAddrV4},
            sync::{
                Arc,
                atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
            },
            thread,
            time::{Duration, Instant},
        },
    };

    const USAGE: &str = "usage: xdp-sender --interface NAME --cpus N[,N..] \
                        --local IPv4:PORT --dest IPv4:PORT \
                        [--payload-len N] [--count N] [--duration-ms N] [--zero-copy]";

    /// Stats sampling cadence.
    const STATS_INTERVAL: Duration = Duration::from_secs(1);

    static SHUTDOWN: AtomicBool = AtomicBool::new(false);

    pub fn run() -> Result<(), Box<dyn Error>> {
        install_signal_handlers();
        let args = Args::parse()?;

        let n_workers = args.cpus.len();
        let exit = Arc::new(AtomicBool::new(false));

        let config = XdpConfig::new(Some(args.interface.clone()), args.cpus.clone(), args.zero_copy);
        let (transmitter, sender) = TransmitterBuilder::new(config, exit.clone())?.build();

        assert_eq!(
            sender.len(),
            n_workers,
            "transmitter exposed {} channels for {} CPUs",
            sender.len(),
            n_workers
        );

        eprintln!(
            "xdp-sender blast: iface={} cpus={:?} channels={} local={} dest={} \
             payload_len={} zero_copy={}",
            args.interface,
            args.cpus,
            n_workers,
            args.local,
            args.dest,
            args.payload_len,
            args.zero_copy,
        );

        // One counter per producer channel — keeps the producer hot path
        // contention-free. Stats thread sums on demand.
        let counters: Vec<Arc<AtomicU64>> =
            (0..n_workers).map(|_| Arc::new(AtomicU64::new(0))).collect();
        let drops: Vec<Arc<AtomicU64>> =
            (0..n_workers).map(|_| Arc::new(AtomicU64::new(0))).collect();

        // Producers wait on this flag so all start sending at the same instant
        // — otherwise the first-to-spawn producer would dominate early counts.
        let start = Arc::new(AtomicBool::new(false));

        let limit = args.limit();
        let mut producers = Vec::with_capacity(n_workers);
        for idx in 0..n_workers {
            let sender = sender.clone();
            let exit = exit.clone();
            let start = start.clone();
            let counter = counters[idx].clone();
            let drop_counter = drops[idx].clone();
            let local = args.local;
            let dest = args.dest;
            let payload_len = args.payload_len;
            producers.push(
                thread::Builder::new()
                    .name(format!("blast-{idx:02}"))
                    .spawn(move || {
                        producer_loop(
                            idx,
                            sender,
                            local,
                            dest,
                            payload_len,
                            counter,
                            drop_counter,
                            start,
                            exit,
                        )
                    })?,
            );
        }

        // Stats reporter — separate from the producers so its 1Hz cadence
        // doesn't perturb the send path.
        let stats_handle = {
            let counters = counters.clone();
            let drops = drops.clone();
            let exit = exit.clone();
            thread::Builder::new()
                .name("blast-stats".into())
                .spawn(move || stats_loop(counters, drops, exit))?
        };

        let started = Instant::now();
        start.store(true, Relaxed);

        // Main thread polls limits + signal handler. Producers and TxLoops
        // run independently; we just decide when to stop them.
        loop {
            if SHUTDOWN.load(Relaxed) {
                eprintln!("xdp-sender blast: signal received, stopping");
                break;
            }
            let sent_total = sum(&counters);
            if !limit.keep_running(sent_total, started) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        exit.store(true, Relaxed);
        for h in producers {
            let _ = h.join();
        }
        let _ = stats_handle.join();

        // Drop our remaining `XdpSender` clone so the TxLoops' channel
        // receivers disconnect — the TxLoops only exit when no producer
        // can send to them. Without this drop, `transmitter.join()` hangs.
        drop(sender);
        join_transmitter(transmitter);

        let elapsed = started.elapsed();
        let sent = sum(&counters);
        let dropped = sum(&drops);
        let pps = if elapsed.is_zero() {
            0.0
        } else {
            sent as f64 / elapsed.as_secs_f64()
        };
        println!(
            "xdp-sender blast: {sent} packets in {elapsed:?} ({pps:.0} pkt/s), \
             {dropped} channel-full drops"
        );
        Ok(())
    }

    /// Per-producer hot loop. Writes a sequence number into the first 8 bytes
    /// of the payload (matches fast-socket-rs's `write_sequence`) so the
    /// receiver can detect drops / reorders.
    #[allow(clippy::too_many_arguments)]
    fn producer_loop(
        idx: usize,
        sender: XdpSender,
        local: SocketAddrV4,
        dest: SocketAddrV4,
        payload_len: usize,
        counter: Arc<AtomicU64>,
        drop_counter: Arc<AtomicU64>,
        start: Arc<AtomicBool>,
        exit: Arc<AtomicBool>,
    ) {
        while !start.load(Relaxed) && !exit.load(Relaxed) && !SHUTDOWN.load(Relaxed) {
            thread::yield_now();
        }

        let dest_socket = SocketAddr::V4(dest);
        let mut payload = vec![0u8; payload_len];
        let mut seq: u64 = 0;
        // Batch local counter updates to keep the producer→stats memory traffic
        // light. fast-socket-rs flushes every 64; same default here.
        const FLUSH_EVERY: u64 = 64;
        let mut pending = 0u64;
        let mut pending_drops = 0u64;

        while !exit.load(Relaxed) && !SHUTDOWN.load(Relaxed) {
            write_sequence(&mut payload, seq);
            let bytes = Bytes::copy_from_slice(&payload);
            let pkt = BytesTxPacket::new(local, dest_socket, None, bytes);
            match sender.try_send(idx, pkt) {
                Ok(()) => {
                    seq = seq.wrapping_add(1);
                    pending += 1;
                    if pending >= FLUSH_EVERY {
                        counter.fetch_add(pending, Relaxed);
                        pending = 0;
                    }
                }
                Err(TrySendError::Full(_)) => {
                    pending_drops += 1;
                    if pending_drops >= FLUSH_EVERY {
                        drop_counter.fetch_add(pending_drops, Relaxed);
                        pending_drops = 0;
                    }
                    std::hint::spin_loop();
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        if pending > 0 {
            counter.fetch_add(pending, Relaxed);
        }
        if pending_drops > 0 {
            drop_counter.fetch_add(pending_drops, Relaxed);
        }
    }

    fn stats_loop(
        counters: Vec<Arc<AtomicU64>>,
        drops: Vec<Arc<AtomicU64>>,
        exit: Arc<AtomicBool>,
    ) {
        let mut prev_sent = 0u64;
        let mut prev_drops = 0u64;
        while !exit.load(Relaxed) && !SHUTDOWN.load(Relaxed) {
            thread::sleep(STATS_INTERVAL);
            if exit.load(Relaxed) || SHUTDOWN.load(Relaxed) {
                break;
            }
            let sent = sum(&counters);
            let dropped = sum(&drops);
            eprintln!(
                "xdp-sender blast: pkt/s={} drops/s={} total_sent={} total_drops={}",
                sent.saturating_sub(prev_sent),
                dropped.saturating_sub(prev_drops),
                sent,
                dropped,
            );
            prev_sent = sent;
            prev_drops = dropped;
        }
    }

    #[inline]
    fn write_sequence(buf: &mut [u8], seq: u64) {
        if buf.len() >= 8 {
            buf[0..8].copy_from_slice(&seq.to_le_bytes());
        }
    }

    #[inline]
    fn sum(counters: &[Arc<AtomicU64>]) -> u64 {
        counters.iter().map(|c| c.load(Relaxed)).sum()
    }

    fn join_transmitter(transmitter: Transmitter) {
        if transmitter.join().is_err() {
            eprintln!("xdp-sender blast: a transmitter thread panicked");
        }
    }

    // -------------------------------------------------------------- args ----

    #[derive(Debug)]
    struct Args {
        interface: String,
        cpus: Vec<usize>,
        local: SocketAddrV4,
        dest: SocketAddrV4,
        payload_len: usize,
        count: Option<u64>,
        duration_ms: Option<u64>,
        zero_copy: bool,
    }

    impl Args {
        fn parse() -> Result<Self, Box<dyn Error>> {
            let mut argv: Vec<String> = std::env::args().skip(1).collect();
            let mut interface: Option<String> = None;
            let mut cpus: Option<Vec<usize>> = None;
            let mut local: Option<SocketAddrV4> = None;
            let mut dest: Option<SocketAddrV4> = None;
            let mut payload_len: usize = 64;
            let mut count: Option<u64> = None;
            let mut duration_ms: Option<u64> = None;
            let mut zero_copy = false;

            while let Some(flag) = argv.first().cloned() {
                argv.remove(0);
                match flag.as_str() {
                    "--interface" | "--iface" => {
                        interface = Some(take(&mut argv, &flag)?);
                    }
                    "--cpus" => {
                        let csv = take(&mut argv, &flag)?;
                        let parsed: Result<Vec<usize>, _> =
                            csv.split(',').map(|s| s.trim().parse::<usize>()).collect();
                        cpus = Some(parsed?);
                    }
                    "--local" => {
                        local = Some(parse_v4(&take(&mut argv, &flag)?, &flag)?);
                    }
                    "--dest" => {
                        dest = Some(parse_v4(&take(&mut argv, &flag)?, &flag)?);
                    }
                    "--payload-len" => {
                        payload_len = take(&mut argv, &flag)?.parse()?;
                    }
                    "--count" => {
                        count = Some(take(&mut argv, &flag)?.parse()?);
                    }
                    "--duration-ms" => {
                        duration_ms = Some(take(&mut argv, &flag)?.parse()?);
                    }
                    "--zero-copy" => {
                        zero_copy = true;
                    }
                    "-h" | "--help" => {
                        println!("{USAGE}");
                        std::process::exit(0);
                    }
                    other => {
                        return Err(format!("unknown argument {other}\n{USAGE}").into());
                    }
                }
            }

            let interface = interface.ok_or_else(|| format!("missing --interface\n{USAGE}"))?;
            let cpus = cpus.ok_or_else(|| format!("missing --cpus\n{USAGE}"))?;
            if cpus.is_empty() {
                return Err(format!("--cpus must list at least one CPU\n{USAGE}").into());
            }
            let local = local.ok_or_else(|| format!("missing --local\n{USAGE}"))?;
            let dest = dest.ok_or_else(|| format!("missing --dest\n{USAGE}"))?;
            if payload_len < 8 {
                return Err("--payload-len must be ≥ 8 (8 bytes reserved for sequence)".into());
            }

            Ok(Self {
                interface,
                cpus,
                local,
                dest,
                payload_len,
                count,
                duration_ms,
                zero_copy,
            })
        }

        fn limit(&self) -> RunLimit {
            RunLimit {
                count: self.count,
                duration: self.duration_ms.map(Duration::from_millis),
            }
        }
    }

    fn take(argv: &mut Vec<String>, flag: &str) -> Result<String, Box<dyn Error>> {
        if argv.is_empty() {
            return Err(format!("{flag} requires a value").into());
        }
        Ok(argv.remove(0))
    }

    fn parse_v4(s: &str, flag: &str) -> Result<SocketAddrV4, Box<dyn Error>> {
        match s.parse::<SocketAddr>()? {
            SocketAddr::V4(addr) => Ok(addr),
            SocketAddr::V6(_) => Err(format!("{flag} must be an IPv4 socket address").into()),
        }
    }

    struct RunLimit {
        count: Option<u64>,
        duration: Option<Duration>,
    }

    impl RunLimit {
        fn keep_running(&self, sent: u64, started: Instant) -> bool {
            if let Some(c) = self.count {
                if sent >= c {
                    return false;
                }
            }
            if let Some(d) = self.duration {
                if started.elapsed() >= d {
                    return false;
                }
            }
            true
        }
    }

    // -------------------------------------------------------- signals -------

    extern "C" fn handle_signal(_: libc::c_int) {
        SHUTDOWN.store(true, Relaxed);
    }

    fn install_signal_handlers() {
        // SAFETY: `handle_signal` only writes to a static AtomicBool, which is
        // async-signal-safe. We install once before any other threads start.
        let handler = handle_signal as *const () as libc::sighandler_t;
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
        }
    }
}
