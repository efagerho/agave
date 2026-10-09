#[cfg(target_os = "linux")]
use super::xdp;
use {
    crate::{
        admin_rpc_service::{self, AdminRpcRequestMetadata, load_staked_nodes_overrides},
        bootstrap::{self, RpcBootstrapConfig},
        cli::{
            self,
            thread_args::{NumThreadConfig, parse_num_threads_args},
        },
        commands::{
            FromClapArgMatches,
            run::args::{RunArgs, validators_set},
        },
        ledger_lockfile,
    },
    agave_snapshots::{
        ArchiveFormat, SnapshotInterval, SnapshotVersion,
        paths::BANK_SNAPSHOTS_DIR,
        snapshot_config::{SnapshotConfig, SnapshotUsage},
    },
    agave_votor::vote_history_storage::FileVoteHistoryStorage,
    agave_votor_transport::MAX_ENDPOINTS,
    agave_xdp::transmitter::XdpConfig,
    arc_swap::ArcSwap,
    bytesize::ByteSize,
    clap::{ArgMatches, crate_name, value_t, values_t},
    crossbeam_channel::unbounded,
    log::*,
    rand::{rng, seq::SliceRandom},
    solana_accounts_db::{
        accounts_db::{
            AccountShrinkThreshold, AccountsDbConfig, TOTAL_IO_URING_BUFFERS_SIZE_LIMIT,
        },
        accounts_file::AccountsFileProvider,
        accounts_index::{
            AccountSecondaryIndexes, AccountsIndexConfig, DEFAULT_NUM_ENTRIES_OVERHEAD,
            DEFAULT_NUM_ENTRIES_TO_EVICT, IndexLimit, IndexLimitThreshold,
            MINIMAL_THRESHOLD_NUM_BYTES, ScanFilter,
        },
        partitioned_rewards::PartitionedEpochRewardsConfig,
        utils::{
            create_all_accounts_run_and_snapshot_dirs, create_and_canonicalize_directories,
            create_and_canonicalize_directory,
        },
    },
    solana_clap_utils::input_parsers::{keypairs_of, pubkey_of, value_of, values_of},
    solana_clock::{DEFAULT_SLOTS_PER_EPOCH, Slot},
    solana_core::{
        banking_stage::transaction_scheduler::scheduler_controller::SchedulerConfig,
        consensus::tower_storage::FileTowerStorage,
        repair::repair_handler::RepairHandlerType,
        resource_limits,
        snapshot_packager_service::SnapshotPackagerService,
        system_monitor_service::SystemMonitorService,
        tpu::MAX_VOTES_PER_SECOND,
        validator::{
            BlockProductionMethod, BlockVerificationMethod, SchedulerPacing, Validator,
            ValidatorConfig, ValidatorLogConfig, ValidatorStartProgress, ValidatorTpuConfig,
            is_snapshot_config_valid,
        },
    },
    solana_genesis_utils::MAX_GENESIS_ARCHIVE_UNPACKED_SIZE,
    solana_gossip::{
        cluster_info::{DEFAULT_CONTACT_SAVE_INTERVAL_MILLIS, NodeConfig},
        contact_info::ContactInfo,
        node::Node,
    },
    solana_keypair::Keypair,
    solana_ledger::{
        blockstore_options::BlockstoreCleanupStrategy,
        shred::filter::TurbineMode,
        use_snapshot_archives_at_startup::{self, UseSnapshotArchivesAtStartup},
    },
    solana_message::v1::MAX_TRANSACTION_SIZE,
    solana_net_utils::{SocketAddrSpace, multihomed_sockets::BindIpAddrs},
    solana_poh::poh_service,
    solana_pubkey::Pubkey,
    solana_runtime::{runtime_config::RuntimeConfig, snapshot_utils},
    solana_signer::Signer,
    solana_streamer::{
        nonblocking::{simple_qos::SimpleQosConfig, swqos::SwQosConfig},
        quic::{QuicStreamerConfig, SimpleQosQuicStreamerConfig, SwQosQuicStreamerConfig},
    },
    solana_turbine::broadcast_stage::BroadcastStageType,
    solana_validator_exit::Exit,
    std::{
        env,
        error::Error,
        fmt::Display,
        fs::File,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        num::{NonZeroU64, NonZeroUsize},
        path::{Path, PathBuf},
        sync::{Arc, RwLock, atomic::AtomicBool},
        time::SystemTime,
    },
};

#[derive(Debug, PartialEq, Eq)]
pub enum Operation {
    Initialize,
    Run,
}

