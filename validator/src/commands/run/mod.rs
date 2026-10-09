pub mod args;
#[cfg(target_os = "linux")]
pub mod capabilities;
mod config;
mod execute;
mod network;
mod storage;
#[cfg(target_os = "linux")]
mod xdp;

pub use {args::add_args, execute::execute};

#[derive(Debug, PartialEq, Eq)]
pub enum Operation {
    Initialize,
    Run,
}

/// The capabilities the validator keeps from process startup
pub struct ProcessCapabilities {
    #[cfg(target_os = "linux")]
    pub primordial_caps: caps::CapsHashSet,
}
