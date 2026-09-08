# `CrdsGossip::crds` lock-wait comparison

> Histogram correction (2026-09-08): older master re-binning tables in this
> document used the wrong bucket decoding. Use the corrected histogram in
> [the latest remeasurement](#post-review-simplifications-remeasurement-2026-09-08).
> Original master means, maxima, and total waits are unchanged.

Both captures measured 60 seconds of futex slow-path acquisition latency.
Fast-path acquisitions are not included.

The pre-fix tracer recorded latency by `(lock, read/write mode)` and retained
only one representative acquisition stack per mode. Therefore, the mode-level
comparison below is exact, while the `process_push_message` comparison uses
the complete pre-fix writer distribution as a proxy for that dominant path.
Pre-fix percentiles are histogram intervals; percentage changes use the
interval midpoint.

## Pre/post comparison by mode

| Mode | Metric | Pre-fix | Post-fix | Change |
|---|---|---:|---:|---:|
| Read | Contended acquisitions | 294 | 197 | -33.0% |
| Read | Mean | 165.616 us | 123.798 us | -25.3% |
| Read | Median | 140-150 us | 76.005 us | ~-47.6% |
| Read | P95 | 470-480 us | 306.512 us | ~-35.5% |
| Read | P99 | 1.6-1.7 ms | 943.686 us | ~-42.8% |
| Read | Max | 1.859 ms | 1.799 ms | -3.2% |
| Write | Contended acquisitions | 550 | 427 | -22.4% |
| Write | Mean | 709.602 us | 536.575 us | -24.4% |
| Write | Median | 280-290 us | 240.963 us | ~-15.5% |
| Write | P95 | 1.9-2.0 ms | 1.832 ms | ~-6.1% |
| Write | P99 | 2.5-2.6 ms | 2.138 ms | ~-16.2% |
| Write | Max | 3.446 ms | 2.537 ms | -26.4% |

Total accumulated slow-path wait fell from 438.972 ms to 253.506 ms
(-42.3%).

## Dominant acquisition path: `process_push_message`

Post-fix, `CrdsGossip::process_push_message` accounts for 397 of 427 writer
contentions (93.0%), 226.299 of 229.117 ms of writer wait (98.8%), and 89.3%
of all CRDS slow-path wait time.

| Metric | Pre writer pool | Post `process_push_message` | Approx. change |
|---|---:|---:|---:|
| Samples | 550 | 397 | -27.8% |
| Mean | 709.602 us | 570.023 us | -19.7% |
| Median | 280-290 us | 287.607 us | essentially unchanged |
| P95 | 1.9-2.0 ms | 1.850 ms | ~-5.1% |
| P99 | 2.5-2.6 ms | 2.145 ms | ~-15.9% |
| Max | 3.446 ms | 2.537 ms | -26.4% |

The pre-fix writer pool may also contain `purge` and other writer paths, so
this path comparison is directional rather than exact.

## Source captures

- Pre-fix: `/tmp/agave-locks-60s-3533529-final2.raw`
- Post-fix: `/tmp/crds-lock-waits-3576230-bloom-fix-60s.csv`

## Repair-peer batching follow-up

This compares the Bloom-filter-only binary with the binary that additionally
resolves `repair_peers` under 64-entry CRDS read-lock batches. Both captures
were 60 seconds and reported zero lost or unmatched events.

### `repair_peers` hold time

| Acquisition path | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before: monolithic `ClusterInfo::repair_peers` guard | 78 | 2.086 ms | 2.089 ms | 2.555 ms | 2.872 ms | 3.179 ms |
| After: pubkey snapshot guard | 86 | 52.892 us | 51.707 us | 75.532 us | 80.314 us | 84.357 us |
| After: 64-peer batch guard | 4,730 | 51.873 us | 50.249 us | 76.892 us | 88.349 us | 177.305 us |

Continuous `repair_peers` hold median fell 97.6% and maximum fell 94.4%.
There were exactly 55 batch guards per invocation in this sample. Total CRDS
read-lock occupancy attributable to each `repair_peers` invocation increased
from approximately 2.086 ms to 2.906 ms (+39.3%), primarily because batched
resolution adds a ContactInfo table lookup for each snapshotted pubkey.

### Futex slow-path wait time

| Mode | Metric | Before batching | After batching | Change |
|---|---|---:|---:|---:|
| Read | Contended acquisitions | 197 | 238 | +20.8% |
| Read | Mean | 123.798 us | 104.651 us | -15.5% |
| Read | Median | 76.005 us | 55.807 us | -26.6% |
| Read | P99 | 943.686 us | 596.772 us | -36.8% |
| Read | Max | 1.799 ms | 877.322 us | -51.2% |
| Write | Contended acquisitions | 427 | 422 | -1.2% |
| Write | Mean | 536.575 us | 693.248 us | +29.2% |
| Write | Median | 240.963 us | 274.970 us | +14.1% |
| Write | P95 | 1.832 ms | 2.711 ms | +48.0% |
| Write | P99 | 2.138 ms | 3.205 ms | +49.9% |
| Write | Max | 2.537 ms | 3.992 ms | +57.3% |
| Combined | Total wait | 253.506 ms | 317.458 ms | +25.2% |

### Dominant waiter: `CrdsGossip::process_push_message`

| Metric | Before batching | After batching | Change |
|---|---:|---:|---:|
| Samples | 397 | 401 | +1.0% |
| Mean | 570.023 us | 719.845 us | +26.3% |
| Median | 287.607 us | 291.513 us | +1.4% |
| P95 | 1.850 ms | 2.719 ms | +47.0% |
| P99 | 2.145 ms | 3.206 ms | +49.5% |
| Max | 2.537 ms | 3.992 ms | +57.3% |
| Total wait | 226.299 ms | 288.658 ms | +27.6% |

The batching change achieved its direct goal of bounding individual
`repair_peers` holds, but this single sample shows worse writer-tail latency
and higher total reader occupancy. The wait captures were consecutive
one-minute workload samples, so another controlled sample would be needed to
separate a repeatable regression from workload variance.

Source captures:

- Before batching: `/tmp/crds-lock-waits-3576230-bloom-fix-60s.csv`
- After batching holds: `/tmp/crds-lock-holds-3589247-repair-batching-60s.csv`
- After batching waits: `/tmp/crds-lock-waits-3589247-repair-batching-60s.csv`
- After batching build ID: `a5d36b1cccfa5647b3f9a7d6f3047c808dfeaef9`

## Lightweight repair-peer projection follow-up

This replaces batching with one direct CRDS traversal that copies only the
pubkey, TVU UDP address, and repair UDP address. Socket validation and all
downstream peer selection occur after the read guard is released. The
comparison below is against the batching sample. Event counts were similar
(377,366 versus 383,394 hold events; 663 versus 660 slow-path waits), and both
captures reported zero lost or unmatched events.

### `repair_peers` hold time

| Version/path | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Batching: pubkey snapshot guard | 86 | 52.892 us | 51.707 us | 75.532 us | 80.314 us | 84.357 us |
| Batching: 64-peer batch guard | 4,730 | 51.873 us | 50.249 us | 76.892 us | 88.349 us | 177.305 us |
| Lightweight: single projection guard | 89 | 990.325 us | 981.456 us | 1.141 ms | 1.278 ms | 1.395 ms |

Batching took exactly 55 batch guards per invocation, for approximately
2.906 ms of total CRDS read-lock occupancy per `repair_peers` call. The
lightweight projection reduced that to 990.325 us (-65.9%). Relative to the
original monolithic full-`ContactInfo` clone, it reduced mean per-call
occupancy by 52.5%, median by 53.0%, and maximum by 56.1%.

### Futex slow-path wait time

| Mode | Metric | Batching | Lightweight | Change |
|---|---|---:|---:|---:|
| Read | Contended acquisitions | 238 | 258 | +8.4% |
| Read | Mean | 104.651 us | 124.627 us | +19.1% |
| Read | Median | 55.807 us | 100.229 us | +79.6% |
| Read | P95 | 306.834 us | 330.534 us | +7.7% |
| Read | P99 | 596.772 us | 562.081 us | -5.8% |
| Read | Max | 877.322 us | 764.937 us | -12.8% |
| Write | Contended acquisitions | 422 | 405 | -4.0% |
| Write | Mean | 693.248 us | 364.961 us | -47.4% |
| Write | Median | 274.970 us | 273.649 us | -0.5% |
| Write | P95 | 2.711 ms | 992.093 us | -63.4% |
| Write | P99 | 3.205 ms | 1.218 ms | -62.0% |
| Write | Max | 3.992 ms | 1.840 ms | -53.9% |
| Combined | Total wait | 317.458 ms | 179.963 ms | -43.3% |

### Dominant waiter: `CrdsGossip::process_push_message`

| Metric | Batching | Lightweight | Change |
|---|---:|---:|---:|
| Samples | 401 | 376 | -6.2% |
| Mean | 719.845 us | 385.692 us | -46.4% |
| Median | 291.513 us | 302.328 us | +3.7% |
| P95 | 2.719 ms | 994.312 us | -63.4% |
| P99 | 3.206 ms | 1.225 ms | -61.8% |
| Max | 3.992 ms | 1.840 ms | -53.9% |
| Total wait | 288.658 ms | 145.020 ms | -49.8% |

The lightweight projection lengthens each individual guard relative to a
64-peer batch, but it removes 55 reacquisitions and the extra ContactInfo hash
lookup per peer. In this sample, that trade reduced total occupancy and the
writer long tail substantially. Read median latency increased, while read P99
and maximum improved.

Source captures (all rows store demangled acquisition-path symbols):

- Holds: `/tmp/crds-lock-holds-3617563-lightweight-repair-peers-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3617563-lightweight-repair-peers-60s.json`
- Waits: `/tmp/crds-lock-waits-3617563-lightweight-repair-peers-60s.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3617563-lightweight-repair-peers-60s.json`
- Build ID: `53d12f399d4f8fbda77b17a053040263116706f8`

## Holder-attributed slow-path follow-up

The wait tracer was extended to capture CRDS guard acquisition and release
intervals alongside futex waits. A conflicting holder is attributed when its
interval overlaps the wait; the terminal blocker is the last conflicting
holder to release before the waiter acquires the lock. A 50 ms holder warmup
precedes the 60-second wait window, and a 100 ms drain preserves intervals
which cross the end of the window.

Capture integrity:

- Build ID: `53d12f399d4f8fbda77b17a053040263116706f8`
- Waits: 747
- Holder intervals: 385,225
- Attributed waits: 713 (95.4%)
- Lost events, nested acquisitions, and unmatched releases: zero
- The 34 waits without an overlapping holder were short: at most 2.534 us for
  readers and 11.976 us for writers.

### Futex slow-path waits

| Mode | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Read | 270 | 109.217 us | 72.841 us | 253.261 us | 643.080 us | 1.024 ms |
| Write | 477 | 327.952 us | 222.848 us | 968.584 us | 1.083 ms | 1.354 ms |
| Combined | 747 | 248.891 us | 132.132 us | 903.529 us | 1.059 ms | 1.354 ms |

### Terminal holders for writer waits

| Holder path | Waits | Total wait | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| `CrdsGossipPull::build_crds_filters` | 117 | 74.114 ms | 654.375 us | 1.061 ms | 1.162 ms | 1.354 ms |
| `ClusterInfo::repair_peers` | 75 | 40.333 ms | 577.828 us | 971.531 us | 1.052 ms | 1.086 ms |
| `CrdsGossip::new_pull_request` | 78 | 22.798 ms | 302.674 us | 538.531 us | 596.112 us | 628.088 us |
| Votor `query_contact_infos` | 32 | 4.696 ms | 141.516 us | 271.633 us | 315.882 us | 332.189 us |
| `ClusterInfo::get_votes` | 60 | 3.370 ms | 51.681 us | 122.543 us | 143.261 us | 167.892 us |
| `CrdsGossip::refresh_push_active_set` | 7 | 2.679 ms | 413.010 us | 550.950 us | 551.952 us | 552.203 us |
| `CrdsGossip::purge` | 10 | 2.369 ms | 233.553 us | 280.473 us | 281.053 us | 281.198 us |
| `ClusterInfo::get_epoch_slots` | 33 | 1.726 ms | 54.185 us | 103.224 us | 136.051 us | 151.279 us |
| `ClusterInfo::save_contact_info` | 1 | 1.058 ms | 1.058 ms | 1.058 ms | 1.058 ms | 1.058 ms |

Of the 37 writer waits of at least 900 us, 29 were held by
`build_crds_filters`, six by `repair_peers`, and one by `save_contact_info`.
The remaining 907 us wait overlapped only 862 ns of `get_node_version`, so it
was scheduler/queue dominated rather than caused by that guard. Actual holder
intervals covered 99.0% of the time in all 37 long writer waits.

The single 1.024 ms reader outlier overlapped a writer guard for only 2.4% of
its duration and was likewise scheduler/queue dominated. Across every wait,
holder intervals covered 95.3% of total wait time; for writer waits the figure
was 97.6%.

Source captures (all rows contain demangled waiter and holder paths):

- Waits: `/tmp/crds-lock-waits-3617563-lightweight-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3617563-lightweight-blockers-60s.blockers.csv`
- Report and histograms: `/tmp/crds-lock-waits-3617563-lightweight-blockers-60s.json`

## Prefix-directed live-table snapshot experiment

Build ID `4bdfdb3cd6ab77f575e031753603e15eba98df80` changed
`build_crds_filters` to select the active bloom-filter prefixes first and use
`CrdsShards` to copy only matching hashes from the live CRDS table. The
validator was restarted and allowed to reach steady state before two separate
60-second captures. During the captures the live table contained about 222k
entries, while the flat purged-hash queue contained about 422k entries and the
failed-insert queue about 600 entries.

The experiment regressed because the selected live-table gather is sequential
and, more importantly, the much larger purged queue is still scanned
sequentially in full under the CRDS read lock.

### Overall futex slow-path waits

| Mode | Metric | Lightweight baseline | Prefix-directed | Change |
|---|---|---:|---:|---:|
| Read | Contended acquisitions | 270 | 274 | +1.5% |
| Read | Mean | 109.217 us | 119.541 us | +9.5% |
| Read | Median | 72.841 us | 73.046 us | +0.3% |
| Read | P95 | 253.261 us | 317.313 us | +25.3% |
| Read | P99 | 643.080 us | 606.437 us | -5.7% |
| Read | Max | 1.024 ms | 1.368 ms | +33.6% |
| Write | Contended acquisitions | 477 | 464 | -2.7% |
| Write | Mean | 327.952 us | 426.192 us | +30.0% |
| Write | Median | 222.848 us | 267.860 us | +20.2% |
| Write | P95 | 968.584 us | 1.281 ms | +32.2% |
| Write | P99 | 1.083 ms | 1.710 ms | +57.9% |
| Write | Max | 1.354 ms | 2.033 ms | +50.1% |

### `build_crds_filters` read-lock hold

The new function has a separate O(1) count acquisition (median 0.771 us) and
the selected-hash snapshot acquisition shown below.

| Metric | Lightweight baseline | Prefix-directed | Change |
|---|---:|---:|---:|
| Acquisitions | 119 | 120 | +0.8% |
| Mean | 1.140 ms | 1.422 ms | +24.7% |
| Median | 1.135 ms | 1.348 ms | +18.8% |
| P95 | 1.331 ms | 1.876 ms | +41.0% |
| P99 | 1.493 ms | 1.956 ms | +31.0% |
| Max | 1.529 ms | 2.019 ms | +32.1% |

### Writer waits terminally blocked by `build_crds_filters`

| Metric | Lightweight baseline | Prefix-directed | Change |
|---|---:|---:|---:|
| Waits | 117 | 119 | +1.7% |
| Mean | 633.450 us | 942.585 us | +48.8% |
| Median | 654.375 us | 901.936 us | +37.8% |
| P95 | 1.061 ms | 1.502 ms | +41.6% |
| P99 | 1.162 ms | 1.848 ms | +59.1% |
| Max | 1.354 ms | 2.033 ms | +50.1% |

Capture integrity: 738 waits and 379,668 holder intervals in the combined
capture, 709 attributed waits, zero lost events, and all combined-tracer
diagnostics zero. The independent hold capture recorded 381,714 guards with
zero lost events; its one unmatched release was a guard acquired before the
capture window.

Source captures (all rows store demangled symbolic acquisition paths):

- Holds: `/tmp/crds-lock-holds-3635777-prefix-directed-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3635777-prefix-directed-60s.json`
- Waits: `/tmp/crds-lock-waits-3635777-prefix-directed-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3635777-prefix-directed-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3635777-prefix-directed-blockers-60s.json`

## `PurgedHashes` prefix index follow-up

The flat purged-hash queue was replaced with `PurgedHashes`, using 4,096
prefix shards so `build_crds_filters` can gather only hashes matching active
bloom-filter prefixes. A first implementation kept `(hash, timestamp)` in
each shard. Although it shortened the filter snapshot, expiration had to scan
all 4,096 shard fronts while holding the CRDS write lock.

The refined implementation stores hashes in the prefix shards and maintains a
compact global `(timestamp, shard)` expiration queue. Expiration therefore
visits only entries that actually expire, while lookup remains prefix-directed.

The two implementations were each deployed, allowed to reach steady state,
and measured with separate 60-second wait and hold captures. The live table
contained about 222k entries and the purged-hash index about 410k--440k
entries. All paths below are preserved as demangled symbols in the raw and
JSON captures.

### Overall futex slow-path waits

| Version | Mode | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| Flat queue baseline | Read | 270 | 109.217 us | 72.841 us | 253.261 us | 643.080 us | 1.024 ms |
| Flat queue baseline | Write | 477 | 327.952 us | 222.848 us | 968.584 us | 1.083 ms | 1.354 ms |
| Flat queue baseline | Combined | 747 | 248.891 us | 132.132 us | 903.529 us | 1.059 ms | 1.354 ms |
| Initial `PurgedHashes` | Read | 304 | 142.328 us | 117.717 us | 365.387 us | 436.123 us | 691.146 us |
| Initial `PurgedHashes` | Write | 462 | 299.686 us | 188.191 us | 917.536 us | 1.238 ms | 1.855 ms |
| Initial `PurgedHashes` | Combined | 766 | 237.236 us | 144.339 us | 811.965 us | 1.135 ms | 1.855 ms |
| Refined expiry queue | Read | 259 | 116.795 us | 71.399 us | 316.685 us | 527.433 us | 689.453 us |
| Refined expiry queue | Write | 485 | 306.090 us | 170.796 us | 910.054 us | 1.275 ms | 1.631 ms |
| Refined expiry queue | Combined | 744 | 240.193 us | 135.617 us | 849.011 us | 1.158 ms | 1.631 ms |

Against the flat-queue baseline, the refined version's writer median was
23.4% lower and P95 was 6.0% lower. Its sampled writer maximum was 20.4%
higher, so this change did not halve the overall long-tail maximum. Compared
with the initial sharded implementation, the refined version reduced writer
median by 9.2% and maximum by 12.1%. One-minute maxima are workload-sensitive;
the hold and blocker tables below identify the direct structural effect.

### `build_crds_filters` snapshot read guard

The prefix-directed versions also take an O(1) count guard. Its median was
0.701 us in the initial version and 0.681 us in the refined version; the table
shows the substantive selected-hash snapshot guard.

| Version | Symbolic acquisition path | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| Flat queue baseline | `CrdsGossipPull::build_crds_filters` | 119 | 1.140 ms | 1.135 ms | 1.331 ms | 1.493 ms | 1.529 ms |
| Prefix-directed live table, flat purged queue | `CrdsGossipPull::build_crds_filters` snapshot | 120 | 1.422 ms | 1.348 ms | 1.876 ms | 1.956 ms | 2.019 ms |
| Initial `PurgedHashes` | `CrdsGossipPull::build_crds_filters` snapshot | 120 | 1.064 ms | 1.013 ms | 1.345 ms | 1.575 ms | 1.688 ms |
| Refined expiry queue | `CrdsGossipPull::build_crds_filters` snapshot | 120 | 1.052 ms | 1.028 ms | 1.334 ms | 1.433 ms | 1.519 ms |

The refined `PurgedHashes` version removed the prefix experiment's regression:
snapshot median fell 23.7% and maximum fell 24.7%. Relative to the original
flat-queue baseline, mean fell 7.7%, median fell 9.4%, and maximum was
essentially unchanged (-0.6%). The initial and refined sharded snapshot
results are close, as expected, because their prefix lookup is identical.

### `CrdsGossip::purge` write guards

`purge` has one guard for dropping expired live values and a second guard for
trimming the purged-hash retention index. This table separates them.

| Version/path | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Flat queue: drop live values | 599 | 223.413 us | 211.452 us | 303.551 us | 509.006 us | 927.271 us |
| Initial `PurgedHashes`: drop live values | 600 | 219.428 us | 213.114 us | 296.460 us | 462.556 us | 938.307 us |
| Refined expiry queue: drop live values | 600 | 218.462 us | 205.920 us | 311.291 us | 664.516 us | 1.043 ms |
| Flat queue: trim purged hashes | 599 | 1.748 us | 1.682 us | 2.623 us | 3.685 us | 5.207 us |
| Initial `PurgedHashes`: trim 4,096 shards | 600 | 90.319 us | 91.466 us | 137.280 us | 173.993 us | 191.455 us |
| Refined global expiry queue: trim expired entries | 600 | 12.720 us | 11.676 us | 19.255 us | 22.452 us | 471.381 us |

The global expiry queue reduced trim mean by 85.9%, median by 87.2%, and P95
by 86.0% versus scanning every shard. There was one 471 us outlier; 599 of 600
trim guards completed below 32 us. The refined trim remains about 10 us slower
at the median than `VecDeque::drain` on the old flat queue, but it is no longer
a dominant periodic critical section.

### Principal terminal blockers for writer waits

| Version | Holder path | Waits | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| Flat queue baseline | `CrdsGossipPull::build_crds_filters` | 117 | 633.450 us | 654.375 us | 1.061 ms | 1.162 ms | 1.354 ms |
| Initial `PurgedHashes` | `CrdsGossipPull::build_crds_filters` | 117 | 560.551 us | 530.213 us | 1.153 ms | 1.310 ms | 1.414 ms |
| Refined expiry queue | `CrdsGossipPull::build_crds_filters` | 112 | 621.182 us | 651.916 us | 1.191 ms | 1.352 ms | 1.631 ms |
| Flat queue baseline | `ClusterInfo::repair_peers` | 75 | 537.767 us | 577.828 us | 971.531 us | 1.052 ms | 1.086 ms |
| Initial `PurgedHashes` | `ClusterInfo::repair_peers` | 65 | 527.130 us | 556.419 us | 906.509 us | 965.782 us | 966.826 us |
| Refined expiry queue | `ClusterInfo::repair_peers` | 91 | 475.160 us | 483.288 us | 902.007 us | 941.589 us | 1.030 ms |

The dominant repeatable tail remains the `build_crds_filters` read guard;
`PurgedHashes` makes it modestly shorter but does not remove its roughly 1 ms
table snapshot. The next structural target is therefore that snapshot rather
than the now-small purge trim guard.

Capture integrity for the refined version: 744 waits, 376,416 holder
intervals, 712 attributed waits, zero lost events, and every combined-tracer
diagnostic zero. The independent hold capture recorded 381,678 guards with
zero lost events and every diagnostic zero.

Source captures:

- Initial sharded holds: `/tmp/crds-lock-holds-3647025-purged-hashes-60s.csv`
- Initial sharded hold report and histograms: `/tmp/crds-lock-holds-3647025-purged-hashes-60s.json`
- Initial sharded waits: `/tmp/crds-lock-waits-3647025-purged-hashes-blockers-60s.csv`
- Initial sharded wait-to-holder relations: `/tmp/crds-lock-waits-3647025-purged-hashes-blockers-60s.blockers.csv`
- Initial sharded wait report and histograms: `/tmp/crds-lock-waits-3647025-purged-hashes-blockers-60s.json`
- Initial sharded build ID: `b16245ee9cc01f9baa675967c9b40ed453fc8426`
- Refined holds: `/tmp/crds-lock-holds-3655489-purged-hashes-expiry-queue-60s.csv`
- Refined hold report and histograms: `/tmp/crds-lock-holds-3655489-purged-hashes-expiry-queue-60s.json`
- Refined waits: `/tmp/crds-lock-waits-3655489-purged-hashes-expiry-queue-blockers-60s.csv`
- Refined wait-to-holder relations: `/tmp/crds-lock-waits-3655489-purged-hashes-expiry-queue-blockers-60s.blockers.csv`
- Refined wait report and histograms: `/tmp/crds-lock-waits-3655489-purged-hashes-expiry-queue-blockers-60s.json`
- Refined build ID: `64cbd5a9274f875f9c2823d98097dd27f0684b8b`

## Per-prefix snapshot guard experiment

Build ID `ae767b2ee2303e07c336aae37cb4a43240365175` changed
`build_crds_filters` to release and reacquire the CRDS read lock for each
active bloom-filter prefix. There were exactly 64 prefix guards per build in
the 60-second hold capture. Prefixes are disjoint, so the change does not
repeat index traversal or hash copying.

The validator was allowed to reach steady state before two separate 60-second
captures. The live table contained about 222k entries and the purged-hash
index about 407k--412k entries. Both captures reported zero lost events and
every probe diagnostic was zero.

### Individual `build_crds_filters` guards

| Version/path | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| One selected-hash snapshot guard | 120 | 1.052 ms | 1.028 ms | 1.334 ms | 1.433 ms | 1.519 ms |
| One guard per active prefix | 7,680 | 17.113 us | 15.782 us | 24.304 us | 28.701 us | 566.663 us |

The change reduced the individual guard median by 98.5% and P99 by 98.0%.
Mean aggregate hold occupancy per build was approximately 1.095 ms
(`64 * 17.113 us`), 4.1% above the previous 1.052 ms mean. The intended
continuous-hold bound was therefore achieved without materially repeating
the snapshot work.

### Futex slow-path waits

| Version | Mode | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| One snapshot guard | Read | 259 | 116.795 us | 71.399 us | 316.685 us | 527.433 us | 689.453 us |
| One snapshot guard | Write | 485 | 306.090 us | 170.796 us | 910.054 us | 1.275 ms | 1.631 ms |
| One snapshot guard | Combined | 744 | 240.193 us | 135.617 us | 849.011 us | 1.158 ms | 1.631 ms |
| Per-prefix guards | Read | 250 | 113.164 us | 76.476 us | 277.789 us | 569.578 us | 830.818 us |
| Per-prefix guards | Write | 464 | 371.948 us | 245.279 us | 1.085 ms | 1.514 ms | 1.677 ms |
| Per-prefix guards | Combined | 714 | 281.337 us | 148.806 us | 986.901 us | 1.376 ms | 1.677 ms |

Despite the much shorter individual guards, writer mean increased 21.5%,
median increased 43.6%, P95 increased 19.2%, and P99 increased 18.7%. The
sampled maximum increased 2.8%; that maximum was terminally attributed to the
single `ClusterInfo::save_contact_info` reader rather than the filter build.

### Writer waits blocked by `build_crds_filters`

| Version | Waits | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| One snapshot guard | 112 | 621.182 us | 651.916 us | 1.191 ms | 1.352 ms | 1.631 ms |
| Per-prefix guards | 118 | 765.420 us | 751.083 us | 1.402 ms | 1.518 ms | 1.527 ms |

The combined blocker trace recorded 4,463 per-prefix build guards overlapping
119 distinct writer waits: 37.5 build guards per wait on average. Of those
waits, 101 began while a build guard was already active and 118 ended after a
build guard. The unlocked intervals therefore did not reliably hand progress
to an already-contending writer; the writer usually observed a train of short
read guards. This explains why reducing each guard did not reduce the futex
wait distribution.

The result argues against retaining per-prefix reacquisition solely as a
contention fix. A writer-aware handoff would be required for batching to help,
or the total snapshot work must instead be removed from the global CRDS lock.

Capture integrity: 714 waits, 381,598 holder intervals, 683 attributed waits,
zero lost events, and every combined-tracer diagnostic zero. The independent
hold capture recorded 384,545 guards with zero lost events and every
diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths):

