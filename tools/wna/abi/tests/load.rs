use wna_abi::*;

#[test]
fn loads_repo_wit_and_orders_slots() {
    let wit = Wit::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../wit")).unwrap();
    let storage = wit.interface("acme:core/storage").unwrap();
    assert_eq!(wit.prefix(storage), "acme_core_storage_v2");
    assert_eq!(wit.iface_id(storage), "acme:core/storage@2.1.0");
    let names: Vec<_> = slots(&wit, storage).iter().map(|s| (s.name.clone(), s.since.as_ref().map(|v| v.to_string()))).collect();
    assert_eq!(names.last().unwrap(), &("count".to_string(), Some("2.1.0".to_string())));
    assert_eq!(names[0].1.as_deref(), Some("2.0.0"));

    let types = wit.interface("wna:test/types").unwrap();
    wit.check_v0(types).unwrap();
    let s: Vec<_> = slots(&wit, types).into_iter().map(|s| s.name).collect();
    assert!(s.contains(&"counter_new".to_string()) && s.contains(&"counter_drop".to_string()));

    let logging = wit.interface("wasi:logging/logging").unwrap();
    assert_eq!(wit.prefix(logging), "wasi_logging_logging_v0_1");

    let w = wit.world("wna:test/server").unwrap();
    let (imports, exports) = wit.world_interfaces(w).unwrap();
    assert_eq!(imports.len(), 1);
    assert_eq!(exports.len(), 1);
}
