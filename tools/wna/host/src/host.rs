//! Feature slots. A slot is the stable table an importer binds to at
//! composition time. Behind it sits either a built-in fallback (a native
//! implementation, or nothing → `TRAP_UNAVAILABLE`) or a verified Wasm
//! instance; installing a package swaps the target atomically.

use crate::bridge::{Bridge, Limits, World};
use crate::dyntable::{Dispatch, DynTable};
use crate::package::{self, Manifest};
use crate::Execution;
use anyhow::{Context, Result, ensure};
use arc_swap::ArcSwap;
use ed25519_dalek::VerifyingKey;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, Weak};
use wasmtime::Engine;
use wasmtime::component::Component;
use wna::Trap;
use wna::abi::*;
use wna_abi::Wit;

pub struct SlotConfig {
    /// Slot name, e.g. `rules`.
    pub name: String,
    /// The world features in this slot target, `ns:pkg/world`.
    pub world: String,
    pub limits: Limits,
}

/// Traps in a row before a slot gives up on a package and falls back.
const MAX_TRAPS: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotState {
    Fallback,
    Active,
    Failed,
}

#[derive(Clone, Debug)]
pub struct SlotStatus {
    pub slot: String,
    pub world: String,
    pub state: SlotState,
    pub version: Option<String>,
    pub message: Option<String>,
}

struct Target {
    iface: Iface,
    /// Keeps a Wasm instance's table alive while calls are in flight.
    _table: Option<Arc<DynTable>>,
    wasm: bool,
}

struct Export {
    id: String,
    fallback: Option<Iface>,
    target: Arc<ArcSwap<Target>>,
    table: DynTable,
}

#[derive(Clone)]
struct Loaded {
    manifest: Manifest,
    component: Component,
}

struct SlotInner {
    current: Option<Loaded>,
    previous: Option<Loaded>,
    state: SlotState,
    message: Option<String>,
    traps: u32,
}

struct Slot {
    cfg: SlotConfig,
    world: Arc<World>,
    exports: Vec<Export>,
    inner: Mutex<SlotInner>,
}

pub struct FeatureHost {
    engine: Engine,
    exec: Execution,
    trusted: Vec<VerifyingKey>,
    app_version: String,
    natives: HashMap<String, Iface>,
    slots: Vec<Slot>,
    me: Weak<FeatureHost>,
}

/// Forwards one exported interface of a slot to its current target.
struct Forward {
    target: Arc<ArcSwap<Target>>,
    host: Weak<FeatureHost>,
    slot: usize,
}

impl Dispatch for Forward {
    unsafe fn call(&self, slot: usize, args: *const c_void, ret: *mut c_void, alloc: *const Alloc) -> Result<(), Trap> {
        let t = self.target.load_full();
        let r = unsafe { t.iface.invoke(TABLE_HEADER_SIZE + 8 * slot, args, ret, alloc) };
        if let Err(e) = &r
            && t.wasm && e.code == TRAP_PANIC
                && let Some(h) = self.host.upgrade() {
                    h.on_trap(self.slot, &e.message);
                }
        r
    }
}

impl FeatureHost {
    /// `natives`: tables for every interface the slots' worlds import, keyed
    /// by `ns:pkg/iface`. `fallbacks`: built-in implementations of exported
    /// interfaces, keyed the same way.
    pub fn new(
        wit: Arc<Wit>,
        exec: Execution,
        trusted: Vec<VerifyingKey>,
        app_version: &str,
        slots: Vec<SlotConfig>,
        natives: HashMap<String, Iface>,
        fallbacks: HashMap<String, Iface>,
    ) -> Result<Arc<FeatureHost>> {
        let engine = crate::engine(exec)?;
        let worlds: Vec<Arc<World>> = slots.iter().map(|s| World::new(wit.clone(), &s.world)).collect::<Result<_>>()?;
        for w in &worlds {
            for &i in &w.imports {
                let n = wit.iface_name(i).unwrap();
                ensure!(natives.contains_key(&n), "slot world {} imports {n}, which no native table provides", w.name());
            }
        }
        Ok(Arc::new_cyclic(|me: &Weak<FeatureHost>| {
            let slots = slots
                .into_iter()
                .zip(worlds)
                .enumerate()
                .map(|(n, (cfg, world))| {
                    let exports = world
                        .exports
                        .iter()
                        .map(|&i| {
                            let name = wit.iface_name(i).unwrap();
                            let fallback = fallbacks.get(&name).copied();
                            let target = Arc::new(ArcSwap::from_pointee(Target { iface: fallback.unwrap_or(Iface::NULL), _table: None, wasm: false }));
                            let fwd: Arc<dyn Dispatch> = Arc::new(Forward { target: target.clone(), host: me.clone(), slot: n });
                            Export { id: wit.iface_id(i), fallback, target, table: DynTable::new(wna_abi::slots(&wit, i).len(), fwd) }
                        })
                        .collect();
                    Slot { cfg, world, exports, inner: Mutex::new(SlotInner { current: None, previous: None, state: SlotState::Fallback, message: None, traps: 0 }) }
                })
                .collect();
            FeatureHost { engine, exec, trusted, app_version: app_version.to_string(), natives, slots, me: me.clone() }
        }))
    }

    /// Interfaces the host exports (one per slot world export), `ns:pkg/iface@x.y.z`.
    pub fn exports(&self) -> Vec<String> {
        self.slots.iter().flat_map(|s| s.exports.iter().map(|e| e.id.clone())).collect()
    }