- Holds: `/tmp/crds-lock-holds-3664681-per-prefix-snapshot-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3664681-per-prefix-snapshot-60s.json`
- Waits: `/tmp/crds-lock-waits-3664681-per-prefix-snapshot-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3664681-per-prefix-snapshot-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3664681-per-prefix-snapshot-blockers-60s.json`

## Task-fair `parking_lot::RwLock` follow-up

Build ID `6b9cf3f517c359ff4ffdfdcf5a7849d65b59cabc` replaced the
standard-library futex `RwLock` behind `CrdsRwLock` with
`parking_lot::RwLock`, retaining the 64 per-prefix acquisitions. The wrapper
keeps the existing caller API and stable hold markers. New stable contended
markers measure the interval from a failed `try_read`/`try_write` through
successful acquisition, including adaptive spinning and any parked wait.

The pre-change capture entered the standard-library `read_contended` and
`write_contended` slow paths; the new capture enters the wrapper's contended
marker after the equivalent fast acquisition attempt fails. Both definitions
measure contended acquisition rather than all acquisitions, but the probe
mechanism is recorded explicitly as `wait_probe` in each JSON report.

The validator was allowed to reach steady state before separate 60-second
wait and hold captures. The live table contained about 222k entries and the
purged-hash index about 403k--411k entries. Both captures reported zero lost
events and every probe diagnostic was zero.

### Futex/parking slow-path waits

| Lock | Mode | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| `std::sync::RwLock` | Read | 250 | 113.164 us | 76.476 us | 277.789 us | 569.578 us | 830.818 us |
| `std::sync::RwLock` | Write | 464 | 371.948 us | 245.279 us | 1.085 ms | 1.514 ms | 1.677 ms |
| `std::sync::RwLock` | Combined | 714 | 281.337 us | 148.806 us | 986.901 us | 1.376 ms | 1.677 ms |
| `parking_lot::RwLock` | Read | 445 | 77.090 us | 34.228 us | 254.472 us | 529.078 us | 848.673 us |
| `parking_lot::RwLock` | Write | 833 | 113.877 us | 22.070 us | 622.975 us | 905.115 us | 1.498 ms |
| `parking_lot::RwLock` | Combined | 1,278 | 101.068 us | 28.259 us | 483.881 us | 844.248 us | 1.498 ms |

Contended event count increased, consistent with task-fair writer reservation
causing more acquisitions to take an explicit slow path. Because the probe
site changed with the lock implementation, event-count comparisons should be
treated more cautiously than duration comparisons. The waits became much
shorter: writer mean fell 69.4%, median fell 91.0%, P95 fell 42.6%, and P99
fell 40.2%. Total writer wait time fell from 172.584 ms to 94.860 ms (-45.0%)
despite the higher event count. Combined total wait fell from 200.875 ms to
129.165 ms (-35.7%). Read total wait increased from 28.291 ms to 34.305 ms
(+21.3%), consistent with preventing readers from barging ahead of a reserved
writer.

The sampled overall maximum fell only 10.7% because the new 1.498 ms maximum
was a single wait terminally blocked by `ClusterInfo::save_contact_info`, not
the filter builder.

### Per-prefix `build_crds_filters` hold

| Lock | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| `std::sync::RwLock` | 7,680 | 17.113 us | 15.782 us | 24.304 us | 28.701 us | 566.663 us |
| `parking_lot::RwLock` | 7,680 | 18.770 us | 15.902 us | 25.415 us | 35.403 us | 675.163 us |

Individual prefix hold times remained essentially unchanged at the median.
The result is therefore attributable to scheduling/handoff behavior rather
than less work under each guard.

### Writer waits blocked by `build_crds_filters`

| Lock | Waits | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| `std::sync::RwLock` | 118 | 765.420 us | 751.083 us | 1.402 ms | 1.518 ms | 1.527 ms |
| `parking_lot::RwLock` | 167 | 33.780 us | 15.791 us | 96.031 us | 425.186 us | 513.499 us |

Median build-blocked writer latency fell 97.9%, P99 fell 72.0%, and maximum
fell 66.4%. More importantly, the standard lock recorded 4,463 build guards
overlapping 119 affected waits (37.5 guards per wait), whereas `parking_lot`
recorded 168 guards across 168 affected waits. A writer reservation now stops
the filter loop after the currently active prefix instead of allowing a train
of new prefix readers.

The dominant recurring writer tail is now `ClusterInfo::repair_peers`: 91
terminal waits with a 528.140 us median, 960.052 us P95, and 1.084 ms maximum.
The independent hold capture measured its guard at a 968.343 us median and
1.186 ms maximum. The single `save_contact_info` sample caused the overall
1.498 ms wait maximum.

Capture integrity: 1,278 waits, 383,852 holder intervals, 1,241 attributed
waits, zero lost events, and every combined-tracer diagnostic zero. The
independent hold capture recorded 382,842 guards with zero lost events and
every diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths):

- Holds: `/tmp/crds-lock-holds-3674906-parking-lot-per-prefix-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3674906-parking-lot-per-prefix-60s.json`
- Waits: `/tmp/crds-lock-waits-3674906-parking-lot-per-prefix-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3674906-parking-lot-per-prefix-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3674906-parking-lot-per-prefix-blockers-60s.json`

## Compact repair-peer index follow-up

Build ID `101b419f7e395556868b396e9cf7a1add5d3661e` adds an
incrementally maintained `IndexMap<Pubkey, RepairPeerIndexEntry>` to `Crds`.
Accepted `ContactInfo` and `LowestSlot` insert, replacement, and removal paths
update the secondary view synchronously. `ClusterInfo::repair_peers` now scans
the compact, contiguous records instead of walking the ContactInfo index and
performing another CRDS table lookup for each node. Missing `LowestSlot`
continues to mean that the peer is eligible, preserving the legacy fallback.

The validator reached RPC health `ok` before separate 60-second hold and wait
captures. The comparison baseline is the immediately preceding
`parking_lot::RwLock` build.

### `repair_peers` read-lock hold time

| Version | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before: ContactInfo scan plus per-node lookup | 86 | 964.362 us | 968.343 us | 1.104 ms | 1.167 ms | 1.186 ms |
| After: compact repair-peer index | 99 | 70.376 us | 53.384 us | 160.544 us | 167.934 us | 169.524 us |

Mean hold time fell 92.7%, median fell 94.5%, P95 fell 85.5%, P99 fell
85.6%, and maximum fell 85.7%.

### Writer waits terminally blocked by `repair_peers`

| Version | Waits | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before | 91 | 517.397 us | 528.140 us | 960.052 us | 1.031 ms | 1.084 ms |
| After | 12 | 47.593 us | 47.826 us | 86.224 us | 94.287 us | 96.303 us |

Terminally blocked writer count fell 86.8%. Mean, median, P95, P99, and
maximum wait duration each fell by approximately 91%.

### All contended acquisitions

| Version | Mode | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| Before | Read | 445 | 77.090 us | 34.228 us | 254.472 us | 529.078 us | 848.673 us |
| After | Read | 420 | 73.889 us | 33.727 us | 241.249 us | 305.014 us | 1.444 ms |
| Before | Write | 833 | 113.877 us | 22.070 us | 622.975 us | 905.115 us | 1.498 ms |
| After | Write | 709 | 69.121 us | 19.607 us | 320.819 us | 492.968 us | 2.135 ms |
| Before | Combined | 1,278 | 101.068 us | 28.259 us | 483.881 us | 844.248 us | 1.498 ms |
| After | Combined | 1,129 | 70.894 us | 23.974 us | 289.445 us | 478.431 us | 2.135 ms |

Writer mean fell 39.3%, median 11.2%, P95 48.5%, and P99 45.5%. Combined
mean fell 29.9%, median 15.2%, P95 40.2%, and P99 43.3%. Total accumulated
writer wait fell from 94.860 ms to 49.007 ms (-48.3%), and combined wait fell
from 129.165 ms to 80.040 ms (-38.0%).

The sampled maximum did not improve: the 2.135 ms writer maximum was one
`CrdsGossip::process_push_message` acquisition terminally blocked by the sole
`ClusterInfo::save_contact_info` snapshot in the wait window. The independent
hold window measured its sole `save_contact_info` guard at 1.559 ms. This is
the next deterministic long-reader target; it is unrelated to the repair-peer
index.

### Write-path maintenance check

| Dominant write guard | Version | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| `CrdsGossip::process_push_message` | Before | 59,740 | 24.310 us | 18.565 us | 64.959 us | 105.704 us | 393.574 us |
| `CrdsGossip::process_push_message` | After | 60,793 | 23.685 us | 18.726 us | 60.594 us | 91.071 us | 695.911 us |

The dominant write guard's median changed by +0.161 us (+0.9%), while its
mean, P95, and P99 improved in this sample. There is no broad write-hold
regression attributable to maintaining the index; isolated maxima remain
sensitive to scheduling.

Capture integrity: 1,129 waits, 383,021 holder intervals, 1,093 attributed
waits, 36 unattributed waits, zero lost events, and every combined-tracer
diagnostic zero. The independent hold capture recorded 388,774 guards with
zero lost events and every diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Holds: `/tmp/crds-lock-holds-3689624-repair-peer-index-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3689624-repair-peer-index-60s.json`
- Waits: `/tmp/crds-lock-waits-3689624-repair-peer-index-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3689624-repair-peer-index-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3689624-repair-peer-index-blockers-60s.json`

## Batched `save_contact_info` follow-up

Build ID `d073e4c60e56b9656f614c321fafe950292706fc` replaces the
monolithic full-value clone in `ClusterInfo::save_contact_info` with one
pubkey-only snapshot followed by 64-pubkey clone batches. The signed
`CrdsValue` file format and filtering behavior are unchanged. The two phases
are deliberately retained as the distinct symbolic acquisition paths
`ClusterInfo::save_contact_info_pubkeys` and
`ClusterInfo::save_contact_info_batch`.

The validator reached RPC health `ok` before separate 60-second hold and wait
captures. The comparison baseline is the immediately preceding compact
repair-peer-index build.

