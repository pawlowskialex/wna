//! The Wasm bridge: runs a component in Wasmtime and exposes its exports as
//! WNA tables; its imports are bound to native WNA tables. Values copy in and
//! out of linear memory, which is the price of isolation.

use crate::dyntable::{Dispatch, DynTable};
use crate::dynval::{Cx, Mem, ResKind, placeholder};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use wasmtime::component::{Component, Func, Linker, ResourceAny, ResourceType, Val};
use wasmtime::{AsContextMut, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wna::Trap;
use wna::abi::*;
use wna::handle::HandleTable;
use wna::lift::{AllocCx, RUST_ALLOC};
use wna_abi::wit_parser::{InterfaceId, Type, TypeDefKind, TypeId, WorldId};
use wna_abi::{SlotKind, Wit, slots};

/// Per-feature sandbox limits (from `wna.toml`).
#[derive(Clone, Debug)]
pub struct Limits {
    pub memory_bytes: usize,
    /// Fuel per call into the feature; `None` = unlimited.
    pub fuel_per_call: Option<u64>,
    /// After a trap, the next call gets a fresh instance (state is lost).
    /// Otherwise later calls fail with `TRAP_UNAVAILABLE`.
    pub restart: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { memory_bytes: 64 << 20, fuel_per_call: None, restart: true }
    }
}

pub struct State {
    limits: StoreLimits,
    pub(crate) guest_handles: Arc<HandleTable<ResourceAny>>,
}

/// A feature world, resolved once and shared by every instance of it.
pub struct World {
    pub wit: Arc<Wit>,
    pub id: WorldId,
    pub imports: Vec<InterfaceId>,
    pub exports: Vec<InterfaceId>,
    kinds: HashMap<TypeId, ResKind>,
}

impl World {
    pub fn new(wit: Arc<Wit>, spec: &str) -> Result<Arc<World>> {
        let id = wit.world(spec)?;
        let (imports, exports) = wit.world_interfaces(id)?;
        let mut kinds = HashMap::new();
        let mut tag = 0;
        for (list, guest) in [(&imports, false), (&exports, true)] {
            for &i in list {
                wit.check_v0(i)?;
                for &t in wit.resolve.interfaces[i].types.values() {
                    if matches!(wit.resolve.types[t].kind, TypeDefKind::Resource) {
                        kinds.insert(t, if guest { ResKind::Guest } else { ResKind::Host(tag) });
                        tag += 1;
                    }
                }
            }
        }
        Ok(Arc::new(World { wit, id, imports, exports, kinds }))
    }

    pub fn name(&self) -> String {
        let w = &self.wit.resolve.worlds[self.id];
        let p = &self.wit.resolve.packages[w.package.unwrap()].name;
        format!("{}:{}/{}", p.namespace, p.name, w.name)
    }

    pub fn version(&self) -> Option<String> {
        let w = &self.wit.resolve.worlds[self.id];
        self.wit.resolve.packages[w.package.unwrap()].name.version.as_ref().map(|v| v.to_string())
    }

    fn kind(&self, t: TypeId) -> Option<ResKind> {
        self.kinds.get(&t).copied()
    }
}

struct FuncInfo {
    params: Vec<Type>,
    offsets: Vec<usize>,
    args_size: usize,
    result: Option<Type>,
    ret_size: usize,
    what: String,
}

impl FuncInfo {
    fn new(wit: &Wit, iface: InterfaceId, f: &wna_abi::wit_parser::Function) -> Self {
        let l = wit.args_layout(f);
        FuncInfo {
            params: f.params.iter().map(|p| p.ty).collect(),
            offsets: l.offsets,
            args_size: l.layout.size,
            result: f.result,
            ret_size: f.result.map(|r| wit.layout(r).size).unwrap_or(0),
            what: format!("{}.{}", wit.iface_name(iface).unwrap_or_default(), f.name),
        }
    }
}

enum Entry {
    Func(Option<Func>, FuncInfo),
    Drop,
}

/// A Wasm feature behind WNA tables. Holds at most one live instance; a
/// trap discards it, as the component model requires.
pub struct Bridge {
    world: Arc<World>,
    engine: Engine,
    linker: Linker<State>,
    component: Component,
    limits: Limits,
    live: Mutex<Option<Live>>,
    /// Thread currently inside the instance (0 = none): a Store runs one
    /// call at a time, and re-entering it from the same thread must trap,
    /// not deadlock.
    owner: AtomicU64,
}

fn thread_tag() -> u64 {
    thread_local!(static TAG: u8 = const { 0 });
    TAG.with(|t| t as *const u8 as u64)
}

struct Live {
    store: Store<State>,
    guest_handles: Arc<HandleTable<ResourceAny>>,
    exports: Vec<Vec<Entry>>,
}