pub fn execute(
    matches: &ArgMatches,
    solana_version: &str,
    operation: Operation,
    config: super::Config,
) -> Result<(), Box<dyn Error>> {
    // Debugging panics is easier with a backtrace
    if env::var_os("RUST_BACKTRACE").is_none() {
        // Safety: env update is made before any spawned threads might access the environment
        unsafe { env::set_var("RUST_BACKTRACE", "1") }
    }

    let run_args = RunArgs::from_clap_arg_match(matches)?;
    let log_config = run_args
        .logfile
        .clone()
        .map(|logfile| {
            println!("log file: {}", logfile.display());
            Validator::register_logrotate_signal_handler().map(|logrotate_flag| {
                ValidatorLogConfig {
                    logfile,
                    logrotate_flag,
                }
            })
        })
        .transpose()?;
    let use_progress_bar = log_config.is_none();
    agave_logger::initialize_logging(run_args.logfile.clone());
    cli::warn_for_deprecated_arguments(matches);
    info!("{} {}", crate_name!(), solana_version);
    info!("Starting validator with: {:#?}", env::args_os());
    solana_metrics::set_host_id(run_args.identity_keypair.pubkey().to_string());
    solana_metrics::set_panic_hook("validator", Some(String::from(solana_version)));
    solana_core::validator::report_target_features();

    let mut run_config = step("parsing configuration", || {
        RunConfig::new(matches, run_args, &operation)
    })?;
    run_config.validator.log_config = log_config;

    let mut ledger_lock = ledger_lockfile(&run_config.ledger_path);
    let _ledger_write_guard = step("locking ledger", || {
        ledger_lock.try_write().map_err(|_| {
            "unable to lock the ledger directory, check if another validator is running"
        })
    })?;
    step("preparing storage", || run_config.prepare_storage())?;

    let exit = Arc::new(AtomicBool::new(false));
    #[cfg(target_os = "linux")]
    let xdp = step("configuring capabilities and XDP", || {
        let validator = &run_config.validator;
        xdp::setup(
            run_config.xdp.clone(),
            validator.snapshot_packager_niceness_adj != 0
                || validator.rpc_config.rpc_niceness_adj != 0,
            config.primordial_caps,
            &run_config.node.bind_ip_addrs,
            exit.clone(),
        )
    })?;
    #[cfg(not(target_os = "linux"))]
    let xdp = {
        let _ = config;
        None::<(_, _)>
    };
    let (xdp_transmit_setup, xdp_network_config_report) = xdp.unzip();
    run_config.validator.xdp_network_config_report = xdp_network_config_report;

    let mut node = step("configuring network", || run_config.bind_node())?;

    let RunConfig {
        identity_keypair,
        ledger_path,
        vote_account,
        authorized_voter_keypairs,
        entrypoints,
        socket_addr_space,
        validator: mut validator_config,
        tpu,
        bootstrap: bootstrap_config,
        do_port_check,
        maximum_local_snapshot_age,
        minimal_snapshot_download_speed,
        maximum_snapshot_download_abort,
        init_complete_file,
        ..
    } = run_config;

    let start_progress = Arc::new(RwLock::new(ValidatorStartProgress::default()));
    let admin_service_post_init = Arc::new(RwLock::new(None));
    let (rpc_to_plugin_manager_sender, rpc_to_plugin_manager_receiver) = (validator_config
        .on_start_geyser_plugin_config_files
        .is_some()
        || validator_config.geyser_plugin_always_enabled)
        .then(unbounded)
        .unzip();
    admin_rpc_service::run(
        &ledger_path,
        AdminRpcRequestMetadata {
            rpc_addr: validator_config.rpc_addrs.map(|(rpc_addr, _)| rpc_addr),
            start_time: SystemTime::now(),
            validator_exit: validator_config.validator_exit.clone(),
            validator_exit_backpressure: validator_config.validator_exit_backpressure.clone(),
            start_progress: start_progress.clone(),
            authorized_voter_keypairs: authorized_voter_keypairs.clone(),
            post_init: admin_service_post_init.clone(),
            tower_storage: validator_config.tower_storage.clone(),
            vote_history_storage: validator_config.vote_history_storage.clone(),
            staked_nodes_overrides: validator_config.staked_nodes_overrides.clone(),
            rpc_to_plugin_manager_sender,
        },
    );

    let cluster_entrypoints: Vec<_> = entrypoints
        .iter()
        .map(ContactInfo::new_gossip_entry_point)
        .collect();
    if !cluster_entrypoints.is_empty() {
        bootstrap::rpc_bootstrap(
            &node,
            &identity_keypair,
            &ledger_path,
            &vote_account,
            authorized_voter_keypairs.clone(),
            &cluster_entrypoints,
            &mut validator_config,
            bootstrap_config,
            do_port_check,
            use_progress_bar,
            maximum_local_snapshot_age,
            &start_progress,
            minimal_snapshot_download_speed,
            maximum_snapshot_download_abort,
            socket_addr_space,
        );
        *start_progress.write().unwrap() = ValidatorStartProgress::Initializing;
    }

    if operation == Operation::Initialize {
        info!("Validator ledger initialization complete");
        return Ok(());
    }

    // Bootstrap code above pushes a contact-info with more recent timestamp to
    // gossip. If the node is staked the contact-info lingers in gossip causing
    // false duplicate nodes error.
    // Below line refreshes the timestamp on contact-info so that it overrides
    // the one pushed by bootstrap.
    node.info.hot_swap_pubkey(identity_keypair.pubkey());

    let validator = step("starting validator", || {
        Validator::new_with_exit(
            node,
            identity_keypair,
            &ledger_path,
            &vote_account,
            authorized_voter_keypairs,
            cluster_entrypoints,
            &validator_config,
            rpc_to_plugin_manager_receiver,
            start_progress,
            socket_addr_space,
            tpu,
            admin_service_post_init,
            xdp_transmit_setup,
            exit,
        )
        .map_err(|err| format!("{err:?}"))
    })?;

    if let Some(filename) = init_complete_file {
        File::create(&filename)
            .map_err(|err| format!("unable to create {}: {err}", filename.display()))?;
    }
    info!("Validator initialized");
    validator.listen_for_signals()?;
    validator.close();
    info!("Validator exiting...");

    Ok(())
}

fn step<T, E: Display>(name: &str, f: impl FnOnce() -> Result<T, E>) -> Result<T, Box<dyn Error>> {
    info!("Startup: {name}");
    f().map_err(|err| format!("{name}: {err}").into())
}