### `save_contact_info` read-lock holds

| Version/path | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before: monolithic clone | 1 | 1.559 ms | 1.559 ms | 1.559 ms | 1.559 ms | 1.559 ms |
| After: pubkey snapshot | 1 | 60.834 us | 60.834 us | 60.834 us | 60.834 us | 60.834 us |
| After: 64-pubkey clone batch | 55 | 43.858 us | 42.719 us | 52.397 us | 75.410 us | 96.624 us |

The maximum continuous `save_contact_info` hold fell 93.8%, from 1.559 ms to
96.624 us. Total occupancy per save rose from 1.559 ms to approximately
2.473 ms because of repeated indexed lookups, but this work occurs once per
minute and writers can run between every bounded batch.

### Writer waits terminally blocked by `save_contact_info`

| Version/path | Waits | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before: monolithic clone | 1 | 2.135 ms | 2.135 ms | 2.135 ms | 2.135 ms | 2.135 ms |
| After: 64-pubkey clone batch | 2 | 30.688 us | 30.688 us | 31.187 us | 31.232 us | 31.243 us |

The sampled maximum writer wait caused by saving contact info fell 98.5%.
The pubkey-snapshot phase did not terminally block a recorded wait.

### All contended acquisitions

| Version | Mode | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| Before | Read | 420 | 73.889 us | 33.727 us | 241.249 us | 305.014 us | 1.444 ms |
| After | Read | 414 | 66.511 us | 33.036 us | 215.972 us | 299.702 us | 1.264 ms |
| Before | Write | 709 | 69.121 us | 19.607 us | 320.819 us | 492.968 us | 2.135 ms |
| After | Write | 664 | 61.647 us | 18.135 us | 278.689 us | 487.187 us | 1.095 ms |
| Before | Combined | 1,129 | 70.894 us | 23.974 us | 289.445 us | 478.431 us | 2.135 ms |
| After | Combined | 1,078 | 63.515 us | 22.801 us | 256.722 us | 476.693 us | 1.264 ms |

Writer mean fell 10.8%, median 7.5%, P95 13.1%, and maximum 48.7%. Combined
mean fell 10.4%, median 4.9%, P95 11.3%, and maximum 40.8%. Total accumulated
writer wait fell from 49.007 ms to 40.934 ms (-16.5%), and combined wait fell
from 80.040 ms to 68.470 ms (-14.5%).

The new writer maximum is two acquisitions terminally blocked by
`ClusterInfo::all_peers`, with a 1.020 ms median and 1.095 ms maximum. The new
combined maximum is a 1.264 ms reader acquisition terminally blocked by the
live-hash `CrdsGossip::purge` write guard. Neither maximum is associated with
`save_contact_info`.

Wait-capture integrity: 1,078 waits, 383,172 holder intervals, 1,023
attributed waits, 55 unattributed waits, zero lost events, and every
combined-tracer diagnostic zero. The independent hold capture recorded
373,876 guards with zero lost events. It reported one unmatched read release
at the capture boundary (a guard acquired before collection was enabled) and
all other diagnostics zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Holds: `/tmp/crds-lock-holds-3698274-save-contact-info-batches-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3698274-save-contact-info-batches-60s.json`
- Waits: `/tmp/crds-lock-waits-3698274-save-contact-info-batches-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3698274-save-contact-info-batches-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3698274-save-contact-info-batches-blockers-60s.json`

## Batched `all_peers` follow-up

Build ID `200ff614167a40ba3dc94d43c1a2d4e89573a594` replaces the single
full-table `ClusterInfo::all_peers` read guard with a pubkey-only snapshot
followed by 64-pubkey lookup batches. Contact-info values and their original
CRDS local timestamps are copied together. The two phases have distinct,
durable symbolic acquisition paths: `ClusterInfo::all_peers_pubkeys` and
`ClusterInfo::all_peers_batch`.

The validator reached RPC health `ok` before measurement. Because `all_peers`
is invoked infrequently, two `getClusterNodes` requests were issued 30 seconds
apart during each controlled 60-second target capture. A separate passive
60-second wait capture was collected without test traffic for the overall
before/after comparison.

### `all_peers` read-lock holds

| Version/path | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before: monolithic scan | 2 | 1.360 ms | 1.360 ms | 1.788 ms | 1.826 ms | 1.835 ms |
| After: pubkey snapshot | 4 | 52.630 us | 53.524 us | 62.299 us | 63.033 us | 63.217 us |
| After: 64-pubkey lookup batch | 220 | 38.744 us | 32.540 us | 41.757 us | 53.045 us | 1.637 ms |

The normal batch hold was reduced sharply: its median is 97.6% below the old
monolithic median, and its P99 is 97.1% below the old monolithic maximum. The
observed maximum fell only 10.8%, however. One batch was descheduled while it
held the guard and lasted 1.637 ms; the other 99% of batches completed within
53.045 us. Batching bounds the work done per guard but cannot bound operating
system scheduling pauses while a guard is live.

Repeated indexed lookups increased estimated total `all_peers` read-lock
occupancy from about 1.360 ms to 2.184 ms per invocation (+60.5%). Writers can
run between each batch, so this trades throughput for much shorter normal
continuous holds.

### Writer waits terminally blocked by `all_peers`

| Version/path | Waits | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before: monolithic scan | 2 | 1.020 ms | 1.020 ms | 1.087 ms | 1.094 ms | 1.095 ms |
| After: 64-pubkey lookup batch | 9 | 237.737 us | 25.586 us | 1.163 ms | 1.761 ms | 1.910 ms |

The controlled sample's median fell 97.5%, but its maximum increased 74.4%.
Eight of the nine waits were approximately 13--46 us; the remaining writer
waited 1.910 ms on the batch that was descheduled. Total attributed writer
wait was 2.140 ms after versus 2.039 ms before (+4.9%); excluding that single
scheduling outlier, the after total is approximately 0.230 ms. The pubkey
snapshot did not terminally block a recorded writer.

### Passive all-contended-acquisition comparison

| Version | Mode | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| Before | Read | 414 | 66.511 us | 33.036 us | 215.972 us | 299.702 us | 1.264 ms |
| After | Read | 486 | 70.008 us | 31.524 us | 233.870 us | 306.745 us | 890.971 us |
| Before | Write | 664 | 61.647 us | 18.135 us | 278.689 us | 487.187 us | 1.095 ms |
| After | Write | 736 | 46.610 us | 19.101 us | 217.756 us | 399.143 us | 476.999 us |
| Before | Combined | 1,078 | 63.515 us | 22.801 us | 256.722 us | 476.693 us | 1.264 ms |
| After | Combined | 1,222 | 55.916 us | 23.012 us | 227.835 us | 356.929 us | 890.971 us |

In the passive sample, writer mean fell 24.4%, P95 fell 21.9%, P99 fell
18.1%, and maximum fell 56.4%; writer median rose 0.966 us (+5.3%). Combined
mean fell 12.0%, P95 fell 11.3%, P99 fell 25.1%, and maximum fell 29.5%;
combined median rose 0.211 us (+0.9%). The passive window did not invoke
`all_peers`, so these overall changes are useful system-level observations,
not direct attribution to the change.

Controlled wait-capture integrity: 1,182 waits, 397,167 holder intervals,
1,127 attributed waits, 55 unattributed waits, zero lost events, and every
combined-tracer diagnostic zero. Controlled hold-capture integrity: 392,154
guards, zero lost events, and every diagnostic zero. Passive wait-capture
integrity: 1,222 waits, 387,175 holder intervals, 1,170 attributed waits, 52
unattributed waits, zero lost events, and every diagnostic zero. The
independent passive hold capture recorded 376,028 guards with zero lost events
and every diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Controlled holds: `/tmp/crds-lock-holds-3707373-all-peers-batches-controlled-60s.csv`
- Controlled hold report and histograms: `/tmp/crds-lock-holds-3707373-all-peers-batches-controlled-60s.json`
- Controlled waits: `/tmp/crds-lock-waits-3707373-all-peers-batches-blockers-60s.csv`
- Controlled wait-to-holder relations: `/tmp/crds-lock-waits-3707373-all-peers-batches-blockers-60s.blockers.csv`
- Controlled wait report and histograms: `/tmp/crds-lock-waits-3707373-all-peers-batches-blockers-60s.json`
- Passive holds: `/tmp/crds-lock-holds-3707373-all-peers-batches-60s.csv`
- Passive hold report and histograms: `/tmp/crds-lock-holds-3707373-all-peers-batches-60s.json`
- Passive waits: `/tmp/crds-lock-waits-3707373-all-peers-batches-passive-blockers-60s.csv`
- Passive wait-to-holder relations: `/tmp/crds-lock-waits-3707373-all-peers-batches-passive-blockers-60s.blockers.csv`
- Passive wait report and histograms: `/tmp/crds-lock-waits-3707373-all-peers-batches-passive-blockers-60s.json`

## Batched purge discovery and removal follow-up

Build ID `0bc52ba55a18de06ac7d9cf6000c28433d812bfa` replaces the
monolithic `CrdsGossipPull::purge_active` write guard with a pubkey snapshot,
64-pubkey candidate-discovery read guards, and 64-label removal write guards.
Every removal candidate is revalidated under the write guard so a record
refreshed after discovery is preserved. The phases are retained as distinct
symbolic acquisition paths `CrdsGossipPull::purge_pubkey_snapshot`,
`CrdsGossipPull::purge_find_candidates_batch`, and
`CrdsGossipPull::purge_remove_batch`.

The comparison baseline is the immediately preceding batched-`all_peers`
build ID `200ff614167a40ba3dc94d43c1a2d4e89573a594`. The validator reached
RPC health `ok` before independent passive 60-second hold and wait captures;
no synthetic RPC traffic was issued.

### Active-purge lock holds

| Version/path | Mode | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| Before: monolithic scan and removal | Write | 600 | 276.743 us | 250.982 us | 416.596 us | 799.336 us | 4.776 ms |
| After: pubkey snapshot | Read | 600 | 14.703 us | 12.848 us | 24.479 us | 34.243 us | 195.660 us |
| After: 64-pubkey discovery batch | Read | 32,962 | 13.968 us | 12.588 us | 26.266 us | 31.764 us | 76.005 us |
| After: 64-label removal batch | Write | 103 | 10.287 us | 10.114 us | 14.631 us | 15.886 us | 16.903 us |

The active purge's longest continuous hold fell 95.9%, from 4.776 ms to
195.660 us. Its exclusive-hold median fell 96.0%, P99 fell 98.0%, and maximum
fell 99.6% when comparing the old monolithic write phase with the new removal
phase.

The tradeoff is more total lock occupancy. The monolithic phase occupied the
lock for 166.046 ms during the old minute. Snapshot, discovery, and removal
occupied it for 470.297 ms during the new minute (+183.2%), almost all in
shared discovery guards. This is approximately 7.84 ms of shared lock
occupancy per second. Readers can overlap those guards, but writers encounter
them more frequently.

### Waits terminally blocked by active-purge phases

| Version/path | Waiting mode | N | Mean | Median | P95 | P99 | Max |
|---|---|---:|---:|---:|---:|---:|---:|
| Before: monolithic scan/removal | Read | 174 | 137.085 us | 119.130 us | 266.773 us | 360.942 us | 890.971 us |
| Before: monolithic scan/removal | Write | 9 | 215.134 us | 213.335 us | 269.444 us | 274.530 us | 275.801 us |
| After: pubkey snapshot | Write | 11 | 19.747 us | 18.646 us | 30.703 us | 33.026 us | 33.607 us |
| After: discovery batch | Write | 524 | 16.177 us | 14.720 us | 30.095 us | 36.610 us | 234.213 us |
| After: removal batch | Read | 3 | 11.953 us | 9.903 us | 16.050 us | 16.596 us | 16.733 us |

Active-purge-attributed wait count rose from 183 to 538 because writers meet
many short discovery batches. Nevertheless, accumulated attributed wait fell
from 25.789 ms to 8.730 ms (-66.1%), and the maximum fell from 890.971 us to
234.213 us (-73.7%). Most importantly for readers, the maximum caused by the
active exclusive purge phase fell from 890.971 us to 16.733 us (-98.1%).

Including the separate purged-hash trimming guard, all purge-family
accumulated wait fell from 31.808 ms to 9.416 ms (-70.4%). The trimming phase
was not structurally changed by this patch.

### Passive all-contended-acquisition comparison

| Version | Mode | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Before | Read | 486 | 70.008 us | 31.524 us | 233.870 us | 306.745 us | 890.971 us | 34.024 ms |
| After | Read | 813 | 31.310 us | 26.036 us | 63.596 us | 98.428 us | 420.651 us | 25.455 ms |
| Before | Write | 736 | 46.610 us | 19.101 us | 217.756 us | 399.143 us | 476.999 us | 34.305 ms |
| After | Write | 973 | 43.859 us | 16.763 us | 237.252 us | 455.228 us | 550.441 us | 42.675 ms |
| Before | Combined | 1,222 | 55.916 us | 23.012 us | 227.835 us | 356.929 us | 890.971 us | 68.329 ms |
| After | Combined | 1,786 | 38.147 us | 20.889 us | 132.921 us | 416.667 us | 550.441 us | 68.130 ms |

System-wide read mean fell 55.3%, median 17.4%, P95 72.8%, P99 67.9%, maximum
52.8%, and accumulated wait 25.2%. System-wide writer mean and median fell
5.9% and 12.2%, but writer P99 rose 14.0%, maximum rose 15.4%, and accumulated
wait rose 24.4%. Combined mean fell 31.8%, median 9.2%, P95 41.7%, maximum
38.2%, and accumulated wait 0.3%; combined P99 rose 16.7%.

The new system-wide maximum and writer P99 came from
`CrdsGossip::new_pull_request`, not from purge. The after window also recorded
substantially more concurrent gossip activity (for example, 747 read waits
terminally blocked by `process_push_message`, versus 239 before), so changes
outside the directly attributed purge rows should be treated as sample-level
observations rather than causal effects of the patch.

Hold-capture integrity: 413,031 guards, zero lost events, and every diagnostic
zero. Wait-capture integrity: 1,786 waits, 411,967 holder intervals, 1,730
attributed waits, 56 unattributed waits, zero lost events, and every diagnostic
zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Holds: `/tmp/crds-lock-holds-3724782-purge-batches-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3724782-purge-batches-60s.json`
- Waits: `/tmp/crds-lock-waits-3724782-purge-batches-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3724782-purge-batches-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3724782-purge-batches-blockers-60s.json`

## Current versus original-master slow-path summary

This section compares the original master capture
`/tmp/agave-locks-60s-3533529-final2.raw` with current build ID
`0bc52ba55a18de06ac7d9cf6000c28433d812bfa`. Both acquisition-start windows
were 60 seconds. Master used standard-library `RwLock` futex-entry probes;
current uses `CrdsRwLock`'s `parking_lot` marker from failed fast acquisition
through successful acquisition. Duration comparisons are informative, while
event-count comparisons are affected by the change in lock and probe.

### Slow-path acquisition latency

Master percentiles were retained as fine-histogram intervals. Approximate
changes use each interval's midpoint.

| Mode | Metric | Master | Current | Change |
|---|---|---:|---:|---:|
| Read | Slow-path acquisitions | 294 | 813 | +176.5% |
| Read | Mean | 165.616 us | 31.310 us | -81.1% |
| Read | Median | 140--150 us | 26.036 us | ~-82.0% |
| Read | P95 | 470--480 us | 63.596 us | ~-86.6% |
| Read | P99 | 1.6--1.7 ms | 98.428 us | ~-94.0% |
| Read | Max | 1.859 ms | 420.651 us | -77.4% |
| Read | Accumulated wait | 48.691 ms | 25.455 ms | -47.7% |
| Write | Slow-path acquisitions | 550 | 973 | +76.9% |
| Write | Mean | 709.602 us | 43.859 us | -93.8% |
| Write | Median | 280--290 us | 16.763 us | ~-94.1% |
| Write | P95 | 1.9--2.0 ms | 237.252 us | ~-87.8% |
| Write | P99 | 2.5--2.6 ms | 455.228 us | ~-82.1% |
| Write | Max | 3.446 ms | 550.441 us | -84.0% |
| Write | Accumulated wait | 390.281 ms | 42.675 ms | -89.1% |
| Combined | Slow-path acquisitions | 844 | 1,786 | +111.6% |
| Combined | Mean | 520.109 us | 38.147 us | -92.7% |
| Combined | Max | 3.446 ms | 550.441 us | -84.0% |
| Combined | Accumulated wait | 438.972 ms | 68.130 ms | -84.5% |

### Wait-interval histogram

Current counts are exact from individual wait rows. Master counts were
reconstructed from the raw fine-grained `hist(delta_ns, 4)` buckets and
assigned by bucket midpoint to the common decimal intervals below. Counts
sum exactly to each capture's mode totals; master boundary assignments are
approximate. The final bucket is combined to avoid implying precision beyond
the master histogram and its separately recorded maximum.

| Wait interval | Master read | Master write | Master total | Current read | Current write | Current total |
|---|---:|---:|---:|---:|---:|---:|
| [0, 0.25) us | 28 | 33 | 61 | 0 | 0 | 0 |
| [0.25, 0.5) us | 4 | 7 | 11 | 0 | 0 | 0 |
| [0.5, 1) us | 36 | 30 | 66 | 0 | 2 | 2 |
| [1, 2) us | 17 | 37 | 54 | 7 | 18 | 25 |
| [2, 4) us | 13 | 25 | 38 | 5 | 52 | 57 |
| [4, 8) us | 12 | 20 | 32 | 8 | 74 | 82 |
| [8, 16) us | 12 | 19 | 31 | 137 | 318 | 455 |
| [16, 32) us | 13 | 33 | 46 | 365 | 313 | 678 |
| [32, 64) us | 127 | 71 | 198 | 250 | 59 | 309 |
| [64, 128) us | 16 | 20 | 36 | 34 | 51 | 85 |
| [128, 256) us | 7 | 33 | 40 | 6 | 42 | 48 |
| [256, 512) us | 1 | 10 | 11 | 1 | 42 | 43 |
| [512, 1024) us | 1 | 1 | 2 | 0 | 2 | 2 |
| >= 1024 us | 7 | 211 | 218 | 0 | 0 | 0 |
| **Total** | **294** | **550** | **844** | **813** | **973** | **1,786** |

Master recorded 218 slow paths at or above approximately 1 ms. Current
recorded none; its longest wait was 550.441 us.

### Current acquisition-path ranking

The tables rank exact symbolic acquisition callsites. `N` is included because
the first three median ranks contain only one or two observations.

#### By median

