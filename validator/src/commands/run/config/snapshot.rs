use {
    agave_snapshots::{
        ArchiveFormat, SnapshotInterval, SnapshotVersion,
        paths::BANK_SNAPSHOTS_DIR,
        snapshot_config::{SnapshotConfig, SnapshotUsage},
    },
    clap::{ArgMatches, value_t},
    log::*,
    solana_accounts_db::accounts_db::TOTAL_IO_URING_BUFFERS_SIZE_LIMIT,
    solana_clock::DEFAULT_SLOTS_PER_EPOCH,
    solana_core::{resource_limits, validator::is_snapshot_config_valid},
    std::{
        error::Error,
        num::{NonZeroU64, NonZeroUsize},
        path::Path,
    },
};

/// The snapshot directories are created and canonicalized by `storage::prepare`
pub(super) fn new_config(
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
        return Err("--full-snapshot-interval-slots must be larger than \
                    --snapshot-interval-slots when incremental snapshots are enabled"
            .into());
    }
    Ok(snapshot_config)
}
