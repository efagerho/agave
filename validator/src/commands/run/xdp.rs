use {
    super::{ProcessCapabilities, capabilities, config::RunConfig},
    agave_cpu_utils::cpu_affinity,
    agave_validator_config::{self as config, DeviceSelector, EffectiveConfig, RouteSelector},
    agave_xdp::{
        device::NetworkDevice,
        transmitter::{QueueCpuBinding, TransmitterBuilder, XdpConfig},
    },
    log::info,
    solana_core::{
        system_monitor_service::XdpNetworkConfigReport,
        validator::{XdpComponents, XdpTransmitSetup},
    },
    std::{
        collections::BTreeSet,
        error::Error,
        net::{IpAddr, Ipv4Addr},
        sync::{Arc, atomic::AtomicBool},
    },
};

struct ResolvedXdp {
    logical_interface: String,
    config: XdpConfig,
    src_ip: Ipv4Addr,
    components: XdpComponents,
}

/// Restricts the process to the capabilities the configuration requires, creating the XDP
/// transmitter while the XDP ones are held, and records the XDP device in the validator config
pub(super) fn setup(
    config: &mut RunConfig,
    process_capabilities: ProcessCapabilities,
    exit: Arc<AtomicBool>,
) -> Result<Option<XdpTransmitSetup>, Box<dyn Error>> {
    // Resolving needs no capabilities, so configuration errors are reported first
    let resolved = config
        .xdp
        .as_ref()
        .map(|policy| {
            resolve(
                policy,
                config.validator.poh_pinned_cpu_core,
                config.node.bind_ip_addrs.active(),
            )
        })
        .transpose()?;
    let zero_copy = resolved.as_ref().map(|resolved| resolved.config.zero_copy);
    let xdp = capabilities::restrict(&config.validator, process_capabilities, zero_copy, || {
        resolved.map(|resolved| build(resolved, exit)).transpose()
    })?;
    let (setup, report) = xdp.unzip();
    config.validator.xdp_network_config_report = report;
    Ok(setup)
}

/// Resolves the policy against this host's CPUs and network devices
fn resolve(
    policy: &EffectiveConfig,
    poh_core: Option<usize>,
    bind_ip: IpAddr,
) -> Result<ResolvedXdp, String> {
    let allowed_cpus: BTreeSet<_> = cpu_affinity(None)
        .map_err(|error| format!("failed to query process CPU affinity for XDP: {error}"))?
        .into_iter()
        .map(|cpu| *cpu)
        .collect();
    let runtime = config::resolve_runtime(policy, &allowed_cpus, poh_core)?;
    let device = resolve_xdp_device(&runtime.interface_label, &runtime.device)?;
    let src_ip = resolve_xdp_source_ipv4(&runtime.interface_label, &device, bind_ip)?;
    info!(
        "XDP policy {runtime:?} on device {} with source IPv4 {src_ip}",
        device.name()
    );
    let config::RuntimeXdpConfig {
        interface_label,
        queues,
        zero_copy,
        components,
        ..
    } = runtime;
    let queues = queues
        .into_iter()
        .map(|binding| QueueCpuBinding {
            queue: binding.queue,
            cpu: binding.cpu,
        })
        .collect();
    Ok(ResolvedXdp {
        logical_interface: interface_label,
        config: XdpConfig::new(Some(device.name()), queues, zero_copy),
        src_ip,
        components: XdpComponents {
            tpu: Some(components.tpu),
            turbine: Some(components.turbine),
            repair: Some(components.repair),
            gossip: Some(components.gossip),
            votor: Some(components.votor),
        },
    })
}

/// Creates the XDP transmitter on the resolved device
fn build(
    resolved: ResolvedXdp,
    exit: Arc<AtomicBool>,
) -> Result<(XdpTransmitSetup, XdpNetworkConfigReport), Box<dyn Error>> {
    let ResolvedXdp {
        logical_interface,
        config,
        src_ip,
        components,
    } = resolved;
    let report = XdpNetworkConfigReport {
        zero_copy: config.zero_copy,
        interface: config.interface.clone().unwrap_or_default(),
    };
    let transmitter_builder = TransmitterBuilder::new(config, exit).map_err(|e| {
        let remediation = if report.zero_copy {
            "Check the configured workers; if zero-copy is unsupported, pass --no-xdp-zero-copy, \
             or pass --no-xdp."
        } else {
            "Check the configured workers or pass --no-xdp."
        };
        format!(
            "failed to create the XDP transmitter for logical interface `{logical_interface}`, \
             device `{}`: {e}. {remediation}",
            report.interface
        )
    })?;
    Ok((
        XdpTransmitSetup {
            transmitter_builder,
            src_ip,
            components,
        },
        report,
    ))
}

fn resolve_xdp_device(
    logical_interface: &str,
    selector: &DeviceSelector,
) -> Result<NetworkDevice, String> {
    match selector {
        DeviceSelector::Name(name) => NetworkDevice::new(name).map_err(|error| {
            format!(
                "XDP logical interface `{logical_interface}` selects device.name {name:?}, which \
                 is not usable: {error}; fix the name or pass --no-xdp"
            )
        }),
        DeviceSelector::Route(RouteSelector::Default) => NetworkDevice::new_from_default_route()
            .map_err(|error| {
                format!(
                    "failed to open the default-route device for XDP logical interface \
                     `{logical_interface}`: {error}; set device.name or pass --no-xdp"
                )
            }),
    }
}

fn resolve_xdp_source_ipv4(
    logical_interface: &str,
    device: &NetworkDevice,
    bind_ip: IpAddr,
) -> Result<Ipv4Addr, String> {
    match bind_ip {
        IpAddr::V4(ip) if !ip.is_unspecified() => Ok(ip),
        // --bind-address only accepts IPv4 addresses. Source IP lookup uses the device name, with
        // bond-master fallback.
        _ => agave_xdp::interface_ipv4(device.name()).map_err(|error| {
            format!(
                "cannot select an IPv4 source address for XDP logical interface \
                 `{logical_interface}`, device `{}`: {error}; assign an IPv4 address to the \
                 device, pass --bind-address, or pass --no-xdp",
                device.name()
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_missing_device_is_a_targeted_error() {
        let Err(error) =
            resolve_xdp_device("primary", &DeviceSelector::Name("nosuchnic0".to_string()))
        else {
            panic!("missing device unexpectedly resolved")
        };
        assert!(error.contains("\"nosuchnic0\""), "{error}");
    }

    #[test]
    fn test_source_ipv4_respects_bind_address() {
        let device = NetworkDevice::new("lo").unwrap();
        let explicit = Ipv4Addr::new(192, 0, 2, 1);
        for (bind, expected) in [
            (explicit, explicit),
            (Ipv4Addr::UNSPECIFIED, Ipv4Addr::LOCALHOST),
        ] {
            let result = resolve_xdp_source_ipv4("primary", &device, bind.into());
            assert_eq!(result.unwrap(), expected, "{bind}");
        }
    }
}