| Rank | Mode | Acquisition path | N | Median | P99 | Max |
|---:|---|---|---:|---:|---:|---:|
| 1 | Read | `ClusterInfo::get_epoch_slots+0x54` | 1 | 72.871 us | 72.871 us | 72.871 us |
| 2 | Read | `ClusterInfo::repair_peers+0xcf` | 2 | 41.602 us | 60.096 us | 60.473 us |
| 3 | Read | `ClusterInfo::save_contact_info_batch+0x5d` | 2 | 36.455 us | 48.854 us | 49.107 us |
| 4 | Read | `CrdsGossipPull::purge_find_candidates_batch+0x55` | 555 | 27.468 us | 122.431 us | 420.651 us |
| 5 | Read | `CrdsGossipPull::build_crds_filters+0x307` | 153 | 26.807 us | 90.874 us | 202.059 us |
| 6 | Read | `ClusterInfo::generate_new_gossip_requests+0x6a0` | 2 | 25.931 us | 27.692 us | 27.728 us |
| 7 | Read | Votor `ClusterInfo::query_contact_infos+0x53` | 2 | 21.250 us | 37.844 us | 38.183 us |
| 8 | Read | `CrdsGossipPull::purge_pubkey_snapshot+0x3f` | 4 | 21.024 us | 29.350 us | 29.561 us |
| 9 | Read | `ClusterInfo::process_packets+0x26bb` | 5 | 20.839 us | 25.680 us | 25.746 us |
| 10 | Read | `ClusterInfo::get_votes+0x54` | 8 | 20.212 us | 111.764 us | 117.502 us |
| 11 | Read | `ClusterInfo::new_push_requests+0x251` | 10 | 19.943 us | 30.405 us | 30.682 us |
| 12 | Read | `CrdsGossipPull::build_crds_filters+0xae` | 3 | 19.837 us | 21.044 us | 21.069 us |
| 13 | Write | `CrdsGossip::process_pull_responses+0x89` | 16 | 18.015 us | 83.932 us | 93.780 us |
| 14 | Write | `CrdsGossip::process_push_message+0xa5` | 922 | 16.828 us | 457.888 us | 550.441 us |
| 15 | Read | `ClusterInfo::push_lowest_slot+0xd7` | 8 | 15.987 us | 43.979 us | 45.082 us |
| 16 | Read | `ClusterInfo::process_packets+0x165` | 10 | 15.982 us | 24.181 us | 24.614 us |
| 17 | Read | `ClusterInfo::trim_crds_table+0x53` | 19 | 15.752 us | 35.846 us | 36.340 us |
| 18 | Read | `CrdsGossipPush::new_push_messages+0x16b` | 11 | 13.949 us | 27.391 us | 27.828 us |
| 19 | Read | `submit_gossip_stats+0x50` | 1 | 13.859 us | 13.859 us | 13.859 us |
| 20 | Read | `ClusterInfo::get_duplicate_shreds+0x54` | 15 | 13.499 us | 53.130 us | 54.715 us |
| 21 | Write | `CrdsGossip::purge+0x16e` | 32 | 12.242 us | 67.283 us | 73.312 us |
| 22 | Write | `ClusterInfo::flush_push_queue+0xf0` | 1 | 12.127 us | 12.127 us | 12.127 us |
| 23 | Write | `CrdsGossipPull::purge_remove_batch+0x3a` | 2 | 7.505 us | 13.565 us | 13.689 us |
| 24 | Read | `CrdsGossip::new_pull_request+0xe1` | 2 | 2.288 us | 2.685 us | 2.693 us |

#### By maximum

| Rank | Mode | Acquisition path | N | Median | P99 | Max |
|---:|---|---|---:|---:|---:|---:|
| 1 | Write | `CrdsGossip::process_push_message+0xa5` | 922 | 16.828 us | 457.888 us | 550.441 us |
| 2 | Read | `CrdsGossipPull::purge_find_candidates_batch+0x55` | 555 | 27.468 us | 122.431 us | 420.651 us |
| 3 | Read | `CrdsGossipPull::build_crds_filters+0x307` | 153 | 26.807 us | 90.874 us | 202.059 us |
| 4 | Read | `ClusterInfo::get_votes+0x54` | 8 | 20.212 us | 111.764 us | 117.502 us |
| 5 | Write | `CrdsGossip::process_pull_responses+0x89` | 16 | 18.015 us | 83.932 us | 93.780 us |
| 6 | Write | `CrdsGossip::purge+0x16e` | 32 | 12.242 us | 67.283 us | 73.312 us |
| 7 | Read | `ClusterInfo::get_epoch_slots+0x54` | 1 | 72.871 us | 72.871 us | 72.871 us |
| 8 | Read | `ClusterInfo::repair_peers+0xcf` | 2 | 41.602 us | 60.096 us | 60.473 us |
| 9 | Read | `ClusterInfo::get_duplicate_shreds+0x54` | 15 | 13.499 us | 53.130 us | 54.715 us |
| 10 | Read | `ClusterInfo::save_contact_info_batch+0x5d` | 2 | 36.455 us | 48.854 us | 49.107 us |
| 11 | Read | `ClusterInfo::push_lowest_slot+0xd7` | 8 | 15.987 us | 43.979 us | 45.082 us |
| 12 | Read | Votor `ClusterInfo::query_contact_infos+0x53` | 2 | 21.250 us | 37.844 us | 38.183 us |
| 13 | Read | `ClusterInfo::trim_crds_table+0x53` | 19 | 15.752 us | 35.846 us | 36.340 us |
| 14 | Read | `ClusterInfo::new_push_requests+0x251` | 10 | 19.943 us | 30.405 us | 30.682 us |
| 15 | Read | `CrdsGossipPull::purge_pubkey_snapshot+0x3f` | 4 | 21.024 us | 29.350 us | 29.561 us |
| 16 | Read | `CrdsGossipPush::new_push_messages+0x16b` | 11 | 13.949 us | 27.391 us | 27.828 us |
| 17 | Read | `ClusterInfo::generate_new_gossip_requests+0x6a0` | 2 | 25.931 us | 27.692 us | 27.728 us |
| 18 | Read | `ClusterInfo::process_packets+0x26bb` | 5 | 20.839 us | 25.680 us | 25.746 us |
| 19 | Read | `ClusterInfo::process_packets+0x165` | 10 | 15.982 us | 24.181 us | 24.614 us |
| 20 | Read | `CrdsGossipPull::build_crds_filters+0xae` | 3 | 19.837 us | 21.044 us | 21.069 us |
| 21 | Read | `submit_gossip_stats+0x50` | 1 | 13.859 us | 13.859 us | 13.859 us |
| 22 | Write | `CrdsGossipPull::purge_remove_batch+0x3a` | 2 | 7.505 us | 13.565 us | 13.689 us |
| 23 | Write | `ClusterInfo::flush_push_queue+0xf0` | 1 | 12.127 us | 12.127 us | 12.127 us |
| 24 | Read | `CrdsGossip::new_pull_request+0xe1` | 2 | 2.288 us | 2.685 us | 2.693 us |

### Path comparison available from master

The master tracer kept only one representative acquisition stack for each
`(lock, mode)`, not a stack for every event. It therefore cannot support a
complete per-path ranking. Its representative read stack was
`ClusterInfo::get_duplicate_shreds`; its representative and dominant write
stack was `CrdsGossip::process_push_message`. The master mode pools are used
as directional proxies below, not exact path distributions.

| Proxy comparison | Metric | Master mode pool | Current path | Approx. change |
|---|---|---:|---:|---:|
| Read / `get_duplicate_shreds` | Mean | 165.616 us | 21.629 us | -86.9% |
| Read / `get_duplicate_shreds` | Median | 140--150 us | 13.499 us | ~-90.7% |
| Read / `get_duplicate_shreds` | P95 | 470--480 us | 46.787 us | ~-90.2% |
| Read / `get_duplicate_shreds` | P99 | 1.6--1.7 ms | 53.130 us | ~-96.8% |
| Read / `get_duplicate_shreds` | Max | 1.859 ms | 54.715 us | -97.1% |
| Write / `process_push_message` | Mean | 709.602 us | 45.192 us | -93.6% |
| Write / `process_push_message` | Median | 280--290 us | 16.828 us | ~-94.1% |
| Write / `process_push_message` | P95 | 1.9--2.0 ms | 243.436 us | ~-87.5% |
| Write / `process_push_message` | P99 | 2.5--2.6 ms | 457.888 us | ~-82.0% |
| Write / `process_push_message` | Max | 3.446 ms | 550.441 us | -84.0% |

Source-data limitations are retained explicitly: reproducing a true master
per-path ranking would require running the master binary again with the
current symbolic tracer. No lock addresses from the current binary are used
as durable identifiers in these tables.

## Shortened `get_gossip_nodes` follow-up

Build ID `e1212839a2b4a52e24c5c161dd23cb8be7c7c957` changes gossip-node
selection so the CRDS read guard only copies each ContactInfo's gossip
address, shred version, pubkey, and local timestamp. Socket validation,
shred-version and validator filtering, stake lookup, activity filtering, and
random sampling now run after the guard is released. The guarded helper is
kept out of line as the durable symbolic hold path
`crds_gossip::snapshot_gossip_nodes`.

The comparison baseline is the immediately preceding active-purge batching
build ID `0bc52ba55a18de06ac7d9cf6000c28433d812bfa`. Both builds were measured
using independent passive 60-second hold and wait captures after RPC health
reported `ok`; no synthetic RPC traffic was issued.

### Gossip-node selection read-lock hold

The before row combines the two consumers of `get_gossip_nodes`:
`CrdsGossip::new_pull_request` (120 calls) and
`CrdsGossip::refresh_push_active_set` (8 calls). The after row represents the
same 128-call cadence through their common snapshot helper.

| Version/path | N | Mean | Median | P95 | P99 | Max | Accumulated hold |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: full selection under guard | 128 | 518.442 us | 508.778 us | 652.600 us | 829.234 us | 858.206 us | 66.361 ms |
| After: `snapshot_gossip_nodes` | 128 | 145.084 us | 136.053 us | 202.664 us | 256.079 us | 390.710 us | 18.571 ms |

Mean hold fell 72.0%, median 73.3%, P95 68.9%, P99 69.1%, maximum 54.5%,
and accumulated hold 72.0%.

### Writer waits terminally blocked by gossip-node selection

| Version/blocker | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: `new_pull_request` or `refresh_push_active_set` | 78 | 250.469 us | 240.728 us | 498.629 us | 540.386 us | 550.441 us | 19.537 ms |
| After: `snapshot_gossip_nodes` | 27 | 75.768 us | 76.045 us | 124.258 us | 213.619 us | 244.618 us | 2.046 ms |

Terminally blocked writer count fell 65.4%, mean wait 69.7%, median 68.4%,
P95 75.1%, P99 60.5%, maximum 55.6%, and accumulated wait 89.5%. The target
path is no longer responsible for the overall writer maximum.

### Passive all-contended-acquisition comparison

| Version | Mode | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Before | Read | 813 | 31.310 us | 26.036 us | 63.596 us | 98.428 us | 420.651 us | 25.455 ms |
| After | Read | 858 | 38.441 us | 28.684 us | 105.808 us | 164.881 us | 476.458 us | 32.983 ms |
| Before | Write | 973 | 43.859 us | 16.763 us | 237.252 us | 455.228 us | 550.441 us | 42.675 ms |
| After | Write | 937 | 25.635 us | 15.492 us | 97.049 us | 196.667 us | 357.694 us | 24.020 ms |
| Before | Combined | 1,786 | 38.147 us | 20.889 us | 132.921 us | 416.667 us | 550.441 us | 68.130 ms |
| After | Combined | 1,795 | 31.756 us | 19.898 us | 102.769 us | 190.333 us | 476.458 us | 57.002 ms |

Writer mean fell 41.6%, P95 59.1%, P99 56.8%, maximum 35.0%, and
accumulated wait 43.7%. Combined mean fell 16.8%, P95 22.7%, P99 54.3%,
maximum 13.4%, and accumulated wait 16.3%. Read-side metrics increased in
this independent window; the overall read maximum and the dominant
read-wait accumulation were blocked by `process_push_message`, so this does
not oppose the direct gossip-node result.

### Leading terminal blockers after the change

| Waiting mode | Terminal blocker | N | Accumulated wait | Median | P99 | Max |
|---|---|---:|---:|---:|---:|---:|
| Read | `CrdsGossipPush::process_push_message` | 789 | 32.218 ms | 30.812 us | 166.176 us | 476.458 us |
| Write | `CrdsGossipPull::purge_find_candidates_batch` | 527 | 7.828 ms | 14.160 us | 32.660 us | 47.866 us |
| Write | Votor `ClusterInfo::query_contact_infos` | 27 | 3.459 ms | 125.063 us | 272.803 us | 277.273 us |
| Write | `ClusterInfo::get_votes` | 49 | 3.370 ms | 65.311 us | 225.747 us | 245.930 us |
| Write | `CrdsGossipPull::build_crds_filters` | 152 | 3.100 ms | 15.792 us | 154.131 us | 357.694 us |
| Write | `crds_gossip::snapshot_gossip_nodes` | 27 | 2.046 ms | 76.045 us | 213.619 us | 244.618 us |

Hold-capture integrity: 411,437 guards, zero lost events, and every
diagnostic zero. Wait-capture integrity: 1,795 waits, 422,261 holder
intervals, 1,732 attributed waits, 63 unattributed waits, zero lost events,
and every diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Holds: `/tmp/crds-lock-holds-3759248-gossip-node-snapshot-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3759248-gossip-node-snapshot-60s.json`
- Waits: `/tmp/crds-lock-waits-3759248-gossip-node-snapshot-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3759248-gossip-node-snapshot-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3759248-gossip-node-snapshot-blockers-60s.json`

## Shortened `process_push_message` follow-up

Build ID `38ca61e38bb9cef8ea595e7374ed810e9de6f204` moves work out of
`CrdsGossipPush::process_push_message`'s CRDS write guard. Wallclock filtering
and result-storage allocation now happen before acquisition. The guarded
phase only calls `Crds::insert` and records its outcome into preallocated
storage. `ReceivedCache` updates, result-set construction, counter updates,
and input allocation teardown happen after the CRDS guard is released. This
also removes the previous nesting of the `ReceivedCache` mutex around the
CRDS write acquisition; the cache is documented as a lagging view.

The comparison baseline is the immediately preceding shortened-gossip-node
build ID `e1212839a2b4a52e24c5c161dd23cb8be7c7c957`. Both builds were
measured using independent passive 60-second hold and wait captures after
RPC health reported `ok`; no synthetic RPC traffic was issued. The target
call count differed by only 1.0% between hold windows.

### `process_push_message` write-lock hold

| Version | N | Mean | Median | P95 | P99 | Max | Accumulated hold |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before | 59,697 | 22.000 us | 16.513 us | 60.826 us | 95.382 us | 984.060 us | 1,313.338 ms |
| After | 59,100 | 17.475 us | 13.449 us | 46.234 us | 73.932 us | 1.000 ms | 1,032.784 ms |

Mean hold fell 20.6%, median 18.6%, P95 24.0%, P99 22.5%, and accumulated
hold 21.4%. The single maximum was essentially unchanged (+1.6%) and is an
isolated scheduler-scale observation; the distribution through P99 moved in
the intended direction.

### Reader waits terminally blocked by `process_push_message`

| Version | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before | 789 | 40.834 us | 30.812 us | 110.643 us | 166.176 us | 476.458 us | 32.218 ms |
| After | 716 | 30.751 us | 26.742 us | 63.240 us | 99.196 us | 342.143 us | 22.018 ms |

Mean wait fell 24.7%, median 13.2%, P95 42.8%, P99 40.3%, maximum 28.2%,
and accumulated wait 31.7%.

### Passive all-contended-acquisition comparison

| Version | Mode | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Before | Read | 858 | 38.441 us | 28.684 us | 105.808 us | 164.881 us | 476.458 us | 32.983 ms |
| After | Read | 783 | 29.103 us | 25.315 us | 60.873 us | 97.579 us | 342.143 us | 22.788 ms |
| Before | Write | 937 | 25.635 us | 15.492 us | 97.049 us | 196.667 us | 357.694 us | 24.020 ms |
| After | Write | 902 | 27.533 us | 15.141 us | 110.868 us | 222.533 us | 547.296 us | 24.834 ms |
| Before | Combined | 1,795 | 31.756 us | 19.898 us | 102.769 us | 190.333 us | 476.458 us | 57.002 ms |
| After | Combined | 1,685 | 28.262 us | 19.247 us | 80.703 us | 179.519 us | 547.296 us | 47.622 ms |

Read mean fell 24.3%, P95 42.5%, P99 40.8%, maximum 28.2%, and accumulated
wait 30.9%. Combined mean fell 11.0%, P95 21.5%, P99 5.7%, and accumulated
wait 16.5%. The overall maximum rose 14.9% because of one writer wait
terminally blocked by `build_crds_filters` at 547.296 us. Writer P95 and P99
also increased in this independent sample; the direct target attribution
above separates that workload variation from the `process_push_message`
effect.

### Leading terminal blockers after the change

| Waiting mode | Terminal blocker | N | Accumulated wait | Median | P99 | Max |
|---|---|---:|---:|---:|---:|---:|
| Read | `CrdsGossipPush::process_push_message` | 716 | 22.018 ms | 26.742 us | 99.196 us | 342.143 us |
| Write | `CrdsGossipPull::purge_find_candidates_batch` | 501 | 7.179 ms | 13.900 us | 34.267 us | 76.246 us |
| Write | `CrdsGossipPull::build_crds_filters` | 147 | 4.212 ms | 15.932 us | 422.846 us | 547.296 us |
| Write | Votor `ClusterInfo::query_contact_infos` | 30 | 4.122 ms | 121.713 us | 310.770 us | 313.654 us |
| Write | `ClusterInfo::get_votes` | 51 | 3.422 ms | 48.346 us | 196.481 us | 216.068 us |
| Write | `ClusterInfo::get_epoch_slots` | 42 | 2.583 ms | 58.306 us | 162.371 us | 183.864 us |
| Write | `crds_gossip::snapshot_gossip_nodes` | 13 | 1.216 ms | 102.622 us | 212.208 us | 222.598 us |

Hold-capture integrity: 409,231 guards, zero lost events, and every
diagnostic zero. Wait-capture integrity: 1,685 waits, 421,645 holder
intervals, 1,633 attributed waits, 52 unattributed waits, zero lost events,
and every diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Holds: `/tmp/crds-lock-holds-3771025-process-push-shortened-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3771025-process-push-shortened-60s.json`
- Waits: `/tmp/crds-lock-waits-3771025-process-push-shortened-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3771025-process-push-shortened-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3771025-process-push-shortened-blockers-60s.json`

## Batched `query_contact_infos` follow-up

Build ID `904b946244872547e7030415f6696956ebe40654` changes
`ClusterInfo::query_contact_infos` from one read guard over the complete Votor
peer set to 64-pubkey lookup batches. The input pubkeys and output storage are
materialized before the first guard; input order and one output per requested
pubkey are preserved. The bounded guarded helper is kept out of line as the
durable symbolic path `ClusterInfo::query_contact_infos_batch`.

The comparison baseline is the immediately preceding shortened-push build ID
`38ca61e38bb9cef8ea595e7374ed810e9de6f204`. Both builds were measured with
independent passive 60-second hold and wait captures after RPC health reported
`ok`; no synthetic RPC traffic was issued. The Votor caller ran 120 times in
both hold windows. Each after-change invocation used 11 lock batches.