/// Validator configuration parsed and validated from the command line, before any startup step
/// touches the system.
pub struct RunConfig {
    pub identity_keypair: Arc<Keypair>,
    pub ledger_path: PathBuf,
    pub vote_account: Pubkey,
    pub authorized_voter_keypairs: Arc<RwLock<Vec<Arc<Keypair>>>>,
    pub entrypoints: Vec<SocketAddr>,
    pub socket_addr_space: SocketAddrSpace,
    /// An unspecified `advertised_ip` is resolved by `bind_node`
    pub node: NodeConfig,
    pub public_rpc_addr: Option<SocketAddr>,
    pub private_rpc: bool,
    pub restricted_repair_only_mode: bool,
    pub check_os_network_limits: bool,
    pub xdp: Option<XdpConfig>,
    pub validator: ValidatorConfig,
    pub tpu: ValidatorTpuConfig,
    pub bootstrap: RpcBootstrapConfig,
    pub do_port_check: bool,
    pub maximum_local_snapshot_age: Slot,
    pub minimal_snapshot_download_speed: f32,
    pub maximum_snapshot_download_abort: u64,
    pub init_complete_file: Option<PathBuf>,
}

impl RunConfig {
    pub fn new(
        matches: &ArgMatches,
        run_args: RunArgs,
        operation: &Operation,
    ) -> Result<Self, Box<dyn Error>> {
        let threads = parse_num_threads_args(matches);
        let identity_keypair = Arc::new(run_args.identity_keypair);
        let identity = identity_keypair.pubkey();
        let ledger_path = run_args.ledger_path;

        for addr in &run_args.entrypoints {
            if !run_args.socket_addr_space.check(addr) {
                return Err(format!("invalid entrypoint address: {addr}").into());
            }
        }

        let bind_addresses = BindIpAddrs::new(
            matches
                .values_of("bind_address")
                .expect("bind_address should always be present due to default")
                .map(solana_net_utils::parse_host)
                .collect::<Result<_, _>>()?,
        )
        .map_err(|err| format!("invalid bind_addresses: {err}"))?;
        for (arg, flag) in [
            ("advertised_ip", "--advertised-ip"),
            ("public_tpu_addr", "--public-tpu-address"),
            ("public_tvu_addr", "--public-tvu-address"),
        ] {
            if bind_addresses.len() > 1 && matches.is_present(arg) {
                return Err(format!("{flag} cannot be used in a multihoming context").into());
            }
        }

        let num_votor_quic_endpoints = value_t!(matches, "num_votor_endpoints", NonZeroUsize)?;
        if num_votor_quic_endpoints.get() > MAX_ENDPOINTS {
            return Err(format!("--num-votor-endpoints must be at most {MAX_ENDPOINTS}").into());
        }

        let private_rpc = matches.is_present("private_rpc");
        let rpc_bind_address = match matches.value_of("rpc_bind_address") {
            Some(addr) => solana_net_utils::parse_host(addr)
                .map_err(|err| format!("failed to parse --rpc-bind-address: {err}"))?,
            None if private_rpc => IpAddr::V4(Ipv4Addr::LOCALHOST),
            None => bind_addresses.active(),
        };

        let advertised_ip = match matches.value_of("advertised_ip") {
            Some(ip) => solana_net_utils::parse_host(ip)
                .map_err(|err| format!("failed to parse --advertised-ip: {err}"))?,
            None if bind_addresses.active().is_loopback() => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            None => bind_addresses.active(),
        };

        #[cfg(target_os = "linux")]
        let poh_pinned_cpu_core = value_of(matches, "poh_pinned_cpu_core")
            .or_else(|| value_of(matches, "experimental_poh_pinned_cpu_core"))
            .or(poh_service::DEFAULT_PINNED_CPU_CORE);
        #[cfg(not(target_os = "linux"))]
        let poh_pinned_cpu_core = None;

        #[cfg(target_os = "linux")]
        let xdp = xdp::build_xdp_config(matches, operation, &bind_addresses, poh_pinned_cpu_core)?;
        #[cfg(not(target_os = "linux"))]
        let xdp = {
            let _ = operation;
            None
        };

        let node = NodeConfig {
            advertised_ip,
            gossip_port: value_t!(matches, "gossip_port", u16).unwrap_or(0),
            port_range: solana_net_utils::parse_port_range(
                matches.value_of("dynamic_port_range").unwrap(),
            )
            .ok_or("invalid --dynamic-port-range")?,
            bind_ip_addrs: bind_addresses,
            public_tpu_addr: host_port_of(matches, "public_tpu_addr", "--public-tpu-address")?,
            public_tpu_forwards_addr: host_port_of(
                matches,
                "public_tpu_forwards_addr",
                "--public-tpu-forwards-address",
            )?,
            public_tvu_addr: host_port_of(matches, "public_tvu_addr", "--public-tvu-address")?,
            num_tvu_receive_sockets: threads.tvu_receive_threads,
            num_tvu_retransmit_sockets: threads.tvu_retransmit_threads,
            num_quic_endpoints: value_t!(matches, "num_quic_endpoints", NonZeroUsize)?,
            num_votor_quic_endpoints,
        };

        let authorized_voter_keypairs = Arc::new(RwLock::new(
            keypairs_of(matches, "authorized_voter_keypairs")
                .map(|keypairs| keypairs.into_iter().map(Arc::new).collect())
                .unwrap_or_else(|| vec![identity_keypair.clone()]),
        ));

        let staked_nodes_overrides_path = matches.value_of("staked_nodes_overrides");
        let staked_nodes_overrides = staked_nodes_overrides_path
            .map(|path| {
                load_staked_nodes_overrides(&path.to_string()).map_err(|err| {
                    format!("failed to load --staked-nodes-overrides from {path}: {err}")
                })
            })
            .transpose()?
            .unwrap_or_default()
            .staked_map_id;

        let restricted_repair_only_mode = matches.is_present("restricted_repair_only_mode");
        let mut voting_disabled = matches.is_present("no_voting") || restricted_repair_only_mode;
        let vote_account = pubkey_of(matches, "vote_account").unwrap_or_else(|| {
            if !voting_disabled {
                warn!("--vote-account not specified, validator will not vote");
                voting_disabled = true;
            }
            Keypair::new().pubkey()
        });

        let snapshot_config = new_snapshot_config(
            matches,
            &ledger_path,
            run_args.rpc_bootstrap_config.incremental_snapshot_fetch,
        )?;
        let skip_transaction_signatures_in_status_cache =
            !run_args.json_rpc_config.full_api && !snapshot_config.should_generate_snapshots();
        if skip_transaction_signatures_in_status_cache {
            info!(
                "Transaction signatures will not be stored in the status cache because full RPC \
                 and snapshot generation are disabled"
            );
        }

        let validator = ValidatorConfig {
            log_config: None,
            require_tower: matches.is_present("require_tower"),
            require_vote_history: !matches.is_present("do_not_require_vote_history"),
            tower_storage: Arc::new(FileTowerStorage::new(
                value_t!(matches, "tower", PathBuf).unwrap_or_else(|_| ledger_path.clone()),
            )),
            vote_history_storage: Arc::new(FileVoteHistoryStorage::new(ledger_path.clone())),
            max_genesis_archive_unpacked_size: MAX_GENESIS_ARCHIVE_UNPACKED_SIZE,
            expected_genesis_hash: value_of(matches, "expected_genesis_hash"),
            fixed_leader_schedule: None,
            expected_bank_hash: value_of(matches, "expected_bank_hash"),
            expected_shred_version: value_t!(matches, "expected_shred_version", u16).ok(),
            new_hard_forks: values_of(matches, "hard_forks"),
            runtime_config: RuntimeConfig {
                log_messages_bytes_limit: value_of(matches, "log_messages_bytes_limit"),
                skip_transaction_signatures_in_status_cache,
                ..RuntimeConfig::default()
            },
            rpc_config: run_args.json_rpc_config,
            on_start_geyser_plugin_config_files: values_of(matches, "geyser_plugin_config"),
            geyser_plugin_always_enabled: matches.is_present("geyser_plugin_always_enabled"),
            rpc_addrs: value_t!(matches, "rpc_port", u16).ok().map(|rpc_port| {
                (
                    SocketAddr::new(rpc_bind_address, rpc_port),
                    SocketAddr::new(rpc_bind_address, rpc_port + 1),
                    // If additional ports are added, +2 needs to be skipped to avoid a conflict
                    // with the websocket port (which is +2) in web3.js This odd port shifting is
                    // tracked at https://github.com/solana-labs/solana/issues/12250
                )
            }),
            pubsub_config: run_args.pub_sub_config,
            voting_disabled,
            wait_for_supermajority: value_t!(matches, "wait_for_supermajority", Slot).ok(),
            known_validators: run_args.known_validators,
            repair_validators: validators_set(
                &identity,
                matches,
                "repair_validators",
                "--repair-validator",
            )?,
            should_check_duplicate_instance: true,
            repair_whitelist: Arc::new(RwLock::new(
                validators_set(&identity, matches, "repair_whitelist", "--repair-whitelist")?
                    .unwrap_or_default(),
            )),
            // Identities named on the command line carry no address: the peer list resolves
            // them from gossip.
            votor_peer_overrides: Arc::new(ArcSwap::from_pointee(
                validators_set(
                    &identity,
                    matches,
                    "votor_peer_overrides",
                    "--votor-peer-overrides",
                )?
                .unwrap_or_default()
                .into_iter()
                .map(|pubkey| (pubkey, None))
                .collect(),
            )),
            repair_handler_type: RepairHandlerType::default(),
            gossip_validators: validators_set(
                &identity,
                matches,
                "gossip_validators",
                "--gossip-validator",
            )?,
            blockstore_cleanup_strategy: BlockstoreCleanupStrategy::from_clap_arg_match(matches)?,
            blockstore_options: run_args.blockstore_options,
            run_verification: !matches.is_present("skip_startup_ledger_verification"),
            debug_keys: values_of(matches, "debug_key")
                .map(|keys: Vec<Pubkey>| Arc::new(keys.into_iter().collect())),
            filter_keys: Arc::new(run_args.filter_keys),
            warp_slot: None,
            generator_config: None,
            contact_debug_interval: value_t!(matches, "contact_debug_interval", u64)?,
            contact_save_interval: DEFAULT_CONTACT_SAVE_INTERVAL_MILLIS,
            send_transaction_service_config: run_args.send_transaction_service_config,
            no_poh_speed_test: matches.is_present("no_poh_speed_test"),
            no_os_memory_stats_reporting: matches.is_present("no_os_memory_stats_reporting"),
            no_os_network_stats_reporting: matches.is_present("no_os_network_stats_reporting"),
            xdp_network_config_report: None,
            no_os_cpu_stats_reporting: matches.is_present("no_os_cpu_stats_reporting"),
            no_os_disk_stats_reporting: matches.is_present("no_os_disk_stats_reporting"),
            // The validator needs to open many files, check that the process has
            // permission to do so in order to fail quickly and give a direct error
            enforce_ulimit_nofile: true,
            poh_pinned_cpu_core,
            poh_hashes_per_batch: value_of(matches, "poh_hashes_per_batch")
                .unwrap_or(poh_service::DEFAULT_HASHES_PER_BATCH),
            process_ledger_before_services: matches.is_present("process_ledger_before_services"),
            // Replaced by the run and snapshot directories in `prepare_storage`
            account_paths: values_t!(matches, "account_paths", String).map_or_else(
                |_| vec![ledger_path.join("accounts")],
                |paths| {
                    paths
                        .iter()
                        .flat_map(|p| p.split(','))
                        .map(PathBuf::from)
                        .collect()
                },
            ),
            account_snapshot_paths: vec![],
            accounts_db_config: new_accounts_db_config(matches, &ledger_path, &threads)?,
            snapshot_config,
            no_wait_for_vote_to_start_leader: matches
                .is_present("no_wait_for_vote_to_start_leader"),
            wait_to_vote_slot: value_t!(matches, "wait_to_vote_slot", Slot).ok(),
            staked_nodes_overrides: Arc::new(RwLock::new(staked_nodes_overrides)),
            use_snapshot_archives_at_startup: value_t!(
                matches,
                use_snapshot_archives_at_startup::cli::NAME,
                UseSnapshotArchivesAtStartup
            )?,
            ip_echo_server_threads: threads.ip_echo_server_threads,
            rayon_global_threads: threads.rayon_global_threads,
            replay_forks_threads: threads.replay_forks_threads,
            replay_transactions_threads: threads.replay_transactions_threads,
            tvu_shred_sigverify_threads: threads.tvu_sigverify_threads,
            tvu_bls_sigverify_threads: threads.tvu_bls_sigverify_threads,
            delay_leader_block_for_pending_fork: !matches
                .is_present("no_delay_leader_block_for_pending_fork"),
            turbine_mode: TurbineMode::default(),
            broadcast_stage_type: BroadcastStageType::Standard,
            block_verification_method: value_t!(
                matches,
                "block_verification_method",
                BlockVerificationMethod
            )?,
            unified_scheduler_handler_threads: value_t!(
                matches,
                "unified_scheduler_handler_threads",
                usize
            )
            .ok(),
            replay_arenas: value_t!(matches, "replay_arenas", usize).ok(),
            block_production_method: value_t!(
                matches,
                "block_production_method",
                BlockProductionMethod
            )?,
            block_production_num_workers: threads.block_production_num_workers,
            block_production_scheduler_config: SchedulerConfig {
                scheduler_pacing: value_t!(
                    matches,
                    "block_production_pacing_fill_time_millis",
                    SchedulerPacing
                )?,
            },
            enable_block_production_forwarding: staked_nodes_overrides_path.is_some(),
            enable_scheduler_bindings: matches.is_present("enable_scheduler_bindings"),
            banking_trace_dir_byte_limit: value_t!(matches, "banking_trace_dir_byte_limit", u64)?,
            validator_exit: Arc::new(RwLock::new(Exit::default())),
            validator_exit_backpressure: [(
                SnapshotPackagerService::NAME.to_string(),
                Arc::new(AtomicBool::new(false)),
            )]
            .into(),
            snapshot_packager_niceness_adj: value_t!(
                matches,
                "snapshot_packager_niceness_adj",
                i8
            )?,
        };
        validator.block_production_method.warn_if_deprecated_value();

        Ok(Self {
            identity_keypair,
            ledger_path,
            vote_account,
            authorized_voter_keypairs,
            entrypoints: run_args.entrypoints,
            socket_addr_space: run_args.socket_addr_space,
            node,
            public_rpc_addr: host_port_of(matches, "public_rpc_addr", "--public-rpc-address")?,
            private_rpc,
            restricted_repair_only_mode,
            check_os_network_limits: !matches.is_present("no_os_network_limits_test"),
            xdp,
            validator,
            tpu: new_tpu_config(matches, &threads)?,
            bootstrap: run_args.rpc_bootstrap_config,
            do_port_check: !matches.is_present("no_port_check"),
            maximum_local_snapshot_age: value_t!(matches, "maximum_local_snapshot_age", u64)?,
            minimal_snapshot_download_speed: value_t!(
                matches,
                "minimal_snapshot_download_speed",
                f32
            )?,
            maximum_snapshot_download_abort: value_t!(
                matches,
                "maximum_snapshot_download_abort",
                u64
            )?,
            init_complete_file: value_of(matches, "init_complete_file"),
        })
    }

