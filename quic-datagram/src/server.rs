//! Server-side connection lifecycle.

use {
    crate::{
        admission::Admission,
        close_codes,
        connection_table::{ConnectionTable, InsertOutcome},
        endpoint::Datagram,
        error::Error,
        read_loop::read_datagram_loop,
        stats::{QuicDatagramStats, record_error},
    },
    crossbeam_channel::Sender,
    log::debug,
    quinn::Incoming,
    solana_pubkey::Pubkey,
    solana_tls_utils::get_remote_pubkey,
    std::sync::Arc,
};

/// A server-side connection representation
pub(crate) struct ServerConnection<A: Admission> {
    pub(crate) incoming: Incoming,
    pub(crate) local_pubkey: Pubkey,
    pub(crate) ingress: Sender<Datagram>,
    pub(crate) admission: Arc<A>,
    pub(crate) table: Arc<ConnectionTable>,
    pub(crate) stats: Arc<QuicDatagramStats>,
}

impl<A: Admission> ServerConnection<A> {
    /// Spawn a tokio task that drives this connection to completion.
    /// Returns immediately; the task logs and records any error before
    /// exiting.
    pub(crate) fn spawn(self) {
        let remote_addr = self.incoming.remote_address();
        let stats = self.stats.clone();
        tokio::spawn(async move {
            if let Err(err) = self.run().await {
                debug!("Failed processing incoming connection from ({remote_addr}): {err:?}");
                record_error(&err, &stats);
            }
        });
    }

    /// accept, validate identity / admission, install in the table, run the
    /// read loop, reap on exit.
    async fn run(self) -> Result<(), Error> {
        // Snapshot the identity generation BEFORE the handshake starts.
        // If our identity rotates while quinn is driving the handshake,
        // `insert_connection` below will see a mismatched gen and
        // return `Stale`, and we close the resulting (wrong-identity)
        // connection with `IDENTITY_ROTATED` instead of installing it.
        let gen_at_start = self.table.current_generation();
        let remote_addr = self.incoming.remote_address();
        let connecting = self.incoming.accept()?;
        let connection = connecting.await?;
        let Some(peer) = get_remote_pubkey(&connection) else {
            close_codes::INVALID_IDENTITY.close(&connection);
            return Err(Error::InvalidIdentity(remote_addr));
        };

        // Check the pubkey tiebreaker: the lower pubkey dials, the higher
        // listens. An inbound from a peer with pubkey >= local
        // violates the rule.
        if peer >= self.local_pubkey {
            close_codes::WRONG_DIRECTION.close(&connection);
            return Err(Error::WrongDirection(peer));
        }

        if !self.admission.allow(&peer) {
            close_codes::NOT_ADMITTED.close(&connection);
            return Err(Error::NotAdmitted(peer));
        }

        // On cap-induced Rejected, the table will try to evict a
        // now-unadmitted peer before giving up - epoch-boundary churn
        // can momentarily push the staked-set union past `MAX_PEERS`.
        match self.table.insert_connection_or_evict(
            peer,
            connection.clone(),
            gen_at_start,
            |pk| self.admission.allow(pk),
            &self.stats,
        ) {
            InsertOutcome::Rejected => {
                close_codes::TABLE_FULL.close(&connection);
                return Err(Error::TableFull);
            }
            InsertOutcome::Stale => {
                close_codes::IDENTITY_ROTATED.close(&connection);
                return Err(Error::IdentityRotated(peer));
            }
            InsertOutcome::Inserted | InsertOutcome::Replaced => {}
        }

        // We're already on the per-incoming task; run the read loop
        // inline rather than chaining another spawn.
        let stable_id = connection.stable_id();
        read_datagram_loop(connection, peer, remote_addr, self.ingress, self.stats).await;
        // Reap our slot only if it still points at *this* connection
        // (a newer one may have taken our place).
        self.table.maybe_reap_connection(&peer, stable_id);
        Ok(())
    }
}
