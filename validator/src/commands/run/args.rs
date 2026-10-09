use {
    crate::{
        cli::{DefaultArgs, hash_validator, port_range_validator, port_validator},
        commands::{FromClapArgMatches, Result},
    },
    agave_snapshots::{SUPPORTED_ARCHIVE_COMPRESSION, SnapshotVersion},
    bytesize::ByteSize,
    clap::{App, Arg, ArgMatches},
    solana_clap_utils::{
        hidden_unless_forced,
        input_parsers::keypair_of,
        input_validators::{
            is_keypair_or_ask_keyword, is_non_zero, is_parsable, is_pow2, is_pubkey,
            is_pubkey_or_keypair, is_slot, is_within_range, validate_cpu_ranges,
            validate_maximum_full_snapshot_archives_to_retain,
            validate_maximum_incremental_snapshot_archives_to_retain,
        },
        keypair::SKIP_SEED_PHRASE_VALIDATION_ARG,
    },
    solana_core::{
        banking_trace::DirByteLimit,
        validator::{BlockProductionMethod, BlockVerificationMethod},
    },
    solana_keypair::Keypair,
    solana_ledger::use_snapshot_archives_at_startup,
    solana_signer::Signer,
    solana_unified_scheduler_pool::DefaultSchedulerPool,
    std::path::PathBuf,
};

const EXCLUDE_KEY: &str = "account-index-exclude-key";
const INCLUDE_KEY: &str = "account-index-include-key";

pub mod account_secondary_indexes;
pub mod blockstore_options;
pub mod json_rpc_config;
pub mod pub_sub_config;
pub mod rpc_bigtable_config;
pub mod rpc_bootstrap_config;
pub mod send_transaction_config;

/// The settings needed before logging starts
pub struct RunArgs {
    pub identity_keypair: Keypair,
    pub ledger_path: PathBuf,
    pub logfile: Option<PathBuf>,
}

impl FromClapArgMatches for RunArgs {
    fn from_clap_arg_match(matches: &ArgMatches) -> Result<Self> {
        let identity_keypair =
            keypair_of(matches, "identity").ok_or(clap::Error::with_description(
                "The --identity <KEYPAIR> argument is required",
                clap::ErrorKind::ArgumentNotFound,
            ))?;

        let ledger_path = PathBuf::from(matches.value_of("ledger_path").ok_or(
            clap::Error::with_description(
                "The --ledger <DIR> argument is required",
                clap::ErrorKind::ArgumentNotFound,
            ),
        )?);
        // The directory is created and canonicalized when the ledger is locked
        let ledger_path = std::path::absolute(&ledger_path)?;

        let logfile = matches
            .value_of("logfile")
            .map(String::from)
            .unwrap_or_else(|| format!("agave-validator-{}.log", identity_keypair.pubkey()));
        let logfile = if logfile == "-" {
            None
        } else {
            Some(PathBuf::from(logfile))
        };

        Ok(RunArgs {
            identity_keypair,
            ledger_path,
            logfile,
        })
    }
}