    /// Creates the account and snapshot directories and replaces their paths with canonical ones
    pub fn prepare_storage(&mut self) -> Result<(), Box<dyn Error>> {
        let config = &mut self.validator;
        let account_paths = create_and_canonicalize_directories(&config.account_paths)
            .map_err(|err| format!("unable to access account path: {err}"))?;
        let snapshot_config = &mut config.snapshot_config;
        for dir in [
            &mut snapshot_config.bank_snapshots_dir,
            &mut snapshot_config.full_snapshot_archives_dir,
            &mut snapshot_config.incremental_snapshot_archives_dir,
        ] {
            *dir = create_and_canonicalize_directory(&dir)
                .map_err(|err| format!("failed to create directory '{}': {err}", dir.display()))?;
        }
        let snapshots_dir = snapshot_config.bank_snapshots_dir.parent();
        if account_paths
            .iter()
            .any(|path| Some(path.as_path()) == snapshots_dir)
        {
            return Err(
                "the --accounts and --snapshots paths must be unique since they both create \
                 'snapshots' subdirectories, otherwise there may be collisions"
                    .into(),
            );
        }
        (config.account_paths, config.account_snapshot_paths) =
            create_all_accounts_run_and_snapshot_dirs(&account_paths)
                .map_err(|err| format!("unable to create account directories: {err}"))?;
        snapshot_utils::remove_tmp_snapshot_archives(&snapshot_config.full_snapshot_archives_dir);
        snapshot_utils::remove_tmp_snapshot_archives(
            &snapshot_config.incremental_snapshot_archives_dir,
        );
        Ok(())
    }

