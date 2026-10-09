use {
    agave_validator_config::{self as config_file, CliApplication, CliOverrides, EffectiveConfig},
    clap::ArgMatches,
    log::*,
    solana_clap_utils::input_parsers::parse_cpu_ranges,
    solana_net_utils::multihomed_sockets::BindIpAddrs,
};

/// Applies the XDP command line flags to the policy and validates it, returning the policy when
/// XDP is active on this platform
pub(super) fn new_config(
    file: EffectiveConfig,
    matches: &ArgMatches,
    bind_addresses: &BindIpAddrs,
) -> Result<Option<EffectiveConfig>, String> {
    let CliApplication { config, warnings } =
        config_file::apply_cli(file, cli_overrides(matches)?)?;
    for warning in warnings {
        warn!("{warning}");
    }
    if config.is_xdp_active() && cfg!(target_os = "linux") && bind_addresses.len() > 1 {
        return Err(
            "XDP does not support multiple --bind-address values; select one IPv4 address, or \
             pass --no-xdp"
                .into(),
        );
    }
    for warning in config_file::validate_policy(&config)? {
        warn!("{warning}");
    }
    if !config.is_xdp_active() {
        return Ok(None);
    }
    if cfg!(not(target_os = "linux")) {
        // Only report inactivity the operator can act on. The built-in policy enables
        // XDP everywhere, so warning about it unprompted would fire on every startup.
        if matches.is_present("config_file") {
            warn!(
                "XDP transmit is unavailable on this platform; the configured XDP policy is valid \
                 but inactive"
            );
        }
        return Ok(None);
    }
    Ok(Some(config))
}

fn cli_overrides(matches: &ArgMatches) -> Result<CliOverrides, String> {
    let zero_copy = if matches.is_present("xdp_zero_copy") {
        Some(true)
    } else if matches.is_present("no_xdp_zero_copy") {
        Some(false)
    } else {
        None
    };
    Ok(CliOverrides {
        no_xdp: matches.is_present("no_xdp"),
        interface: matches.value_of("xdp_interface").map(str::to_string),
        cpu_cores: matches
            .value_of("xdp_cpu_cores")
            .map(|value| {
                parse_cpu_ranges(value)
                    .map_err(|error| format!("invalid --xdp-cpu-cores `{value}`: {error}"))
            })
            .transpose()?,
        zero_copy,
    })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{cli::DefaultArgs, commands::run::args::add_args},
        std::{io::Write, net::Ipv4Addr},
        tempfile::NamedTempFile,
    };

    fn ipv4_bind() -> BindIpAddrs {
        BindIpAddrs::new(vec![Ipv4Addr::UNSPECIFIED.into()]).unwrap()
    }

    fn new_config_from(
        file: Option<&str>,
        args: &[&str],
        bind_addresses: &BindIpAddrs,
    ) -> Result<Option<EffectiveConfig>, String> {
        let file = file.map(|contents| {
            let mut file = NamedTempFile::new().unwrap();
            file.write_all(contents.as_bytes()).unwrap();
            file
        });
        let config = config_file::load(file.as_ref().map(NamedTempFile::path))?;
        let default_args = DefaultArgs::default();
        let matches = add_args(clap::App::new("test"), &default_args)
            .get_matches_from(["test"].iter().chain(args));
        new_config(config, &matches, bind_addresses)
    }

    #[test]
    fn test_disabled_xdp_skips_host_checks() {
        // An active policy would fail both CPU and device resolution.
        let file = |enabled| {
            format!(
                "schema_version = 1\n[xdp]\nenabled = \
                 {enabled}\n[interfaces.primary]\ndevice.name = \
                 \"nosuchnic0\"\n[interfaces.primary.xdp]\nworkers.cpus = [4294967295]\n"
            )
        };
        for (case, file, args) in [
            ("CLI", file(true), &["--no-xdp"][..]),
            ("file", file(false), &[]),
        ] {
            for addresses in [
                vec![Ipv4Addr::UNSPECIFIED.into()],
                vec![
                    Ipv4Addr::new(192, 0, 2, 1).into(),
                    Ipv4Addr::new(192, 0, 2, 2).into(),
                ],
            ] {
                let binds = BindIpAddrs::new(addresses).unwrap();
                assert!(
                    new_config_from(Some(&file), args, &binds)
                        .unwrap()
                        .is_none(),
                    "{case}"
                );
            }
        }
    }

    #[test]
    fn test_policy_is_validated() {
        for (file, expected_error) in [
            (None, None),
            (Some("schema_version = 1"), None),
            (
                Some("schema_version = 1\n[tpu.xdp]\ntx.queues = [1]"),
                Some("tpu.xdp.tx.queues references queue(s) 1 not declared"),
            ),
            (
                Some(
                    "schema_version = 1\n[xdp]\nenabled = false\n[tpu.xdp]\ntx.queues = \
                     [1]\ntx.interface = \"other\"",
                ),
                None,
            ),
        ] {
            let result = new_config_from(file, &[], &ipv4_bind());
            match expected_error {
                Some(expected) => {
                    let error = result.unwrap_err();
                    assert!(error.contains(expected), "{file:?}: {error}");
                }
                None => {
                    result.unwrap();
                }
            }
        }
    }

    #[test]
    fn test_zero_copy_cli_flags_are_parsed_and_conflicts_rejected() {
        use clap::ErrorKind::ArgumentConflict;

        let default_args = DefaultArgs::default();
        let app = add_args(clap::App::new("test"), &default_args);
        for (flags, expected) in [
            (vec![], Ok(None)),
            (vec!["--xdp-zero-copy"], Ok(Some(true))),
            (vec!["--no-xdp-zero-copy"], Ok(Some(false))),
            (
                vec!["--xdp-zero-copy", "--no-xdp-zero-copy"],
                Err(ArgumentConflict),
            ),
            (
                vec!["--no-xdp", "--no-xdp-zero-copy"],
                Err(ArgumentConflict),
            ),
        ] {
            let result = app
                .clone()
                .get_matches_from_safe(["test"].iter().chain(&flags))
                .map(|matches| cli_overrides(&matches).unwrap().zero_copy)
                .map_err(|error| error.kind);
            assert_eq!(result, expected, "{flags:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_multihoming_is_rejected() {
        let binds = BindIpAddrs::new(vec![
            Ipv4Addr::new(192, 0, 2, 1).into(),
            Ipv4Addr::new(192, 0, 2, 2).into(),
        ])
        .unwrap();
        let error = new_config_from(None, &[], &binds).unwrap_err();
        assert!(
            error.starts_with("XDP does not support multiple --bind-address values"),
            "{error}"
        );
    }

    #[test]
    fn test_empty_cli_cpu_selection_is_rejected() {
        let error = new_config_from(None, &["--xdp-cpu-cores", "5-3"], &ipv4_bind()).unwrap_err();
        assert!(
            error.contains("--xdp-cpu-cores must contain between 1") && error.contains("found 0"),
            "{error}"
        );
    }
}
