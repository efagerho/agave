use {
    crate::cli::thread_args::NumThreadConfig,
    bytesize::ByteSize,
    clap::{ArgMatches, value_t},
    log::*,
    solana_accounts_db::{
        accounts_db::{AccountShrinkThreshold, AccountsDbConfig},
        accounts_file::AccountsFileProvider,
        accounts_index::{
            AccountSecondaryIndexes, AccountsIndexConfig, DEFAULT_NUM_ENTRIES_OVERHEAD,
            DEFAULT_NUM_ENTRIES_TO_EVICT, IndexLimit, IndexLimitThreshold,
            MINIMAL_THRESHOLD_NUM_BYTES, ScanFilter,
        },
    },
    solana_clap_utils::input_parsers::{value_of, values_of},
    std::{error::Error, path::Path},
};

pub(super) fn new_config(
    matches: &ArgMatches,
    ledger_path: &Path,
    threads: &NumThreadConfig,
    account_indexes: AccountSecondaryIndexes,
) -> Result<AccountsDbConfig, Box<dyn Error>> {
    let shrink_ratio = value_t!(matches, "accounts_shrink_ratio", f64)?;
    if !(0.0..=1.0).contains(&shrink_ratio) {
        return Err(format!(
            "--accounts-shrink-ratio must be between 0.0 and 1.0 inclusive: {shrink_ratio}"
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
        account_indexes: Some(account_indexes),
        bank_hash_details_dir: ledger_path.to_path_buf(),
        shrink_ratio,
        read_cache_limit_bytes,
        write_cache_limit_bytes,
        ancient_append_vec_offset: value_t!(matches, "accounts_db_ancient_append_vecs", i64).ok(),
        ancient_storage_ideal_size: value_t!(
            matches,
            "accounts_db_ancient_storage_ideal_size",
            u64
        )
        .ok(),
        max_ancient_storages: value_t!(matches, "accounts_db_max_ancient_storages", usize).ok(),
        verify_index: matches.is_present("accounts_db_verify_index"),
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
        ..AccountsDbConfig::default()
    })
}
