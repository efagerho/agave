use {
    super::{Operation, args::RunArgs},
    crate::{
        admin_rpc_service::load_staked_nodes_overrides, bootstrap::RpcBootstrapConfig,
        cli::thread_args::parse_num_threads_args, commands::FromClapArgMatches,
    },
    agave_votor::vote_history_storage::FileVoteHistoryStorage,
    agave_xdp::transmitter::XdpConfig,
    arc_swap::ArcSwap,
    clap::{ArgMatches, value_t, values_t},
    log::*,
    solana_clap_utils::input_parsers::{keypairs_of, pubkey_of, value_of, values_of},
    solana_clock::Slot,
    solana_core::{
        banking_stage::transaction_scheduler::scheduler_controller::SchedulerConfig,
        consensus::tower_storage::FileTowerStorage,
        repair::repair_handler::RepairHandlerType,
        snapshot_packager_service::SnapshotPackagerService,
        validator::{
            BlockProductionMethod, BlockVerificationMethod, SchedulerPacing, ValidatorConfig,
            ValidatorLogConfig, ValidatorTpuConfig,
        },
    },
    solana_genesis_utils::MAX_GENESIS_ARCHIVE_UNPACKED_SIZE,
    solana_gossip::cluster_info::{DEFAULT_CONTACT_SAVE_INTERVAL_MILLIS, NodeConfig},
    solana_keypair::Keypair,
    solana_ledger::{
        blockstore_options::{BlockstoreCleanupStrategy, BlockstoreOptions},
        shred::filter::TurbineMode,
        use_snapshot_archives_at_startup::{self, UseSnapshotArchivesAtStartup},
    },
    solana_net_utils::SocketAddrSpace,
    solana_poh::poh_service,
    solana_pubkey::Pubkey,
    solana_rpc::{rpc::JsonRpcConfig, rpc_pubsub_service::PubSubConfig},
    solana_runtime::runtime_config::RuntimeConfig,
    solana_send_transaction_service::send_transaction_service::Config as SendTransactionServiceConfig,
    solana_signer::Signer,
    solana_turbine::broadcast_stage::BroadcastStageType,
    solana_validator_exit::Exit,
    std::{
        collections::HashSet,
        error::Error,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        path::PathBuf,
        sync::{Arc, RwLock, atomic::AtomicBool},
    },
};

mod accounts_db;
mod network;
mod snapshot;
mod tpu;
#[cfg(target_os = "linux")]
mod xdp;

/// Validator configuration built and validated from the command line
pub struct RunConfig {
    pub identity_keypair: Arc<Keypair>,
    pub ledger_path: PathBuf,
    pub vote_account: Pubkey,
    pub authorized_voter_keypairs: Arc<RwLock<Vec<Arc<Keypair>>>>,
    pub entrypoints: Vec<SocketAddr>,
    pub socket_addr_space: SocketAddrSpace,
    /// An unspecified `advertised_ip` is resolved by `network::bind_node`
    pub node: NodeConfig,
    pub public_rpc_addr: Option<SocketAddr>,
    pub private_rpc: bool,
    pub restricted_repair_only_mode: bool,
    pub check_os_network_limits: bool,
    pub xdp: Option<XdpConfig>,
    pub validator: ValidatorConfig,
    pub tpu: ValidatorTpuConfig,
    pub bootstrap: RpcBootstrapConfig,
    pub init_complete_file: Option<PathBuf>,
}

