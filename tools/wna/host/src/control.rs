//! `wna:host/features`: the shell's handle on downloadable features.

use crate::FeatureHost;
use crate::host::SlotState;
use std::sync::{Arc, OnceLock, Weak};
use wna::Trap;
use wna::abi::Iface;

wna::generate!({ world: "wna:host/control", path: "../../../wit", name: "wna:feature-host" });

use exports::wna::host::features::{Guest, SlotStatus, State};

pub const ID: &str = "wna:host/features@0.1.0";

/// One feature host per process serves the control interface.
static HOST: OnceLock<Weak<FeatureHost>> = OnceLock::new();

fn host() -> Result<Arc<FeatureHost>, String> {
    HOST.get().and_then(Weak::upgrade).ok_or_else(|| "no feature host".to_string())
}

/// Binds the control interface to `host` and returns its table.
pub fn bind(host: &Arc<FeatureHost>) -> Result<Iface, Trap> {
    HOST.set(Arc::downgrade(host)).map_err(|_| Trap::new(wna::abi::TRAP_UNAVAILABLE, "only one feature host per process"))?;
    let d = unsafe { wna::compose::Descriptor::from_ptr(wna_feature_host__wna_component()) };
    d.instantiate(&[])?.export(ID)
}

struct Control;

impl Guest for Control {
    fn install(slot: String, package: Vec<u8>) -> Result<String, String> {
        host()?.install(&slot, &package).map_err(|e| format!("{e:#}"))
    }

    fn rollback(slot: String) -> Result<(), String> {
        host()?.rollback(&slot).map_err(|e| format!("{e:#}"))
    }

    fn status() -> Vec<SlotStatus> {
        let Ok(h) = host() else { return Vec::new() };
        h.status()
            .into_iter()
            .map(|s| SlotStatus {
                slot: s.slot,
                world: s.world,
                state: match s.state {
                    SlotState::Fallback => State::Fallback,
                    SlotState::Active => State::Active,
                    SlotState::Failed => State::Failed,
                },
                version: s.version,
                message: s.message,
            })
            .collect()
    }
}

export!(Control);
