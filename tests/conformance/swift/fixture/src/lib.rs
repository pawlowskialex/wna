//! C entry points to the Rust conformance components, so suites in other
//! languages can run against the Rust server and drive the Rust client.

use std::panic::{AssertUnwindSafe, catch_unwind};
use wna::abi::*;
use wna::compose::Descriptor;

fn sink() -> Iface {
    let d = unsafe { Descriptor::from_ptr(wna_test_rust_sink::wna_test_rust_sink__wna_component()) };
    d.instantiate(&[]).unwrap().export("wna:test/sink").unwrap()
}

/// A Rust sink provider's `wna:test/sink`.
#[unsafe(no_mangle)]
pub extern "C" fn wna_fixture_rust_sink(out: *mut Iface) {
    unsafe { *out = sink() };
}

/// A Rust server's `wna:test/types`, bound to `sink`.
#[unsafe(no_mangle)]
pub extern "C" fn wna_fixture_rust_server(sink: Iface, out: *mut Iface) {
    let d = unsafe { Descriptor::from_ptr(wna_test_rust_server::wna_test_rust_server__wna_component()) };
    unsafe { *out = d.instantiate(&[sink]).unwrap().export("wna:test/types").unwrap() };
}

/// Runs the Rust client's suite against `types`/`sink`. Returns 0 on
/// success; otherwise writes the failure into `msg` (static, UTF-8).
#[unsafe(no_mangle)]
pub extern "C" fn wna_fixture_run_rust_client(types: Iface, sink: Iface, msg: *mut WnaList<u8>) -> i32 {
    let r = catch_unwind(AssertUnwindSafe(|| {
        wna_test_rust_client::bind(types, sink);
        wna_test_rust_client::run_suite(false);
    }));
    match r {
        Ok(()) => 0,
        Err(p) => {
            let m = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .or_else(|| p.downcast_ref::<wna::Trap>().map(|t| t.to_string()))
                .unwrap_or_else(|| "panic".into());
            let m: &'static str = Box::leak(m.into_boxed_str());
            unsafe { *msg = WnaList { ptr: m.as_ptr() as *mut u8, len: m.len() } };
            1
        }
    }
}
