use {
    crate::cli::thread_args::NumThreadConfig,
    clap::{ArgMatches, value_t},
    solana_core::{tpu::MAX_VOTES_PER_SECOND, validator::ValidatorTpuConfig},
    solana_message::v1::MAX_TRANSACTION_SIZE,
    solana_streamer::{
        nonblocking::{simple_qos::SimpleQosConfig, swqos::SwQosConfig},
        quic::{QuicStreamerConfig, SimpleQosQuicStreamerConfig, SwQosQuicStreamerConfig},
    },
};

pub(super) fn new_config(
    matches: &ArgMatches,
    threads: &NumThreadConfig,
) -> Result<ValidatorTpuConfig, clap::Error> {
    let max_connections_per_ipaddr_per_min =
        value_t!(matches, "tpu_max_connections_per_ipaddr_per_minute", u64)?;
    let max_connections_per_peer = value_t!(matches, "tpu_max_connections_per_peer", usize).ok();
    let max_connections_per_peer =
        |name: &str| max_connections_per_peer.map_or_else(|| value_t!(matches, name, usize), Ok);
    let sw_qos = |num_threads,
                  max_staked_connections: &str,
                  max_unstaked_connections: &str|
     -> Result<_, clap::Error> {
        Ok(SwQosQuicStreamerConfig {
            quic_streamer_config: QuicStreamerConfig {
                max_connections_per_ipaddr_per_min,
                num_threads,
                stream_receive_window_size: MAX_TRANSACTION_SIZE as u32,
                max_stream_data_bytes: MAX_TRANSACTION_SIZE as u32,
                ..Default::default()
            },
            qos_config: SwQosConfig {
                max_connections_per_staked_peer: max_connections_per_peer(
                    "tpu_max_connections_per_staked_peer",
                )?,
                max_connections_per_unstaked_peer: max_connections_per_peer(
                    "tpu_max_connections_per_unstaked_peer",
                )?,
                max_staked_connections: value_t!(matches, max_staked_connections, usize)?,
                max_unstaked_connections: value_t!(matches, max_unstaked_connections, usize)?,
                max_streams_per_ms: value_t!(matches, "tpu_max_streams_per_ms", u64)?,
            },
        })
    };
    Ok(ValidatorTpuConfig {
        vote_use_quic: value_t!(matches, "vote_use_quic", bool)?,
        tpu_quic_server_config: sw_qos(
            threads.tpu_transaction_receive_threads,
            "tpu_max_staked_connections",
            "tpu_max_unstaked_connections",
        )?,
        tpu_fwd_quic_server_config: sw_qos(
            threads.tpu_transaction_forward_receive_threads,
            "tpu_max_fwd_staked_connections",
            "tpu_max_fwd_unstaked_connections",
        )?,
        vote_quic_server_config: SimpleQosQuicStreamerConfig {
            quic_streamer_config: QuicStreamerConfig {
                max_connections_per_ipaddr_per_min,
                num_threads: threads.tpu_vote_transaction_receive_threads,
                ..Default::default()
            },
            qos_config: SimpleQosConfig {
                max_streams_per_second: MAX_VOTES_PER_SECOND,
                ..Default::default()
            },
        },
        sigverify_threads: threads.tpu_sigverify_threads,
    })
}