    /// The stable table for an exported interface (`ns:pkg/iface`, version optional).
    pub fn export(&self, iface: &str) -> Option<Iface> {
        self.slots.iter().flat_map(|s| &s.exports).find(|e| wna::component::iface_matches(iface, &e.id)).map(|e| e.table.iface())
    }

    fn slot(&self, name: &str) -> Result<(usize, &Slot)> {
        self.slots.iter().enumerate().find(|(_, s)| s.cfg.name == name).with_context(|| format!("no slot named {name}"))
    }

    /// Verifies and installs a package into a slot. Returns the installed
    /// version. On any failure the slot keeps what it had.
    pub fn install(&self, slot: &str, bytes: &[u8]) -> Result<String> {
        let (n, s) = self.slot(slot)?;
        let pkg = package::open(bytes, &self.trusted)?;
        let m = &pkg.manifest;
        ensure!(m.world == s.world.name(), "package targets {}, slot {slot} hosts {}", m.world, s.world.name());
        if let Some(app_world) = s.world.version() {
            ensure!(
                wna::component::version_compatible(&m.world_version, &app_world),
                "package needs world {}@{}; this app provides {app_world}",
                m.world,
                m.world_version
            );
        }
        if let Some(min) = &m.min_app_version {
            ensure!(wna::component::version_compatible(min, &self.app_version), "package needs app {min} or later");
        }
        let component = self.load_component(&pkg)?;
        let loaded = Loaded { manifest: pkg.manifest.clone(), component };
        self.activate(n, &loaded)?;
        let mut inner = s.inner.lock().unwrap();
        inner.previous = inner.current.replace(loaded);
        inner.state = SlotState::Active;
        inner.message = None;
        inner.traps = 0;
        Ok(pkg.manifest.version)
    }

    fn load_component(&self, pkg: &package::Package) -> Result<Component> {
        let key = self.exec.artifact_key();
        if let Some(bytes) = pkg.precompiled.get(&key) {
            // Safe to deserialize: the artifact's hash is covered by the
            // package signature, and Wasmtime checks version/config compatibility.
            match unsafe { Component::deserialize(&self.engine, bytes) } {
                Ok(c) => return Ok(c),
                Err(e) if cfg!(feature = "cranelift") => eprintln!("wna-host: precompiled {key} unusable ({e:#}); compiling"),
                Err(e) => return Err(anyhow::Error::from(e)).context("precompiled artifact is incompatible with this runtime"),
            }
        }
        #[cfg(feature = "cranelift")]
        return Component::new(&self.engine, &pkg.component).map_err(anyhow::Error::from).context("compiling feature");
        #[cfg(not(feature = "cranelift"))]
        anyhow::bail!("package has no precompiled `{key}` artifact and this build cannot compile Wasm")
    }

    /// Instantiates a loaded package and points the slot's tables at it.
    fn activate(&self, n: usize, loaded: &Loaded) -> Result<()> {
        let s = &self.slots[n];
        let bridge = Bridge::instantiate(&self.engine, &s.world, &loaded.component, &self.natives, s.cfg.limits.clone())
            .with_context(|| format!("feature {}@{} cannot run in slot {}", loaded.manifest.name, loaded.manifest.version, s.cfg.name))?;
        for (id, t) in bridge.tables() {
            let e = s.exports.iter().find(|e| e.id == id).context("bridge table for an unknown export")?;
            let t = Arc::new(t);
            e.target.store(Arc::new(Target { iface: t.iface(), _table: Some(t), wasm: true }));
        }
        Ok(())
    }

    fn fall_back(&self, s: &Slot) {
        for e in &s.exports {
            e.target.store(Arc::new(Target { iface: e.fallback.unwrap_or(Iface::NULL), _table: None, wasm: false }));
        }
    }

    /// The bridge restarts a trapped instance on its own; repeated traps
    /// mean the package is broken, so the slot falls back to the built-in.
    fn on_trap(&self, n: usize, message: &str) {
        let s = &self.slots[n];
        let mut inner = s.inner.lock().unwrap();
        inner.traps += 1;
        inner.message = Some(message.to_string());
        if inner.traps >= MAX_TRAPS {
            self.fall_back(s);
            inner.state = SlotState::Failed;
        }
    }

    /// Reinstates the previous package, or the built-in fallback.
    pub fn rollback(&self, slot: &str) -> Result<()> {
        let (n, s) = self.slot(slot)?;
        let mut inner = s.inner.lock().unwrap();
        match inner.previous.take() {
            Some(prev) => {
                self.activate(n, &prev)?;
                inner.current = Some(prev);
                inner.state = SlotState::Active;
            }
            None => {
                self.fall_back(s);
                inner.current = None;
                inner.state = SlotState::Fallback;
            }
        }
        inner.traps = 0;
        inner.message = None;
        Ok(())
    }

    pub fn status(&self) -> Vec<SlotStatus> {
        self.slots
            .iter()
            .map(|s| {
                let i = s.inner.lock().unwrap();
                SlotStatus {
                    slot: s.cfg.name.clone(),
                    world: s.world.name(),
                    state: i.state.clone(),
                    version: i.current.as_ref().map(|l| l.manifest.version.clone()),
                    message: i.message.clone(),
                }
            })
            .collect()
    }

    pub fn arc(&self) -> Option<Arc<FeatureHost>> {
        self.me.upgrade()
    }
}
