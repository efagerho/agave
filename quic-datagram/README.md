# solana-quic-datagram

A QUIC-datagram transport designed for Solana's votor consensus traffic.
Single [`quinn::Endpoint`] per node bound to one UDP socket, playing both
client and server roles. Peer identity is the ed25519 pubkey embedded in
a self-signed TLS cert; admission is gated by an `Admission` trait.
Bidirectional and unidirectional streams are disabled at the transport
level - only QUIC datagrams flow.

## Why this architecture

- **No HoL blocking.** Stream-based transports retransmit every message
  on the same connection when a packet is lost. A vote that didn't arrive
  within ~one slot is worthless. So for consensus messages
  losing one shouldn't delay the transmission of the next.
- **No per-message stream open/close.** Votor sends small (≤1200 byte)
  payloads at ~4/slot/peer. A stream per message is pure overhead.
- **Fire-and-forget semantics match production.** `VotingService`'s
  `broadcast_consensus_message` already uses `try_send` with
  drop-on-full; datagrams reflect that on the wire.
- **No congestion control.** Votor traffic is not flexible, it has
  fixed bandwidth demands and can not slow down in response to
  congestion.
- **ACKs throttled.** `AckFrequencyConfig` is tuned to minimize the
  amount of ACK packets that we have to carry on the wire.

## Architecture

One tokio task - `endpoint::EndpointLoop::run` - multiplexes every
event source via a single `tokio::select!`:

- server-accept (`endpoint.accept()`)
- client egress (`egress_rx.recv()`)
- identity rotation (`identity_rx.changed()`)
- metrics-report timer (every 2 s)

Heavy per-event work (TLS handshake, dial, etc.) is spawned onto its own
short-lived task so a slow handshake never blocks dispatch.


## Connection table

`Arc<DashMap<Pubkey, ConnectionTableEntry>>`, where:

```text
enum ConnectionTableEntry {
    Dialing(IdGeneration),    // placeholder; one dial task in flight
    Established(Connection),  // live connection for fan-out
}
```

This is needed to avoid having more than one connection to a given peer.
The server-accept pipeline (TLS, identity validation, admission) lives
entirely in the spawned per-incoming task and only touches the table at
the end via `insert_connection`. There's no useful in-flight server-side
state worth modeling in the table.

## Lex-pubkey tiebreaker

For every peer pair, the node with the **lower** pubkey is the
dialer; the higher pubkey listens. The rule is enforced on both sides:

- Server closes any inbound from a peer with `pubkey >= local` with
  `WRONG_DIRECTION`.
- Client drops egress to peers with `pubkey <= local` when no cached
  connection exists (counted as `egress_dropped_higher_pubkey`); it
  waits for the peer to dial in, then `send_over_inbound` finds the
  inbound and uses it for the higher→lower direction.

This partitions connections cleanly: the lex-higher side only ever holds
inbound (server-accepted) entries; the lex-lower side only
ever holds outbound (we-dialed) ones. That partition is load-bearing
for the `PEER_MOVED` logic below.

### No buffering during `Dialing`

When no connection exists to a peer, the first packet to be sent is
buffered while dialing is going on. This is critical for standstill to
resolve in a timely manner. A second egress that arrives while a dial
s in flight is **dropped on the floor** (counted in
`egress_dropped_dial_in_progress`). Not buffered, not parked. Rationale:

- Votor sends ≤4 pkt/slot/peer (~50 ms apart). A dial-in-flight window
  loses ~a few packets; the upper layer keeps producing fresh ones.
- Buffering would require a per-peer wakeup primitive (the per-peer
  `Notify` we explicitly avoided) plus a queue, both with their own
  capacity-bound questions.
- Old votes aren't worth delivering by the time a fresh dial completes.
- Standstill is covered by buffering of the first packet.

### Why no `Backoff` state

A peer that's permanently down will hammer with one dial per egress to
that pubkey (votor-paced, ~5 attempts/s per peer worst case). No
exponential backoff. Trade-off: simpler state machine, marginally more
wasted dials against dead peers. Acceptable because the failure cost
(one quinn `connect` + handshake-timeout) is bounded and the volume is
votor-paced.

## Connection replacement

