//! The WNA feature host.
//!
//! * [`bridge`]: runs a Wasm component behind WNA tables (generic over worlds).
//! * [`package`]: the signed feature package format.
//! * [`host`]: slots that serve a built-in fallback until a verified package
//!   is installed, then swap the Wasm instance in without the importer noticing.

pub mod bridge;
pub mod dyntable;
pub mod dynval;
pub mod host;
pub mod package;

pub use bridge::{Bridge, Limits, World};
pub use wasmtime;
pub use host::{FeatureHost, SlotConfig};

use anyhow::Result;
use wasmtime::{Config, Engine};

/// How this process executes Wasm.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Execution {
    /// Cranelift-compiled machine code (Android, desktop).
    Native,
    /// The Pulley interpreter: no JIT, no downloaded machine code (iOS).
    Pulley,
}

impl Execution {
    /// The platform default: Pulley on iOS, native elsewhere.
    pub fn platform_default() -> Self {
        if cfg!(target_os = "ios") { Execution::Pulley } else { Execution::Native }
    }

    /// Key for precompiled artifacts in a package: `pulley64` or the
    /// target triple of this process (`aarch64-linux-android`, ...).
    pub fn artifact_key(&self) -> String {
        match self {
            Execution::Pulley => "pulley64".into(),
            Execution::Native => native_triple().into(),
        }
    }
}

fn native_triple() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "android") => "aarch64-linux-android",
        ("x86_64", "android") => "x86_64-linux-android",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        _ => "unknown",
    }
}

pub fn engine(exec: Execution) -> Result<Engine> {
    let mut c = Config::new();
    c.consume_fuel(true);
    if exec == Execution::Pulley {
        c.target("pulley64")?;
        // iOS rejects multi-GiB virtual reservations: keep linear memories small.
        c.memory_reservation(16 << 20);
        c.memory_guard_size(64 << 10);
        c.memory_reservation_for_growth(1 << 20);
    }
    Ok(Engine::new(&c)?)
}

/// An engine that compiles for an artifact key (for publishing).
pub fn engine_for_artifact(key: &str) -> Result<Engine> {
    if key == "pulley64" {
        return engine(Execution::Pulley);
    }
    let mut c = Config::new();
    c.consume_fuel(true);
    c.target(key)?;
    Ok(Engine::new(&c)?)
}