### Votor ContactInfo query read-lock holds

| Version/path | N | Mean | Median | P95 | P99 | Max | Accumulated hold |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: monolithic `query_contact_infos` | 120 | 287.400 us | 293.686 us | 319.096 us | 329.471 us | 339.079 us | 34.488 ms |
| After: `query_contact_infos_batch` | 1,320 | 22.715 us | 23.983 us | 30.665 us | 39.811 us | 60.103 us | 29.984 ms |

Individual-hold mean fell 92.1%, median 91.8%, P95 90.4%, P99 87.9%, and
maximum 82.3%. Total shared-lock occupancy fell 13.1% despite the expected
11x increase in acquisition count.

### Writer waits terminally blocked by the Votor query

| Version/blocker | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: `query_contact_infos` | 30 | 137.409 us | 121.713 us | 299.853 us | 310.770 us | 313.654 us | 4.122 ms |
| After: `query_contact_infos_batch` | 21 | 20.879 us | 18.565 us | 26.556 us | 62.550 us | 71.549 us | 0.438 ms |

Blocked-writer count fell 30.0%, mean wait 84.8%, median 84.7%, P95 91.1%,
P99 79.9%, maximum 77.2%, and accumulated wait 89.4%.

### Passive all-contended-acquisition comparison

| Version | Mode | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Before | Read | 783 | 29.103 us | 25.315 us | 60.873 us | 97.579 us | 342.143 us | 22.788 ms |
| After | Read | 821 | 27.688 us | 23.973 us | 56.287 us | 105.859 us | 227.675 us | 22.732 ms |
| Before | Write | 902 | 27.533 us | 15.141 us | 110.868 us | 222.533 us | 547.296 us | 24.834 ms |
| After | Write | 906 | 23.333 us | 14.931 us | 74.896 us | 159.714 us | 467.166 us | 21.140 ms |
| Before | Combined | 1,685 | 28.262 us | 19.247 us | 80.703 us | 179.519 us | 547.296 us | 47.622 ms |
| After | Combined | 1,727 | 25.403 us | 18.936 us | 67.406 us | 133.106 us | 467.166 us | 43.872 ms |

Writer mean fell 15.3%, P95 32.4%, P99 28.2%, maximum 14.6%, and
accumulated wait 14.9%. Combined mean fell 10.1%, P95 16.5%, P99 25.9%,
maximum 14.6%, and accumulated wait 7.9%. Read maximum fell 33.5%; its P99
rose 8.5% in the independent window while its accumulated wait was nearly
unchanged (-0.2%).

### Leading terminal blockers after the change

| Waiting mode | Terminal blocker | N | Accumulated wait | Median | P99 | Max |
|---|---|---:|---:|---:|---:|---:|
| Read | `CrdsGossipPush::process_push_message` | 759 | 22.133 ms | 25.185 us | 107.355 us | 227.675 us |
| Write | `CrdsGossipPull::purge_find_candidates_batch` | 497 | 7.105 ms | 13.819 us | 34.546 us | 43.510 us |
| Write | `CrdsGossipPull::build_crds_filters` | 151 | 4.051 ms | 14.680 us | 412.275 us | 467.166 us |
| Write | `ClusterInfo::get_votes` | 50 | 2.747 ms | 45.643 us | 188.506 us | 229.046 us |
| Write | `ClusterInfo::get_epoch_slots` | 38 | 2.375 ms | 59.042 us | 155.173 us | 182.472 us |
| Write | `crds_gossip::snapshot_gossip_nodes` | 19 | 1.317 ms | 63.498 us | 158.001 us | 160.011 us |
| Write | `ClusterInfo::time_gossip_read_lock` | 7 | 0.971 ms | 119.415 us | 212.426 us | 212.493 us |
| Write | `ClusterInfo::repair_peers` | 10 | 0.636 ms | 49.184 us | 150.546 us | 154.063 us |
| Write | `ClusterInfo::query_contact_infos_batch` | 21 | 0.438 ms | 18.565 us | 62.550 us | 71.549 us |

Hold-capture integrity: 395,434 guards, zero lost events, and every
diagnostic zero. Wait-capture integrity: 1,727 waits, 420,948 holder
intervals, 1,664 attributed waits, 63 unattributed waits, zero lost events,
and every diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Holds: `/tmp/crds-lock-holds-3779810-query-contact-info-batches-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3779810-query-contact-info-batches-60s.json`
- Waits: `/tmp/crds-lock-waits-3779810-query-contact-info-batches-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3779810-query-contact-info-batches-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3779810-query-contact-info-batches-blockers-60s.json`

## Batched `get_votes` follow-up

Build ID `0135f697bcaeecae9d3be1ee1f57d713332196f7` changes
`ClusterInfo::get_votes` and `ClusterInfo::get_votes_with_labels` from one read
guard over every pending vote to 64-vote clone batches. Each guarded helper is
kept out of line so the acquisition remains available after the binary is
replaced as the durable symbolic paths `ClusterInfo::get_votes_batch` and
`ClusterInfo::get_votes_with_labels_batch`. Vote order, labels, and cursor
advancement are preserved.

The comparison baseline is the immediately preceding batched-contact-query
build ID `904b946244872547e7030415f6696956ebe40654`. Both builds were
measured with independent passive 60-second hold and wait captures after RPC
health reported `ok`; no synthetic RPC traffic was issued.

### Vote read-lock holds

| Version/path | N | Mean | Median | P95 | P99 | Max | Accumulated hold |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: `ClusterInfo::get_votes` | 594 | 79.490 us | 71.379 us | 162.107 us | 191.294 us | 263.664 us | 47.217 ms |
| After: `ClusterInfo::get_votes_batch` | 2,307 | 23.549 us | 24.244 us | 36.797 us | 46.667 us | 140.694 us | 54.328 ms |

Individual-hold mean fell 70.4%, median 66.0%, P95 77.3%, P99 75.6%, and
maximum 46.6%. As expected, acquisition count increased 3.88x. Accumulated
hold rose 15.1%, reflecting the repeated lock/probe and batch-allocation
overhead, but the bounded intervals materially reduce the time for which a
waiting writer remains blocked.

### Writer waits terminally blocked by vote reads

| Version/blocker | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: `ClusterInfo::get_votes` | 50 | 54.940 us | 45.643 us | 136.958 us | 188.506 us | 229.046 us | 2.747 ms |
| After: `ClusterInfo::get_votes_batch` | 58 | 21.116 us | 20.123 us | 35.876 us | 49.098 us | 56.519 us | 1.225 ms |

Mean wait fell 61.6%, median 55.9%, P95 73.8%, P99 74.0%, maximum 75.3%,
and accumulated wait 55.4%. The modest increase in overlap count does not
offset the much shorter blocked intervals.

### Vote-blocked writer wait histogram

| Wait interval | Before | After |
|---|---:|---:|
| 4--8 us | 0 | 2 |
| 8--16 us | 5 | 19 |
| 16--32 us | 11 | 30 |
| 32--64 us | 20 | 7 |
| 64--128 us | 11 | 0 |
| 128--256 us | 3 | 0 |

After batching, every writer wait attributed to the vote path completed
below 64 us; before batching there were 14 observations at or above 64 us.

### Passive all-contended-acquisition comparison

| Version | Mode | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Before | Read | 821 | 27.688 us | 23.973 us | 56.287 us | 105.859 us | 227.675 us | 22.732 ms |
| After | Read | 794 | 30.453 us | 26.462 us | 64.239 us | 94.402 us | 590.756 us | 24.179 ms |
| Before | Write | 906 | 23.333 us | 14.931 us | 74.896 us | 159.714 us | 467.166 us | 21.140 ms |
| After | Write | 835 | 21.988 us | 13.309 us | 69.953 us | 182.716 us | 579.851 us | 18.360 ms |
| Before | Combined | 1,727 | 25.403 us | 18.936 us | 67.406 us | 133.106 us | 467.166 us | 43.872 ms |
| After | Combined | 1,629 | 26.114 us | 19.057 us | 65.361 us | 125.355 us | 590.756 us | 42.540 ms |

Combined P95 fell 3.0%, P99 5.8%, and accumulated wait 3.0%. The overall
maximum rose because of independent outliers under `process_push_message`
and `build_crds_filters`; neither was caused by the vote path. The direct
terminal-blocker comparison above isolates the effect of this change.

### Leading terminal blockers after the change

| Waiting mode | Terminal blocker | N | Accumulated wait | Median | P99 | Max |
|---|---|---:|---:|---:|---:|---:|
| Read | `CrdsGossipPush::process_push_message` | 737 | 23.461 ms | 27.197 us | 93.865 us | 590.756 us |
| Write | `CrdsGossipPull::build_crds_filters` | 176 | 5.392 ms | 16.954 us | 377.715 us | 579.851 us |
| Write | `CrdsGossipPull::purge_find_candidates_batch` | 385 | 5.161 ms | 11.937 us | 40.939 us | 203.741 us |
| Write | `ClusterInfo::get_epoch_slots` | 29 | 2.181 ms | 68.444 us | 199.190 us | 227.784 us |
| Write | `ClusterInfo::get_votes_batch` | 58 | 1.225 ms | 20.123 us | 49.098 us | 56.519 us |
| Write | `crds_gossip::snapshot_gossip_nodes` | 19 | 1.169 ms | 69.456 us | 116.870 us | 119.275 us |
| Write | `ClusterInfo::repair_peers` | 10 | 0.722 ms | 55.331 us | 173.122 us | 178.386 us |
| Write | `ClusterInfo::time_gossip_read_lock` | 6 | 0.607 ms | 96.244 us | 185.462 us | 187.449 us |
| Write | `ClusterInfo::query_contact_infos_batch` | 29 | 0.529 ms | 18.085 us | 39.076 us | 42.188 us |

Hold-capture integrity: 417,481 guards, zero lost events, and every
diagnostic zero. Wait-capture integrity: 1,629 waits, 425,319 holder
intervals, 1,562 attributed waits, 67 unattributed waits, zero lost events,
and every diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Holds: `/tmp/crds-lock-holds-3790619-get-votes-batches-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3790619-get-votes-batches-60s.json`
- Waits: `/tmp/crds-lock-waits-3790619-get-votes-batches-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3790619-get-votes-batches-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3790619-get-votes-batches-blockers-60s.json`

## Batched `get_epoch_slots` follow-up

Build ID `3dfefaccef81ae24489039dd64a2f251b5ba5847` changes
`ClusterInfo::get_epoch_slots` from one read guard over every pending record to
64-record clone batches. The guarded helper is kept out of line as the durable
symbolic acquisition path `ClusterInfo::get_epoch_slots_batch`. Insertion
order and cursor advancement across batches are preserved.

The comparison baseline is the immediately preceding batched-votes build ID
`0135f697bcaeecae9d3be1ee1f57d713332196f7`. Both builds were measured with
independent passive 60-second hold and wait captures after RPC health reported
`ok`; no synthetic RPC traffic was issued.

### Epoch-slots read-lock holds

| Version/path | N | Mean | Median | P95 | P99 | Max | Accumulated hold |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: `ClusterInfo::get_epoch_slots` | 394 | 96.998 us | 102.101 us | 172.543 us | 206.399 us | 242.305 us | 38.217 ms |
| After: `ClusterInfo::get_epoch_slots_batch` | 2,067 | 19.089 us | 19.126 us | 29.785 us | 36.575 us | 554.105 us | 39.457 ms |

Individual-hold mean fell 80.3%, median 81.3%, P95 82.7%, and P99 82.3%.
Acquisition count increased 5.25x and accumulated hold rose 3.2%. The after
maximum is one scheduler-preempted observation in the 512--1024 us bucket;
the next-largest hold was below 128 us, and the outlier did not overlap a
measured writer wait.

### Writer waits terminally blocked by epoch-slots reads

| Version/blocker | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: `ClusterInfo::get_epoch_slots` | 29 | 75.201 us | 68.444 us | 125.223 us | 199.190 us | 227.784 us | 2.181 ms |
| After: `ClusterInfo::get_epoch_slots_batch` | 45 | 16.634 us | 15.471 us | 29.617 us | 31.719 us | 32.054 us | 0.749 ms |

Mean wait fell 77.9%, median 77.4%, P95 76.3%, P99 84.1%, maximum 85.9%,
and accumulated wait 65.7%. Although the number of holder overlaps rose with
the additional acquisitions, their bounded duration substantially reduced
writer delay.

### Epoch-slots-blocked writer wait histogram

| Wait interval | Before | After |
|---|---:|---:|
| 1--2 us | 0 | 1 |
| 2--4 us | 0 | 5 |
| 4--8 us | 2 | 1 |
| 8--16 us | 1 | 17 |
| 16--32 us | 2 | 20 |
| 32--64 us | 9 | 1 |
| 64--128 us | 14 | 0 |
| 128--256 us | 1 | 0 |

After batching, all 45 waits completed below 64 us; before batching, 15 of
29 waits were at or above 64 us.

### Passive all-contended-acquisition comparison

| Version | Mode | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Before | Read | 794 | 30.453 us | 26.462 us | 64.239 us | 94.402 us | 590.756 us | 24.179 ms |
| After | Read | 864 | 28.425 us | 24.058 us | 60.190 us | 105.993 us | 585.138 us | 24.559 ms |
| Before | Write | 835 | 21.988 us | 13.309 us | 69.953 us | 182.716 us | 579.851 us | 18.360 ms |
| After | Write | 871 | 20.604 us | 13.829 us | 57.064 us | 160.885 us | 553.184 us | 17.946 ms |
| Before | Combined | 1,629 | 26.114 us | 19.057 us | 65.361 us | 125.355 us | 590.756 us | 42.540 ms |
| After | Combined | 1,735 | 24.499 us | 18.455 us | 59.771 us | 120.932 us | 585.138 us | 42.505 ms |

Combined mean fell 6.2%, median 3.2%, P95 8.6%, P99 3.5%, maximum 1.0%,
and accumulated wait was essentially unchanged (-0.1%). Writer accumulated
wait fell 2.3%, P95 18.4%, P99 11.9%, and maximum 4.6%. Read P99 rose in the
independent sample due to `process_push_message`, not the epoch-slots path.

### Leading terminal blockers after the change

| Waiting mode | Terminal blocker | N | Accumulated wait | Median | P99 | Max |
|---|---|---:|---:|---:|---:|---:|
| Read | `CrdsGossipPush::process_push_message` | 809 | 24.008 ms | 25.385 us | 107.491 us | 585.138 us |
| Write | `CrdsGossipPull::purge_find_candidates_batch` | 406 | 6.084 ms | 12.592 us | 36.604 us | 553.184 us |
| Write | `CrdsGossipPull::build_crds_filters` | 173 | 4.722 ms | 17.024 us | 323.593 us | 455.690 us |
| Write | `crds_gossip::snapshot_gossip_nodes` | 27 | 1.767 ms | 69.366 us | 164.026 us | 179.257 us |
| Write | `ClusterInfo::get_votes_batch` | 59 | 1.414 ms | 22.601 us | 66.622 us | 72.070 us |
| Write | `ClusterInfo::get_epoch_slots_batch` | 45 | 0.749 ms | 15.471 us | 31.719 us | 32.054 us |
| Write | `ClusterInfo::query_contact_infos_batch` | 33 | 0.642 ms | 18.105 us | 39.196 us | 40.176 us |
| Write | `ClusterInfo::repair_peers` | 6 | 0.548 ms | 58.056 us | 244.469 us | 250.366 us |
| Write | `ClusterInfo::time_gossip_read_lock` | 3 | 0.483 ms | 201.939 us | 212.920 us | 213.144 us |
| Write | `CrdsGossipPull::filter_crds_values` | 7 | 0.371 ms | 68.264 us | 94.989 us | 95.011 us |

Hold-capture integrity: 408,417 guards, zero lost events, and every
diagnostic zero. Wait-capture integrity: 1,735 waits, 424,312 holder
intervals, 1,661 attributed waits, 74 unattributed waits, zero lost events,
and every diagnostic zero.

Source captures (all rows store demangled symbolic acquisition paths; JSON
reports include per-path histograms):

- Holds: `/tmp/crds-lock-holds-3797236-get-epoch-slots-batches-60s.csv`
- Hold report and histograms: `/tmp/crds-lock-holds-3797236-get-epoch-slots-batches-60s.json`
- Waits: `/tmp/crds-lock-waits-3797236-get-epoch-slots-batches-blockers-60s.csv`
- Wait-to-holder relations: `/tmp/crds-lock-waits-3797236-get-epoch-slots-batches-blockers-60s.blockers.csv`
- Wait report and histograms: `/tmp/crds-lock-waits-3797236-get-epoch-slots-batches-blockers-60s.json`

## Complete current slow-path tables after epoch-slots batching

This section reproduces the original slow-path tables for the latest build,
`3dfefaccef81ae24489039dd64a2f251b5ba5847`. The capture covers the one
physical `CrdsGossip::crds` lock. "All paths" below means all 25 symbolic
read/write acquisition callsites observed contending for that lock; it does
not include unrelated locks elsewhere in the validator.

The window was 60.000 seconds and recorded 1,735 slow-path waits and 424,312
holder intervals. There were zero lost events and every tracer diagnostic was
zero. Of the waits, 1,661 had an overlapping holder and 74 did not.

### Futex slow-path acquisition latency

| Mode | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---:|---:|---:|---:|---:|---:|---:|
| Read | 864 | 28.425 us | 24.058 us | 60.190 us | 105.993 us | 585.138 us | 24.559 ms |
| Write | 871 | 20.604 us | 13.829 us | 57.064 us | 160.885 us | 553.184 us | 17.946 ms |
| Combined | 1,735 | 24.499 us | 18.455 us | 59.771 us | 120.932 us | 585.138 us | 42.505 ms |

### Original master versus current

Master percentiles were retained as fine-histogram intervals. Approximate
changes use interval midpoints.

| Mode | Metric | Original master | Current | Change |
|---|---|---:|---:|---:|
| Read | Slow-path acquisitions | 294 | 864 | +193.9% |
| Read | Mean | 165.616 us | 28.425 us | -82.8% |
| Read | Median | 140--150 us | 24.058 us | ~-83.4% |
| Read | P95 | 470--480 us | 60.190 us | ~-87.3% |
| Read | P99 | 1.6--1.7 ms | 105.993 us | ~-93.6% |
| Read | Max | 1.859 ms | 585.138 us | -68.5% |
| Read | Accumulated wait | 48.691 ms | 24.559 ms | -49.6% |
| Write | Slow-path acquisitions | 550 | 871 | +58.4% |
| Write | Mean | 709.602 us | 20.604 us | -97.1% |
| Write | Median | 280--290 us | 13.829 us | ~-95.1% |
| Write | P95 | 1.9--2.0 ms | 57.064 us | ~-97.1% |
| Write | P99 | 2.5--2.6 ms | 160.885 us | ~-93.7% |
| Write | Max | 3.446 ms | 553.184 us | -84.0% |
| Write | Accumulated wait | 390.281 ms | 17.946 ms | -95.4% |
| Combined | Slow-path acquisitions | 844 | 1,735 | +105.6% |
| Combined | Mean | 520.109 us | 24.499 us | -95.3% |
| Combined | Max | 3.446 ms | 585.138 us | -83.0% |
| Combined | Accumulated wait | 438.972 ms | 42.505 ms | -90.3% |