    /// Resolves the advertised address and shred version, then binds the node's sockets
    pub fn bind_node(&mut self) -> Result<Node, Box<dyn Error>> {
        if self.check_os_network_limits {
            if !SystemMonitorService::check_os_network_limits() {
                return Err("OS network limit test failed. See \
                            https://docs.anza.xyz/operations/guides/validator-start#system-tuning"
                    .into());
            }
            info!("OS network limits test passed.");
        }

        let bind_ip = self.node.bind_ip_addrs.active();
        if self.node.advertised_ip.is_unspecified() {
            self.node.advertised_ip = if self.entrypoints.is_empty() {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                query_entrypoints(&self.entrypoints, "public IP address", |entrypoint| {
                    solana_net_utils::get_public_ip_addr_with_binding(entrypoint, bind_ip)
                        .map_err(|err| err.to_string())
                })
                .ok_or("unable to determine the validator's public IP address")?
            };
        }
        // TODO: Once entrypoints are updated to return shred-version, this should
        // abort if it fails to obtain a shred-version, so that nodes always join
        // gossip with a valid shred-version. The code to adopt entrypoint shred
        // version can then be deleted from gossip and get_rpc_node above.
        if self.validator.expected_shred_version.is_none() {
            self.validator.expected_shred_version =
                query_entrypoints(&self.entrypoints, "shred version", |entrypoint| {
                    match solana_net_utils::get_cluster_shred_version_with_binding(
                        entrypoint, bind_ip,
                    ) {
                        Ok(0) => Err("shred version is zero".to_string()),
                        result => result.map_err(|err| err.to_string()),
                    }
                });
        }

        let mut node =
            Node::new_with_external_ip(&self.identity_keypair.pubkey(), self.node.clone());
        if self.restricted_repair_only_mode {
            // When in --restricted_repair_only_mode is enabled only the gossip and repair ports
            // need to be reachable by the entrypoint to respond to gossip pull requests and
            // repair requests initiated by the node.  All other ports are unused.
            node.info.remove_tpu();
            node.info.remove_tpu_forwards();
            node.info.remove_tvu();
            node.info.remove_serve_repair();
            node.info.remove_alpenglow();
            // A node in this configuration shouldn't be an entrypoint to other nodes
            node.sockets.ip_echo = None;
        }
        if !self.private_rpc {
            let ip = self.node.advertised_ip;
            let public_rpc_addrs = self.public_rpc_addr.map(|addr| (addr, addr)).or(self
                .validator
                .rpc_addrs
                .map(|(rpc, pubsub)| {
                    (
                        SocketAddr::new(ip, rpc.port()),
                        SocketAddr::new(ip, pubsub.port()),
                    )
                }));
            if let Some((rpc, pubsub)) = public_rpc_addrs {
                node.info
                    .set_rpc(rpc)
                    .map_err(|err| format!("invalid RPC address {rpc}: {err}"))?;
                node.info
                    .set_rpc_pubsub(pubsub)
                    .map_err(|err| format!("invalid RPC pubsub address {pubsub}: {err}"))?;
            }
        }
        Ok(node)
    }
}

