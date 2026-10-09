#[cfg(target_os = "linux")]
use super::xdp;
use {
    super::{Operation, ProcessCapabilities, args::RunArgs, config::RunConfig, network, storage},
    crate::{
        admin_rpc_service::{self, AdminRpcRequestMetadata},
        bootstrap, cli,
        commands::FromClapArgMatches,
    },
    clap::{ArgMatches, crate_name},
    crossbeam_channel::unbounded,
    log::*,
    solana_core::validator::{Validator, ValidatorLogConfig, ValidatorStartProgress},
    solana_gossip::contact_info::ContactInfo,
    solana_signer::Signer,
    std::{
        env,
        error::Error,
        fmt::Display,
        fs::File,
        path::PathBuf,
        sync::{Arc, RwLock, atomic::AtomicBool},
        time::SystemTime,
    },
};

pub fn execute(
    matches: &ArgMatches,
    solana_version: &str,
    operation: Operation,
    process_capabilities: ProcessCapabilities,
) -> Result<(), Box<dyn Error>> {
    // Debugging panics is easier with a backtrace
    if env::var_os("RUST_BACKTRACE").is_none() {
        // Safety: env update is made before any spawned threads might access the environment
        unsafe { env::set_var("RUST_BACKTRACE", "1") }
    }

    // Log to stderr until the configured logging starts
    agave_logger::setup_with_default_filter();

    // Everything after this step, logging included, is driven by the configuration
    let mut run_args = step("loading configuration", || {
        RunArgs::from_clap_arg_match(matches)
    })?;
    let _ledger_lock = step("locking ledger", || {
        storage::lock_ledger(&mut run_args.ledger_path)
    })?;

    let log_config = step("starting logging", || {
        start_logging(run_args.logfile.clone())
    })?;
    let use_progress_bar = log_config.is_none();
    cli::warn_for_deprecated_arguments(matches);
    info!("{} {}", crate_name!(), solana_version);
    info!("Starting validator with: {:#?}", env::args_os());
    solana_metrics::set_host_id(run_args.identity_keypair.pubkey().to_string());
    solana_metrics::set_panic_hook("validator", Some(String::from(solana_version)));
    solana_core::validator::report_target_features();

    let mut run_config = step("validating configuration", || {
        RunConfig::new(matches, run_args, log_config, &operation)
    })?;

    step("preparing storage", || {
        storage::prepare(&mut run_config.validator)
    })?;

    let exit = Arc::new(AtomicBool::new(false));
    #[cfg(target_os = "linux")]
    let xdp_transmit_setup = step("configuring capabilities and XDP", || {
        xdp::setup(&mut run_config, process_capabilities, exit.clone())
    })?;
    #[cfg(not(target_os = "linux"))]
    let xdp_transmit_setup = {
        let _ = process_capabilities;
        None
    };

    let mut node = step("configuring network", || {
        network::bind_node(&mut run_config)
    })?;

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
        step("bootstrapping from the cluster", || {
            bootstrap::rpc_bootstrap(
                &node,
                &identity_keypair,
                &ledger_path,
                &vote_account,
                authorized_voter_keypairs.clone(),
                &cluster_entrypoints,
                &mut validator_config,
                bootstrap_config,
                use_progress_bar,
                &start_progress,
                socket_addr_space,
            )
        })?;
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

/// Logs to the log file, or to stderr without one
fn start_logging(logfile: Option<PathBuf>) -> Result<Option<ValidatorLogConfig>, Box<dyn Error>> {
    let log_config = logfile
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
    agave_logger::initialize_logging(logfile);
    Ok(log_config)
}
