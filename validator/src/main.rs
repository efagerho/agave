#![allow(clippy::arithmetic_side_effects)]
use {
    agave_validator::{
        cli::{DefaultArgs, app},
        commands,
    },
    jemallocator::Jemalloc,
    log::error,
    std::{path::PathBuf, process::exit},
};

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

pub fn main() {
    let default_args = DefaultArgs::new();
    let solana_version = solana_version::version!();
    let cli_app = app(solana_version, &default_args);
    let matches = cli_app.get_matches();

    let ledger_path = PathBuf::from(matches.value_of("ledger_path").unwrap());

    let (subcommand, maybe_subcommand_matches) = matches.subcommand();

    #[cfg(target_os = "linux")]
    let process_capabilities = commands::run::capabilities::restrict_at_startup(
        subcommand.is_empty() || subcommand == "run",
    );
    #[cfg(not(target_os = "linux"))]
    let process_capabilities = commands::run::ProcessCapabilities {};

    match (subcommand, maybe_subcommand_matches) {
        ("print-default-config", _) => {
            print!("{}", agave_validator_config::DEFAULT_CONFIG);
            Ok(())
        }
        ("init", _) | ("", _) | ("run", _) => {
            let (operation, failure) = if subcommand == "init" {
                (commands::run::Operation::Initialize, "initialize")
            } else {
                (commands::run::Operation::Run, "start")
            };
            commands::run::execute(&matches, solana_version, operation, process_capabilities)
                .inspect_err(|err| error!("Failed to {failure} validator: {err}"))
                .map_err(commands::Error::Dynamic)
        }
        ("authorized-voter", Some(authorized_voter_subcommand_matches)) => {
            commands::authorized_voter::execute(authorized_voter_subcommand_matches, &ledger_path)
        }
        ("plugin", Some(plugin_subcommand_matches)) => {
            commands::plugin::execute(plugin_subcommand_matches, &ledger_path)
        }
        ("contact-info", Some(subcommand_matches)) => {
            commands::contact_info::execute(subcommand_matches, &ledger_path)
        }
        ("exit", Some(subcommand_matches)) => {
            commands::exit::execute(subcommand_matches, &ledger_path)
        }
        ("monitor", _) => commands::monitor::execute(&matches, &ledger_path),
        ("staked-nodes-overrides", Some(subcommand_matches)) => {
            commands::staked_nodes_overrides::execute(subcommand_matches, &ledger_path)
        }
        ("set-identity", Some(subcommand_matches)) => {
            commands::set_identity::execute(subcommand_matches, &ledger_path)
        }
        ("set-log-filter", Some(subcommand_matches)) => {
            commands::set_log_filter::execute(subcommand_matches, &ledger_path)
        }
        ("wait-for-restart-window", Some(subcommand_matches)) => {
            commands::wait_for_restart_window::execute(subcommand_matches, &ledger_path)
        }
        ("repair-shred-from-peer", Some(subcommand_matches)) => {
            commands::repair_shred_from_peer::execute(subcommand_matches, &ledger_path)
        }
        ("repair-whitelist", Some(repair_whitelist_subcommand_matches)) => {
            commands::repair_whitelist::execute(repair_whitelist_subcommand_matches, &ledger_path)
        }
        ("set-public-address", Some(subcommand_matches)) => {
            commands::set_public_address::execute(subcommand_matches, &ledger_path)
        }
        ("manage-block-production", Some(subcommand_matches)) => {
            commands::manage_block_production::execute(subcommand_matches, &ledger_path)
        }
        ("blockstore", Some(subcommand_matches)) => {
            commands::blockstore::execute(subcommand_matches, &ledger_path)
        }
        _ => unreachable!(),
    }
    .unwrap_or_else(|err| {
        println!("Validator command failed: {err}");
        exit(1);
    })
}