fn host_port_of(
    matches: &ArgMatches,
    name: &str,
    flag: &str,
) -> Result<Option<SocketAddr>, String> {
    matches
        .value_of(name)
        .map(|addr| {
            solana_net_utils::parse_host_port(addr)
                .map_err(|err| format!("failed to parse {flag}: {err}"))
        })
        .transpose()
}

fn query_entrypoints<T: Display>(
    entrypoints: &[SocketAddr],
    what: &str,
    query: impl Fn(&SocketAddr) -> Result<T, String>,
) -> Option<T> {
    let mut entrypoints = entrypoints.to_vec();
    entrypoints.shuffle(&mut rng());
    entrypoints.iter().find_map(|entrypoint| {
        info!("Contacting {entrypoint} to determine the {what}");
        query(entrypoint)
            .inspect(|value| info!("Obtained {what} {value} from {entrypoint}"))
            .inspect_err(|err| warn!("Failed to obtain {what} from {entrypoint}: {err}"))
            .ok()
    })
}

fn new_tpu_config(
    matches: &ArgMatches,
    threads: &NumThreadConfig,
) -> Result<ValidatorTpuConfig, clap::Error> {
    let max_connections_per_ipaddr_per_min =
        value_t!(matches, "tpu_max_connections_per_ipaddr_per_minute", u64)?;
    let max_connections_per_peer = value_t!(matches, "tpu_max_connections_per_peer", usize).ok();
    let max_connections_per_peer =
        |name: &str| max_connections_per_peer.map_or_else(|| value_t!(matches, name, usize), Ok);
    let sw_qos = |num_threads,
                  max_staked_connections: &str,
                  max_unstaked_connections: &str|
     -> Result<_, clap::Error> {
        Ok(SwQosQuicStreamerConfig {
            quic_streamer_config: QuicStreamerConfig {
                max_connections_per_ipaddr_per_min,
                num_threads,
                stream_receive_window_size: MAX_TRANSACTION_SIZE as u32,
                max_stream_data_bytes: MAX_TRANSACTION_SIZE as u32,
                ..Default::default()
            },
            qos_config: SwQosConfig {
                max_connections_per_staked_peer: max_connections_per_peer(
                    "tpu_max_connections_per_staked_peer",
                )?,
                max_connections_per_unstaked_peer: max_connections_per_peer(
                    "tpu_max_connections_per_unstaked_peer",
                )?,
                max_staked_connections: value_t!(matches, max_staked_connections, usize)?,
                max_unstaked_connections: value_t!(matches, max_unstaked_connections, usize)?,
                max_streams_per_ms: value_t!(matches, "tpu_max_streams_per_ms", u64)?,
            },
        })
    };
    Ok(ValidatorTpuConfig {
        vote_use_quic: value_t!(matches, "vote_use_quic", bool)?,
        tpu_quic_server_config: sw_qos(
            threads.tpu_transaction_receive_threads,
            "tpu_max_staked_connections",
            "tpu_max_unstaked_connections",
        )?,
        tpu_fwd_quic_server_config: sw_qos(
            threads.tpu_transaction_forward_receive_threads,
            "tpu_max_fwd_staked_connections",
            "tpu_max_fwd_unstaked_connections",
        )?,
        vote_quic_server_config: SimpleQosQuicStreamerConfig {
            quic_streamer_config: QuicStreamerConfig {
                max_connections_per_ipaddr_per_min,
                num_threads: threads.tpu_vote_transaction_receive_threads,
                ..Default::default()
            },
            qos_config: SimpleQosConfig {
                max_streams_per_second: MAX_VOTES_PER_SECOND,
                ..Default::default()
            },
        },
        sigverify_threads: threads.tpu_sigverify_threads,
    })
}