### Wait-interval histogram across all paths

Current counts are exact. Master counts were reconstructed from the original
fine-grained histogram and assigned to these common intervals by bucket
midpoint, as described in the earlier baseline section.

| Wait interval | Master read | Master write | Master total | Current read | Current write | Current total |
|---|---:|---:|---:|---:|---:|---:|
| [0, 0.25) us | 28 | 33 | 61 | 0 | 0 | 0 |
| [0.25, 0.5) us | 4 | 7 | 11 | 0 | 0 | 0 |
| [0.5, 1) us | 36 | 30 | 66 | 3 | 7 | 10 |
| [1, 2) us | 17 | 37 | 54 | 9 | 39 | 48 |
| [2, 4) us | 13 | 25 | 38 | 12 | 68 | 80 |
| [4, 8) us | 12 | 20 | 32 | 10 | 77 | 87 |
| [8, 16) us | 12 | 19 | 31 | 178 | 321 | 499 |
| [16, 32) us | 13 | 33 | 46 | 402 | 280 | 682 |
| [32, 64) us | 127 | 71 | 198 | 216 | 39 | 255 |
| [64, 128) us | 16 | 20 | 36 | 30 | 28 | 58 |
| [128, 256) us | 7 | 33 | 40 | 3 | 8 | 11 |
| [256, 512) us | 1 | 10 | 11 | 0 | 3 | 3 |
| [512, 1024) us | 1 | 1 | 2 | 1 | 1 | 2 |
| >= 1024 us | 7 | 211 | 218 | 0 | 0 | 0 |
| **Total** | **294** | **550** | **844** | **864** | **871** | **1,735** |

The current capture had no wait at or above 1 ms. Original master had 218;
its longest wait was 3.446 ms versus 585.138 us currently.

### Acquisition-path ranking by median

The offset is retained because two functions contain more than one distinct
CRDS acquisition site. Small-`N` rows should not be interpreted as stable
distribution estimates.

| Rank | Mode | Symbolic acquisition path | N | Mean | Median | P95 | P99 | Max | Total wait |
|---:|---|---|---:|---:|---:|---:|---:|---:|---:|
| 1 | Write | `CrdsGossipPull::purge_remove_batch+0x39` | 2 | 59.026 us | 59.026 us | 91.080 us | 93.929 us | 94.641 us | 0.118 ms |
| 2 | Read | `CrdsGossipPull::build_crds_filters+0xae` | 5 | 26.643 us | 28.640 us | 39.262 us | 39.536 us | 39.604 us | 0.133 ms |
| 3 | Read | `CrdsGossipPull::build_crds_filters+0x307` | 167 | 32.366 us | 27.287 us | 73.458 us | 116.009 us | 157.828 us | 5.405 ms |
| 4 | Read | `CrdsGossipPull::purge_find_candidates_batch+0x55` | 439 | 30.606 us | 26.337 us | 58.572 us | 106.674 us | 585.138 us | 13.436 ms |
| 5 | Read | `ClusterInfo::get_epoch_slots_batch+0x54` | 46 | 26.533 us | 26.032 us | 52.377 us | 71.644 us | 75.073 us | 1.221 ms |
| 6 | Read | `CrdsGossipPull::purge_pubkey_snapshot+0x3f` | 6 | 26.613 us | 24.023 us | 41.715 us | 42.750 us | 43.009 us | 0.160 ms |
| 7 | Read | `ClusterInfo::get_votes_batch+0x54` | 74 | 28.048 us | 23.672 us | 73.431 us | 102.810 us | 120.997 us | 2.076 ms |
| 8 | Write | `CrdsGossip::purge+0x48` | 31 | 23.577 us | 17.705 us | 68.394 us | 87.065 us | 95.011 us | 0.731 ms |
| 9 | Read | `ClusterInfo::push_lowest_slot+0xd7` | 18 | 18.760 us | 17.470 us | 35.661 us | 56.824 us | 62.115 us | 0.338 ms |
| 10 | Read | Votor `ClusterInfo::query_contact_infos_batch+0x59` | 31 | 24.322 us | 17.394 us | 50.655 us | 55.614 us | 57.359 us | 0.754 ms |
| 11 | Read | `CrdsGossipPush::new_push_messages+0x17d` | 11 | 17.414 us | 17.043 us | 35.669 us | 41.244 us | 42.638 us | 0.192 ms |
| 12 | Read | `ClusterInfo::process_packets+0x26d8` | 4 | 17.397 us | 16.813 us | 22.318 us | 22.993 us | 23.162 us | 0.070 ms |
| 13 | Read | `ClusterInfo::new_push_requests+0x251` | 6 | 18.035 us | 16.368 us | 30.077 us | 32.780 us | 33.456 us | 0.108 ms |
| 14 | Read | `ClusterInfo::all_peers_batch+0x54` | 2 | 13.894 us | 13.894 us | 19.414 us | 19.904 us | 20.027 us | 0.028 ms |
| 15 | Write | `CrdsGossipPush::process_push_message+0x235` | 824 | 20.518 us | 13.814 us | 49.118 us | 173.221 us | 553.184 us | 16.906 ms |
| 16 | Read | `ClusterInfo::trim_crds_table+0x53` | 20 | 14.196 us | 13.514 us | 30.335 us | 44.393 us | 47.907 us | 0.284 ms |
| 17 | Read | `ClusterInfo::save_contact_info_batch+0x5d` | 1 | 12.767 us | 12.767 us | 12.767 us | 12.767 us | 12.767 us | 0.013 ms |
| 18 | Read | `ClusterInfo::process_packets+0x165` | 8 | 10.603 us | 12.693 us | 16.375 us | 16.725 us | 16.813 us | 0.085 ms |
| 19 | Read | `ClusterInfo::get_node_version+0x51` | 9 | 11.309 us | 10.995 us | 16.713 us | 18.219 us | 18.596 us | 0.102 ms |
| 20 | Read | `ClusterInfo::get_duplicate_shreds+0x54` | 11 | 11.256 us | 10.675 us | 21.625 us | 23.463 us | 23.923 us | 0.124 ms |
| 21 | Write | `CrdsGossipPull::process_pull_responses+0x8a` | 14 | 13.610 us | 9.969 us | 29.319 us | 29.673 us | 29.761 us | 0.191 ms |
| 22 | Read | `cluster_info_metrics::submit_gossip_stats+0x52` | 1 | 9.153 us | 9.153 us | 9.153 us | 9.153 us | 9.153 us | 0.009 ms |
| 23 | Read | `ClusterInfo::repair_peers+0xcf` | 2 | 5.918 us | 5.918 us | 10.568 us | 10.982 us | 11.085 us | 0.012 ms |
| 24 | Read | `crds_gossip::snapshot_gossip_nodes+0x3f` | 2 | 5.223 us | 5.223 us | 7.696 us | 7.916 us | 7.971 us | 0.010 ms |
| 25 | Read | `ClusterInfo::generate_new_gossip_requests+0x6a0` | 1 | 1.853 us | 1.853 us | 1.853 us | 1.853 us | 1.853 us | 0.002 ms |

### Acquisition-path ranking by maximum

| Rank | Mode | Symbolic acquisition path | N | Median | P99 | Max |
|---:|---|---|---:|---:|---:|---:|
| 1 | Read | `CrdsGossipPull::purge_find_candidates_batch+0x55` | 439 | 26.337 us | 106.674 us | 585.138 us |
| 2 | Write | `CrdsGossipPush::process_push_message+0x235` | 824 | 13.814 us | 173.221 us | 553.184 us |
| 3 | Read | `CrdsGossipPull::build_crds_filters+0x307` | 167 | 27.287 us | 116.009 us | 157.828 us |
| 4 | Read | `ClusterInfo::get_votes_batch+0x54` | 74 | 23.672 us | 102.810 us | 120.997 us |
| 5 | Write | `CrdsGossip::purge+0x48` | 31 | 17.705 us | 87.065 us | 95.011 us |
| 6 | Write | `CrdsGossipPull::purge_remove_batch+0x39` | 2 | 59.026 us | 93.929 us | 94.641 us |
| 7 | Read | `ClusterInfo::get_epoch_slots_batch+0x54` | 46 | 26.032 us | 71.644 us | 75.073 us |
| 8 | Read | `ClusterInfo::push_lowest_slot+0xd7` | 18 | 17.470 us | 56.824 us | 62.115 us |
| 9 | Read | Votor `ClusterInfo::query_contact_infos_batch+0x59` | 31 | 17.394 us | 55.614 us | 57.359 us |
| 10 | Read | `ClusterInfo::trim_crds_table+0x53` | 20 | 13.514 us | 44.393 us | 47.907 us |
| 11 | Read | `CrdsGossipPull::purge_pubkey_snapshot+0x3f` | 6 | 24.023 us | 42.750 us | 43.009 us |
| 12 | Read | `CrdsGossipPush::new_push_messages+0x17d` | 11 | 17.043 us | 41.244 us | 42.638 us |
| 13 | Read | `CrdsGossipPull::build_crds_filters+0xae` | 5 | 28.640 us | 39.536 us | 39.604 us |
| 14 | Read | `ClusterInfo::new_push_requests+0x251` | 6 | 16.368 us | 32.780 us | 33.456 us |
| 15 | Write | `CrdsGossipPull::process_pull_responses+0x8a` | 14 | 9.969 us | 29.673 us | 29.761 us |
| 16 | Read | `ClusterInfo::get_duplicate_shreds+0x54` | 11 | 10.675 us | 23.463 us | 23.923 us |
| 17 | Read | `ClusterInfo::process_packets+0x26d8` | 4 | 16.813 us | 22.993 us | 23.162 us |
| 18 | Read | `ClusterInfo::all_peers_batch+0x54` | 2 | 13.894 us | 19.904 us | 20.027 us |
| 19 | Read | `ClusterInfo::get_node_version+0x51` | 9 | 10.995 us | 18.219 us | 18.596 us |
| 20 | Read | `ClusterInfo::process_packets+0x165` | 8 | 12.693 us | 16.725 us | 16.813 us |
| 21 | Read | `ClusterInfo::save_contact_info_batch+0x5d` | 1 | 12.767 us | 12.767 us | 12.767 us |
| 22 | Read | `ClusterInfo::repair_peers+0xcf` | 2 | 5.918 us | 10.982 us | 11.085 us |
| 23 | Read | `cluster_info_metrics::submit_gossip_stats+0x52` | 1 | 9.153 us | 9.153 us | 9.153 us |
| 24 | Read | `crds_gossip::snapshot_gossip_nodes+0x3f` | 2 | 5.223 us | 7.916 us | 7.971 us |
| 25 | Read | `ClusterInfo::generate_new_gossip_requests+0x6a0` | 1 | 1.853 us | 1.853 us | 1.853 us |

### Sparse histogram for every acquisition path

Intervals are microseconds and omitted buckets have count zero. These are
waiter acquisition paths, not terminal blocker paths.

| Mode | Symbolic acquisition path | Nonzero histogram buckets (`interval: count`) |
|---|---|---|
| Read | `ClusterInfo::all_peers_batch+0x54` | 4--8: 1; 16--32: 1 |
| Read | `ClusterInfo::generate_new_gossip_requests+0x6a0` | 1--2: 1 |
| Read | `ClusterInfo::get_duplicate_shreds+0x54` | 1--2: 1; 2--4: 2; 4--8: 1; 8--16: 4; 16--32: 3 |
| Read | `ClusterInfo::get_epoch_slots_batch+0x54` | 1--2: 1; 8--16: 10; 16--32: 24; 32--64: 9; 64--128: 2 |
| Read | `ClusterInfo::get_node_version+0x51` | 4--8: 2; 8--16: 6; 16--32: 1 |
| Read | `ClusterInfo::get_votes_batch+0x54` | 0.5--1: 1; 1--2: 2; 8--16: 15; 16--32: 38; 32--64: 13; 64--128: 5 |
| Read | `ClusterInfo::new_push_requests+0x251` | 8--16: 3; 16--32: 2; 32--64: 1 |
| Read | `ClusterInfo::process_packets+0x165` | 2--4: 2; 4--8: 1; 8--16: 4; 16--32: 1 |
| Read | `ClusterInfo::process_packets+0x26d8` | 8--16: 1; 16--32: 3 |
| Read | `ClusterInfo::push_lowest_slot+0xd7` | 2--4: 3; 4--8: 1; 8--16: 4; 16--32: 9; 32--64: 1 |
| Read | `ClusterInfo::repair_peers+0xcf` | 0.5--1: 1; 8--16: 1 |
| Read | `ClusterInfo::save_contact_info_batch+0x5d` | 8--16: 1 |
| Read | `ClusterInfo::trim_crds_table+0x53` | 1--2: 3; 2--4: 2; 4--8: 1; 8--16: 7; 16--32: 6; 32--64: 1 |
| Read | `CrdsGossipPull::build_crds_filters+0x307` | 0.5--1: 1; 8--16: 26; 16--32: 76; 32--64: 51; 64--128: 12; 128--256: 1 |
| Read | `CrdsGossipPull::build_crds_filters+0xae` | 8--16: 1; 16--32: 2; 32--64: 2 |
| Read | `CrdsGossipPull::purge_find_candidates_batch+0x55` | 4--8: 2; 8--16: 77; 16--32: 218; 32--64: 128; 64--128: 11; 128--256: 2; 512--1024: 1 |
| Read | `CrdsGossipPull::purge_pubkey_snapshot+0x3f` | 8--16: 2; 16--32: 2; 32--64: 2 |
| Read | `CrdsGossipPush::new_push_messages+0x17d` | 1--2: 1; 2--4: 1; 8--16: 3; 16--32: 5; 32--64: 1 |
| Read | Votor `ClusterInfo::query_contact_infos_batch+0x59` | 2--4: 1; 8--16: 12; 16--32: 11; 32--64: 7 |
| Read | `crds_gossip::snapshot_gossip_nodes+0x3f` | 2--4: 1; 4--8: 1 |
| Read | `cluster_info_metrics::submit_gossip_stats+0x52` | 8--16: 1 |
| Write | `CrdsGossip::purge+0x48` | 1--2: 2; 2--4: 5; 4--8: 1; 8--16: 5; 16--32: 10; 32--64: 5; 64--128: 3 |
| Write | `CrdsGossipPull::process_pull_responses+0x8a` | 0.5--1: 1; 1--2: 1; 4--8: 2; 8--16: 5; 16--32: 5 |
| Write | `CrdsGossipPull::purge_remove_batch+0x39` | 16--32: 1; 64--128: 1 |
| Write | `CrdsGossipPush::process_push_message+0x235` | 0.5--1: 6; 1--2: 36; 2--4: 63; 4--8: 74; 8--16: 311; 16--32: 264; 32--64: 34; 64--128: 24; 128--256: 8; 256--512: 3; 512--1024: 1 |

## Critical-section optimization sequence (2026-09-08)

Four cumulative changes were deployed and measured independently for 60
seconds after RPC health returned `ok`. Each stage has a distinct build ID and
both the hold and wait captures retain demangled symbolic acquisition paths.
Every capture reported zero lost events and all tracer diagnostics were zero.

| Stage | Build ID | Change |
|---|---|---|
| Baseline | `3dfefaccef81ae24489039dd64a2f251b5ba5847` | Batched epoch-slots build documented above |
| Deferred telemetry | `b54f31f9af30bcb5c22b8e02c7fddb336045193d` | Emit sampled push ingress/egress telemetry outside CRDS guards; use `Mutex::get_mut` for insert statistics |
| Push batches | `4b9f5e2b21974f27ce5dcab455e626a12e907409` | Limit each continuous `process_push_message` write section to 32 values |
| Pull batches | `ecbb7a434d171c13a290f7f1308e190b6bc647b4` | Validate and charge pull requests unlocked; snapshot stable labels and re-fetch values in batches of 64 |
| Gossip index | `40eb1cfccaeed8b474a86b550167f0b4f36d39bb` | Snapshot gossip-node fields from the compact peer index |

### Aggregate futex slow-path results after every stage

Changes in the final column are against the immediately preceding stage.
Accumulated wait is affected by the number of slow-path acquisitions as well
as their latency.

| Stage | N | Mean | Median | P95 | P99 | Max | Accumulated wait | Change vs previous (mean / P95 / P99 / max) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Baseline | 1,735 | 24.499 us | 18.455 us | 59.771 us | 120.932 us | 585.138 us | 42.505 ms | -- |
| Deferred telemetry | 1,962 | 24.032 us | 18.616 us | 56.660 us | 125.708 us | 544.232 us | 47.150 ms | -1.9% / -5.2% / +3.9% / -7.0% |
| Push batches | 2,033 | 22.936 us | 18.515 us | 51.018 us | 106.888 us | 506.811 us | 46.630 ms | -4.6% / -10.0% / -15.0% / -6.9% |
| Pull batches | 2,044 | 22.233 us | 18.656 us | 48.745 us | 84.412 us | 381.147 us | 45.445 ms | -3.1% / -4.5% / -21.0% / -24.8% |
| Gossip index | 1,871 | 20.796 us | 16.994 us | 47.045 us | 78.897 us | 500.331 us | 38.910 ms | -6.5% / -3.5% / -6.5% / +31.3% |

The final stage versus this sequence's baseline reduced combined mean 15.1%,
median 7.9%, P95 21.3%, P99 34.8%, maximum 14.5%, and accumulated wait 8.5%.
Slow-path acquisition count rose 7.8%. The final-stage maximum increase versus
the preceding stage came from unrelated isolated `process_push_message` and
purge-candidate outliers; the gossip-node path itself improved substantially.

### Final versus baseline by lock mode

| Mode | Version | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Read | Baseline | 864 | 28.425 us | 24.058 us | 60.190 us | 105.993 us | 585.138 us | 24.559 ms |
| Read | Final | 938 | 24.498 us | 20.879 us | 51.134 us | 70.577 us | 500.331 us | 22.979 ms |
| Write | Baseline | 871 | 20.604 us | 13.829 us | 57.064 us | 160.885 us | 553.184 us | 17.946 ms |
| Write | Final | 933 | 17.074 us | 13.829 us | 33.190 us | 97.475 us | 447.168 us | 15.930 ms |
| Combined | Baseline | 1,735 | 24.499 us | 18.455 us | 59.771 us | 120.932 us | 585.138 us | 42.505 ms |
| Combined | Final | 1,871 | 20.796 us | 16.994 us | 47.045 us | 78.897 us | 500.331 us | 38.910 ms |

### Candidate-specific results

#### 1. Deferred sampled telemetry

This was neutral at one-minute resolution. The repeatable
`CrdsGossipPush::process_push_message` write-hold P99 fell from 82.747 to
76.395 us (7.7%), but its maximum rose from 764.657 to 813.695 us. The
`new_push_messages` P99 fell from 38.543 to 35.449 us (8.0%), while mean and
median were essentially unchanged. This is expected for telemetry sampled
approximately once per minute; isolated maxima remain sensitive to scheduler
preemption and whether a sample fires in the capture window.