pub fn add_args<'a>(app: App<'a, 'a>, default_args: &'a DefaultArgs) -> App<'a, 'a> {
    app.arg(
        Arg::with_name(SKIP_SEED_PHRASE_VALIDATION_ARG.name)
            .long(SKIP_SEED_PHRASE_VALIDATION_ARG.long)
            .help(SKIP_SEED_PHRASE_VALIDATION_ARG.help),
    )
    .arg(
        Arg::with_name("identity")
            .short("i")
            .long("identity")
            .value_name("KEYPAIR")
            .takes_value(true)
            .validator(is_keypair_or_ask_keyword)
            .help("Validator identity keypair"),
    )
    .arg(
        Arg::with_name("authorized_voter_keypairs")
            .long("authorized-voter")
            .value_name("KEYPAIR")
            .takes_value(true)
            .validator(is_keypair_or_ask_keyword)
            .requires("vote_account")
            .multiple(true)
            .help(
                "Include an additional authorized voter keypair. May be specified multiple times. \
                 [default: the --identity keypair]",
            ),
    )
    .arg(
        Arg::with_name("vote_account")
            .long("vote-account")
            .value_name("ADDRESS")
            .takes_value(true)
            .validator(is_pubkey_or_keypair)
            .requires("identity")
            .help(
                "Validator vote account public key. If unspecified, voting will be disabled. The \
                 authorized voter for the account must either be the --identity keypair or set by \
                 the --authorized-voter argument",
            ),
    )
    .arg(
        Arg::with_name("init_complete_file")
            .long("init-complete-file")
            .value_name("FILE")
            .takes_value(true)
            .help(
                "Create this file if it doesn't already exist once validator initialization is \
                 complete",
            ),
    )
    .arg(
        Arg::with_name("ledger_path")
            .short("l")
            .long("ledger")
            .value_name("DIR")
            .takes_value(true)
            .required(true)
            .default_value(&default_args.ledger_path)
            .help("Use DIR as ledger location"),
    )
    .arg(
        Arg::with_name("entrypoint")
            .short("n")
            .long("entrypoint")
            .value_name("HOST:PORT")
            .takes_value(true)
            .multiple(true)
            .validator(solana_net_utils::is_host_port)
            .help("Rendezvous with the cluster at this gossip entrypoint"),
    )
    .arg(
        Arg::with_name("no_voting")
            .long("no-voting")
            .takes_value(false)
            .help("Launch validator without voting"),
    )
    .arg(
        Arg::with_name("restricted_repair_only_mode")
            .long("restricted-repair-only-mode")
            .takes_value(false)
            .help(
                "Do not publish the Gossip, TPU, TVU or Repair Service ports. Doing so causes the \
                 node to operate in a limited capacity that reduces its exposure to the rest of \
                 the cluster. The --no-voting flag is implicit when this flag is enabled",
            ),
    )
    .arg(
        Arg::with_name("rpc_port")
            .long("rpc-port")
            .value_name("PORT")
            .takes_value(true)
            .validator(port_validator)
            .help("Enable JSON RPC on this port, and the next port for the RPC websocket"),
    )
    .arg(
        Arg::with_name("private_rpc")
            .long("private-rpc")
            .takes_value(false)
            .help("Do not publish the RPC port for use by others"),
    )
    .arg(
        Arg::with_name("no_port_check")
            .long("no-port-check")
            .takes_value(false)
            .hidden(hidden_unless_forced())
            .help("Do not perform TCP/UDP reachable port checks at start-up"),
    )
    .arg(
        Arg::with_name("account_paths")
            .long("accounts")
            .value_name("PATHS")
            .takes_value(true)
            .multiple(true)
            .help(
                "Comma separated persistent accounts location. May be specified multiple times. \
                 [default: <LEDGER>/accounts]",
            ),
    )
    .arg(
        Arg::with_name("snapshots")
            .long("snapshots")
            .value_name("DIR")
            .takes_value(true)
            .help("Use DIR as the base location for snapshots.")
            .long_help(
                "Use DIR as the base location for snapshots. Snapshot archives will use DIR \
                 unless --full-snapshot-archive-path or --incremental-snapshot-archive-path is \
                 specified. Additionally, a subdirectory named \"snapshots\" will be created in \
                 DIR. This subdirectory holds internal files/data that are used when generating \
                 snapshot archives. [default: --ledger value]",
            ),
    )
    .arg(
        Arg::with_name(use_snapshot_archives_at_startup::cli::NAME)
            .long(use_snapshot_archives_at_startup::cli::LONG_ARG)
            .takes_value(true)
            .possible_values(use_snapshot_archives_at_startup::cli::POSSIBLE_VALUES)
            .default_value(use_snapshot_archives_at_startup::cli::default_value())
            .help(use_snapshot_archives_at_startup::cli::HELP)
            .long_help(use_snapshot_archives_at_startup::cli::LONG_HELP),
    )
    .arg(
        Arg::with_name("full_snapshot_archive_path")
            .long("full-snapshot-archive-path")
            .value_name("DIR")
            .takes_value(true)
            .help("Use DIR as full snapshot archives location [default: --snapshots value]"),
    )
    .arg(
        Arg::with_name("incremental_snapshot_archive_path")
            .long("incremental-snapshot-archive-path")
            .conflicts_with("no-incremental-snapshots")
            .value_name("DIR")
            .takes_value(true)
            .help("Use DIR as incremental snapshot archives location [default: --snapshots value]"),
    )
    .arg(
        Arg::with_name("tower")
            .long("tower")
            .value_name("DIR")
            .takes_value(true)
            .help("Use DIR as file tower storage location [default: --ledger value]"),
    )
    .arg(
        Arg::with_name("gossip_port")
            .long("gossip-port")
            .value_name("PORT")
            .takes_value(true)
            .validator(port_validator)
            .help("Gossip port number for the validator"),
    )
    .arg(
        Arg::with_name("public_tpu_addr")
            .long("public-tpu-address")
            .alias("tpu-host-addr")
            .value_name("HOST:PORT")
            .takes_value(true)
            .validator(solana_net_utils::is_host_port)
            .help(
                "Specify TPU QUIC address to advertise in gossip [default: ask --entrypoint or \
                 localhost when --entrypoint is not provided]",
            ),
    )
    .arg(
        Arg::with_name("public_tpu_forwards_addr")
            .long("public-tpu-forwards-address")
            .value_name("HOST:PORT")
            .takes_value(true)
            .validator(solana_net_utils::is_host_port)
            .help(
                "Specify TPU Forwards QUIC address to advertise in gossip [default: ask \
                 --entrypoint or localhostwhen --entrypoint is not provided]",
            ),
    )
    .arg(
        Arg::with_name("public_tvu_addr")
            .long("public-tvu-address")
            .alias("tvu-host-addr")
            .value_name("HOST:PORT")
            .takes_value(true)
            .validator(solana_net_utils::is_host_port)
            .help(
                "Specify TVU address to advertise in gossip [default: ask --entrypoint or \
                 localhost when --entrypoint is not provided]",
            ),
    )
    .arg(
        Arg::with_name("public_rpc_addr")
            .long("public-rpc-address")
            .value_name("HOST:PORT")
            .takes_value(true)
            .conflicts_with("private_rpc")
            .validator(solana_net_utils::is_host_port)
            .help(
                "RPC address for the validator to advertise publicly in gossip. Useful for \
                 validators running behind a load balancer or proxy [default: use \
                 --rpc-bind-address / --rpc-port]",
            ),
    )
    .arg(
        Arg::with_name("dynamic_port_range")
            .long("dynamic-port-range")
            .value_name("MIN_PORT-MAX_PORT")
            .takes_value(true)
            .default_value(&default_args.dynamic_port_range)
            .validator(port_range_validator)
            .help(
                "Range to use for dynamically assigned ports. MIN_PORT-MAX_PORT yields the range \
                 [MIN_PORT, MAX_PORT)",
            ),
    )
    .arg(
        Arg::with_name("maximum_local_snapshot_age")
            .long("maximum-local-snapshot-age")
            .value_name("NUMBER_OF_SLOTS")
            .takes_value(true)
            .default_value(&default_args.maximum_local_snapshot_age)
            .help(
                "Reuse a local snapshot if it's less than this many slots behind the highest \
                 snapshot available for download from other validators",
            ),
    )
    .arg(
        Arg::with_name("no_snapshots")
            .long("no-snapshots")
            .takes_value(false)
            .conflicts_with_all(&[
                "no_incremental_snapshots",
                "snapshot_interval_slots",
                "full_snapshot_interval_slots",
            ])
            .help("Disable all snapshot generation"),
    )
    .arg(
        Arg::with_name("snapshot_interval_slots")
            .long("snapshot-interval-slots")
            .alias("incremental-snapshot-interval-slots")
            .value_name("NUMBER")
            .takes_value(true)
            .default_value(&default_args.incremental_snapshot_archive_interval_slots)
            .validator(is_non_zero)
            .help("Number of slots between generating snapshots")
            .long_help(
                "Number of slots between generating snapshots. If incremental snapshots are \
                 enabled, this sets the incremental snapshot interval. If incremental snapshots \
                 are disabled, this sets the full snapshot interval. Must be greater than zero.",
            ),
    )
    .arg(
        Arg::with_name("full_snapshot_interval_slots")
            .long("full-snapshot-interval-slots")
            .value_name("NUMBER")
            .takes_value(true)
            .default_value(&default_args.full_snapshot_archive_interval_slots)
            .validator(is_non_zero)
            .help("Number of slots between generating full snapshots")
            .long_help(
                "Number of slots between generating full snapshots. Only used when incremental \
                 snapshots are enabled. Must be greater than the incremental snapshot interval. \
                 Must be greater than zero.",
            ),
    )
    .arg(
        Arg::with_name("maximum_full_snapshots_to_retain")
            .long("maximum-full-snapshots-to-retain")
            .alias("maximum-snapshots-to-retain")
            .value_name("NUMBER")
            .takes_value(true)
            .default_value(&default_args.maximum_full_snapshot_archives_to_retain)
            .validator(validate_maximum_full_snapshot_archives_to_retain)
            .help(
                "The maximum number of full snapshot archives to hold on to when purging older \
                 snapshots.",
            ),
    )
    .arg(
        Arg::with_name("maximum_incremental_snapshots_to_retain")
            .long("maximum-incremental-snapshots-to-retain")
            .value_name("NUMBER")
            .takes_value(true)
            .default_value(&default_args.maximum_incremental_snapshot_archives_to_retain)
            .validator(validate_maximum_incremental_snapshot_archives_to_retain)
            .help(
                "The maximum number of incremental snapshot archives to hold on to when purging \
                 older snapshots.",
            ),
    )
    .arg(
        Arg::with_name("snapshot_packager_niceness_adj")
            .long("snapshot-packager-niceness-adjustment")
            .value_name("ADJUSTMENT")
            .takes_value(true)
            .validator(solana_perf::thread::is_niceness_adjustment_valid)
            .default_value(&default_args.snapshot_packager_niceness_adjustment)
            .help(
                "Add this value to niceness of snapshot packager thread. Negative value increases \
                 priority, positive value decreases priority.",
            ),
    )
    .arg(
        Arg::with_name("minimal_snapshot_download_speed")
            .long("minimal-snapshot-download-speed")
            .value_name("MINIMAL_SNAPSHOT_DOWNLOAD_SPEED")
            .takes_value(true)
            .default_value(&default_args.min_snapshot_download_speed)
            .help(
                "The minimal speed of snapshot downloads measured in bytes/second. If the initial \
                 download speed falls below this threshold, the system will retry the download \
                 against a different rpc node.",
            ),
    )
    .arg(
        Arg::with_name("maximum_snapshot_download_abort")
            .long("maximum-snapshot-download-abort")
            .value_name("MAXIMUM_SNAPSHOT_DOWNLOAD_ABORT")
            .takes_value(true)
            .default_value(&default_args.max_snapshot_download_abort)
            .help(
                "The maximum number of times to abort and retry when encountering a slow snapshot \
                 download.",
            ),
    )
    .arg(
        Arg::with_name("contact_debug_interval")
            .long("contact-debug-interval")
            .value_name("CONTACT_DEBUG_INTERVAL")
            .takes_value(true)
            .default_value(&default_args.contact_debug_interval)
            .help("Milliseconds between printing contact debug from gossip."),
    )
    .arg(
        Arg::with_name("no_poh_speed_test")
            .long("no-poh-speed-test")
            .hidden(hidden_unless_forced())
            .help("Skip the check for PoH speed."),
    )
    .arg(
        Arg::with_name("no_os_network_limits_test")
            .hidden(hidden_unless_forced())
            .long("no-os-network-limits-test")
            .help("Skip checks for OS network limits."),
    )
    .arg(
        Arg::with_name("no_os_memory_stats_reporting")
            .long("no-os-memory-stats-reporting")
            .hidden(hidden_unless_forced())
            .help("Disable reporting of OS memory statistics."),
    )
    .arg(
        Arg::with_name("no_os_network_stats_reporting")
            .long("no-os-network-stats-reporting")
            .hidden(hidden_unless_forced())
            .help("Disable reporting of OS network statistics."),
    )
    .arg(
        Arg::with_name("no_os_cpu_stats_reporting")
            .long("no-os-cpu-stats-reporting")
            .hidden(hidden_unless_forced())
            .help("Disable reporting of OS CPU statistics."),
    )
    .arg(
        Arg::with_name("no_os_disk_stats_reporting")
            .long("no-os-disk-stats-reporting")
            .hidden(hidden_unless_forced())
            .help("Disable reporting of OS disk statistics."),
    )
    .arg(
        Arg::with_name("snapshot_version")
            .long("snapshot-version")
            .value_name("SNAPSHOT_VERSION")
            .validator(is_parsable::<SnapshotVersion>)
            .takes_value(true)
            .default_value(default_args.snapshot_version.into())
            .help("Output snapshot version"),
    )
    .arg(
        Arg::with_name("skip_startup_ledger_verification")
            .long("skip-startup-ledger-verification")
            .takes_value(false)
            .help("Skip ledger verification at validator bootup."),
    )
    .arg(
        clap::Arg::with_name("require_tower")
            .long("require-tower")
            .takes_value(false)
            .help("Refuse to start if saved tower state is not found"),
    )
    .arg(
        clap::Arg::with_name("do_not_require_vote_history")
            .long("do-not-require-vote-history")
            .takes_value(false)
            .help("Do not require saved vote history state for startup"),
    )
    .arg(
        Arg::with_name("expected_genesis_hash")
            .long("expected-genesis-hash")
            .value_name("HASH")
            .takes_value(true)
            .validator(hash_validator)
            .help("Require the genesis have this hash"),
    )
    .arg(
        Arg::with_name("expected_bank_hash")
            .long("expected-bank-hash")
            .value_name("HASH")
            .takes_value(true)
            .validator(hash_validator)
            .help("When wait-for-supermajority <x>, require the bank at <x> to have this hash"),
    )
    .arg(
        Arg::with_name("expected_shred_version")
            .long("expected-shred-version")
            .value_name("VERSION")
            .takes_value(true)
            .validator(is_parsable::<u16>)
            .help("Require the shred version be this value"),
    )
    .arg(
        Arg::with_name("logfile")
            .short("o")
            .long("log")
            .value_name("FILE")
            .takes_value(true)
            .help(
                "Redirect logging to the specified file, '-' for standard error. Sending the \
                 SIGUSR1 signal to the validator process will cause it to re-open the log file",
            ),
    )
    .arg(
        Arg::with_name("wait_for_supermajority")
            .long("wait-for-supermajority")
            .requires("expected_bank_hash")
            .requires("expected_shred_version")
            .value_name("SLOT")
            .validator(is_slot)
            .help(
                "After processing the ledger and the next slot is SLOT, wait until a \
                 supermajority of stake is visible on gossip before starting PoH",
            ),
    )
    .arg(
        Arg::with_name("wait_to_vote_slot")
            .long("wait-to-vote-slot")
            .value_name("SLOT")
            .takes_value(true)
            .validator(is_slot)
            .help(
                "Do not vote until reaching this slot. This can be used to avoid submitting \
                 slashable votes after restoring a stale/missing Tower / VoteHistory",
            ),
    )
    .arg(
        Arg::with_name("no_wait_for_vote_to_start_leader")
            .hidden(hidden_unless_forced())
            .long("no-wait-for-vote-to-start-leader")
            .help(
                "If the validator starts up with no ledger, it will wait to start block \
                 production until it sees a vote land in a rooted slot. This prevents double \
                 signing. Turn off to risk double signing a block.",
            ),
    )
    .arg(
        Arg::with_name("hard_forks")
            .long("hard-fork")
            .value_name("SLOT")
            .validator(is_slot)
            .multiple(true)
            .takes_value(true)
            .help("Add a hard fork at this slot"),
    )
    .arg(
        Arg::with_name("known_validators")
            .alias("trusted-validator")
            .long("known-validator")
            .validator(is_pubkey)
            .value_name("VALIDATOR IDENTITY")
            .multiple(true)
            .takes_value(true)
            .help(
                "A snapshot hash must be published in gossip by this validator to be accepted. \
                 May be specified multiple times. If unspecified any snapshot hash will be \
                 accepted",
            ),
    )
    .arg(
        Arg::with_name("debug_key")
            .long("debug-key")
            .validator(is_pubkey)
            .value_name("ADDRESS")
            .multiple(true)
            .takes_value(true)
            .help("Log when transactions are processed which reference a given key."),
    )
    .arg(
        Arg::with_name("repair_validators")
            .long("repair-validator")
            .validator(is_pubkey)
            .value_name("VALIDATOR IDENTITY")
            .multiple(true)
            .takes_value(true)
            .help(
                "A list of validators to request repairs from. If specified, repair will not \
                 request from validators outside this set [default: all validators]",
            ),
    )
    .arg(
        Arg::with_name("repair_whitelist")
            .hidden(hidden_unless_forced())
            .long("repair-whitelist")
            .validator(is_pubkey)
            .value_name("VALIDATOR IDENTITY")
            .multiple(true)
            .takes_value(true)
            .help(
                "A list of validators to prioritize repairs from. If specified, repair requests \
                 from validators in the list will be prioritized over requests from other \
                 validators. [default: all validators]",
            ),
    )
    .arg(
        Arg::with_name("gossip_validators")
            .long("gossip-validator")
            .validator(is_pubkey)
            .value_name("VALIDATOR IDENTITY")
            .multiple(true)
            .takes_value(true)
            .help(
                "A list of validators to gossip with. If specified, gossip will not push/pull \
                 from from validators outside this set. [default: all validators]",
            ),
    )
    .arg(
        Arg::with_name("tpu_max_connections_per_ipaddr_per_minute")
            .long("tpu-max-connections-per-ipaddr-per-minute")
            .takes_value(true)
            .default_value(&default_args.tpu_max_connections_per_ipaddr_per_minute)
            .validator(is_parsable::<u32>)
            .hidden(hidden_unless_forced())
            .help("Controls the rate of the clients connections per IpAddr per minute."),
    )
    .arg(
        Arg::with_name("vote_use_quic")
            .long("vote-use-quic")
            .takes_value(true)
            .default_value(&default_args.vote_use_quic)
            .hidden(hidden_unless_forced())
            .help("Controls if to use QUIC to send votes."),
    )
    .arg(
        Arg::with_name("tpu_max_connections_per_peer")
            .long("tpu-max-connections-per-peer")
            .takes_value(true)
            .validator(is_parsable::<u32>)
            .hidden(hidden_unless_forced())
            .help(
                "Controls the max concurrent connections per IpAddr or staked identity.Overrides \
                 tpu-max-connections-per-unstaked-peer and tpu-max-connections-per-staked-peer",
            ),
    )
    .arg(
        Arg::with_name("tpu_max_connections_per_unstaked_peer")
            .long("tpu-max-connections-per-unstaked-peer")
            .takes_value(true)
            .default_value(&default_args.tpu_max_connections_per_unstaked_peer)
            .validator(is_parsable::<u32>)
            .hidden(hidden_unless_forced())
            .help("Controls the max concurrent connections per IpAddr for unstaked clients."),
    )
    .arg(
        Arg::with_name("tpu_max_connections_per_staked_peer")
            .long("tpu-max-connections-per-staked-peer")
            .takes_value(true)
            .default_value(&default_args.tpu_max_connections_per_staked_peer)
            .validator(is_parsable::<u32>)
            .hidden(hidden_unless_forced())
            .help("Controls the max concurrent connections per staked identity."),
    )
    .arg(
        Arg::with_name("tpu_max_staked_connections")
            .long("tpu-max-staked-connections")
            .takes_value(true)
            .default_value(&default_args.tpu_max_staked_connections)
            .validator(is_parsable::<u32>)
            .hidden(hidden_unless_forced())
            .help("Controls the max concurrent connections for TPU from staked nodes."),
    )
    .arg(
        Arg::with_name("tpu_max_unstaked_connections")
            .long("tpu-max-unstaked-connections")
            .takes_value(true)
            .default_value(&default_args.tpu_max_unstaked_connections)
            .validator(is_parsable::<u32>)
            .hidden(hidden_unless_forced())
            .help("Controls the max concurrent connections fort TPU from unstaked nodes."),
    )
    .arg(
        Arg::with_name("tpu_max_fwd_staked_connections")
            .long("tpu-max-fwd-staked-connections")
            .takes_value(true)
            .default_value(&default_args.tpu_max_fwd_staked_connections)
            .validator(is_parsable::<u32>)
            .hidden(hidden_unless_forced())
            .help("Controls the max concurrent connections for TPU-forward from staked nodes."),
    )
    .arg(
        Arg::with_name("tpu_max_fwd_unstaked_connections")
            .long("tpu-max-fwd-unstaked-connections")
            .takes_value(true)
            .default_value(&default_args.tpu_max_fwd_unstaked_connections)
            .validator(is_parsable::<u32>)
            .hidden(hidden_unless_forced())
            .help("Controls the max concurrent connections for TPU-forward from unstaked nodes."),
    )
    .arg(
        Arg::with_name("tpu_max_streams_per_ms")
            .long("tpu-max-streams-per-ms")
            .takes_value(true)
            .default_value(&default_args.tpu_max_streams_per_ms)
            .validator(is_parsable::<usize>)
            .hidden(hidden_unless_forced())
            .help("Controls the max number of streams for a TPU service."),
    )
    .arg(
        Arg::with_name("num_quic_endpoints")
            .long("num-quic-endpoints")
            .takes_value(true)
            .default_value(&default_args.num_quic_endpoints)
            .validator(is_parsable::<usize>)
            .hidden(hidden_unless_forced())
            .help(
                "The number of QUIC endpoints used for TPU and TPU-Forward. It can be increased \
                 to increase network ingest throughput, at the expense of higher CPU and general \
                 validator load.",
            ),
    )
    .arg(
        Arg::with_name("num_votor_endpoints")
            .long("num-votor-endpoints")
            .takes_value(true)
            .default_value(&default_args.num_votor_endpoints)
            .validator(is_parsable::<usize>)
            .hidden(hidden_unless_forced())
            .help("The number of QUIC endpoints used for the votor transport."),
    )
    .arg(
        Arg::with_name("votor_peer_overrides")
            .long("votor-peer-overrides")
            .validator(is_pubkey)
            .value_name("VALIDATOR IDENTITY")
            .multiple(true)
            .takes_value(true)
            .hidden(hidden_unless_forced())
            .help(
                "A list of additional validator identities for this node to send votor consensus \
                 messages to while staked. By default we only send consensus messages to staked \
                 nodes, however an operator can use this flag to additionally send messages to an \
                 unstaked RPC node for faster state. These identities are resolved from gossip.",
            ),
    )
    .arg(
        Arg::with_name("staked_nodes_overrides")
            .long("staked-nodes-overrides")
            .value_name("PATH")
            .takes_value(true)
            .help(
                "Provide path to a yaml file with custom overrides for stakes of specific \
                 identities. Overriding the amount of stake this validator considers as valid for \
                 other peers in network. The stake amount is used for calculating the number of \
                 QUIC streams permitted from the peer and vote packet sender stage. Format of the \
                 file: `staked_map_id: {<pubkey>: <SOL stake amount>}",
            ),
    )
    .arg(
        Arg::with_name("bind_address")
            .long("bind-address")
            .value_name("HOST")
            .takes_value(true)
            .validator(solana_net_utils::is_host)
            .default_value(&default_args.bind_address)
            .multiple(true)
            .help(
                "Repeatable. IP addresses to bind the validator ports on. First is primary (used \
                 on startup), the rest may be switched to during operation.",
            ),
    )
    .arg(
        Arg::with_name("rpc_bind_address")
            .long("rpc-bind-address")
            .value_name("HOST")
            .takes_value(true)
            .validator(solana_net_utils::is_host)
            .help(
                "IP address to bind the RPC port [default: 127.0.0.1 if --private-rpc is present, \
                 otherwise use --bind-address]",
            ),
    )
    .arg(
        Arg::with_name("geyser_plugin_config")
            .long("geyser-plugin-config")
            .alias("accountsdb-plugin-config")
            .value_name("FILE")
            .takes_value(true)
            .multiple(true)
            .help("Specify the configuration file for the Geyser plugin."),
    )
    .arg(
        Arg::with_name("geyser_plugin_always_enabled")
            .long("geyser-plugin-always-enabled")
            .value_name("BOOLEAN")
            .takes_value(false)
            .help("Еnable Geyser interface even if no Geyser configs are specified."),
    )
    .arg(
        Arg::with_name("snapshot_archive_format")
            .long("snapshot-archive-format")
            .alias("snapshot-compression") // Legacy name used by Solana v1.5.x and older
            .possible_values(SUPPORTED_ARCHIVE_COMPRESSION)
            .default_value(&default_args.snapshot_archive_format)
            .value_name("ARCHIVE_TYPE")
            .takes_value(true)
            .help("Snapshot archive format to use."),
    )
    .arg(
        Arg::with_name("snapshot_zstd_compression_level")
            .long("snapshot-zstd-compression-level")
            .default_value(&default_args.snapshot_zstd_compression_level)
            .value_name("LEVEL")
            .takes_value(true)
            .help("The compression level to use when archiving with zstd")
            .long_help(
                "The compression level to use when archiving with zstd. Higher compression levels \
                 generally produce higher compression ratio at the expense of speed and memory. \
                 See the zstd manpage for more information.",
            ),
    )
    .arg(
        Arg::with_name("poh_pinned_cpu_core")
            .long("poh-pinned-cpu-core")
            .takes_value(true)
            .value_name("CPU_ID")
            .validator(is_parsable::<usize>)
            .help("Specify which CPU core PoH is pinned to. Defaults to CPU 0 on Linux"),
    )
    .arg(
        Arg::with_name("poh_hashes_per_batch")
            .hidden(hidden_unless_forced())
            .long("poh-hashes-per-batch")
            .takes_value(true)
            .value_name("NUM")
            .help("Specify hashes per batch in PoH service"),
    )
    .arg(
        Arg::with_name("process_ledger_before_services")
            .long("process-ledger-before-services")
            .hidden(hidden_unless_forced())
            .help("Process the local ledger fully before starting networking services"),
    )
    .arg(
        Arg::with_name("account_indexes")
            .long("account-index")
            .takes_value(true)
            .multiple(true)
            .possible_values(&["program-id", "spl-token-owner", "spl-token-mint"])
            .value_name("INDEX")
            .help("Enable an accounts index, indexed by the selected account field"),
    )
    .arg(
        Arg::with_name("account_index_exclude_key")
            .long(EXCLUDE_KEY)
            .takes_value(true)
            .validator(is_pubkey)
            .multiple(true)
            .value_name("KEY")
            .help("When account indexes are enabled, exclude this key from the index."),
    )
    .arg(
        Arg::with_name("account_index_include_key")
            .long(INCLUDE_KEY)
            .takes_value(true)
            .validator(is_pubkey)
            .conflicts_with("account_index_exclude_key")
            .multiple(true)
            .value_name("KEY")
            .help(
                "When account indexes are enabled, only include specific keys in the index. This \
                 overrides --account-index-exclude-key.",
            ),
    )
    .arg(
        Arg::with_name("accounts_db_verify_index")
            .long("accounts-db-verify-index")
            .help(
                "Debug option to scan all storages and verify account index slot lists prior to \
                 clean",
            )
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("accounts_db_scan_filter_for_shrinking")
            .long("accounts-db-scan-filter-for-shrinking")
            .takes_value(true)
            .possible_values(&["all", "only-abnormal", "only-abnormal-with-verify"])
            .help(
                "Debug option to use different type of filtering for accounts index scan in \
                 shrinking. \"all\" will scan both in-memory and on-disk accounts index, which is \
                 the default. \"only-abnormal\" will scan in-memory accounts index only for \
                 abnormal entries and skip scanning on-disk accounts index by assuming that \
                 on-disk accounts index contains only normal accounts index entry. \
                 \"only-abnormal-with-verify\" is similar to \"only-abnormal\", which will scan \
                 in-memory index for abnormal entries, but will also verify that on-disk account \
                 entries are indeed normal.",
            )
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("accounts_db_account_storage_file_format")
            .long("accounts-db-account-storage-file-format")
            .takes_value(true)
            .possible_values(&["append-vec", "split-experimental"])
            .default_value("append-vec")
            .help("Selects the account storage file format")
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("accounts_db_ancient_append_vecs")
            .long("accounts-db-ancient-append-vecs")
            .value_name("SLOT-OFFSET")
            .validator(is_parsable::<i64>)
            .takes_value(true)
            .help(
                "AppendVecs that are older than (slots_per_epoch - SLOT-OFFSET) are squashed \
                 together.",
            )
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("accounts_db_ancient_storage_ideal_size")
            .long("accounts-db-ancient-storage-ideal-size")
            .value_name("BYTES")
            .validator(is_parsable::<u64>)
            .takes_value(true)
            .help("The smallest size of ideal ancient storage.")
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("accounts_db_max_ancient_storages")
            .long("accounts-db-max-ancient-storages")
            .value_name("USIZE")
            .validator(is_parsable::<usize>)
            .takes_value(true)
            .help("The number of ancient storages the ancient slot combining should converge to.")
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("accounts_db_write_cache_limit")
            .long("accounts-db-write-cache-limit")
            .value_name("BYTES")
            .validator(is_parsable::<ByteSize>)
            .takes_value(true)
            .help("How large the write cache for account data can become, in bytes")
            .long_help(
                "How large the write cache for account data can become, in bytes. If this is \
                 exceeded, the write cache is flushed more aggressively. Accepts SI and IEC \
                 prefixes, e.g. 17.1GB or 18Gi.",
            ),
    )
    .arg(
        Arg::with_name("accounts_db_read_cache_limit")
            .long("accounts-db-read-cache-limit")
            .value_name("LOW,HIGH")
            .takes_value(true)
            .min_values(2)
            .max_values(2)
            .multiple(false)
            .require_delimiter(true)
            .validator(is_parsable::<ByteSize>)
            .help("How large the read cache for account data can become, in bytes")
            .long_help(
                "How large the read cache for account data can become, in bytes. The values will \
                 be the low and high watermarks for the cache. When the cache exceeds the high \
                 watermark, entries will be evicted until the size reaches the low watermark. \
                 Accepts SI and IEC prefixes, e.g. 8.1GB,8.3GiB. LOW must be <= HIGH.",
            )
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("no_accounts_db_snapshots_direct_io")
            .long("no-accounts-db-snapshots-direct-io")
            .help("Disable direct I/O use for accounts-db snapshot operations")
            .long_help(
                "Do *not* use direct I/O for accounts-db file operations related to snapshot \
                 processsing. Direct I/O can improve performance by bypassing OS page cache, but \
                 requires the file systems hosting snapshots and accounts-db directories to \
                 support files opened with the O_DIRECT flag.",
            ),
    )
    .arg(
        Arg::with_name("accounts_index_bins")
            .long("accounts-index-bins")
            .value_name("BINS")
            .validator(is_pow2)
            .takes_value(true)
            .help("Number of bins to divide the accounts index into"),
    )
    .arg(
        Arg::with_name("accounts_index_limit")
            .long("accounts-index-limit")
            .value_name("VALUE")
            .takes_value(true)
            .possible_values(&[
                "minimal",
                "25GB",
                "50GB",
                "100GB",
                "200GB",
                "400GB",
                "800GB",
                "unlimited",
            ])
            .default_value("unlimited")
            .help("Sets the memory limit for the accounts index")
            .long_help(
                "Sets the memory limit for the accounts index. The size options will limit the \
                 accounts index memory to the specified value. E.g. \"50GB\" means the accounts \
                 index may use up to 50 GB of memory. The \"unlimited\" option keeps the entire \
                 accounts index in memory. All index entries that are not in memory are kept in \
                 the disk-backed index. The disk-backed index has lower performance; prefer \
                 higher explicit limits here. \"minimal\" is deprecated and behaves as \"25GB\".",
            ),
    )
    .arg(
        Arg::with_name("accounts_index_initial_accounts_count")
            .long("accounts-index-initial-accounts-count")
            .value_name("NUMBER")
            .validator(is_parsable::<usize>)
            .takes_value(true)
            .help("Pre-allocate the accounts index, assuming this many accounts")
            .long_help(
                "Pre-allocate the accounts index, assuming this many accounts. Overrides the \
                 account count recorded in the local snapshot directory, which is otherwise used \
                 when starting from local snapshot state rather than a snapshot archive.",
            )
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("accounts_index_path")
            .long("accounts-index-path")
            .value_name("PATH")
            .takes_value(true)
            .multiple(true)
            .help(
                "Persistent accounts-index location. May be specified multiple times. [default: \
                 <LEDGER>/accounts_index]",
            ),
    )
    .arg(
        Arg::with_name("accounts_shrink_optimize_total_space")
            .long("accounts-shrink-optimize-total-space")
            .takes_value(true)
            .value_name("BOOLEAN")
            .default_value(&default_args.accounts_shrink_optimize_total_space)
            .help(
                "When this is set to true, the system will shrink the most sparse accounts and \
                 when the overall shrink ratio is above the specified accounts-shrink-ratio, the \
                 shrink will stop and it will skip all other less sparse accounts.",
            ),
    )
    .arg(
        Arg::with_name("accounts_shrink_ratio")
            .long("accounts-shrink-ratio")
            .takes_value(true)
            .value_name("RATIO")
            .default_value(&default_args.accounts_shrink_ratio)
            .help(
                "Specifies the shrink ratio for the accounts to be shrunk. The shrink ratio is \
                 defined as the ratio of the bytes alive over the  total bytes used. If the \
                 account's shrink ratio is less than this ratio it becomes a candidate for \
                 shrinking. The value must between 0. and 1.0 inclusive.",
            ),
    )
    .arg(
        Arg::with_name("allow_private_addr")
            .long("allow-private-addr")
            .takes_value(false)
            .requires("no_xdp")
            .help("Allow contacting private ip addresses")
            .hidden(hidden_unless_forced()),
    )
    .arg(
        Arg::with_name("log_messages_bytes_limit")
            .long("log-messages-bytes-limit")
            .takes_value(true)
            .validator(is_parsable::<usize>)
            .value_name("BYTES")
            .help("Maximum number of bytes written to the program log before truncation"),
    )
    .arg(
        Arg::with_name("banking_trace_dir_byte_limit")
            // expose friendly alternative name to cli than internal
            // implementation-oriented one
            .long("enable-banking-trace")
            .value_name("BYTES")
            .validator(is_parsable::<DirByteLimit>)
            .takes_value(true)
            .default_value(&default_args.banking_trace_dir_byte_limit)
            .help(
                "Enables the banking trace that writes trace files for simulate-leader-blocks, \
                 retaining up to the specified total bytes in the ledger. Banking trace is \
                 disabled by default.",
            ),
    )
    .arg(
        Arg::with_name("no_delay_leader_block_for_pending_fork")
            .hidden(hidden_unless_forced())
            .long("no-delay-leader-block-for-pending-fork")
            .takes_value(false)
            .help(
                "Disable delaying leader block creation while replaying a block which descends \
                 from the current fork and has a lower slot than our next leader slot. If we \
                 don't delay here, our new leader block will be on a different fork from the \
                 block we are replaying and there is a high chance that the cluster will confirm \
                 that block's fork rather than our leader block's fork because it was created \
                 before we started creating ours.",
            ),
    )
    .arg(
        Arg::with_name("block_verification_method")
            .long("block-verification-method")
            .value_name("METHOD")
            .takes_value(true)
            .possible_values(BlockVerificationMethod::cli_names())
            .default_value(BlockVerificationMethod::default().into())
            .help(BlockVerificationMethod::cli_message()),
    )
    .arg(
        Arg::with_name("block_production_method")
            .long("block-production-method")
            .value_name("METHOD")
            .takes_value(true)
            .possible_values(BlockProductionMethod::cli_names())
            .default_value(BlockProductionMethod::default().into())
            .help(BlockProductionMethod::cli_message()),
    )
    .arg(
        Arg::with_name("block_production_pacing_fill_time_millis")
            .long("block-production-pacing-fill-time-millis")
            .value_name("MILLIS")
            .takes_value(true)
            .default_value(&default_args.block_production_pacing_fill_time_millis)
            .help(
                "Pacing fill time in milliseconds for the central-scheduler block production \
                 method",
            ),
    )
    .arg(
        Arg::with_name("filter_keys")
            .long("filter-keys")
            .value_name("PUBKEY")
            .takes_value(true)
            .min_values(1)
            .validator(is_pubkey)
            .help(
                "Drop internally processed leader-side transactions that touch any listed account \
                 pubkey. Values are space-separated. Using too many keys will negatively impact \
                 performance. External schedulers must implement this filtering themselves",
            ),
    )
    .arg(
        Arg::with_name("enable_scheduler_bindings")
            .long("enable-scheduler-bindings")
            .takes_value(false)
            .help("Enables external processes to connect and manage block production"),
    )
    .arg(
        Arg::with_name("unified_scheduler_handler_threads")
            .long("unified-scheduler-handler-threads")
            .value_name("COUNT")
            .takes_value(true)
            .validator(|s| is_within_range(s, 1..))
            .help(DefaultSchedulerPool::cli_message()),
    )
    .arg(
        Arg::with_name("replay_arenas")
            .long("experimental-replay-arenas")
            .value_name("COUNT")
            .takes_value(true)
            .validator(|s| is_within_range(s, 1..))
            .help(
                "Number of jemalloc arenas for replay. solReplayStage uses arena 0 and unified \
                 scheduler handler threads use the arenas in round-robin order",
            ),
    )
    .arg(
        Arg::with_name("config_file")
            .long("experimental-config-file")
            .takes_value(true)
            .hidden(hidden_unless_forced())
            .value_name("PATH")
            .help(
                "Path to an experimental TOML configuration file. The format may change \
                 incompatibly between releases. Currently configures XDP transmit on Linux",
            ),
    )
    .arg(
        Arg::with_name("no_xdp")
            .long("no-xdp")
            .takes_value(false)
            .conflicts_with("no_xdp_zero_copy")
            .help("Disable XDP transmit and fall back to UDP sockets"),
    )
    .arg(
        Arg::with_name("xdp_interface")
            .long("xdp-interface")
            .takes_value(true)
            .value_name("INTERFACE")
            .conflicts_with("no_xdp")
            .help(
                "Override the configured network interface for XDP transmit. The built-in policy \
                 selects the device carrying the default route",
            ),
    )
    .arg(
        Arg::with_name("xdp_cpu_cores")
            .long("xdp-cpu-cores")
            .takes_value(true)
            .value_name("CPU_LIST")
            .conflicts_with("no_xdp")
            .validator(|value| validate_cpu_ranges(value, "--xdp-cpu-cores"))
            .help(
                "Override the configured XDP workers with CPU cores to reserve (e.g. \"2-4,7\"). \
                 The built-in policy auto-selects one core",
            ),
    )
    .arg(
        Arg::with_name("xdp_zero_copy")
            .long("xdp-zero-copy")
            .takes_value(false)
            .conflicts_with("no_xdp")
            .conflicts_with("no_xdp_zero_copy")
            .help("Enable XDP zero copy mode. Requires hardware and driver support"),
    )
    .arg(
        Arg::with_name("no_xdp_zero_copy")
            .long("no-xdp-zero-copy")
            .takes_value(false)
            .conflicts_with("xdp_zero_copy")
            .conflicts_with("no_xdp")
            .help("Request XDP copy mode, overriding a zero_copy = true file setting"),
    )
    .args(&pub_sub_config::args(/*test_validator:*/ false))
    .args(&json_rpc_config::args())
    .args(&rpc_bigtable_config::args())
    .args(&send_transaction_config::args())
    .args(&rpc_bootstrap_config::args())
    .args(&blockstore_options::args())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::cli::thread_args::thread_args,
        std::{
            fs,
            path::{PathBuf, absolute},
        },
    };

    fn parse_run_args(identity_flag: &str, args: &[&str]) -> (Keypair, RunArgs) {
        let keypair = Keypair::new();
        let tmp_dir = tempfile::tempdir().unwrap();
        let file = tmp_dir.path().join("id.json");
        solana_keypair::write_keypair_file(&keypair, &file).unwrap();
        let default_args = DefaultArgs::default();
        let matches = add_args(App::new("run_command"), &default_args).get_matches_from(
            ["run_command", identity_flag, file.to_str().unwrap()]
                .iter()
                .chain(args),
        );
        (keypair, RunArgs::from_clap_arg_match(&matches).unwrap())
    }

    pub fn verify_args_struct_by_command_run<T>(args: Vec<&str>, expected: T)
    where
        T: FromClapArgMatches + PartialEq + std::fmt::Debug,
    {
        let default_args = DefaultArgs::default();
        let app = add_args(App::new("run_command"), &default_args)
            .args(&thread_args(&default_args.thread_args));
        crate::commands::tests::verify_args_struct_by_command::<T>(
            app,
            [&["run_command"], &args[..]].concat(),
            expected,
        );
    }

    pub fn verify_args_struct_by_command_run_is_error(args: Vec<&str>) {
        let default_args = DefaultArgs::default();
        let app = add_args(App::new("run_command"), &default_args)
            .args(&thread_args(&default_args.thread_args));
        assert!(
            app.get_matches_from_safe([&["run_command"], &args[..]].concat())
                .is_err()
        );
    }

    #[test]
    fn verify_args_struct_by_command_run_with_identity() {
        for flag in ["-i", "--identity"] {
            let (keypair, run_args) = parse_run_args(flag, &[]);
            assert_eq!(run_args.identity_keypair, keypair);
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_ledger_path() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let absolute_path = tmp_dir.path().join("ledger");
        let relative_path = PathBuf::from("relative_ledger");
        for (ledger_path, expected) in [
            (&absolute_path, absolute_path.clone()),
            (&relative_path, absolute(&relative_path).unwrap()),
        ] {
            let (_, run_args) =
                parse_run_args("--identity", &["--ledger", ledger_path.to_str().unwrap()]);
            assert_eq!(run_args.ledger_path, expected);
            assert!(!fs::exists(ledger_path).unwrap());
        }
    }

    #[test]
    fn verify_args_struct_by_command_run_with_invalid_gossip_port() {
        verify_args_struct_by_command_run_is_error(vec!["--gossip-port", "80O"]);
    }

    #[test]
    fn verify_args_struct_by_command_run_with_invalid_filter_keys() {
        verify_args_struct_by_command_run_is_error(vec!["--filter-keys", "not-a-pubkey"]);
    }

    #[test]
    fn verify_args_struct_by_command_run_with_log() {
        let (keypair, run_args) = parse_run_args("--identity", &[]);
        let default_logfile = format!("agave-validator-{}.log", keypair.pubkey());
        assert_eq!(run_args.logfile, Some(PathBuf::from(default_logfile)));
        let (_, run_args) = parse_run_args("--identity", &["-o", "-"]);
        assert_eq!(run_args.logfile, None);
        let (_, run_args) = parse_run_args("--identity", &["--log", "custom_log.log"]);
        assert_eq!(run_args.logfile, Some(PathBuf::from("custom_log.log")));
    }
}