fn new_accounts_db_config(
    matches: &ArgMatches,
    ledger_path: &Path,
    threads: &NumThreadConfig,
) -> Result<AccountsDbConfig, Box<dyn Error>> {
    let shrink_ratio = value_t!(matches, "accounts_shrink_ratio", f64)?;
    if !(0.0..=1.0).contains(&shrink_ratio) {
        return Err(format!(
            "the specified account-shrink-ratio is invalid, it must be between 0. and 1.0 \
             inclusive: {shrink_ratio}"
        )
        .into());
    }
    let shrink_ratio = if value_t!(matches, "accounts_shrink_optimize_total_space", bool)? {
        AccountShrinkThreshold::TotalSpace { shrink_ratio }
    } else {
        AccountShrinkThreshold::IndividualStore { shrink_ratio }
    };

    let index_limit = |num_bytes| {
        IndexLimit::Threshold(IndexLimitThreshold {
            num_bytes,
            num_entries_overhead: DEFAULT_NUM_ENTRIES_OVERHEAD,
            num_entries_to_evict: DEFAULT_NUM_ENTRIES_TO_EVICT,
        })
    };
    // --enable-accounts-disk-index is still handled until it is removed
    let index_limit = match value_t!(matches, "accounts_index_limit", String)?.as_str() {
        _ if matches.is_present("enable_accounts_disk_index") => {
            index_limit(MINIMAL_THRESHOLD_NUM_BYTES)
        }
        "minimal" => {
            warn!(
                "Using `minimal` for `--accounts-index-limit` is deprecated. Using 25GB instead."
            );
            index_limit(MINIMAL_THRESHOLD_NUM_BYTES)
        }
        "unlimited" => IndexLimit::InMemOnly,
        limit => index_limit(limit.parse::<ByteSize>()?.as_u64()),
    };

    let read_cache_limit_bytes =
        match values_of::<ByteSize>(matches, "accounts_db_read_cache_limit").as_deref() {
            Some([lo, hi]) if lo > hi => {
                return Err(format!(
                    "invalid --accounts-db-read-cache-limit: LOW ({lo}) must be <= HIGH ({hi})"
                )
                .into());
            }
            Some([lo, hi]) => Some((usize::try_from(lo.0)?, usize::try_from(hi.0)?)),
            // clap enforces that two values are given
            _ => None,
        };

    const MB: u64 = 1_024 * 1_024;
    let write_cache_limit_bytes = value_of::<ByteSize>(matches, "accounts_db_write_cache_limit")
        .map(|limit| limit.0)
        // --accounts-db-cache-limit-mb was deprecated in v4.2.0
        .or_else(|| value_of::<u64>(matches, "accounts_db_cache_limit_mb").map(|mb| mb * MB));

    Ok(AccountsDbConfig {
        index: Some(AccountsIndexConfig {
            num_flush_threads: Some(threads.accounts_index_flush_threads),
            index_limit,
            bins: value_t!(matches, "accounts_index_bins", usize).ok(),
            num_initial_accounts: value_t!(matches, "accounts_index_initial_accounts_count", usize)
                .ok(),
            drives: Some(
                values_of(matches, "accounts_index_path")
                    .unwrap_or_else(|| vec![ledger_path.join("accounts_index")]),
            ),
            ..AccountsIndexConfig::default()
        }),
        account_indexes: Some(AccountSecondaryIndexes::from_clap_arg_match(matches)?),
        bank_hash_details_dir: ledger_path.to_path_buf(),
        shrink_ratio,
        read_cache_limit_bytes,
        read_cache_evict_sample_size: None,
        read_cache_num_shards: None,
        write_cache_limit_bytes,
        ancient_append_vec_offset: value_t!(matches, "accounts_db_ancient_append_vecs", i64).ok(),
        ancient_storage_ideal_size: value_t!(
            matches,
            "accounts_db_ancient_storage_ideal_size",
            u64
        )
        .ok(),
        max_ancient_storages: value_t!(matches, "accounts_db_max_ancient_storages", usize).ok(),
        skip_initial_hash_calc: false,
        verify_index: matches.is_present("accounts_db_verify_index"),
        partitioned_epoch_rewards_config: PartitionedEpochRewardsConfig::default(),
        scan_filter_for_shrinking: match matches.value_of("accounts_db_scan_filter_for_shrinking") {
            None => ScanFilter::default(),
            Some("all") => ScanFilter::All,
            Some("only-abnormal") => ScanFilter::OnlyAbnormal,
            Some("only-abnormal-with-verify") => ScanFilter::OnlyAbnormalWithVerify,
            Some(filter) => {
                unreachable!("clap allowed --accounts-db-scan-filter-for-shrinking {filter}")
            }
        },
        num_background_threads: Some(threads.accounts_db_background_threads),
        accounts_file_provider: match matches.value_of("accounts_db_account_storage_file_format") {
            Some("append-vec") => AccountsFileProvider::AppendVec,
            Some("split-experimental") => AccountsFileProvider::Split,
            format => {
                unreachable!("clap allowed --accounts-db-account-storage-file-format {format:?}")
            }
        },
    })
}

