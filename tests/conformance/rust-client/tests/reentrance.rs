//! A callback that re-enters the Wasm instance on the same thread must trap
//! with TRAP_REENTRANCE instead of deadlocking.

mod common;

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, OnceLock};
use wna::abi::*;
use wna::Trap;
use wna_host::dyntable::{Dispatch, DynTable};
use wna_host::wasmtime::component::Component;
use wna_host::{Bridge, Execution, Limits, World};

/// A sink whose `on-event` calls `types.echo-string` on the server that is
/// calling it.
struct ReenteringSink {
    names: Vec<String>,
    types: Arc<OnceLock<(Iface, usize)>>,
}

impl Dispatch for ReenteringSink {
    unsafe fn call(&self, slot: usize, _args: *const c_void, ret: *mut c_void, alloc: *const Alloc) -> Result<(), Trap> {
        match self.names[slot].as_str() {
            "listener_new" => unsafe { *(ret as *mut u32) = 1 },
            "listener_count" => unsafe { *(ret as *mut u32) = 0 },
            "listener_on_event" => {
                let (types, offset) = *self.types.get().unwrap();
                let args = WnaList::<u8> { ptr: b"hi".as_ptr() as *mut u8, len: 2 };
                let mut out = WnaList::<u8>::EMPTY;
                return unsafe { types.invoke(offset, &args as *const _ as *const c_void, &mut out as *mut _ as *mut c_void, alloc) };
            }
            _ => {}
        }
        Ok(())
    }
}

#[test]
fn reentering_a_busy_instance_traps() {
    let wit = Arc::new(wna_abi::Wit::load(common::root().join("wit")).unwrap());
    let sink_iface = wit.interface("wna:test/sink").unwrap();
    let types_iface = wit.interface("wna:test/types").unwrap();
    let echo = wna_abi::slots(&wit, types_iface).into_iter().find(|s| s.name == "echo_string").unwrap().offset;

    let cell = Arc::new(OnceLock::new());
    let names = wna_abi::slots(&wit, sink_iface).into_iter().map(|s| s.name).collect();
    let sink = DynTable::new(4, Arc::new(ReenteringSink { names, types: cell.clone() }));

    let bytes = std::fs::read(common::root().join("out/wasm/wna-test-rust-server.wasm")).expect("run `just wasm-test-server` first");
    let engine = wna_host::engine(Execution::Native).unwrap();
    let world = World::new(wit, "wna:test/server").unwrap();
    let natives = HashMap::from([("wna:test/sink".to_string(), sink.iface())]);
    let bridge = Bridge::instantiate(&engine, &world, &Component::new(&engine, &bytes).unwrap(), &natives, Limits::default()).unwrap();
    let tables = bridge.tables();
    let types = tables[0].1.iface();
    cell.set((types, echo)).unwrap();

    // emit(to: borrow<listener>, events) → listener.on-event → types.echo-string (re-entry).
    let emit = wna_abi::slots(&world.wit, types_iface).into_iter().find(|s| s.name == "emit").unwrap().offset;
    let ev = WnaList::<u8> { ptr: b"e".as_ptr() as *mut u8, len: 1 };
    #[repr(C)]
    struct EmitArgs {
        to: u32,
        events: WnaList<WnaList<u8>>,
    }
    let args = EmitArgs { to: 1, events: WnaList { ptr: &ev as *const _ as *mut _, len: 1 } };
    let mut ret = 0u32;
    let err = unsafe { types.invoke(emit, &args as *const _ as *const c_void, &mut ret as *mut _ as *mut c_void, &wna::lift::RUST_ALLOC) }.unwrap_err();
    assert!(err.message.contains("already running"), "{err}");
}
