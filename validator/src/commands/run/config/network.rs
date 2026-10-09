use {
    super::host_port_of,
    crate::cli::thread_args::NumThreadConfig,
    agave_votor_transport::MAX_ENDPOINTS,
    clap::{ArgMatches, value_t},
    solana_gossip::cluster_info::NodeConfig,
    solana_net_utils::multihomed_sockets::BindIpAddrs,
    std::{
        error::Error,
        net::{IpAddr, Ipv4Addr},
        num::NonZeroUsize,
    },
};

/// An unspecified advertised IP is resolved from the cluster entrypoints by `network::bind_node`
pub(super) fn new_config(
    matches: &ArgMatches,
    threads: &NumThreadConfig,
) -> Result<NodeConfig, Box<dyn Error>> {
    let bind_addresses = BindIpAddrs::new(
        matches
            .values_of("bind_address")
            .expect("bind_address should always be present due to default")
            .map(|addr| {
                solana_net_utils::parse_host(addr)
                    .map_err(|err| format!("failed to parse --bind-address {addr}: {err}"))
            })
            .collect::<Result<_, _>>()?,
    )
    .map_err(|err| format!("invalid --bind-address: {err}"))?;
    for (arg, flag) in [
        ("public_tpu_addr", "--public-tpu-address"),
        ("public_tvu_addr", "--public-tvu-address"),
    ] {
        if bind_addresses.len() > 1 && matches.is_present(arg) {
            return Err(format!("{flag} cannot be used in a multihoming context").into());
        }
    }

    let num_votor_quic_endpoints = value_t!(matches, "num_votor_endpoints", NonZeroUsize)?;
    if num_votor_quic_endpoints.get() > MAX_ENDPOINTS {
        return Err(format!("--num-votor-endpoints must be at most {MAX_ENDPOINTS}").into());
    }

    Ok(NodeConfig {
        advertised_ip: match bind_addresses.active() {
            ip if ip.is_loopback() => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ip => ip,
        },
        gossip_port: value_t!(matches, "gossip_port", u16).unwrap_or(0),
        port_range: solana_net_utils::parse_port_range(
            matches.value_of("dynamic_port_range").unwrap(),
        )
        .ok_or("invalid --dynamic-port-range")?,
        bind_ip_addrs: bind_addresses,
        public_tpu_addr: host_port_of(matches, "public_tpu_addr", "--public-tpu-address")?,
        public_tpu_forwards_addr: host_port_of(
            matches,
            "public_tpu_forwards_addr",
            "--public-tpu-forwards-address",
        )?,
        public_tvu_addr: host_port_of(matches, "public_tvu_addr", "--public-tvu-address")?,
        num_tvu_receive_sockets: threads.tvu_receive_threads,
        num_tvu_retransmit_sockets: threads.tvu_retransmit_threads,
        num_quic_endpoints: value_t!(matches, "num_quic_endpoints", NonZeroUsize)?,
        num_votor_quic_endpoints,
    })
}