#### 2. Bounded push insertion

| Metric | Before | After | Change |
|---|---:|---:|---:|
| `process_push_message` hold mean | 17.753 us | 15.409 us | -13.2% |
| Hold median | 13.479 us | 11.576 us | -14.1% |
| Hold P99 | 76.395 us | 63.548 us | -16.8% |
| Hold max | 813.695 us | 640.685 us | -21.3% |
| Reader wait P99 when terminally blocked by this path | 111.422 us | 80.634 us | -27.6% |
| Reader wait max when terminally blocked by this path | 544.232 us | 414.492 us | -23.8% |

#### 3. Batched pull filtering

The old path held one read guard with median/P99/max of
92.708/213.525/2528.834 us. It was split into these symbolic acquisition
sites:

| Read section | N | Mean | Median | P95 | P99 | Max | Accumulated hold |
|---|---:|---:|---:|---:|---:|---:|---:|
| Scan-count accounting (`filter_crds_values+0x2b9`) | 5,427 | 0.521 us | 0.471 us | 0.771 us | 0.941 us | 27.929 us | 2.829 ms |
| Stable-label snapshot (`filter_crds_values+0x4d9`) | 5,427 | 24.968 us | 24.394 us | 37.972 us | 46.793 us | 106.027 us | 135.500 ms |
| 64-value lookup batch (`filter_crds_values+0x6b3`) | 38,035 | 40.567 us | 40.967 us | 56.628 us | 67.753 us | 253.851 us | 1542.971 ms |

For writers terminally blocked by any `filter_crds_values` section, mean,
median, P95, P99, and max fell from 72.201/72.320/121.993/127.117/128.398 us
to 28.939/27.177/45.474/46.996/47.376 us. The maximum fell 63.1%.

The cost is important: filter acquisition count increased from 5,301 to
48,889 and accumulated filter read occupancy rose from 508.318 to 1681.300
ms/minute (+230.8%), because candidates are cloned before Bloom and retention
filtering. Total wait on this blocker rose slightly from 0.505 to 0.550 ms as
the number of overlaps increased from 7 to 19. This change optimizes continuous
hold and waiter tail latency, not CPU or total lock occupancy.

#### 4. Compact gossip-node index

| Metric | Before | After | Change |
|---|---:|---:|---:|
| `snapshot_gossip_nodes` hold mean | 152.941 us | 52.442 us | -65.7% |
| Hold median | 142.307 us | 45.388 us | -68.1% |
| Hold P95 | 210.746 us | 101.156 us | -52.0% |
| Hold P99 | 252.733 us | 118.984 us | -52.9% |
| Hold max | 373.296 us | 140.034 us | -62.5% |
| Accumulated hold | 19.576 ms | 6.713 ms | -65.7% |
| Terminally blocked writer max | 141.605 us | 64.429 us | -54.5% |

The terminal-blocker sample was sparse (13 waits before and 3 after), so the
holder distribution is the stronger evidence for this candidate.

### Captures

- Deferred telemetry holds/waits:
  `/tmp/crds-lock-holds-3804744-deferred-telemetry-60s.json`,
  `/tmp/crds-lock-waits-3804744-deferred-telemetry-blockers-60s.json`
- Push batching holds/waits:
  `/tmp/crds-lock-holds-3809604-push-insert-batches-60s.json`,
  `/tmp/crds-lock-waits-3809604-push-insert-batches-blockers-60s.json`
- Pull filtering holds/waits:
  `/tmp/crds-lock-holds-3816797-pull-filter-batches-60s.json`,
  `/tmp/crds-lock-waits-3816797-pull-filter-batches-blockers-60s.json`
- Compact gossip index holds/waits:
  `/tmp/crds-lock-holds-3822527-gossip-peer-index-60s.json`,
  `/tmp/crds-lock-waits-3822527-gossip-peer-index-blockers-60s.json`

Each wait prefix also has `.csv` and `.blockers.csv` files; each hold prefix
has a `.csv` file. The CSV rows contain the symbolic acquisition or blocker
path in addition to the build-specific instruction address.

## Easy critical-section follow-ups (2026-09-08)

Five further cumulative changes were built, deployed, and measured for 60
seconds apiece. Every retained hold and wait capture reported zero lost events
and zero pairing diagnostics. The final binary has build ID
`c12a81f9d77e97c5955611720ed2446f816d4c58`.

| Stage | Build ID | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Prior compact gossip index | `40eb1cfccaeed8b474a86b550167f0b4f36d39bb` | 1,871 | 20.796 us | 16.994 us | 47.045 us | 78.897 us | 500.331 us | 38.910 ms |
| Pull metadata prefilter | `f4d2543c9f4e18f5d7919027d9fc5183d18fc5b4` | 2,329 | 22.928 us | 19.858 us | 49.463 us | 75.926 us | 245.939 us | 53.399 ms |
| Pull-response batches | `c18cb27470f85a038e0002e0a0fbeed0d12914e7` | 1,954 | 21.175 us | 18.145 us | 46.257 us | 83.166 us | 328.975 us | 41.376 ms |
| Direct pubkey membership | `11064d4482b0ead50d4cb25828db2e2e94f664e3` | 2,038 | 21.409 us | 18.546 us | 44.473 us | 76.018 us | 552.613 us | 43.631 ms |
| 16-value push batches | `13f751dfa694474ca9beb2837096fead20e72087` | 2,066 | 19.646 us | 17.549 us | 37.024 us | 62.228 us | 206.345 us | 40.590 ms |
| 32-value purge batches | `c12a81f9d77e97c5955611720ed2446f816d4c58` | 2,637 | 19.643 us | 16.172 us | 40.618 us | 72.228 us | 320.703 us | 51.798 ms |

The per-stage event count and isolated maximum are workload-sensitive. The
repeatable improvement in the last two stages is in median and tail
percentiles; accumulated wait rose in the final sample because it contained
27.6% more slow-path acquisitions than the preceding minute.

### Final acquisition-latency histogram versus original master

These are futex/parking-lot slow-path acquisitions only. Current counts are
exact. Master counts were reconstructed from the retained fine-grained
`hist(delta_ns, 4)` output and assigned to the common intervals by bucket
midpoint. The master and current probes instrument different lock
implementations, so compare latency distributions; raw event-count changes
are not an acquisition-rate benchmark.

| Wait interval | Master read | Master write | Master total | Current read | Current write | Current total |
|---|---:|---:|---:|---:|---:|---:|
| [0, 0.25) us | 28 | 33 | 61 | 0 | 0 | 0 |
| [0.25, 0.5) us | 4 | 7 | 11 | 1 | 0 | 1 |
| [0.5, 1) us | 36 | 30 | 66 | 1 | 9 | 10 |
| [1, 2) us | 17 | 37 | 54 | 7 | 44 | 51 |
| [2, 4) us | 13 | 25 | 38 | 10 | 72 | 82 |
| [4, 8) us | 12 | 20 | 32 | 21 | 158 | 179 |
| [8, 16) us | 12 | 19 | 31 | 373 | 606 | 979 |
| [16, 32) us | 13 | 33 | 46 | 655 | 359 | 1,014 |
| [32, 64) us | 127 | 71 | 198 | 224 | 63 | 287 |
| [64, 128) us | 16 | 20 | 36 | 13 | 14 | 27 |
| [128, 256) us | 7 | 33 | 40 | 1 | 2 | 3 |
| [256, 512) us | 1 | 10 | 11 | 1 | 3 | 4 |
| [512, 1024) us | 1 | 1 | 2 | 0 | 0 | 0 |
| >= 1024 us | 7 | 211 | 218 | 0 | 0 | 0 |
| **Total** | **294** | **550** | **844** | **1,307** | **1,330** | **2,637** |

Master had 220 of 844 waits (26.1%) at or above 512 us; current had none.
Master had 271 waits (32.1%) at or above 128 us; current had 7 (0.27%).
Current's maximum was 320.703 us versus 3.446 ms on master, a 90.7%
reduction. Current combined mean was 19.643 us versus 520.109 us, a 96.2%
reduction, and total accumulated wait was 51.798 ms versus 438.972 ms, an
88.2% reduction.

Final source captures (all rows retain demangled symbolic paths):

- Holds: `/tmp/crds-lock-holds-3856289-purge-batch-32-60s.csv`
- Hold report and histograms:
  `/tmp/crds-lock-holds-3856289-purge-batch-32-60s.json`
- Waits: `/tmp/crds-lock-waits-3856289-purge-batch-32-blockers-60s.csv`
- Terminal blockers:
  `/tmp/crds-lock-waits-3856289-purge-batch-32-blockers-60s.blockers.csv`
- Wait report and histograms:
  `/tmp/crds-lock-waits-3856289-purge-batch-32-blockers-60s.json`

## Batched pull-response filtering (2026-09-08)

`filter_pull_responses` now classifies at most 32 responses per CRDS read
guard. Each guard only records a preallocated disposition; moving values into
output vectors, dropping rejected values, and updating statistics happen after
the guard is released. The deployed build ID is
`cd1fa3ea4bba236ba7d8572a3badae2d45996338`.

### Target read-hold distribution

| Version / symbolic acquisition path | N | Mean | Median | P95 | P99 | Max | Accumulated hold |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before: `CrdsGossipPull::filter_pull_responses+0xd2` | 1,738 | 69.279 us | 7.726 us | 336.525 us | 840.995 us | 3.839 ms | 120.408 ms |
| After: `CrdsGossipPull::filter_pull_responses_batch+0x9e` | 763 | 1.204 us | 1.081 us | 2.312 us | 3.236 us | 4.736 us | 0.919 ms |

The target P95, P99, maximum, and accumulated hold fell 99.3%, 99.6%, 99.9%,
and 99.2%, respectively. The number of calls differed substantially between
the adjacent workload samples, but the per-acquisition distribution shows the
intended continuous-hold bound directly.

### Aggregate futex slow-path acquisition latency

| Version | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before | 2,637 | 19.643 us | 16.172 us | 40.618 us | 72.228 us | 320.703 us | 51.798 ms |
| After | 2,169 | 18.641 us | 15.551 us | 36.921 us | 59.653 us | 421.963 us | 40.433 ms |
| Change | -17.7% | -5.1% | -3.8% | -9.1% | -17.4% | +31.6% | -21.9% |

The aggregate maximum came from `build_crds_filters`, not the changed path.
No wait in either adjacent capture was terminally blocked by
`filter_pull_responses`, so aggregate changes should be treated as directional
rather than causal. The target holder distribution is the stronger evidence
for this change.

Source captures (all rows retain demangled symbolic paths):

- Holds: `/tmp/crds-lock-holds-3872356-pull-response-filter-batches-60s.csv`
- Hold report and histograms:
  `/tmp/crds-lock-holds-3872356-pull-response-filter-batches-60s.json`
- Waits:
  `/tmp/crds-lock-waits-3872356-pull-response-filter-batches-blockers-60s.csv`
- Terminal blockers:
  `/tmp/crds-lock-waits-3872356-pull-response-filter-batches-blockers-60s.blockers.csv`
- Wait report and histograms:
  `/tmp/crds-lock-waits-3872356-pull-response-filter-batches-blockers-60s.json`

### Post-batching acquisition histogram versus original master

Current counts are exact. As in the earlier comparisons, master counts were
reconstructed from the retained fine-grained histogram and assigned to these
common intervals by bucket midpoint.

| Wait interval | Master read | Master write | Master total | Current read | Current write | Current total |
|---|---:|---:|---:|---:|---:|---:|
| [0, 0.25) us | 28 | 33 | 61 | 0 | 0 | 0 |
| [0.25, 0.5) us | 4 | 7 | 11 | 0 | 0 | 0 |
| [0.5, 1) us | 36 | 30 | 66 | 2 | 6 | 8 |
| [1, 2) us | 17 | 37 | 54 | 7 | 54 | 61 |
| [2, 4) us | 13 | 25 | 38 | 3 | 98 | 101 |
| [4, 8) us | 12 | 20 | 32 | 15 | 134 | 149 |
| [8, 16) us | 12 | 19 | 31 | 306 | 494 | 800 |
| [16, 32) us | 13 | 33 | 46 | 601 | 249 | 850 |
| [32, 64) us | 127 | 71 | 198 | 140 | 39 | 179 |
| [64, 128) us | 16 | 20 | 36 | 3 | 9 | 12 |
| [128, 256) us | 7 | 33 | 40 | 2 | 3 | 5 |
| [256, 512) us | 1 | 10 | 11 | 1 | 3 | 4 |
| [512, 1024) us | 1 | 1 | 2 | 0 | 0 | 0 |
| >= 1024 us | 7 | 211 | 218 | 0 | 0 | 0 |
| **Total** | **294** | **550** | **844** | **1,080** | **1,089** | **2,169** |

Current recorded no wait at or above 512 us, versus 220 (26.1%) on master.
Only 9 current waits (0.41%) were at or above 128 us, versus 271 (32.1%) on
master. Current combined mean, maximum, and accumulated wait were 18.641 us,
421.963 us, and 40.433 ms, reductions of 96.4%, 87.8%, and 90.8% versus
master.

## Code-review corrections (2026-09-08)

This build fixes the review findings without reverting the explicitly selected
`parking_lot` lock:

- Active pull-response insertion and owner timestamp refresh now happen under
  the same write guard, closing the insertion/purge race.
- Batched vote and epoch-slot readers capture an exclusive ending ordinal on
  their first batch, so concurrent inserts are deferred to the next call and
  cannot extend the current snapshot indefinitely.
- The wrapper restores std-compatible write-panic poisoning and exposes
  `is_poisoned` and `clear_poison`.
- The original public `ClusterInfo::repair_peers() -> Vec<ContactInfo>` API is
  restored. Validator internals use the additive compact
  `repair_peer_endpoints()` API.
- Uprobe markers are compiled only with the `crds-lock-instrumentation` feature.
  The deployed profiling binary enables that feature.

The deployed build ID was `545a0dda9dca011a177aa68e32f605e50cf6c27d`,
PID 3888292. Both captures ran for 60 seconds with zero lost, nested, or
unmatched events. The validator reported healthy after the captures.

The parking_lot acquisition probe now has the precise name
`parking_lot_try_lock_fallback`: it measures from a failed optimistic try-lock
through acquisition, including userspace spinning and any subsequent kernel
parking. It is not restricted to kernel futex waits. Accordingly, comparisons
with the original std futex master baseline are directional rather than
identical-population comparisons.

### Aggregate hold time versus the preceding build

| Mode / version | N | Mean | Median | P95 | P99 | Max | Accumulated hold |
|---|---:|---:|---:|---:|---:|---:|---:|
| Read before | 396,159 | 6.209 us | 3.746 us | 21.501 us | 37.812 us | 2.374 ms | 2,459.620 ms |
| Read corrected | 399,650 | 6.762 us | 4.116 us | 22.812 us | 39.655 us | 1.769 ms | 2,702.272 ms |
| Write before | 76,299 | 12.554 us | 11.325 us | 27.328 us | 36.300 us | 721.326 us | 957.835 ms |
| Write corrected | 84,020 | 13.263 us | 12.187 us | 28.439 us | 38.499 us | 779.978 us | 1,114.370 ms |

The adjacent sample had 10.1% more write acquisitions. Read maximum hold fell
25.5%; write maximum hold increased 8.1%. Per-acquisition central values were
4-10% higher and should be treated as workload variation because the review
corrections were primarily semantic rather than contention optimizations.

### Aggregate contended acquisition latency versus the preceding build

| Version | N | Mean | Median | P95 | P99 | Max | Accumulated wait |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before corrections | 2,169 | 18.641 us | 15.551 us | 36.921 us | 59.653 us | 421.963 us | 40.433 ms |
| Corrected | 2,513 | 20.024 us | 16.583 us | 39.098 us | 65.464 us | 439.727 us | 50.322 ms |
| Change | +15.9% | +7.4% | +6.6% | +5.9% | +9.7% | +4.2% | +24.5% |

The maximum remained below 440 us and no acquisition reached 512 us. Against
the original master baseline, the corrected combined mean, maximum, and
accumulated wait were lower by 96.2%, 87.2%, and 88.5%, respectively.

### Corrected `process_pull_responses` path

The preceding build used separate insertion and owner-refresh guards. The
corrected build performs both operations atomically in one bounded guard.

| Hold path | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before: insertion guard | 370 | 3.397 us | 2.679 us | 7.281 us | 9.972 us | 19.858 us |
| Before: owner-refresh guard | 300 | 1.013 us | 0.967 us | 1.955 us | 2.715 us | 2.954 us |
| Corrected: atomic guard | 286 | 3.910 us | 3.289 us | 8.321 us | 11.663 us | 16.763 us |

| Wait path | N | Mean | Median | P95 | P99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| Before: insertion guard | 13 | 10.995 us | 8.532 us | 21.954 us | 33.663 us | 36.590 us |
| Before: owner-refresh guard | 1 | 11.977 us | 11.977 us | 11.977 us | 11.977 us | 11.977 us |
| Corrected: atomic guard | 22 | 11.453 us | 12.668 us | 18.958 us | 22.636 us | 23.582 us |

### Current wait-path ranking by maximum

Paths with at least five samples are shown. The stored JSON contains every
symbolic path, including low-sample paths.

| Rank | Mode | Symbolic acquisition path | N | Median | Max |
|---:|---|---|---:|---:|---:|
| 1 | Read | `CrdsGossipPull::purge_find_candidates_batch` | 766 | 22.181 us | 439.727 us |
| 2 | Write | `CrdsGossipPush::process_push_message` | 1,214 | 12.442 us | 439.618 us |
| 3 | Read | `CrdsGossipPull::build_crds_filters` | 193 | 22.781 us | 261.371 us |
| 4 | Read | `ClusterInfo::get_votes_batch` | 93 | 19.567 us | 226.023 us |
| 5 | Write | `CrdsGossip::purge` | 24 | 9.058 us | 91.386 us |
| 6 | Read | `CrdsGossipPull::purge_pubkey_snapshot` | 5 | 17.664 us | 51.341 us |
| 7 | Read | `ClusterInfo::get_epoch_slots_batch` | 55 | 23.963 us | 48.878 us |
| 8 | Read | `ClusterInfo::push_lowest_slot` | 12 | 15.377 us | 42.248 us |
| 9 | Read | `ClusterInfo::query_contact_infos_batch` | 42 | 21.600 us | 42.158 us |
| 10 | Write | `CrdsGossipPull::purge_remove_batch` | 9 | 2.504 us | 40.656 us |

### Corrected acquisition histogram versus original master