A new successful handshake from a pubkey already in `Established`
closes the prior connection with `REPLACED` and installs the new one -
the validator hot-spare path (a backup instance of a peer's identity
re-dials). The displaced side observes `REPLACED` in its read loop and
simply reaps the connection; it re-handshakes on its next send if it
still has traffic for the peer.

> Note: stake-weighted reaction to replacement (the "handover" /
> self-shutdown mechanism), per-peer/per-subnet rate limiting, and the
> external banlist are intentionally **not** part of this crate yet —
> they land in a follow-up PR. This crate is the core datagram transport
> only.

## PEER_MOVED (address-aware eviction)

On the lex-lower (dialer) side, the cached `Established`'s
`remote_address()` is exactly the addr we dialed. If a later egress
arrives with a different addr for the same pubkey (e.g., gossip
published a new addr for the peer), the slot is atomically swapped to
`Dialing`, the displaced connection is closed with `PEER_MOVED`, and a
fresh dial spawns at the new addr.

The lex-higher side **does not** do this check. Its cached entries are
inbound; their `remote_address()` is the peer's NAT-mapped source addr,
which can legitimately differ from the gossip-published one the caller
hands us. The higher side trusts whatever inbound connection it
accepted, addr argument ignored on cache hit.

## Security posture

This crate is the core transport. The protections present today are:

1. **Bidirectional TLS ID validation.** Peer's cert must yield a
   recoverable Solana ed25519 pubkey; client checks the attested pubkey
   matches the targeted one, server checks the inbound pubkey.
2. **Stake check** (`Admission` trait, default impl
   `StakedNodesAdmission`). Peer must be in the current staked-set
   snapshot. IPv6 / multicast initiators are ignored at accept.
3. **MAX_PEERS = 2000.** Defensive cap on the connection table.

Deferred to a follow-up PR (not in this crate yet): QUIC RETRY
address-validation, per-subnet handshake rate limiting, per-connection
RX token-bucket rate limiting, and the application banlist (e.g. for BLS
sigverify failures).


## Identity rotation

`KeyUpdater::update_key(new_keypair)` publishes a new
`IdentitySnapshot` through a `tokio::sync::watch`. The control loop
observes the change and:

1. Rebuilds server + client TLS configs.
2. Swaps them into the quinn endpoint.
3. Evicts every cached connection with `IDENTITY_ROTATED` close.
4. Adopts the new pubkey for the lex-tiebreak rule going forward.

`Dialing` placeholders are dropped too. In-flight dial tasks were
dispatched with the pre-rotation generation tag; when they finish their
handshake and try to install the connection, the generation check
inside `insert_connection` trips, returns `InsertOutcome::Stale`, and
the new connection is closed with `IDENTITY_ROTATED`. No TLS-level
failure is needed - the gen counter is the single source of truth for
"this dial belongs to the previous identity."

## Payload size

A datagram payload is bounded by quinn's negotiated `max_datagram_size`
(transport-config-driven, currently a fixed 1280). Payloads above that
bound fail at `send_datagram` with `SendDatagramError::TooLarge` and the
crate does no fragmentation - callers are expected to keep messages
under the cap.

## Egress / ingress channels

- **Egress** (caller → endpoint, `EGRESS_CHANNEL_CAP = 16384`).
  Bounded mpsc. Callers must `try_send` and accept drop-on-full -
  matches `VotingService::broadcast_consensus_message`. Sized to absorb
  a full slot's burst with headroom.
- **Ingress** (endpoint → caller, caller-supplied
  `crossbeam_channel::Sender`). Drop-on-full counted in
  `datagram_ingress_dropped_channel_full`.

### Epoch boundary handling

At the end of an epoch, we may experience churn in the admission control
policy. Some peers will not be admissible anymore, while some new will be
allowed in. We do not proactively break connections to peers which are not
admitted anymore, instead they are evicted when we are inserting a new
connection while out of room in the connection table. This keeps the logic
simpler than the explicit notify during epoch change.

## What this crate is not

- Not a general-purpose datagram transport. The lex-tiebreaker, the
  drop-on-`Dialing` behavior, the lack of backoff, and the
  identity-rotation evict-all all assume a votor-style workload (small
  peer set, predictable cadence, no retransmits). With some effort this
  could be adapted for e.g. turbine.
- Not a buffered transport. There is no per-peer queue. Bytes that hit
  a full channel or a `Dialing` placeholder are gone.
