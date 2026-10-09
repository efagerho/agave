use {
    super::config::RunConfig,
    log::*,
    rand::{rng, seq::SliceRandom},
    solana_core::system_monitor_service::SystemMonitorService,
    solana_gossip::node::Node,
    solana_signer::Signer,
    std::{
        error::Error,
        fmt::Display,
        net::{IpAddr, Ipv4Addr, SocketAddr},
    },
};

/// Resolves the advertised address and shred version, then binds the node's sockets
pub(super) fn bind_node(config: &mut RunConfig) -> Result<Node, Box<dyn Error>> {
    if config.check_os_network_limits {
        if !SystemMonitorService::check_os_network_limits() {
            return Err("OS network limit test failed. See \
                        https://docs.anza.xyz/operations/guides/validator-start#system-tuning"
                .into());
        }
        info!("OS network limits test passed.");
    }

    let bind_ip = config.node.bind_ip_addrs.active();
    if config.node.advertised_ip.is_unspecified() {
        config.node.advertised_ip = if config.entrypoints.is_empty() {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            query_entrypoints(&config.entrypoints, "public IP address", |entrypoint| {
                solana_net_utils::get_public_ip_addr_with_binding(entrypoint, bind_ip)
                    .map_err(|err| err.to_string())
            })
            .ok_or("unable to determine the validator's public IP address")?
        };
    }
    // TODO: Once entrypoints are updated to return shred-version, this should
    // abort if it fails to obtain a shred-version, so that nodes always join
    // gossip with a valid shred-version. The code to adopt entrypoint shred
    // version can then be deleted from gossip and get_rpc_node above.
    if config.validator.expected_shred_version.is_none() {
        config.validator.expected_shred_version =
            query_entrypoints(&config.entrypoints, "shred version", |entrypoint| {
                match solana_net_utils::get_cluster_shred_version_with_binding(entrypoint, bind_ip)
                {
                    Ok(0) => Err("shred version is zero".to_string()),
                    result => result.map_err(|err| err.to_string()),
                }
            });
    }

    let mut node =
        Node::new_with_external_ip(&config.identity_keypair.pubkey(), config.node.clone());
    if config.restricted_repair_only_mode {
        // When in --restricted_repair_only_mode is enabled only the gossip and repair ports
        // need to be reachable by the entrypoint to respond to gossip pull requests and
        // repair requests initiated by the node.  All other ports are unused.
        node.info.remove_tpu();
        node.info.remove_tpu_forwards();
        node.info.remove_tvu();
        node.info.remove_serve_repair();
        node.info.remove_alpenglow();
        // A node in this configuration shouldn't be an entrypoint to other nodes
        node.sockets.ip_echo = None;
    }
    if !config.private_rpc {
        let ip = config.node.advertised_ip;
        let public_rpc_addrs = config.public_rpc_addr.map(|addr| (addr, addr)).or(config
            .validator
            .rpc_addrs
            .map(|(rpc, pubsub)| {
                (
                    SocketAddr::new(ip, rpc.port()),
                    SocketAddr::new(ip, pubsub.port()),
                )
            }));
        if let Some((rpc, pubsub)) = public_rpc_addrs {
            node.info
                .set_rpc(rpc)
                .map_err(|err| format!("invalid RPC address {rpc}: {err}"))?;
            node.info
                .set_rpc_pubsub(pubsub)
                .map_err(|err| format!("invalid RPC pubsub address {pubsub}: {err}"))?;
        }
    }
    Ok(node)
}

fn query_entrypoints<T: Display>(
    entrypoints: &[SocketAddr],
    what: &str,
    query: impl Fn(&SocketAddr) -> Result<T, String>,
) -> Option<T> {
    let mut entrypoints = entrypoints.to_vec();
    entrypoints.shuffle(&mut rng());
    entrypoints.iter().find_map(|entrypoint| {
        info!("Contacting {entrypoint} to determine the {what}");
        query(entrypoint)
            .inspect(|value| info!("Obtained {what} {value} from {entrypoint}"))
            .inspect_err(|err| warn!("Failed to obtain {what} from {entrypoint}: {err}"))
            .ok()
    })
}
