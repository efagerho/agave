use {
    super::*,
    crate::xdp::{MAX_XDP_WORKERS, WorkerPolicy},
    std::io::Write as _,
};

fn load_config(contents: &str) -> Result<EffectiveConfig, String> {
    let mut file = tempfile::NamedTempFile::new().expect("create temporary config file for test");
    file.write_all(contents.as_bytes())
        .expect("write test config to temporary file");
    load(Some(file.path()))
}

fn load_valid_config(contents: &str) -> EffectiveConfig {
    load_config(contents)
        .unwrap_or_else(|error| panic!("invalid test config:\n{contents}\n{error}"))
}

fn load_worker_config(workers: &str) -> Result<EffectiveConfig, String> {
    load_config(&format!(
        r#"
schema_version = 1
[interfaces.primary]
device.name = "eth0"
[interfaces.primary.xdp]
workers = {workers}
"#
    ))
}

#[test]
fn test_embedded_defaults_pass_structural_validation() {
    for config in [load(None).unwrap(), load_valid_config("schema_version = 1")] {
        assert!(config.is_xdp_active());
        assert_eq!(config.interfaces.len(), 1);
        let interface = &config.interfaces["primary"];
        assert_eq!(
            interface.device,
            DeviceSelector::Route(RouteSelector::Default)
        );
        assert!(!interface.xdp.zero_copy);
        assert_eq!(interface.xdp.workers, WorkerPolicy::Auto { count: 1 });
        for (_, component) in config.get_named_components() {
            assert_eq!(component.tx.interface, "primary");
            assert!(matches!(component.tx.queues, xdp::QueueSelection::All));
        }
    }
}

#[test]
fn test_matching_interface_inherits_unspecified_fields() {
    let config = load_valid_config("schema_version = 1\ninterfaces.primary.xdp.zero_copy = true");
    assert_eq!(config.interfaces.len(), 1);
    let interface = &config.interfaces["primary"];
    assert_eq!(
        interface.device,
        DeviceSelector::Route(RouteSelector::Default)
    );
    assert!(interface.xdp.zero_copy);
    assert_eq!(interface.xdp.workers, WorkerPolicy::Auto { count: 1 });
}

#[test]
fn test_device_replaces_atomically() {
    let config = load_valid_config("schema_version = 1\ninterfaces.primary.device.name = 'eth0'");
    assert_eq!(
        config.interfaces["primary"].device,
        DeviceSelector::Name("eth0".to_string()),
    );
}

#[test]
fn test_interface_labels_replace_defaults() {
    let renamed = load_valid_config(
        r#"
schema_version = 1
[interfaces.fast]
device.name = "eth0"
xdp.zero_copy = false
xdp.workers.cpus = [8]
"#,
    );
    assert_eq!(renamed.interfaces.len(), 1);
    assert!(renamed.interfaces.contains_key("fast"));

    let empty = load_valid_config("schema_version = 1\ninterfaces = {}");
    assert!(empty.interfaces.is_empty());
}

#[test]
fn test_unknown_fields_are_rejected() {
    for field in [
        "unknown",
        "xdp.unknown",
        "interfaces.primary.unknown",
        "interfaces.primary.xdp.unknown",
        "gossip.xdp.tx.unknown",
    ] {
        let error = load_config(&format!("schema_version = 1\n{field} = true")).unwrap_err();
        assert!(
            error.contains("unknown field `unknown`"),
            "{field}: {error}"
        );
    }
}

#[test]
fn test_new_interface_error_identifies_missing_field() {
    let error = load_config(
        r#"
schema_version = 1
[interfaces.fast.xdp]
zero_copy = false
"#,
    )
    .unwrap_err();
    assert!(error.contains("missing field `workers`"), "{error}");
    assert!(error.contains("interfaces.fast.xdp"), "{error}");
}

#[test]
fn test_workers_replace_atomically() {
    let config = load_valid_config(
        r#"
schema_version = 1
[interfaces.primary.xdp]
workers.cpus = [8, 9]
"#,
    );
    assert_eq!(
        config.interfaces["primary"].xdp.workers,
        WorkerPolicy::Cpus(vec![8, 9])
    );
}

