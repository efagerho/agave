use {
    solana_accounts_db::utils::{
        create_all_accounts_run_and_snapshot_dirs, create_and_canonicalize_directories,
        create_and_canonicalize_directory,
    },
    solana_core::validator::ValidatorConfig,
    solana_runtime::snapshot_utils,
    std::{
        error::Error,
        fs::{self, File},
        path::PathBuf,
    },
};

/// Locks the ledger directory until the returned file is dropped, and canonicalizes its path
pub(super) fn lock_ledger(ledger_path: &mut PathBuf) -> Result<File, String> {
    let lock_file = crate::lock_ledger(ledger_path)?;
    *ledger_path = fs::canonicalize(&ledger_path)
        .map_err(|err| format!("unable to canonicalize {}: {err}", ledger_path.display()))?;
    Ok(lock_file)
}

/// Creates the account and snapshot directories and replaces their paths with canonical ones
pub(super) fn prepare(config: &mut ValidatorConfig) -> Result<(), Box<dyn Error>> {
    let account_paths = create_and_canonicalize_directories(&config.account_paths)
        .map_err(|err| format!("unable to access account path: {err}"))?;
    let snapshot_config = &mut config.snapshot_config;
    let snapshots_dir = snapshot_config
        .bank_snapshots_dir
        .parent()
        .expect("bank snapshots are in the snapshots directory");
    let snapshots_dir = create_and_canonicalize_directory(snapshots_dir).map_err(|err| {
        format!(
            "failed to create directory '{}': {err}",
            snapshots_dir.display()
        )
    })?;
    if account_paths.contains(&snapshots_dir) {
        return Err(
            "the --accounts and --snapshots paths must be unique since they both create \
             'snapshots' subdirectories, otherwise there may be collisions"
                .into(),
        );
    }
    for dir in [
        &mut snapshot_config.bank_snapshots_dir,
        &mut snapshot_config.full_snapshot_archives_dir,
        &mut snapshot_config.incremental_snapshot_archives_dir,
    ] {
        *dir = create_and_canonicalize_directory(&dir)
            .map_err(|err| format!("failed to create directory '{}': {err}", dir.display()))?;
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
