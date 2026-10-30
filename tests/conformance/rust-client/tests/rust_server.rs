//! Rust client ↔ Rust server, composed by hand the way `wna_compose` does it.

use wna::compose::Descriptor;

#[test]
fn rust_server() {
    let sink = unsafe { Descriptor::from_ptr(wna_test_rust_sink::wna_test_rust_sink__wna_component()) };
    let server = unsafe { Descriptor::from_ptr(wna_test_rust_server::wna_test_rust_server__wna_component()) };
    assert_eq!(server.imports(), ["wna:test/sink@1.0.0"]);
    assert_eq!(server.exports(), ["wna:test/types@1.0.0"]);

    let sink = sink.instantiate(&[]).unwrap();
    let sink_iface = sink.export("wna:test/sink@1.0.0").unwrap();
    let server = server.instantiate(&[sink_iface]).unwrap();
    let types = server.export("wna:test/types").unwrap();
    assert!(server.export("wna:test/types@2.0.0").is_err(), "major mismatch is refused");

    wna_test_rust_client::bind(types, sink_iface);
    wna_test_rust_client::run_suite();
}