#[test]
fn test_invalid_selectors_are_rejected_after_merge() {
    for label in ["primary", "fast"] {
        for (field, value, expected) in [
            (
                "device",
                r#"{ route = "default", name = "eth0" }"#,
                "more than 1 element",
            ),
            ("device", r#"{ route = "other" }"#, "expected `default`"),
            ("device", "{}", "found 0 elements"),
            (
                "xdp.workers",
                "{ auto = { count = 1 }, cpus = [8] }",
                "more than 1 element",
            ),
        ] {
            let contents = format!(
                r#"
schema_version = 1
[interfaces.{label}]
{field} = {value}
"#
            );
            let error = load_config(&contents).unwrap_err();
            assert!(error.contains(expected), "{contents}: {error}");
            assert!(
                error.contains(&format!("interfaces.{label}.{field}")),
                "{contents}: {error}"
            );
        }
    }
}

#[test]
fn test_invalid_worker_policies_are_rejected() {
    for (workers, expected) in [
        ("{}", "found 0 elements"),
        ("{ unused = \"warn\" }", "unknown variant `unused`"),
        (
            "{ auto = { count = 1, unused = true } }",
            "unknown field `unused`",
        ),
        (
            "{ cpus = [8, 9, 8] }",
            "workers.cpus contains duplicate CPU 8",
        ),
        (
            "{ bindings = [{ queue = 3, cpu = 8 }, { queue = 3, cpu = 9 }] }",
            "workers.bindings contains duplicate queue 3",
        ),
        (
            "{ bindings = [{ queue = 3, cpu = 8 }, { queue = 7, cpu = 8 }] }",
            "workers.bindings contains duplicate CPU 8",
        ),
        (
            "{ auto = { count = 1 }, bindings = [{ queue = 0, cpu = 8 }] }",
            "more than 1 element",
        ),
        (
            "{ cpus = [8], bindings = [{ queue = 0, cpu = 8 }] }",
            "more than 1 element",
        ),
    ] {
        let error = load_worker_config(workers).unwrap_err();
        assert!(error.contains(expected), "{workers}: {error}");
        assert!(error.contains("interfaces.primary.xdp.workers"), "{error}");
    }
}

#[test]
fn test_bindings_require_named_device() {
    let error = load_config(
        r#"
schema_version = 1
[interfaces.primary.xdp]
workers.bindings = [{ queue = 0, cpu = 8 }]
"#,
    )
    .unwrap_err();
    assert!(
        error.contains("workers.bindings requires device.name"),
        "{error}"
    );
}

#[test]
fn test_invalid_schema_versions_are_rejected() {
    for (contents, expected) in [
        ("[xdp]\nenabled = false", "missing required schema_version"),
        ("schema_version = 2", "supports version 1"),
        ("schema_version = \"one\"", "non-integer schema_version"),
    ] {
        let error = load_config(contents).unwrap_err();
        assert!(error.contains(expected), "{contents}: {error}");
    }
}

#[test]
fn test_every_worker_mode_enforces_the_same_cardinality_limit() {
    for (count, should_succeed) in [
        (0, false),
        (1, true),
        (MAX_XDP_WORKERS, true),
        (MAX_XDP_WORKERS + 1, false),
    ] {
        // Use structured arrays for the cases with thousands of workers.
        let cpus: Vec<_> = (0..count).collect();
        let bindings = (0..count)
            .map(|cpu| {
                toml::toml! {
                    queue = (cpu)
                    cpu = (cpu)
                }
            })
            .collect::<Vec<_>>();
        for (field, policy) in [
            ("auto.count", toml::toml! { auto.count = (count) }),
            ("cpus", toml::toml! { cpus = (cpus) }),
            ("bindings", toml::toml! { bindings = (bindings) }),
        ] {
            let result = policy
                .try_into::<WorkerPolicy>()
                .expect("worker fixture should deserialize before cardinality validation")
                .validate();
            if should_succeed {
                result.unwrap_or_else(|error| panic!("{field}/{count}: {error}"));
            } else {
                let error = result.unwrap_err();
                let expected = format!(
                    "workers.{field} must contain between 1 and {MAX_XDP_WORKERS} workers; found \
                     {count}"
                );
                assert!(error.contains(&expected), "{field}/{count}: {error}");
            }
        }
    }
}

#[test]
fn test_component_level_switches_are_rejected() {
    for component in ["tpu", "turbine", "repair", "gossip", "votor"] {
        let error = load_config(&format!(
            r#"
schema_version = 1
[{component}.xdp]
enabled = true
"#
        ))
        .unwrap_err();
        assert!(
            error.contains("unknown field `enabled`"),
            "{component}: {error}"
        );
    }
}

#[test]
fn test_invalid_queue_selections_are_rejected() {
    let syntax_error = "non-negative 32-bit queue IDs";
    let too_many: Vec<_> = (0..=MAX_XDP_WORKERS).collect();
    for (queues, expected) in [
        ("true", syntax_error),
        ("\"other\"", syntax_error),
        ("[]", "tx.queues must not be an empty queue list"),
        ("[3, 3]", "tx.queues contains duplicate queue 3"),
        ("[-1]", syntax_error),
        ("[4294967296]", syntax_error),
        ("[\"zero\"]", syntax_error),
        (
            &format!("{too_many:?}"),
            "tx.queues exceeds MAX_XDP_WORKERS",
        ),
    ] {
        for component in ["tpu", "turbine", "repair", "gossip", "votor"] {
            for enabled in [true, false] {
                let error = load_config(&format!(
                    r#"
schema_version = 1
xdp.enabled = {enabled}
[{component}.xdp]
tx.queues = {queues}
"#
                ))
                .unwrap_err();
                assert!(
                    error.contains(expected),
                    "{component}, XDP enabled={enabled}, queues={queues}: {error}"
                );
                assert!(
                    error.contains(&format!("{component}.xdp.tx.queues")),
                    "{error}"
                );
            }
        }
    }
}
