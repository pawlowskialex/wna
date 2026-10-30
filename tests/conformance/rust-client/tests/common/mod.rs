//! Runs the suite against `wna-test-rust-server` compiled to a Wasm component
//! and hosted by the bridge. Build it first: `just wasm-test-server`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use wna::compose::Descriptor;
use wna_host::wasmtime::component::Component;
use wna_host::{Bridge, Execution, Limits, World};

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

pub fn run_against_wasm(exec: Execution) {
    let path = std::env::var("WNA_TEST_SERVER_WASM").map(PathBuf::from).unwrap_or_else(|_| root().join("out/wasm/wna-test-rust-server.wasm"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}; run `just wasm-test-server` first", path.display()));

    let wit = Arc::new(wna_abi::Wit::load(root().join("wit")).unwrap());
    let world = World::new(wit, "wna:test/server").unwrap();
    let engine = wna_host::engine(exec).unwrap();
    let component = Component::new(&engine, &bytes).unwrap();

    let sink = unsafe { Descriptor::from_ptr(wna_test_rust_sink::wna_test_rust_sink__wna_component()) }.instantiate(&[]).unwrap();
    let sink_iface = sink.export("wna:test/sink").unwrap();
    let natives = HashMap::from([("wna:test/sink".to_string(), sink_iface)]);
    let bridge = Bridge::instantiate(&engine, &world, &component, &natives, Limits::default()).unwrap();
    let tables = bridge.tables();
    let (_, types) = tables.iter().find(|(id, _)| id.starts_with("wna:test/types")).unwrap();

    wna_test_rust_client::bind(types.iface(), sink_iface);
    wna_test_rust_client::run_suite(true);
}