impl RunConfig {
    pub fn new(
        matches: &ArgMatches,
        run_args: RunArgs,
        log_config: Option<ValidatorLogConfig>,
        operation: &Operation,
    ) -> Result<Self, Box<dyn Error>> {
        let threads = parse_num_threads_args(matches)?;
        let identity_keypair = Arc::new(run_args.identity_keypair);
        let identity = identity_keypair.pubkey();
        let validators = |name: &str, flag: &str| validators_set(&identity, matches, name, flag);
        let ledger_path = run_args.ledger_path;

        let entrypoints = entrypoints(matches)?;
        let socket_addr_space = SocketAddrSpace::new(matches.is_present("allow_private_addr"));
        for addr in &entrypoints {
            if !socket_addr_space.check(addr) {
                return Err(format!(
                    "--entrypoint {addr} is a private address, which requires --allow-private-addr"
                )
                .into());
            }
        }

        let node = network::new_config(matches, &threads)?;

        let private_rpc = matches.is_present("private_rpc");
        let rpc_bind_address = match matches.value_of("rpc_bind_address") {
            Some(addr) => solana_net_utils::parse_host(addr)
                .map_err(|err| format!("failed to parse --rpc-bind-address: {err}"))?,
            None if private_rpc => IpAddr::V4(Ipv4Addr::LOCALHOST),
            None => node.bind_ip_addrs.active(),
        };

        #[cfg(target_os = "linux")]
        let poh_pinned_cpu_core = value_of(matches, "poh_pinned_cpu_core")
            .or_else(|| value_of(matches, "experimental_poh_pinned_cpu_core"))
            .or(poh_service::DEFAULT_PINNED_CPU_CORE);
        #[cfg(not(target_os = "linux"))]
        let poh_pinned_cpu_core = None;

        #[cfg(target_os = "linux")]
        let xdp = xdp::new_config(matches, operation, &node.bind_ip_addrs, poh_pinned_cpu_core)?;
        #[cfg(not(target_os = "linux"))]
        let xdp = {
            let _ = operation;
            None
        };

        let authorized_voter_keypairs = Arc::new(RwLock::new(
            keypairs_of(matches, "authorized_voter_keypairs")
                .map(|keypairs| keypairs.into_iter().map(Arc::new).collect())
                .unwrap_or_else(|| vec![identity_keypair.clone()]),
        ));

        let staked_nodes_overrides_path = matches.value_of("staked_nodes_overrides");
        let staked_nodes_overrides = staked_nodes_overrides_path
            .map(|path| {
                load_staked_nodes_overrides(path).map_err(|err| {
                    format!("failed to load --staked-nodes-overrides from {path}: {err}")
                })
            })
            .transpose()?
            .unwrap_or_default()
            .staked_map_id;

        let restricted_repair_only_mode = matches.is_present("restricted_repair_only_mode");
        let mut voting_disabled = matches.is_present("no_voting") || restricted_repair_only_mode;
        let vote_account = pubkey_of(matches, "vote_account");
        if vote_account.is_none() && !voting_disabled {
            warn!("--vote-account not specified, validator will not vote");
            voting_disabled = true;
        }
        let vote_account = vote_account.unwrap_or_else(|| Keypair::new().pubkey());

        let bootstrap = RpcBootstrapConfig::from_clap_arg_match(matches)?;
        let rpc_config = JsonRpcConfig::from_clap_arg_match(matches)?;
        let snapshot_config =
            snapshot::new_config(matches, &ledger_path, bootstrap.incremental_snapshot_fetch)?;
        let accounts_db_config = accounts_db::new_config(
            matches,
            &ledger_path,
            &threads,
            rpc_config.account_indexes.clone(),
        )?;
        let skip_transaction_signatures_in_status_cache =
            !rpc_config.full_api && !snapshot_config.should_generate_snapshots();
        if skip_transaction_signatures_in_status_cache {
            info!(
                "Transaction signatures will not be stored in the status cache because full RPC \
                 and snapshot generation are disabled"
            );
        }

        let validator = ValidatorConfig {
            log_config,
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
            rpc_config,
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
            pubsub_config: PubSubConfig::from_clap_arg_match(matches)?,
            voting_disabled,
            wait_for_supermajority: value_t!(matches, "wait_for_supermajority", Slot).ok(),
            known_validators: validators("known_validators", "--known-validator")?,
            repair_validators: validators("repair_validators", "--repair-validator")?,
            should_check_duplicate_instance: true,
            repair_whitelist: Arc::new(RwLock::new(
                validators("repair_whitelist", "--repair-whitelist")?.unwrap_or_default(),
            )),
            // Identities named on the command line carry no address: the peer list resolves
            // them from gossip.
            votor_peer_overrides: Arc::new(ArcSwap::from_pointee(
                validators("votor_peer_overrides", "--votor-peer-overrides")?
                    .unwrap_or_default()
                    .into_iter()
                    .map(|pubkey| (pubkey, None))
                    .collect(),
            )),
            repair_handler_type: RepairHandlerType::default(),
            gossip_validators: validators("gossip_validators", "--gossip-validator")?,
            blockstore_cleanup_strategy: BlockstoreCleanupStrategy::from_clap_arg_match(matches)?,
            blockstore_options: BlockstoreOptions::from_clap_arg_match(matches)?,
            run_verification: !matches.is_present("skip_startup_ledger_verification"),
            debug_keys: pubkeys(matches, "debug_key").map(Arc::new),
            filter_keys: Arc::new(pubkeys(matches, "filter_keys").unwrap_or_default()),
            warp_slot: None,
            generator_config: None,
            contact_debug_interval: value_t!(matches, "contact_debug_interval", u64)?,
            contact_save_interval: DEFAULT_CONTACT_SAVE_INTERVAL_MILLIS,
            send_transaction_service_config: SendTransactionServiceConfig::from_clap_arg_match(
                matches,
            )?,
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
            // Replaced by the run and snapshot directories in `storage::prepare`
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
            accounts_db_config,
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
            entrypoints,
            socket_addr_space,
            node,
            public_rpc_addr: host_port_of(matches, "public_rpc_addr", "--public-rpc-address")?,
            private_rpc,
            restricted_repair_only_mode,
            check_os_network_limits: !matches.is_present("no_os_network_limits_test"),
            xdp,
            validator,
            tpu: tpu::new_config(matches, &threads)?,
            bootstrap,
            init_complete_file: value_of(matches, "init_complete_file"),
        })
    }
}