| Wait interval | Master read | Master write | Master total | Corrected read | Corrected write | Corrected total |
|---|---:|---:|---:|---:|---:|---:|
| [0, 0.25) us | 28 | 33 | 61 | 0 | 0 | 0 |
| [0.25, 0.5) us | 4 | 7 | 11 | 0 | 0 | 0 |
| [0.5, 1) us | 36 | 30 | 66 | 1 | 10 | 11 |
| [1, 2) us | 17 | 37 | 54 | 4 | 60 | 64 |
| [2, 4) us | 13 | 25 | 38 | 7 | 91 | 98 |
| [4, 8) us | 12 | 20 | 32 | 23 | 128 | 151 |
| [8, 16) us | 12 | 19 | 31 | 313 | 566 | 879 |
| [16, 32) us | 13 | 33 | 46 | 679 | 339 | 1,018 |
| [32, 64) us | 127 | 71 | 198 | 208 | 58 | 266 |
| [64, 128) us | 16 | 20 | 36 | 5 | 7 | 12 |
| [128, 256) us | 7 | 33 | 40 | 2 | 5 | 7 |
| [256, 512) us | 1 | 10 | 11 | 2 | 5 | 7 |
| [512, 1024) us | 1 | 1 | 2 | 0 | 0 | 0 |
| >= 1024 us | 7 | 211 | 218 | 0 | 0 | 0 |
| **Total** | **294** | **550** | **844** | **1,244** | **1,269** | **2,513** |

Source captures (all retain demangled symbolic paths):

- Holds: `/tmp/crds-lock-holds-3888292-review-fixes-60s.csv`
- Hold report and histograms:
  `/tmp/crds-lock-holds-3888292-review-fixes-60s.json`
- Waits:
  `/tmp/crds-lock-waits-3888292-review-fixes-blockers-60s.csv`
- Wait-to-holder relations:
  `/tmp/crds-lock-waits-3888292-review-fixes-blockers-60s.blockers.csv`
- Wait report and histograms:
  `/tmp/crds-lock-waits-3888292-review-fixes-blockers-60s.json`

Validation:

- `cargo test -p solana-gossip --lib`: 213 passed.
- `cargo test -p solana-gossip --lib --features crds-lock-instrumentation`:
  214 passed.
- `git diff --check master`: clean.

## Review fixes and simplifications (2026-09-08; not remeasured)

These source changes follow the review of the current worktree against master.
No new latency capture or validator deployment accompanies this section; the
historical measurements above have not been changed.

- Deduplicate pull-response owners per insertion batch and refresh them before
  releasing that same write guard.
- Limit pull clone batches to the remaining output quota, continuing after
  rejected or disappeared candidates. Split broad metadata snapshots using the
  existing hash shards and stop visiting shards once the output quota is full.
  This is a per-shard work split, not a hard time bound for a skewed shard.
- Append vote, epoch-slot, saved-contact, and all-peer batches directly to
  caller-owned output vectors with capacity reserved before locking.
- Return purge-label iterators instead of allocating an intermediate vector
  for each owner; trim purged hashes with one front/pop pass.
- Consolidate the repair/gossip index's contact fields into one optional entry.
- Store parking-lot guards directly, avoid spurious poisoning during unrelated
  unwinding, and compile profiling-only try-lock paths and no-inline attributes
  only when instrumentation is enabled.

The wait profiler now writes schema version 2. JSON and CSV fields previously
named `terminal_blocker*` are named `last_overlapping_holder*`;
`terminal_waits` becomes `last_holder_waits`, and release gaps use
`last_holder_release_to_acquire_*`. These represent direct conflicting
overlaps, not a causal allocation of the wait duration. A separate JSON list,
`inferred_reader_dependencies`, records possible existing-reader -> queued-writer
-> new-reader chains, with symbolic acquisition paths for all three participants.
These intervals can overlap and must not be summed as explained wait time.
Actual queue state is not traced, so these chains are explicitly inferred.
Historical schema-1 reports retain their original names and values.

Deferred destruction of replaced/rejected CRDS values remains a separate,
unimplemented experiment. No performance improvement is claimed for this batch
until it has been deployed and measured.

Validation for this batch:

- `cargo test -p solana-gossip --lib --quiet`: 219 passed.
- `cargo test -p solana-gossip --lib --features crds-lock-instrumentation --quiet`:
  219 passed.
- `cargo check -p solana-core --lib --quiet`: passed.
- `python3 -m unittest discover -s scripts -p test_measure_crds_lock_waits.py`:
  5 passed without attaching to the validator.
- Nightly rustfmt checks for the edited Rust sources and
  `git diff --check master`: passed.

## Post-review simplifications: remeasurement (2026-09-08)

Instrumented build `5fe8d4b34580866e550551d206594fae37822480` was deployed with the authorized systemd stop/copy/start sequence. PID `3913354` reported RPC health `ok` before profiling and after the final captures. All captures below were separate passive 60-second windows; no synthetic traffic was generated.

The first captures exposed a compiler-inlining change: all write callers were reported as `CrdsRwLock::write`. Their timing data is retained below, but those reports cannot provide writer-path rankings. The tracers now use frame pointers to skip only known CRDS lock/timing wrappers and store the resolved symbolic acquisition paths. The second captures used this corrected lookup on the SAME running binary, without a second restart. The extra caller lookup precedes the start timestamp but still adds tracing work while locked; adjacent workload windows and instrumentation overhead prevent causal attribution of small changes.

All four captures reported zero lost, nested, or unmatched events. Final holds: 528,216 acquisitions. Final waits: 3,196 acquisitions and 531,746 holder intervals; 3,090 waits had directly conflicting overlaps and 106 did not. Two possible reader -> queued-writer -> reader dependencies were saved separately as inferred, with all participants symbolized.

### Aggregate contended acquisition latency

These are failed-try-lock-to-acquisition intervals, including spinning and any kernel parking, not a futex-syscall-only population.

| Capture | N | Mean us | Median us | P95 us | P99 us | Max us | Total wait ms |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Before simplifications | 2,513 | 20.024 | 16.583 | 39.098 | 65.464 | 439.727 | 50.322 |
| Current first capture (wrapper-level writers) | 2,769 | 20.827 | 17.474 | 41.141 | 85.414 | 525.506 | 57.671 |
| Current final capture (resolved callers) | 3,196 | 21.314 | 18.180 | 40.371 | 78.344 | 528.981 | 68.119 |

Final versus preceding build: mean +6.4%, median +9.6%, P99 +19.7%, maximum +20.3%. Event count rose 27.2%. Neither current capture demonstrates an aggregate wait-latency improvement.

| Mode | Capture | N | Mean us | Median us | P95 us | P99 us | Max us |
| --- | --- | --- | --- | --- | --- | --- | --- |
| read | Before | 1244 | 23.958 | 21.550 | 43.017 | 60.005 | 439.727 |
| read | Current final | 1581 | 24.722 | 22.361 | 43.620 | 72.604 | 295.628 |
| write | Before | 1269 | 16.169 | 12.337 | 33.123 | 90.399 | 439.618 |
| write | Current final | 1615 | 17.977 | 14.450 | 33.141 | 104.090 | 528.981 |

### Hold-time comparison

| Mode | Capture | N | Mean us | Median us | P99 us | Max us |
| --- | --- | --- | --- | --- | --- | --- |
| read | Before | 399650 | 6.762 | 4.116 | 39.655 | 1,768.783 |
| read | Current first | 439015 | 6.105 | 4.216 | 32.543 | 954.960 |
| read | Current final | 445074 | 6.396 | 4.296 | 34.117 | 1,668.555 |
| write | Before | 84020 | 13.263 | 12.187 | 38.499 | 779.978 |
| write | Current first | 78752 | 13.147 | 11.836 | 38.994 | 2,082.697 |
| write | Current final | 83142 | 12.870 | 11.686 | 37.712 | 633.215 |

Selected targeted paths follow. The pull snapshot unit changed from an entire request prefix to an existing hash shard, so its per-acquisition improvement should not be read as an equivalent reduction in total CPU work. `process_push_message` is now attributed to its inlining parent `CrdsGossip`, rather than `CrdsGossipPush`.

| Hold path | N before -> current | Mean us | Median us | P99 us | Max us |
| --- | --- | --- | --- | --- | --- |
| Pull metadata snapshot | 5,441 -> 43,578 | 37.443 -> 4.292 | 35.980 -> 4.085 | 71.413 -> 8.162 | 363.633 -> 56.618 |
| Pull clone batch | 3,166 -> 7,960 | 2.450 -> 1.963 | 1.542 -> 1.162 | 24.284 -> 33.333 | 72.861 -> 81.903 |
| get_votes_batch | 2,269 -> 2,280 | 24.627 -> 22.039 | 25.095 -> 22.531 | 49.575 -> 45.129 | 238.900 -> 65.531 |
| get_epoch_slots_batch | 2,071 -> 2,083 | 18.275 -> 15.981 | 18.786 -> 16.273 | 35.277 -> 31.390 | 69.516 -> 67.533 |
| save_contact_info_batch | 55 -> 55 | 36.223 -> 37.082 | 35.829 -> 35.139 | 46.138 -> 105.948 | 46.484 -> 178.727 |
| purge_find_candidates_batch | 65,365 -> 65,400 | 7.417 -> 8.501 | 6.469 -> 7.741 | 16.673 -> 17.704 | 341.612 -> 55.236 |
| process_pull_responses | 286 -> 306 | 3.910 -> 3.899 | 3.289 -> 3.465 | 11.663 -> 13.730 | 16.763 -> 17.634 |
| process_push_message | 83,064 -> 82,149 | 13.364 -> 12.926 | 12.297 -> 11.756 | 38.593 -> 37.777 | 779.978 -> 633.215 |
| repair_peer_endpoints | 90 -> 90 | 79.575 -> 69.657 | 60.118 -> 53.123 | 220.426 -> 182.924 | 237.448 -> 194.198 |
| snapshot_gossip_nodes | 128 -> 128 | 48.579 -> 70.482 | 43.675 -> 56.293 | 102.333 -> 457.622 | 123.320 -> 548.568 |
| purge trim write guard | 600 -> 600 | 4.087 -> 10.199 | 1.367 -> 9.253 | 17.339 -> 22.234 | 62.556 -> 26.256 |

`all_peers` was not observed in the final hold window (it did appear during the wait window), so no all-peers hold improvement is claimed. Some targets regressed in this sample, including purge candidate discovery and gossip-node snapshots. The first hold capture also contained a 2.083 ms write outlier; the later 633 us maximum does not invalidate that earlier observation.

### Current wait-path rankings

Both rankings use every observed acquisition path, including small-N paths. Full demangled names are preserved in the JSON/CSV; this table abbreviates module and generic arguments but retains offsets to distinguish multiple guards.

| Max rank | Median rank | Mode | Acquisition path | N | Mean us | Median us | P99 us | Max us |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 22 | write | `CrdsGossip::purge+0x40` | 51 | 23.947 | 10.925 | 281.574 | 528.981 |
| 2 | 17 | write | `CrdsGossip::process_push_message+0x2e9` | 1534 | 17.804 | 14.495 | 103.188 | 391.872 |
| 3 | 3 | read | `CrdsGossipPull::purge_find_candidates_batch+0x55` | 1072 | 26.293 | 23.543 | 80.732 | 295.628 |
| 4 | 5 | read | `CrdsGossipPull::build_crds_filters+0x326` | 193 | 24.605 | 22.942 | 54.410 | 78.138 |
| 5 | 10 | read | `ClusterInfo::get_votes_batch+0x71` | 81 | 21.315 | 18.866 | 72.131 | 72.700 |
| 6 | 16 | write | `CrdsGossipPull::process_pull_responses+0x190` | 25 | 16.896 | 15.371 | 56.704 | 67.293 |
| 7 | 7 | read | `ClusterInfo::query_contact_infos_batch+0x5a` | 38 | 21.457 | 20.398 | 50.840 | 57.069 |
| 8 | 1 | read | `ClusterInfo::get_epoch_slots_batch+0x73` | 55 | 24.766 | 24.594 | 52.069 | 54.616 |
| 9 | 13 | read | `CrdsGossipPull::purge_pubkey_snapshot+0x3f` | 7 | 18.888 | 16.733 | 46.363 | 47.846 |
| 10 | 9 | read | `ClusterInfo::new_push_requests+0x251` | 16 | 19.759 | 19.612 | 34.645 | 35.609 |
| 11 | 4 | read | `ClusterInfo::all_peers_batch+0x5a` | 4 | 25.540 | 23.032 | 35.243 | 35.599 |
| 12 | 24 | write | `CrdsGossipPull::purge_remove_batch+0x2e` | 5 | 15.750 | 8.763 | 33.184 | 33.246 |
| 13 | 21 | read | `ClusterInfo::trim_crds_table+0x53` | 21 | 12.578 | 11.116 | 29.823 | 31.022 |
| 14 | 15 | read | `CrdsGossipPush::new_push_messages+0x19f` | 23 | 16.229 | 15.531 | 29.775 | 30.171 |
| 15 | 20 | read | `ClusterInfo::get_duplicate_shreds+0x54` | 15 | 12.694 | 11.707 | 29.407 | 29.581 |
| 16 | 8 | read | `CrdsGossipPull::filter_crds_values+0x8a9` | 6 | 19.566 | 19.997 | 28.338 | 28.489 |
| 17 | 11 | read | `ClusterInfo::push_lowest_slot+0xd7` | 5 | 18.408 | 18.366 | 27.843 | 28.089 |
| 18 | 23 | read | `ClusterInfo::repair_peer_endpoints+0xcf` | 4 | 13.023 | 10.870 | 27.383 | 27.889 |
| 19 | 14 | read | `ClusterInfo::get_node_version+0x51` | 9 | 16.085 | 16.623 | 26.730 | 27.157 |
| 20 | 6 | read | `ClusterInfo::process_packets+0x267a` | 14 | 19.407 | 20.634 | 25.630 | 25.895 |
| 21 | 2 | read | `ClusterInfo::save_contact_info_batch+0x62` | 1 | 24.154 | 24.154 | 24.154 | 24.154 |
| 22 | 19 | read | `CrdsGossipPull::build_crds_filters+0xb1` | 4 | 12.001 | 11.821 | 21.079 | 21.269 |
| 23 | 12 | read | `snapshot_gossip_nodes+0x3f` | 2 | 17.123 | 17.123 | 20.863 | 20.939 |
| 24 | 26 | read | `ClusterInfo::process_packets+0x165` | 9 | 7.578 | 2.333 | 19.363 | 19.627 |
| 25 | 18 | read | `ClusterInfo::tvu_peers+0x93` | 1 | 13.559 | 13.559 | 13.559 | 13.559 |
| 26 | 25 | read | `submit_gossip_stats+0x56` | 1 | 3.075 | 3.075 | 3.075 | 3.075 |

The original master capture retained mode distributions and only representative callers, not complete per-acquisition-path distributions. Exact master-vs-current path rankings cannot be reconstructed; comparisons to the entire master writer pool remain proxies.

### Original master baseline and corrected histogram

Correction to earlier histogram sections: the original raw map uses adaptive DECIMAL bins, not logarithmic `hist(delta_ns, 4)` bins. The earlier re-binned master histogram tables are incorrect and are superseded by this table. The original decoded `/tmp/agave-lock-histograms-60s-final.csv` (lock `L1765`) is authoritative here. Its bins were cross-checked against the read/write raw map counts. The recorded master counts, means, maxima, and total wait are unaffected.

| Interval us | Master read | Master write | Master total | Previous total | Current read | Current write | Current total |
| --- | --- | --- | --- | --- | --- | --- | --- |
| [0, 0.25) | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| [0.25, 0.5) | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| [0.5, 1) | 0 | 3 | 3 | 11 | 0 | 8 | 8 |
| [1, 2) | 11 | 16 | 27 | 64 | 12 | 56 | 68 |
| [2, 4) | 9 | 8 | 17 | 98 | 14 | 69 | 83 |
| [4, 8) | 8 | 6 | 14 | 151 | 25 | 117 | 142 |
| [8, 16) | 25 | 23 | 48 | 879 | 356 | 670 | 1026 |
| [16, 32) | 29 | 41 | 70 | 1018 | 859 | 601 | 1460 |
| [32, 64) | 24 | 51 | 75 | 266 | 285 | 67 | 352 |
| [64, 128) | 32 | 69 | 101 | 12 | 23 | 13 | 36 |
| [128, 256) | 117 | 53 | 170 | 7 | 6 | 8 | 14 |
| [256, 512) | 27 | 37 | 64 | 7 | 1 | 5 | 6 |
| [512, 1024) | 8 | 40 | 48 | 0 | 0 | 1 | 1 |
| >= 1024 | 4 | 203 | 207 | 0 | 0 | 0 | 0 |
| Total | 294 | 550 | 844 | 2513 | 1581 | 1615 | 3196 |

Current and previous counts are exact. Master intervals straddling a common boundary are assigned by their midpoint, so the re-binning remains approximate. Master and current instrument different lock implementations/slow-path entry points and are not identical-population comparisons.

Master combined mean was 520.109 us, median bin 170-180 us, maximum 3,446.480 us, and accumulated wait 438.972 ms. Current mean is lower by 95.9%, maximum by 84.7%, and accumulated wait by 84.5%. Current had one wait >=512 us and none >=1024 us; the corrected master histogram assigns 255 waits to >=512 us.

### Long-tail attribution

The final maximum was `CrdsGossip::purge` waiting 528.981 us, with a `ClusterInfo::process_packets` read hold overlapping it; 60.484 us elapsed between that holder release marker and acquisition. The next-largest wait, push insertion at 391.872 us, had a 327.032 us gap after its last overlapping purge holder. These gaps are not themselves lock hold time and can include wakeup/scheduling/probe costs or incompletely observed ownership; overlaps do not prove causality.

The newly exposed `tvu_peers` read path was previously hidden under `time_gossip_read_lock`: 14 holds, median 196.377 us and max 259.659 us. This identifies a remaining relatively long reader without claiming it caused the largest wait in this capture.

### Saved artifacts and tracer validation

- Final holds: `/tmp/crds-lock-holds-3913354-review-simplifications-symbolic-60s.{csv,json}`
- Final waits: `/tmp/crds-lock-waits-3913354-review-simplifications-symbolic-blockers-60s.{csv,json}`
- Final direct overlaps: `/tmp/crds-lock-waits-3913354-review-simplifications-symbolic-blockers-60s.blockers.csv`
- Inferred reader chains and their symbolic paths: `inferred_reader_dependencies` in the final wait JSON.
- Initial limited-attribution captures: `/tmp/crds-lock-holds-3913354-review-simplifications-60s.{csv,json}` and `/tmp/crds-lock-waits-3913354-review-simplifications-blockers-60s.{csv,json,blockers.csv}`.
- Previous build: `/tmp/crds-lock-holds-3888292-review-fixes-60s.{csv,json}` and `/tmp/crds-lock-waits-3888292-review-fixes-blockers-60s.{csv,json}`.
- Backup of the prior deployed binary: `/tmp/agave-before-review-simplifications.HhBQ52/agave-validator`.
- Tracer helper `scripts/crds_lock_profile.py` resolves runtime ELF wrapper ranges and follows at most four frame-pointer links, only while the caller is a known wrapper. Both final reports retain the skipped symbolic wrapper names. No final acquisition path was left under the lock wrapper or generic timing helper.
- Offline interval/ELF caller-lookup tests: 7 passed. `git diff --check master`: clean.

All final CSV/JSON paths retain symbolized acquisition paths and the reports retain the build ID; later binary replacement does not invalidate those names. No further optimization or rollback was made after observing these results.