/// Builds the linker for a world: every import becomes host functions that
/// call the native table bound to it.
fn linker(engine: &Engine, world: &Arc<World>, natives: &HashMap<String, Iface>) -> Result<Linker<State>> {
    let wit = &world.wit;
    let mut linker = Linker::<State>::new(engine);
    for &iface in &world.imports {
        let id = wit.iface_id(iface);
        let name = wit.iface_name(iface).unwrap();
        let native = *natives.get(&name).with_context(|| format!("no native table for import {id}"))?;
        let mut inst = linker.instance(&id)?;
        for slot in slots(wit, iface) {
            match slot.kind {
                SlotKind::Drop(r) => {
                    let Some(ResKind::Host(tag)) = world.kind(r) else { bail!("resource in import {id} is not host-owned") };
                    let offset = slot.offset;
                    let rname = wit.resolve.types[r].name.clone().unwrap();
                    inst.resource(&rname, ResourceType::host_dynamic(tag), move |_store, rep| unsafe {
                        native.invoke(offset, &rep as *const u32 as *const c_void, std::ptr::null_mut(), &RUST_ALLOC).map_err(wasmtime::Error::new)
                    })?;
                }
                SlotKind::Func(_) => {}
            }
        }
        for slot in slots(wit, iface) {
            let SlotKind::Func(f) = slot.kind else { continue };
            let info = FuncInfo::new(wit, iface, f);
            let offset = slot.offset;
            let world = world.clone();
            inst.func_new(&f.name, move |mut store, _ty, params: &[Val], results: &mut [Val]| {
                let guest_handles = store.data().guest_handles.clone();
                let kinds = |t: TypeId| world.kind(t);
                let mut cx = Cx { wit: &world.wit, store: store.as_context_mut(), kinds: &kinds, guest_handles, lent: Vec::new() };
                let mut scratch = Vec::new();
                let mut args = vec![0u64; info.args_size.div_ceil(8)];
                let mut ret = vec![0u64; info.ret_size.div_ceil(8)];
                let r = unsafe {
                    for ((ty, off), v) in info.params.iter().zip(&info.offsets).zip(params) {
                        cx.write(*ty, v, (args.as_mut_ptr() as *mut u8).add(*off), &mut Mem::Scratch(&mut scratch))?;
                    }
                    native.invoke(
                        offset,
                        if info.params.is_empty() { std::ptr::null() } else { args.as_ptr() as *const c_void },
                        if info.result.is_some() { ret.as_mut_ptr() as *mut c_void } else { std::ptr::null_mut() },
                        &RUST_ALLOC,
                    )
                };
                cx.end_call();
                r.map_err(|t| wasmtime::format_err!("{}: {t}", info.what))?;
                if let Some(rt) = info.result {
                    results[0] = unsafe { cx.read(rt, ret.as_ptr() as *const u8, true)? };
                }
                Ok(())
            })?;
        }
    }
    Ok(linker)
}

impl Bridge {
    /// Instantiates `component` for `world`, binding its imports to `natives`
    /// (keyed by `ns:pkg/iface`). Fails if the component imports anything
    /// outside its world.
    pub fn instantiate(engine: &Engine, world: &Arc<World>, component: &Component, natives: &HashMap<String, Iface>, limits: Limits) -> Result<Arc<Bridge>> {
        let linker = linker(engine, world, natives)?;
        let live = Self::live(engine, world, &linker, component, &limits)?;
        Ok(Arc::new(Bridge { world: world.clone(), engine: engine.clone(), linker, component: component.clone(), limits, live: Mutex::new(Some(live)), owner: AtomicU64::new(0) }))
    }

    fn live(engine: &Engine, world: &Arc<World>, linker: &Linker<State>, component: &Component, limits: &Limits) -> Result<Live> {
        let wit = &world.wit;
        let guest_handles = Arc::new(HandleTable::new());
        let mut store = Store::new(
            engine,
            State { limits: StoreLimitsBuilder::new().memory_size(limits.memory_bytes).instances(16).build(), guest_handles: guest_handles.clone() },
        );
        store.limiter(|s| &mut s.limits);
        store.set_fuel(limits.fuel_per_call.unwrap_or(u64::MAX))?;
        let instance = linker.instantiate(&mut store, component).map_err(anyhow::Error::from).context("instantiating feature")?;

        // Find each exported interface by name; the component may be built
        // against an older minor than the app's world.
        let provided: Vec<String> = component.component_type().exports(engine).map(|(n, _)| n.to_string()).collect();
        let mut exports = Vec::new();
        for &iface in &world.exports {
            let want = wit.iface_id(iface);
            let found = provided.iter().find(|p| wna::component::iface_matches(p, &want) || wna::component::iface_matches(&want, p));
            let idx = found.and_then(|n| instance.get_export_index(&mut store, None, n));
            let mut entries = Vec::new();
            for slot in slots(wit, iface) {
                entries.push(match slot.kind {
                    SlotKind::Drop(_) => Entry::Drop,
                    SlotKind::Func(f) => {
                        let func = idx.as_ref().and_then(|i| instance.get_export_index(&mut store, Some(i), &f.name)).and_then(|fi| instance.get_func(&mut store, fi));
                        Entry::Func(func, FuncInfo::new(wit, iface, f))
                    }
                });
            }
            exports.push(entries);
        }
        Ok(Live { store, guest_handles, exports })
    }