/// The snapshot directories are created and canonicalized by `RunConfig::prepare_storage`
fn new_snapshot_config(
    matches: &ArgMatches,
    ledger_path: &Path,
    incremental_snapshot_fetch: bool,
) -> Result<SnapshotConfig, Box<dyn Error>> {
    let (full_snapshot_archive_interval, incremental_snapshot_archive_interval) = if matches
        .is_present("no_snapshots")
    {
        (SnapshotInterval::Disabled, SnapshotInterval::Disabled)
    } else {
        let interval_slots = value_t!(matches, "snapshot_interval_slots", NonZeroU64)?;
        if incremental_snapshot_fetch {
            // --snapshot-interval-slots is the incremental snapshot interval
            let full_interval_slots =
                value_t!(matches, "full_snapshot_interval_slots", NonZeroU64)?;
            (
                SnapshotInterval::Slots(full_interval_slots),
                SnapshotInterval::Slots(interval_slots),
            )
        } else {
            // --snapshot-interval-slots is the *full* snapshot interval
            if matches.occurrences_of("full_snapshot_interval_slots") > 0 {
                warn!(
                    "Incremental snapshots are disabled, yet --full-snapshot-interval-slots was \
                     specified! Note that --full-snapshot-interval-slots is *ignored* when \
                     incremental snapshots are disabled. Use --snapshot-interval-slots instead.",
                );
            }
            (
                SnapshotInterval::Slots(interval_slots),
                SnapshotInterval::Disabled,
            )
        }
    };

    let describe = |interval| match interval {
        SnapshotInterval::Disabled => "disabled".to_string(),
        SnapshotInterval::Slots(interval) => format!("{interval} slots"),
    };
    info!(
        "Snapshot configuration: full snapshot interval: {}, incremental snapshot interval: {}",
        describe(full_snapshot_archive_interval),
        describe(incremental_snapshot_archive_interval),
    );
    // It is unlikely that a full snapshot interval greater than an epoch is a good idea.
    // Minimally we should warn the user in case this was a mistake.
    if let SnapshotInterval::Slots(interval) = full_snapshot_archive_interval
        && interval.get() > DEFAULT_SLOTS_PER_EPOCH
    {
        warn!(
            "The full snapshot interval is excessively large: {interval}! This will negatively \
             impact the background cleanup tasks in accounts-db. Consider a smaller value.",
        );
    }

    let mut archive_format =
        ArchiveFormat::from_cli_arg(&value_t!(matches, "snapshot_archive_format", String)?)
            .ok_or("unrecognized --snapshot-archive-format")?;
    if let ArchiveFormat::TarZstd { config } = &mut archive_format {
        config.compression_level = value_t!(matches, "snapshot_zstd_compression_level", i32)?;
    }

    let snapshots_dir = matches.value_of("snapshots").map_or(ledger_path, Path::new);
    let archives_dir = |name| {
        matches
            .value_of(name)
            .map_or(snapshots_dir, Path::new)
            .to_path_buf()
    };
    let snapshot_config = SnapshotConfig {
        usage: if full_snapshot_archive_interval == SnapshotInterval::Disabled {
            SnapshotUsage::LoadOnly
        } else {
            SnapshotUsage::LoadAndGenerate
        },
        full_snapshot_archive_interval,
        incremental_snapshot_archive_interval,
        bank_snapshots_dir: snapshots_dir.join(BANK_SNAPSHOTS_DIR),
        full_snapshot_archives_dir: archives_dir("full_snapshot_archive_path"),
        incremental_snapshot_archives_dir: archives_dir("incremental_snapshot_archive_path"),
        archive_format,
        snapshot_version: value_t!(matches, "snapshot_version", SnapshotVersion)?,
        maximum_full_snapshot_archives_to_retain: value_t!(
            matches,
            "maximum_full_snapshots_to_retain",
            NonZeroUsize
        )?,
        maximum_incremental_snapshot_archives_to_retain: value_t!(
            matches,
            "maximum_incremental_snapshots_to_retain",
            NonZeroUsize
        )?,
        use_registered_io_uring_buffers: resource_limits::check_memlock_limit_for_disk_io(
            TOTAL_IO_URING_BUFFERS_SIZE_LIMIT,
        ),
        use_direct_io: !matches.is_present("no_accounts_db_snapshots_direct_io"),
    };
    if !is_snapshot_config_valid(&snapshot_config) {
        return Err(
            "invalid snapshot configuration provided: snapshot intervals are incompatible. full \
             snapshot interval MUST be larger than incremental snapshot interval (if enabled)"
                .into(),
        );
    }
    Ok(snapshot_config)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::cli::{DefaultArgs, app},
        solana_keypair::write_keypair_file,
        tempfile::TempDir,
    };

    fn new_run_config(args: &[&str]) -> Result<RunConfig, Box<dyn Error>> {
        let dir = TempDir::new().unwrap();
        let identity = dir.path().join("identity.json");
        write_keypair_file(&Keypair::new(), &identity).unwrap();
        let ledger = dir.path().join("ledger");
        let default_args = DefaultArgs::default();
        let matches = app("test", &default_args).get_matches_from(
            [
                "agave-validator",
                "--no-xdp",
                "--identity",
                identity.to_str().unwrap(),
                "--ledger",
                ledger.to_str().unwrap(),
            ]
            .iter()
            .chain(args),
        );
        RunConfig::new(
            &matches,
            RunArgs::from_clap_arg_match(&matches)?,
            &Operation::Run,
        )
    }

    #[test]
    fn test_default_run_config_defers_network_resolution() {
        let config = new_run_config(&[]).unwrap();
        assert!(config.node.advertised_ip.is_unspecified());
        assert_eq!(config.validator.expected_shred_version, None);
        assert!(config.xdp.is_none());
    }

    #[test]
    fn test_multihoming_rejects_public_tvu_address() {
        let err = new_run_config(&[
            "--bind-address",
            "1.1.1.1",
            "--bind-address",
            "2.2.2.2",
            "--public-tvu-address",
            "1.1.1.1:8000",
        ])
        .err()
        .unwrap();
        assert_eq!(
            err.to_string(),
            "--public-tvu-address cannot be used in a multihoming context"
        );
    }
}
