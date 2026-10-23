#![cfg(feature = "agave-unstable-api")]

//! Validator configuration schema, file loading, and structural validation.
//! Component references, host CPU assignment, and device setup are checked separately.

mod file;
pub mod interface;
pub mod xdp;

pub use {
    file::{DEFAULT_CONFIG, load},
    interface::{DeviceSelector, RouteSelector},
    xdp::QueueCpuBinding,
};
use {
    interface::{EffectiveInterface, format_interface_path},
    serde::Deserialize,
    std::collections::BTreeMap,
    xdp::{ComponentXdp, GlobalXdp, WorkerPolicy},
};

const SCHEMA_VERSION: i64 = 1;

fn validate_schema_version(version: i64) -> Result<(), String> {
    if version != SCHEMA_VERSION {
        return Err(format!(
            "schema_version {version} is unsupported; this binary supports version \
             {SCHEMA_VERSION}"
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectiveComponent {
    xdp: ComponentXdp,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveConfig {
    schema_version: i64,
    xdp: GlobalXdp,
    interfaces: BTreeMap<String, EffectiveInterface>,
    gossip: EffectiveComponent,
    repair: EffectiveComponent,
    tpu: EffectiveComponent,
    turbine: EffectiveComponent,
    votor: EffectiveComponent,
}

impl EffectiveConfig {
    pub fn is_xdp_active(&self) -> bool {
        self.xdp.enabled
    }

    /// Validate schema constraints without checking component references or host state,
    /// even when XDP is disabled. Deserialization alone does not check them.
    pub fn validate_structural(&self) -> Result<(), String> {
        validate_schema_version(self.schema_version)?;
        for (label, interface) in &self.interfaces {
            interface.xdp.workers.validate().map_err(|error| {
                format!("{}.xdp.workers: {error}", format_interface_path(label))
            })?;
            if matches!(interface.xdp.workers, WorkerPolicy::Bindings(_))
                && !matches!(interface.device, DeviceSelector::Name(_))
            {
                return Err(format!(
                    "{}.xdp.workers.bindings requires device.name; use workers.auto/workers.cpus \
                     with device.route, or name the device",
                    format_interface_path(label)
                ));
            }
        }
        for (name, component) in self.get_named_components() {
            component
                .tx
                .queues
                .validate()
                .map_err(|error| format!("{name}.xdp.tx.queues: {error}"))?;
        }
        Ok(())
    }

    fn get_named_components(&self) -> [(&'static str, &ComponentXdp); 5] {
        [
            ("gossip", &self.gossip.xdp),
            ("repair", &self.repair.xdp),
            ("tpu", &self.tpu.xdp),
            ("turbine", &self.turbine.xdp),
            ("votor", &self.votor.xdp),
        ]
    }
}

#[cfg(test)]
mod tests;
