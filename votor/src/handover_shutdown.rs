//! Stake-weighted handover-shutdown monitor.
//!
//! Drains the `handover_events` channel exposed by the votor datagram
//! endpoint. Each event = "this peer pubkey has closed our connection
//! with HANDOVER," meaning the cluster has accepted a different
//! instance of our validator identity as canonical and shunned us.
//!
//! Distinct peers are accumulated in a set; their epoch stake is
//! summed and compared against the total epoch stake. Once the ratio
//! reaches the handover-shutdown threshold the validator's exit flag
//! is set — the cluster has structurally rejected us and continuing to
//! send votes risks disrupting consensus.

use {
    solana_pubkey::Pubkey,
    solana_runtime::bank_forks::BankForks,
    std::{
        collections::HashSet,
        sync::{
            Arc, RwLock,
            atomic::{AtomicBool, Ordering},
        },
        thread::{Builder, JoinHandle},
    },
    tokio::sync::mpsc,
};

/// Fraction of total epoch stake whose HANDOVER closes against us
/// triggers a node shutdown. Expressed as numerator/denominator so the
/// check is integer-only (no f64 rounding).
///
/// 41/100 is set just below alpenglow's safety threshold: once at
/// least that much stake has handed us over, the cluster is operating
/// without us being part of its trusted set and our continued
/// participation can only harm consensus.
pub const HANDOVER_SHUTDOWN_STAKE_NUMERATOR: u128 = 41;
pub const HANDOVER_SHUTDOWN_STAKE_DENOMINATOR: u128 = 100;

pub fn spawn(
    handover_events: mpsc::Receiver<Pubkey>,
    bank_forks: Arc<RwLock<BankForks>>,
    exit: Arc<AtomicBool>,
) -> JoinHandle<()> {
    Builder::new()
        .name("solVotorHndovr".to_string())
        .spawn(move || run(handover_events, bank_forks, exit))
        .expect("spawn handover-shutdown thread")
}

fn run(
    mut handover_events: mpsc::Receiver<Pubkey>,
    bank_forks: Arc<RwLock<BankForks>>,
    exit: Arc<AtomicBool>,
) {
    let mut evicting_peers: HashSet<Pubkey> = HashSet::new();
    while let Some(peer) = handover_events.blocking_recv() {
        if exit.load(Ordering::Relaxed) {
            break;
        }
        if !evicting_peers.insert(peer) {
            // Same peer already counted — duplicate event (e.g., the
            // peer reconnected and handed us over again). Ignore.
            continue;
        }
        let (evicting_stake, total_stake) = stake_snapshot(&bank_forks, &evicting_peers);
        warn!(
            "Votor handover: peer {peer} has handed us over; {} distinct peers totalling \
             {evicting_stake} stake out of {total_stake} have done so so far",
            evicting_peers.len(),
        );
        if shutdown_triggered(evicting_stake, total_stake) {
            let num = HANDOVER_SHUTDOWN_STAKE_NUMERATOR;
            let denom = HANDOVER_SHUTDOWN_STAKE_DENOMINATOR;
            error!(
                "Votor handover threshold reached: {evicting_stake}/{total_stake} stake has \
                 handed us over (>= {num}/{denom}). Setting exit flag — a different instance of \
                 our identity is the canonical node on this cluster.",
            );
            exit.store(true, Ordering::Relaxed);
            break;
        }
    }
}

/// Returns (sum of evicting peers' stake, total epoch stake) using the
/// working bank's current epoch. Re-reads BankForks on every call so
/// epoch transitions and stake updates are picked up automatically.
fn stake_snapshot(bank_forks: &Arc<RwLock<BankForks>>, evicting: &HashSet<Pubkey>) -> (u64, u64) {
    let bank = bank_forks.read().unwrap().working_bank();
    let epoch = bank.epoch();
    let Some(staked) = bank.epoch_staked_nodes(epoch) else {
        return (0, 0);
    };
    let total_stake: u64 = staked.values().sum();
    let evicting_stake: u64 = evicting
        .iter()
        .filter_map(|pk| staked.get(pk).copied())
        .sum();
    (evicting_stake, total_stake)
}

