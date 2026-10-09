#![allow(clippy::arithmetic_side_effects)]
pub use solana_test_validator as test_validator;
use {
    console::style,
    indicatif::{ProgressDrawTarget, ProgressStyle},
    std::{
        borrow::Cow,
        fmt::Display,
        fs::{self, File, OpenOptions},
        path::Path,
        time::Duration,
    },
};

pub mod admin_rpc_service;
pub mod bootstrap;
pub mod cli;
pub mod commands;
pub mod dashboard;

pub fn format_name_value(name: &str, value: &str) -> String {
    format!("{} {}", style(name).bold(), value)
}
/// Pretty print a "name value"
pub fn println_name_value(name: &str, value: &str) {
    println!("{}", format_name_value(name, value));
}

const SPINNER_TEMPLATE: &str = "{spinner:.green} {wide_msg}";
const MULTILINE_SPINNER_TEMPLATE: &str = "{spinner:.green} {msg}";

/// Creates a new process bar for processing that will take an unknown amount of time
pub fn new_spinner_progress_bar() -> ProgressBar {
    new_spinner_progress_bar_with_template(SPINNER_TEMPLATE)
}

/// Creates a spinner that preserves multiline messages instead of truncating them to one line.
pub(crate) fn new_multiline_spinner_progress_bar() -> ProgressBar {
    new_spinner_progress_bar_with_template(MULTILINE_SPINNER_TEMPLATE)
}

fn new_spinner_progress_bar_with_template(template: &str) -> ProgressBar {
    let progress_bar = indicatif::ProgressBar::new(42);
    progress_bar.set_draw_target(ProgressDrawTarget::stdout());
    progress_bar.set_style(spinner_progress_style(template));
    progress_bar.enable_steady_tick(Duration::from_millis(100));

    ProgressBar {
        progress_bar,
        is_term: console::Term::stdout().is_term(),
    }
}

fn spinner_progress_style(template: &str) -> ProgressStyle {
    ProgressStyle::default_spinner()
        .template(template)
        .expect("ProgressStyle::template direct input to be correct")
}

pub struct ProgressBar {
    progress_bar: indicatif::ProgressBar,
    is_term: bool,
}

impl ProgressBar {
    pub fn set_message<T: Into<Cow<'static, str>> + Display>(&self, msg: T) {
        if self.is_term {
            self.progress_bar.set_message(msg);
        } else {
            println!("{msg}");
        }
    }

    pub fn println<I: AsRef<str>>(&self, msg: I) {
        self.progress_bar.println(msg);
    }

    pub fn abandon_with_message<T: Into<Cow<'static, str>> + Display>(&self, msg: T) {
        if self.is_term {
            self.progress_bar.abandon_with_message(msg);
        } else {
            println!("{msg}");
        }
    }
}

/// Creates the ledger directory if needed and locks it for this process until the returned file
/// is dropped
pub fn lock_ledger(ledger_path: &Path) -> Result<File, String> {
    fs::create_dir_all(ledger_path).map_err(|err| {
        format!(
            "unable to create ledger directory {}: {err}",
            ledger_path.display()
        )
    })?;
    let lock_path = ledger_path.join("ledger.lock");
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| format!("unable to open {}: {err}", lock_path.display()))?;
    lock_file.try_lock().map_err(|err| {
        format!(
            "unable to lock {}, check if another validator is running: {err}",
            lock_path.display()
        )
    })?;
    Ok(lock_file)
}