    /// One table per exported interface, keyed by `ns:pkg/iface@version`.
    pub fn tables(self: &Arc<Self>) -> Vec<(String, DynTable)> {
        self.world
            .exports
            .iter()
            .enumerate()
            .map(|(i, &iface)| {
                let target: Arc<dyn Dispatch> = Arc::new(Exported { bridge: self.clone(), index: i });
                (self.world.wit.iface_id(iface), DynTable::new(slots(&self.world.wit, iface).len(), target))
            })
            .collect()
    }

    pub fn is_live(&self) -> bool {
        self.live.lock().map(|l| l.is_some()).unwrap_or(false)
    }

    unsafe fn call(&self, export: usize, slot: usize, args: *const c_void, ret: *mut c_void, alloc: *const Alloc) -> Result<(), Trap> {
        let me = thread_tag();
        if self.owner.load(Ordering::Acquire) == me {
            return Err(Trap::new(TRAP_REENTRANCE, "the feature instance is already running on this thread"));
        }
        let mut guard = self.live.lock().unwrap_or_else(|e| e.into_inner());
        self.owner.store(me, Ordering::Release);
        struct Leave<'a>(&'a AtomicU64);
        impl Drop for Leave<'_> {
            fn drop(&mut self) {
                self.0.store(0, Ordering::Release);
            }
        }
        let _leave = Leave(&self.owner);
        if guard.is_none() {
            if !self.limits.restart {
                return Err(Trap::new(TRAP_UNAVAILABLE, "the feature instance trapped earlier"));
            }
            *guard = Some(
                Self::live(&self.engine, &self.world, &self.linker, &self.component, &self.limits)
                    .map_err(|e| Trap::new(TRAP_UNAVAILABLE, format!("restarting feature: {e:#}")))?,
            );
        }
        let live = guard.as_mut().unwrap();
        let _ = live.store.set_fuel(self.limits.fuel_per_call.unwrap_or(u64::MAX));
        let world = &self.world;
        let kinds = |t: TypeId| world.kind(t);
        let mut cx = Cx { wit: &world.wit, store: live.store.as_context_mut(), kinds: &kinds, guest_handles: live.guest_handles.clone(), lent: Vec::new() };
        // Keep a WNA trap's own code (e.g. a stale handle); anything else is a bad value.
        let invalid = |e: wasmtime::Error| e.downcast_ref::<Trap>().cloned().unwrap_or_else(|| Trap::invalid(format!("{e:#}")));
        let trapped = |e: wasmtime::Error| Trap::new(TRAP_PANIC, format!("feature trapped: {e:#}"));
        let r = unsafe {
            match &live.exports[export][slot] {
                Entry::Drop => {
                    let h = *(args as *const u32);
                    {
                        let any = live.guest_handles.remove(h)?;
                        any.resource_drop(&mut cx.store).map_err(trapped)
                    }
                }
                Entry::Func(None, info) => return Err(Trap::new(TRAP_UNAVAILABLE, format!("{}: not provided by this feature version", info.what))),
                Entry::Func(Some(func), info) => {
                    let mut params = Vec::with_capacity(info.params.len());
                    for (ty, off) in info.params.iter().zip(&info.offsets) {
                        params.push(cx.read(*ty, (args as *const u8).add(*off), false).map_err(invalid)?);
                    }
                    let mut results = vec![placeholder(); info.result.is_some() as usize];
                    match func.call(&mut cx.store, &params, &mut results) {
                        Err(e) => Err(trapped(e)),
                        Ok(()) => {
                            if let Some(rt) = info.result {
                                let mut acx = AllocCx::new(&*alloc);
                                cx.write(rt, &results[0], ret as *mut u8, &mut Mem::Alloc(&mut acx)).map_err(invalid)?;
                                acx.commit();
                            }
                            cx.end_call();
                            Ok(())
                        }
                    }
                }
            }
        };
        if r.as_ref().is_err_and(|t| t.code == TRAP_PANIC) {
            // A trapped instance may not be entered again.
            *guard = None;
        }
        r
    }
}

struct Exported {
    bridge: Arc<Bridge>,
    index: usize,
}

impl Dispatch for Exported {
    unsafe fn call(&self, slot: usize, args: *const c_void, ret: *mut c_void, alloc: *const Alloc) -> Result<(), Trap> {
        unsafe { self.bridge.call(self.index, slot, args, ret, alloc) }
    }
}