#[inline]
fn shutdown_triggered(evicting_stake: u64, total_stake: u64) -> bool {
    if total_stake == 0 {
        return false;
    }
    // evicting / total >= NUMER / DENOM
    // ⇔ evicting * DENOM >= total * NUMER (no floats)
    (evicting_stake as u128).saturating_mul(HANDOVER_SHUTDOWN_STAKE_DENOMINATOR)
        >= (total_stake as u128).saturating_mul(HANDOVER_SHUTDOWN_STAKE_NUMERATOR)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use {
        super::*,
        solana_keypair::Signer,
        solana_runtime::{
            bank::Bank,
            genesis_utils::{
                ValidatorVoteKeypairs, create_genesis_config_with_alpenglow_vote_accounts,
            },
        },
        std::time::{Duration, Instant},
    };

    #[test]
    fn shutdown_triggered_zero_total() {
        assert!(!shutdown_triggered(100, 0));
    }

    #[test]
    fn shutdown_triggered_below_threshold() {
        // 40/100 < 41/100 — under threshold.
        assert!(!shutdown_triggered(40, 100));
        // 410/1001 < 41/100.
        assert!(!shutdown_triggered(410, 1001));
    }

    #[test]
    fn shutdown_triggered_at_threshold() {
        // 41/100 >= 41/100.
        assert!(shutdown_triggered(41, 100));
    }

    #[test]
    fn end_to_end_triggers_exit_above_threshold() {
        agave_logger::setup();
        // Build a 10-validator BankForks with uniform 100-lamport stakes
        // (total stake = 1000). At least ceil(0.41 * 1000) = 410 stake
        // must accumulate from distinct evicting peers to trip the
        // shutdown — i.e., 5 of the 10 validators handing us over
        // (5 × 100 = 500 ≥ 410).
        let validators: Vec<ValidatorVoteKeypairs> =
            (0..10).map(|_| ValidatorVoteKeypairs::new_rand()).collect();
        let stakes = vec![100u64; validators.len()];
        let genesis =
            create_genesis_config_with_alpenglow_vote_accounts(1_000_000_000, &validators, stakes);
        let bank0 = Bank::new_for_tests(&genesis.genesis_config);
        let bank_forks = solana_runtime::bank_forks::BankForks::new_rw_arc(bank0);

        let (tx, rx) = mpsc::channel(16);
        let exit = Arc::new(AtomicBool::new(false));
        let handle = spawn(rx, bank_forks.clone(), exit.clone());

        // Send 4 distinct evictions — 400/1000 = 40% < 41%, no trip.
        for v in validators.iter().take(4) {
            tx.blocking_send(v.node_keypair.pubkey())
                .expect("send handover event");
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !exit.load(Ordering::Relaxed),
            "below-threshold should not trip"
        );

        // 5th distinct eviction — 500/1000 = 50% ≥ 41%, must trip.
        tx.blocking_send(validators[4].node_keypair.pubkey())
            .expect("send 5th eviction");

        // Wait for the handover_shutdown thread to observe the trip.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if exit.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            exit.load(Ordering::Relaxed),
            "5/10 staked peers (50% > 41%) must trip exit"
        );

        // Close the channel; thread loop exits on next iteration. Drop
        // tx to drop the channel and let the thread observe closure.
        drop(tx);
        handle.join().expect("handover_shutdown thread");
    }

    #[test]
    fn duplicate_evictions_do_not_double_count() {
        agave_logger::setup();
        let validators: Vec<ValidatorVoteKeypairs> =
            (0..10).map(|_| ValidatorVoteKeypairs::new_rand()).collect();
        let stakes = vec![100u64; validators.len()];
        let genesis =
            create_genesis_config_with_alpenglow_vote_accounts(1_000_000_000, &validators, stakes);
        let bank0 = Bank::new_for_tests(&genesis.genesis_config);
        let bank_forks = solana_runtime::bank_forks::BankForks::new_rw_arc(bank0);

        let (tx, rx) = mpsc::channel(16);
        let exit = Arc::new(AtomicBool::new(false));
        let handle = spawn(rx, bank_forks.clone(), exit.clone());

        // Same 4 peers handing over us repeatedly should NOT trip the
        // threshold — total distinct stake stays at 400/1000.
        for _ in 0..3 {
            for v in validators.iter().take(4) {
                tx.blocking_send(v.node_keypair.pubkey())
                    .expect("send duplicate event");
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !exit.load(Ordering::Relaxed),
            "duplicate evictions must not accumulate stake"
        );

        drop(tx);
        handle.join().expect("handover_shutdown thread");
    }

    #[test]
    fn shutdown_triggered_above_threshold() {
        assert!(shutdown_triggered(50, 100));
        assert!(shutdown_triggered(u64::MAX, u64::MAX));
    }
}
