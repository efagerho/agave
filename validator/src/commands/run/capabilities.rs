use {
    super::ProcessCapabilities,
    caps::{
        CapSet,
        Capability::{
            CAP_BPF, CAP_DAC_OVERRIDE, CAP_NET_ADMIN, CAP_NET_RAW, CAP_PERFMON, CAP_SYS_NICE,
        },
        CapsHashSet,
    },
    log::warn,
    solana_core::validator::ValidatorConfig,
    std::error::Error,
};

/// Drops the capabilities that no subcommand needs, keeping the ones the validator may need when
/// `runs_validator`
pub fn restrict_at_startup(runs_validator: bool) -> ProcessCapabilities {
    // we don't aim to execve from the validator, so we can clear most of the
    // capability sets. clear the ambient set explicitly even though it is
    // implicitly cleared by clearing the inheritable set
    caps::clear(None, CapSet::Ambient).expect("linux allows ambient capset to be cleared");
    caps::clear(None, CapSet::Inheritable).expect("linux allows inheritable capset to be cleared");

    // we'll raise caps when and where we need them
    let primordial_caps = if runs_validator {
        // the CAP_DAC_OVERRIDE (file permissions bypass) cap isn't typically needed.
        // however if it is already in our effective set, likely due to the operator
        // foolishly running the node as root, either via sudo or suid, we need to
        // retain it to ensure expected filesystem accessibility
        let effective =
            caps::read(None, CapSet::Effective).expect("linux allows effective capset to be read");
        let primordial_caps: CapsHashSet = [CAP_DAC_OVERRIDE]
            .into_iter()
            .filter(|cap| effective.contains(cap))
            .collect();

        caps::set(None, CapSet::Effective, &primordial_caps)
            .expect("linux allows effective capset to be set");

        primordial_caps
    } else {
        caps::clear(None, CapSet::Effective).expect("linux allows effective capset to be cleared");
        // we only need caps to run the actual valididator. clear them here
        // for all other subcommands
        caps::clear(None, CapSet::Permitted).expect("linux allows permitted capset to be cleared");

        CapsHashSet::new()
    };

    ProcessCapabilities { primordial_caps }
}

/// Drops every capability the configuration does not require, holding the ones XDP needs only
/// while `with_xdp_caps` runs. `xdp_zero_copy` is `None` when XDP is disabled.
pub(super) fn restrict<T>(
    config: &ValidatorConfig,
    process_capabilities: ProcessCapabilities,
    xdp_zero_copy: Option<bool>,
    with_xdp_caps: impl FnOnce() -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    let ProcessCapabilities { primordial_caps } = process_capabilities;
    let mut supported_caps = primordial_caps.clone();
    supported_caps.extend([
        CAP_BPF,
        CAP_NET_ADMIN,
        CAP_NET_RAW,
        CAP_PERFMON,
        CAP_SYS_NICE,
    ]);
    let mut retained_caps = primordial_caps;
    if config.snapshot_packager_niceness_adj != 0 || config.rpc_config.rpc_niceness_adj != 0 {
        retained_caps.insert(CAP_SYS_NICE);
    }
    let mut required_caps = retained_caps.clone();
    if let Some(zero_copy) = xdp_zero_copy {
        required_caps.extend([CAP_NET_ADMIN, CAP_NET_RAW]);
        if zero_copy {
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
    let result = with_xdp_caps();
    // we're done with caps needed to init xdp now. remove them from our process
    set_caps(&retained_caps)?;
    result
}

fn set_caps(caps: &CapsHashSet) -> Result<(), caps::errors::CapsError> {
    caps::set(None, CapSet::Effective, caps)?;
    caps::set(None, CapSet::Permitted, caps)
}
