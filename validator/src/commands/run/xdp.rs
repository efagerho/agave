use {
    super::{ProcessCapabilities, capabilities, config::RunConfig},
    agave_xdp::{
        device::NetworkDevice,
        interface_ipv4,
        transmitter::{TransmitterBuilder, XdpConfig},
    },
    solana_core::{
        system_monitor_service::XdpNetworkConfigReport,
        validator::{XdpComponents, XdpTransmitSetup},
    },
    solana_net_utils::multihomed_sockets::BindIpAddrs,
    std::{
        error::Error,
        net::IpAddr,
        sync::{Arc, atomic::AtomicBool},
    },
};

/// Restricts the process to the capabilities the configuration requires, creating the XDP
/// transmitter while the XDP ones are held, and records the XDP device in the validator config
pub(super) fn setup(
    config: &mut RunConfig,
    process_capabilities: ProcessCapabilities,
    exit: Arc<AtomicBool>,
) -> Result<Option<XdpTransmitSetup>, Box<dyn Error>> {
    let zero_copy = config.xdp.as_ref().map(|xdp_config| xdp_config.zero_copy);
    let xdp = capabilities::restrict(&config.validator, process_capabilities, zero_copy, || {
        config
            .xdp
            .clone()
            .map(|xdp_config| build(xdp_config, &config.node.bind_ip_addrs, exit))
            .transpose()
    })?;
    let (setup, report) = xdp.unzip();
    config.validator.xdp_network_config_report = report;
    Ok(setup)
}

fn build(
    mut xdp_config: XdpConfig,
    bind_addresses: &BindIpAddrs,
    exit: Arc<AtomicBool>,
) -> Result<(XdpTransmitSetup, XdpNetworkConfigReport), Box<dyn Error>> {
    let device = match &xdp_config.interface {
        Some(interface) => NetworkDevice::new(interface)
            .map_err(|err| format!("unable to open XDP interface {interface}: {err}"))?,
        None => NetworkDevice::new_from_default_route()
            .map_err(|err| format!("unable to find the default route interface: {err}"))?,
    };
    let interface = device.name().to_string();
    // Keep the transmitter and metrics on the selected XDP device. Source IP lookup
    // uses the same interface name, with bond-master fallback.
    xdp_config.interface = Some(interface.clone());
    let zero_copy = xdp_config.zero_copy;
    // --bind-address only accepts IPv4 addresses
    let src_ip = match bind_addresses.active() {
        IpAddr::V4(ip) if !ip.is_unspecified() => ip,
        _ => interface_ipv4(&interface)
            .map_err(|err| format!("unable to get the IPv4 address of {interface}: {err}"))?,
    };
    // Nothing can express per-component queue assignments yet, so every
    // component transmits over the whole queue set.
    let all_positions: Box<[usize]> = (0..xdp_config.queues.len()).collect();
    let transmitter_builder = TransmitterBuilder::new(xdp_config, exit)
        .map_err(|err| format!("failed to create XDP transmitter: {err}"))?;
    Ok((
        XdpTransmitSetup {
            transmitter_builder,
            src_ip,
            components: XdpComponents {
                tpu: Some(all_positions.clone()),
                turbine: Some(all_positions.clone()),
                repair: Some(all_positions.clone()),
                gossip: Some(all_positions.clone()),
                votor: Some(all_positions),
            },
        },
        XdpNetworkConfigReport {
            zero_copy,
            interface,
        },
    ))
}
