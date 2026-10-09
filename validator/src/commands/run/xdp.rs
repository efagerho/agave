use {
    super::execute::Operation,
    agave_cpu_utils::cpu_affinity,
    agave_xdp::{
        device::NetworkDevice,
        interface_ipv4,
        transmitter::{QueueCpuBinding, TransmitterBuilder, XdpConfig},
    },
    caps::{
        CapSet,
        Capability::{CAP_BPF, CAP_NET_ADMIN, CAP_NET_RAW, CAP_PERFMON, CAP_SYS_NICE},
        CapsHashSet,
    },
    clap::ArgMatches,
    log::{info, warn},
    solana_clap_utils::input_parsers::parse_cpu_ranges,
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

/// Drops every capability the configuration does not require, holding the XDP ones only while
/// setting up XDP
pub(super) fn setup(
    xdp_config: Option<XdpConfig>,
    sys_nice: bool,
    primordial_caps: CapsHashSet,
    bind_addresses: &BindIpAddrs,
    exit: Arc<AtomicBool>,
) -> Result<Option<(XdpTransmitSetup, XdpNetworkConfigReport)>, Box<dyn Error>> {
    let mut supported_caps = primordial_caps.clone();
    supported_caps.extend([
        CAP_BPF,
        CAP_NET_ADMIN,
        CAP_NET_RAW,
        CAP_PERFMON,
        CAP_SYS_NICE,
    ]);
    let mut retained_caps = primordial_caps;
    if sys_nice {
        retained_caps.insert(CAP_SYS_NICE);
    }
    let mut required_caps = retained_caps.clone();
    if let Some(xdp_config) = &xdp_config {
        required_caps.extend([CAP_NET_ADMIN, CAP_NET_RAW]);
        if xdp_config.zero_copy {
            required_caps.extend([CAP_BPF, CAP_PERFMON]);
        }
    }

    let permitted_caps = caps::read(None, CapSet::Permitted)?;
    let missing_caps: Vec<_> = required_caps.difference(&permitted_caps).collect();
    if !missing_caps.is_empty() {
        return Err(format!(
            "the current configuration requires the following capabilities, which have not been \
             permitted to the current process: {missing_caps:?}"
        )
        .into());
    }
    let extra_caps: Vec<_> = permitted_caps.difference(&supported_caps).collect();
    if !extra_caps.is_empty() {
        warn!(
            "dropping extraneous capabilities ({extra_caps:?}) from the current process. consider \
             removing them from your operational configuration.",
        );
    }

    set_caps(&required_caps)?;
    // XDP _MUST_ be setup _BEFORE_ the app spawns any threads to ensure linux
    // capabilities do not leak, leaving the process in a state where it could
    // potentially be used as a privilege escalation gadget
    let xdp = xdp_config
        .map(|xdp_config| build_xdp_transmit_setup(xdp_config, bind_addresses, exit))
        .transpose();
    // we're done with caps needed to init xdp now. remove them from our process
    set_caps(&retained_caps)?;
    xdp
}

fn set_caps(caps: &CapsHashSet) -> Result<(), caps::errors::CapsError> {
    caps::set(None, CapSet::Effective, caps)?;
    caps::set(None, CapSet::Permitted, caps)
}

fn build_xdp_transmit_setup(
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
    let src_ip = match bind_addresses.active() {
        IpAddr::V4(ip) if !ip.is_unspecified() => ip,
        IpAddr::V4(_unspecified) => interface_ipv4(&interface)
            .map_err(|err| format!("unable to get the IPv4 address of {interface}: {err}"))?,
        IpAddr::V6(_) => return Err("XDP does not support IPv6".into()),
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

pub(super) fn build_xdp_config(
    matches: &ArgMatches,
    operation: &Operation,
    bind_addresses: &BindIpAddrs,
    poh_pinned_cpu_core: Option<usize>,
) -> Result<Option<XdpConfig>, String> {
    // XDP is not needed for init, which only initializes the ledger and exits
    if matches.is_present("no_xdp") || *operation == Operation::Initialize {
        return Ok(None);
    }
    if bind_addresses.len() > 1 {
        return Err(
            "XDP cannot be used in a multihoming context; pass --no-xdp to disable XDP".to_string(),
        );
    }
    let cpus = match matches.value_of("xdp_cpu_cores") {
        Some(cpu_str) => {
            let cpus =
                parse_cpu_ranges(cpu_str).expect("clap validator already accepted this CPU list");
            if cpus.is_empty() {
                return Err(format!("--xdp-cpu-cores `{cpu_str}` selects no CPUs"));
            }
            if let Some(poh_core) = poh_pinned_cpu_core
                && cpus.contains(&poh_core)
            {
                return Err(format!(
                    "--xdp-cpu-cores includes PoH core {poh_core}; XDP and PoH must not share a \
                     CPU core"
                ));
            }
            cpus
        }
        // Auto-select a single core, avoiding the PoH core.
        None => {
            let allowed = cpu_affinity(None).map_err(|err| {
                format!(
                    "failed to query CPU affinity: {err}. Pass --no-xdp to disable XDP, or \
                     provide --xdp-cpu-cores explicitly."
                )
            })?;
            let cpu = allowed
                .iter()
                .rev()
                .map(|cpu| **cpu)
                .find(|cpu| Some(*cpu) != poh_pinned_cpu_core)
                .ok_or_else(|| {
                    format!(
                        "XDP requires a dedicated CPU core separate from PoH (core \
                         {poh_pinned_cpu_core:?}), but none is available. Pass --no-xdp to \
                         disable XDP."
                    )
                })?;
            vec![cpu]
        }
    };
    info!("XDP enabled on CPU cores: {cpus:?}");
    // Map the CPU list onto hardware queues sequentially (queue i -> cpus[i]).
    let queues = cpus
        .into_iter()
        .zip(0..)
        .map(|(cpu, queue)| QueueCpuBinding { queue, cpu })
        .collect();
    Ok(Some(XdpConfig::new(
        matches.value_of("xdp_interface"),
        queues,
        matches.is_present("xdp_zero_copy"),
    )))
}

#[cfg(test)]
mod xdp_tests {
    use {
        super::*,
        crate::{cli::DefaultArgs, commands::run::args::add_args},
        solana_net_utils::multihomed_sockets::BindIpAddrs,
        std::net::{IpAddr, Ipv4Addr},
    };

    const POH_CORE: Option<usize> = Some(0);

    fn build_single_ip_bind() -> BindIpAddrs {
        BindIpAddrs::new(vec![Ipv4Addr::UNSPECIFIED.into()])
            .expect("a single unspecified IPv4 bind address should be valid")
    }

    fn build_multihoming_bind() -> BindIpAddrs {
        BindIpAddrs::new(vec![
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(2, 2, 2, 2)),
        ])
        .expect("two distinct specified IPv4 bind addresses should be valid")
    }

    #[test]
    fn test_no_xdp_flag_disables_xdp() {
        let default_args = DefaultArgs::default();
        let app = add_args(clap::App::new("agave-validator"), &default_args);
        let matches = app.get_matches_from(vec!["agave-validator", "--no-xdp"]);
        let result = build_xdp_config(&matches, &Operation::Run, &build_single_ip_bind(), POH_CORE);
        assert!(
            result
                .expect("--no-xdp should bypass XDP configuration validation")
                .is_none(),
            "--no-xdp must disable XDP"
        );
    }

    #[test]
    fn test_xdp_copy_mode_selection() {
        for (flag, zero_copy) in [
            (None, false),
            (Some("--xdp-zero-copy"), true),
            (Some("--no-xdp-zero-copy"), false),
        ] {
            let default_args = DefaultArgs::default();
            let app = add_args(clap::App::new("agave-validator"), &default_args);
            let mut args = vec!["agave-validator", "--xdp-cpu-cores", "1"];
            args.extend(flag);
            let matches = app
                .get_matches_from_safe(args)
                .expect("valid XDP copy mode selection should be accepted");
            let config =
                build_xdp_config(&matches, &Operation::Run, &build_single_ip_bind(), POH_CORE)
                    .expect("distinct XDP and PoH cores should be valid")
                    .expect("copy mode selection should keep XDP enabled");
            assert_eq!(
                config.zero_copy, zero_copy,
                "XDP copy mode must match the selection {flag:?}"
            );
        }
    }

    #[test]
    fn test_empty_xdp_cpu_cores_is_error() {
        let default_args = DefaultArgs::default();
        let app = add_args(clap::App::new("agave-validator"), &default_args);
        let matches = app.get_matches_from(vec!["agave-validator", "--xdp-cpu-cores", "5-3"]);
        let result = build_xdp_config(&matches, &Operation::Run, &build_single_ip_bind(), POH_CORE);
        assert!(
            result.unwrap_err().contains("selects no CPUs"),
            "empty XDP CPU core selection must produce an error"
        );
    }

    #[test]
    fn test_init_disables_xdp() {
        let default_args = DefaultArgs::default();
        let app = add_args(clap::App::new("agave-validator"), &default_args);
        let matches = app.get_matches_from(vec!["agave-validator"]);
        let result = build_xdp_config(
            &matches,
            &Operation::Initialize,
            &build_single_ip_bind(),
            POH_CORE,
        );
        assert!(
            result
                .expect("initialization should bypass XDP configuration validation")
                .is_none(),
            "init operation must disable XDP"
        );
    }

    #[test]
    fn test_multihoming_is_error() {
        let default_args = DefaultArgs::default();
        let app = add_args(clap::App::new("agave-validator"), &default_args);
        let matches = app.get_matches_from(vec!["agave-validator"]);
        let result = build_xdp_config(
            &matches,
            &Operation::Run,
            &build_multihoming_bind(),
            POH_CORE,
        );
        assert!(
            result.unwrap_err().contains("multihoming"),
            "multihoming context must produce an error"
        );
    }

    #[test]
    fn test_explicit_xdp_core_conflicts_with_poh_core_is_error() {
        let default_args = DefaultArgs::default();
        let app = add_args(clap::App::new("agave-validator"), &default_args);
        let matches = app.get_matches_from(vec!["agave-validator", "--xdp-cpu-cores", "0"]);
        let result = build_xdp_config(&matches, &Operation::Run, &build_single_ip_bind(), POH_CORE);
        assert!(
            result.unwrap_err().contains("PoH core"),
            "XDP core overlapping PoH core must produce an error"
        );
    }
}
