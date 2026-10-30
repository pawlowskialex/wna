//! Running a composition plan. `wna::compose!` validates a
//! `wna.compose.toml` at build time (every import satisfied by a compatible
//! version, no cycles) and emits a [`Plan`] in instantiation order; this
//! module executes it at startup.

use crate::{Execution, FeatureHost, Limits, SlotConfig};
use std::collections::HashMap;
use std::sync::Arc;
use wna::Trap;
use wna::abi::*;
use wna::compose::Descriptor;

pub struct Plan {
    /// The WIT the app was built against (for the feature host's bridge).
    pub wit: &'static [(&'static str, &'static [(&'static str, &'static str)])],
    /// The shell world's exports, in declaration order: what `compose` receives.
    pub shell_exports: &'static [&'static str],
    /// The shell world's imports, in declaration order, and the instance
    /// that provides each: what `compose` returns.
    pub shell_imports: &'static [(&'static str, &'static str)],
    /// Instances in dependency order.
    pub instances: &'static [Instance],
}

pub struct Instance {
    pub name: &'static str,
    pub kind: Kind,
    /// `(ns:pkg/iface, providing instance)`.
    pub imports: &'static [(&'static str, &'static str)],
}

pub enum Kind {
    /// A component, reached through its descriptor symbol.
    Component(unsafe extern "C" fn() -> *const Component),
    /// The built-in feature host.
    FeatureHost { app_version: &'static str, trusted_keys: &'static [&'static str], slots: &'static [Slot] },
}

pub struct Slot {
    pub name: &'static str,
    pub world: &'static str,
    /// Instance providing the built-in implementation of the slot's exports.
    pub fallback: Option<&'static str>,
    pub memory_mb: usize,
    pub fuel: Option<u64>,
}

/// A composed app: the shell's imports, in the shell world's order.
pub struct App {
    pub imports: Vec<Iface>,
    pub features: Option<Arc<FeatureHost>>,
}

type Exports = Vec<(String, Iface)>;

fn find(exports: &HashMap<&str, Exports>, from: &str, iface: &str) -> Result<Iface, Trap> {
    let list = exports.get(from).ok_or_else(|| Trap::new(TRAP_UNAVAILABLE, format!("instance {from} is not composed yet")))?;
    list.iter()
        .find(|(id, _)| wna::component::iface_matches(iface, id))
        .map(|(_, i)| *i)
        .ok_or_else(|| Trap::new(TRAP_UNAVAILABLE, format!("{from} does not export {iface}")))
}

/// Instantiates every component of `plan`. `shell` holds the shell's
/// exports in the order of `plan.shell_exports`.
pub fn compose(plan: &Plan, shell: &[Iface]) -> Result<App, Trap> {
    if shell.len() != plan.shell_exports.len() {
        return Err(Trap::new(TRAP_UNAVAILABLE, format!("shell supplied {} exports, the shell world has {}", shell.len(), plan.shell_exports.len())));
    }
    let mut exports: HashMap<&str, Exports> = HashMap::new();
    exports.insert("shell", plan.shell_exports.iter().map(|s| s.to_string()).zip(shell.iter().copied()).collect());
    let mut features = None;

    for inst in plan.instances {
        let resolve = |iface: &str| -> Result<Iface, Trap> {
            let (_, from) = inst
                .imports
                .iter()
                .find(|(name, _)| wna::component::iface_matches(name, iface) || wna::component::iface_matches(iface, name))
                .ok_or_else(|| Trap::new(TRAP_UNAVAILABLE, format!("{}: no wiring for import {iface}", inst.name)))?;
            find(&exports, from, iface)
        };
        let provided: Exports = match &inst.kind {
            Kind::Component(desc) => {
                let d = unsafe { Descriptor::from_ptr(desc()) };
                let imports = d.imports().iter().map(|i| resolve(i)).collect::<Result<Vec<_>, _>>()?;
                let i = d.instantiate(&imports)?;
                d.exports().iter().map(|e| Ok((e.to_string(), i.export(e)?))).collect::<Result<_, Trap>>()?
            }
            Kind::FeatureHost { app_version, trusted_keys, slots } => {
                let wit = Arc::new(wna_abi::Wit::from_sources(plan.wit).map_err(|e| Trap::invalid(format!("embedded WIT: {e:#}")))?);
                let mut natives = HashMap::new();
                for (name, _) in inst.imports {
                    natives.insert(name.to_string(), resolve(name)?);
                }
                let mut fallbacks = HashMap::new();
                let mut cfgs = Vec::new();
                for s in *slots {
                    if let Some(fb) = s.fallback {
                        let w = wit.world(s.world).map_err(|e| Trap::invalid(e.to_string()))?;
                        let (_, ex) = wit.world_interfaces(w).map_err(|e| Trap::invalid(e.to_string()))?;
                        for i in ex {
                            let n = wit.iface_name(i).unwrap();
                            fallbacks.insert(n.clone(), find(&exports, fb, &n)?);
                        }
                    }
                    cfgs.push(SlotConfig {
                        name: s.name.to_string(),
                        world: s.world.to_string(),
                        limits: Limits { memory_bytes: s.memory_mb << 20, fuel_per_call: s.fuel, restart: true },
                    });
                }
                let keys = trusted_keys
                    .iter()
                    .map(|k| crate::package::verifying_key_from_hex(k))
                    .collect::<anyhow::Result<Vec<_>>>()
                    .map_err(|e| Trap::invalid(format!("trusted key: {e:#}")))?;
                let host = FeatureHost::new(wit, Execution::platform_default(), keys, app_version, cfgs, natives, fallbacks)
                    .map_err(|e| Trap::new(TRAP_UNAVAILABLE, format!("feature host: {e:#}")))?;
                let mut list: Exports = host.exports().into_iter().map(|id| { let i = host.export(&id).unwrap(); (id, i) }).collect();
                list.push((crate::control::ID.to_string(), crate::control::bind(&host)?));
                features = Some(host);
                list
            }
        };
        exports.insert(inst.name, provided);
    }

    let imports = plan.shell_imports.iter().map(|(iface, from)| find(&exports, from, iface)).collect::<Result<_, _>>()?;
    Ok(App { imports, features })
}