/// Parses the unique cluster entrypoints
fn entrypoints(matches: &ArgMatches) -> Result<Vec<SocketAddr>, String> {
    let mut entrypoints = values_t!(matches, "entrypoint", String).unwrap_or_default();
    // sort() + dedup() to yield a vector of unique elements
    entrypoints.sort();
    entrypoints.dedup();
    entrypoints
        .iter()
        .map(|entrypoint| {
            solana_net_utils::parse_host_port(entrypoint)
                .map_err(|err| format!("failed to parse --entrypoint: {err}"))
        })
        .collect()
}

fn pubkeys(matches: &ArgMatches, name: &str) -> Option<HashSet<Pubkey>> {
    values_of::<Pubkey>(matches, name).map(|keys| keys.into_iter().collect())
}

fn validators_set(
    identity: &Pubkey,
    matches: &ArgMatches,
    name: &str,
    flag: &str,
) -> Result<Option<HashSet<Pubkey>>, String> {
    let Some(validators) = pubkeys(matches, name) else {
        return Ok(None);
    };
    if validators.contains(identity) {
        return Err(format!(
            "the validator's identity pubkey cannot be a {flag}: {identity}"
        ));
    }
    Ok(Some(validators))
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
            None,
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
    fn test_entrypoints_are_sorted_and_unique() {
        let default_args = DefaultArgs::default();
        let matches = app("test", &default_args).get_matches_from([
            "agave-validator",
            "-n",
            "127.0.0.1:8001",
            "--entrypoint",
            "127.0.0.1:8000",
            "--entrypoint",
            "127.0.0.1:8001",
        ]);
        assert_eq!(
            entrypoints(&matches),
            Ok(vec![
                SocketAddr::from(([127, 0, 0, 1], 8000)),
                SocketAddr::from(([127, 0, 0, 1], 8001)),
            ])
        );
    }

    #[test]
    fn test_validator_keys() {
        let known = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        let config = new_run_config(&[
            "--known-validator",
            &known.to_string(),
            "--trusted-validator",
            &other.to_string(),
            "--known-validator",
            &known.to_string(),
            "--filter-keys",
            &other.to_string(),
            "--allow-private-addr",
        ])
        .unwrap();
        assert_eq!(
            config.validator.known_validators,
            Some(HashSet::from([known, other]))
        );
        assert_eq!(*config.validator.filter_keys, HashSet::from([other]));
        assert_eq!(config.socket_addr_space, SocketAddrSpace::Unspecified);
    }

    #[test]
    fn test_identity_cannot_be_a_known_validator() {
        let identity = Pubkey::new_unique();
        let default_args = DefaultArgs::default();
        let matches = app("test", &default_args).get_matches_from([
            "agave-validator",
            "--known-validator",
            &identity.to_string(),
        ]);
        assert_eq!(
            validators_set(&identity, &matches, "known_validators", "--known-validator"),
            Err(format!(
                "the validator's identity pubkey cannot be a --known-validator: {identity}"
            ))
        );
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
