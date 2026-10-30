//! Native side of the Kotlin conformance tests: the Rust server (composed
//! with the Rust sink) and the Rust client suite, as C functions plus the
//! JNI entry points of `wnatest.Fixture`.

use std::ffi::c_void;
use std::panic::catch_unwind;
use wna::Trap;
use wna::abi::{Iface, WnaList};
use wna::compose::Descriptor;

unsafe extern "C" {
    fn malloc(n: usize) -> *mut c_void;
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(t) = p.downcast_ref::<Trap>() {
        t.to_string()
    } else if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".into()
    }
}

/// Instantiates the Rust sink and the Rust server importing it.
///
/// # Safety
/// The out pointers must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wna_fixture_rust_server(out_types: *mut Iface, out_sink: *mut Iface) -> i32 {
    let r = catch_unwind(|| -> Result<(Iface, Iface), Trap> {
        let sink = unsafe { Descriptor::from_ptr(wna_test_rust_sink::wna_test_rust_sink__wna_component()) }.instantiate(&[])?;
        let sink = sink.export("wna:test/sink@1.0.0")?;
        let server = unsafe { Descriptor::from_ptr(wna_test_rust_server::wna_test_rust_server__wna_component()) }.instantiate(&[sink])?;
        Ok((server.export("wna:test/types@1.0.0")?, sink))
    });
    match r {
        Ok(Ok((types, sink))) => {
            unsafe {
                *out_types = types;
                *out_sink = sink;
            }
            0
        }
        Ok(Err(t)) => t.code,
        Err(_) => wna::abi::TRAP_PANIC,
    }
}

/// Binds the Rust client to `types`/`sink` and runs `run_suite(false)` on
/// a new native thread (so a JVM exporter sees a thread it must attach).
/// On failure returns nonzero and stores a malloc'd UTF-8 message in `msg`.
/// The client binds once per process: later calls reuse the first tables.
///
/// # Safety
/// The tables must be valid; `msg` must be writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wna_fixture_run_rust_client(types: Iface, sink: Iface, msg: *mut WnaList<u8>) -> i32 {
    let suite = std::thread::spawn(move || {
        wna_test_rust_client::bind(types, sink);
        wna_test_rust_client::run_suite(false);
    });
    match suite.join() {
        Ok(()) => 0,
        Err(p) => {
            let m = panic_message(p);
            if !msg.is_null() {
                unsafe {
                    let p = malloc(m.len().max(1)) as *mut u8;
                    if !p.is_null() {
                        std::ptr::copy_nonoverlapping(m.as_ptr(), p, m.len());
                        *msg = WnaList { ptr: p, len: m.len() };
                    }
                }
            }
            1
        }
    }
}

// JNI entry points (`object wnatest.Fixture`, `@JvmStatic external`).
// Pointers cross as `jlong`.

/// `rustServer(out: Long): Int`: writes `{types, sink}` (two `wna_iface`s) at `out`.
///
/// # Safety
/// `out` must point to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_wnatest_Fixture_rustServer(_env: *mut c_void, _cls: *mut c_void, out: i64) -> i32 {
    let out = out as usize as *mut Iface;
    unsafe { wna_fixture_rust_server(out, out.add(1)) }
}

/// `runRustClient(typesTable, typesInst, sinkTable, sinkInst, msgOut): Int`.
///
/// # Safety
/// Arguments must be valid tables and a writable `{ptr, len}` (or 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Java_wnatest_Fixture_runRustClient(_env: *mut c_void, _cls: *mut c_void, tt: i64, ti: i64, st: i64, si: i64, msg: i64) -> i32 {
    let iface = |t: i64, i: i64| Iface { table: t as usize as *const c_void, inst: i as usize as *mut c_void };
    unsafe { wna_fixture_run_rust_client(iface(tt, ti), iface(st, si), msg as usize as *mut WnaList<u8>) }
}
