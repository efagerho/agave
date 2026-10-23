//! XDP configuration types and structural validation.

use {serde::Deserialize, std::collections::BTreeSet};

pub(crate) const MAX_XDP_WORKERS: usize = 4096;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WorkerPolicy {
    Auto { count: usize },
    Cpus(Vec<usize>),
    Bindings(Vec<QueueCpuBinding>),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueCpuBinding {
    pub queue: u32,
    pub cpu: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    rename_all = "snake_case",
    expecting = "\"all\" or an array of non-negative 32-bit queue IDs"
)]
pub(crate) enum QueueSelection {
    All,
    #[serde(untagged)]
    Explicit(Vec<u32>),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InterfaceXdp {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Consumed by runtime resolution in a follow-up PR")
    )]
    pub(crate) zero_copy: bool,
    pub(crate) workers: WorkerPolicy,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComponentXdp {
    pub(crate) tx: ComponentTx,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComponentTx {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Checked by policy validation in a follow-up PR")
    )]
    pub(crate) interface: String,
    pub(crate) queues: QueueSelection,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GlobalXdp {
    pub(crate) enabled: bool,
}

pub(crate) fn validate_pool_len(len: usize, field: &str) -> Result<(), String> {
    if len == 0 || len > MAX_XDP_WORKERS {
        return Err(format!(
            "{field} must contain between 1 and {MAX_XDP_WORKERS} workers; found {len}"
        ));
    }
    Ok(())
}

pub(crate) fn validate_unique_cpus(cpus: &[usize], field: &str) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    if let Some(cpu) = cpus.iter().find(|cpu| !seen.insert(**cpu)) {
        return Err(format!("{field} contains duplicate CPU {cpu}"));
    }
    Ok(())
}

impl WorkerPolicy {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Auto { count } => validate_pool_len(*count, "workers.auto.count"),
            Self::Cpus(cpus) => {
                validate_pool_len(cpus.len(), "workers.cpus")?;
                validate_unique_cpus(cpus, "workers.cpus")
            }
            Self::Bindings(bindings) => {
                validate_pool_len(bindings.len(), "workers.bindings")?;
                let mut queues = BTreeSet::new();
                let mut cpus = BTreeSet::new();
                for QueueCpuBinding { queue, cpu } in bindings {
                    if !queues.insert(queue) {
                        return Err(format!("workers.bindings contains duplicate queue {queue}"));
                    }
                    if !cpus.insert(cpu) {
                        return Err(format!("workers.bindings contains duplicate CPU {cpu}"));
                    }
                }
                Ok(())
            }
        }
    }
}

impl QueueSelection {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let Self::Explicit(queues) = self else {
            return Ok(());
        };
        if queues.is_empty() {
            return Err("tx.queues must not be an empty queue list".to_string());
        }
        if queues.len() > MAX_XDP_WORKERS {
            return Err(format!(
                "tx.queues exceeds MAX_XDP_WORKERS ({MAX_XDP_WORKERS})"
            ));
        }
        let mut seen = BTreeSet::new();
        for queue in queues {
            if !seen.insert(*queue) {
                return Err(format!("tx.queues contains duplicate queue {queue}"));
            }
        }
        Ok(())
    }
}
