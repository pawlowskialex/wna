//! The whole stack, as a shell sees it: compose, use the built-in feature,
//! hot-install the signed Wasm build of the same feature into the same slot,
//! reject bad packages, roll back.

use acme_rust_shell::{features, platform, rules, storage};
use rules::Fact;
use std::collections::BTreeMap;
use std::path::PathBuf;
use wna_host::package::{self, Manifest};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn package(component: &[u8], version: &str, world: &str, key: &ed25519_dalek::SigningKey) -> Vec<u8> {
    let m = Manifest {
        name: "smart-reminders".into(),
        version: version.into(),
        world: world.into(),
        world_version: "1.2.0".into(),
        min_app_version: Some("1.0.0".into()),
        size: 0,
        sha256: String::new(),
        precompiled: BTreeMap::new(),
        wasmtime: String::new(),
    };
    package::build(m, component, &BTreeMap::new(), key).unwrap()
}

fn facts(now: u64) -> Vec<Fact> {
    vec![Fact { key: "now".into(), value: now.to_string() }]
}

#[test]
fn compose_fallback_install_rollback() {
    let data = std::env::temp_dir().join(format!("acme-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    acme_rust_shell::start(&[("data-dir", data.to_str().unwrap())]).unwrap();

    // Storage is the Rust core; rules is the slot, serving the built-in.
    let now = 1_000_000_000u64;
    storage::put("reminder/dentist", format!("{}|Dentist", now + 3_600_000).as_bytes()).unwrap();
    storage::put("reminder/old", format!("{}|Old", now - 60_000).as_bytes()).unwrap();
    assert_eq!(rules::describe(), "smart-reminders 1.0.0 (native)");
    assert!(matches!(storage::get(""), Err(storage::Error::InvalidKey(_))));
    assert_eq!(storage::count("reminder/"), 2);

    let actions = rules::evaluate(&facts(now)).unwrap();
    let kinds: Vec<_> = actions.iter().map(|a| (a.kind.as_str(), a.target.as_str())).collect();
    assert_eq!(kinds, [("notify", "dentist"), ("expired", "old")]);
    assert_eq!(actions[0].at_ms, Some(now + 3_600_000 - 15 * 60_000));
    assert_eq!(platform().scheduled.lock().unwrap().len(), 1);
    assert!(data.join("store.json").exists(), "core persisted to the shell's data dir");

    let status = features::status();
    assert_eq!(status[0].slot, "rules");
    assert_eq!(status[0].state, features::State::Fallback);

    // Install the Wasm build of the same feature, signed with the pinned key.
    let wasm = std::fs::read(root().join("out/wasm/smart-reminders.wasm")).expect("run `just features` first");
    let key = package::signing_key_from_hex(&std::fs::read_to_string(root().join("keys/dev-signing.key")).unwrap()).unwrap();
    assert_eq!(features::install("rules", &package(&wasm, "1.1.0", "acme:feature/rules-engine", &key)).unwrap(), "1.1.0");
    assert_eq!(features::status()[0].state, features::State::Active);

    // Same table the shell bound at startup; now a sandboxed blob answers.
    assert_eq!(rules::describe(), "smart-reminders 1.0.0 (wasm)");
    // It sees the same core storage: `dentist` was scheduled by the native build.
    storage::put("reminder/standup", format!("{}|Standup", now + 7_200_000).as_bytes()).unwrap();
    let actions = rules::evaluate(&facts(now)).unwrap();
    assert_eq!(actions.iter().map(|a| a.target.as_str()).collect::<Vec<_>>(), ["standup"]);
    assert_eq!(platform().scheduled.lock().unwrap().len(), 2);
    assert!(rules::evaluate(&[]).unwrap_err().contains("now"), "domain errors cross the sandbox");

    // An untrusted signature is refused; the slot keeps what it had.
    let rogue = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let err = features::install("rules", &package(&wasm, "6.6.6", "acme:feature/rules-engine", &rogue)).unwrap_err();
    assert!(err.contains("signature"), "{err}");
    // A component importing outside its world cannot link.
    let server = std::fs::read(root().join("out/wasm/wna-test-rust-server.wasm")).expect("run `just wasm-test-server` first");
    let err = features::install("rules", &package(&server, "6.6.7", "acme:feature/rules-engine", &key)).unwrap_err();
    assert!(err.contains("cannot run in slot"), "{err}");
    // A package for a newer world than this app provides is refused.
    let err = features::install("rules", &package_with_world(&wasm, &key, "1.3.0")).unwrap_err();
    assert!(err.contains("this app provides 1.2.0"), "{err}");
    assert_eq!(rules::describe(), "smart-reminders 1.0.0 (wasm)");

    // Roll back: no previous package, so the built-in serves again.
    features::rollback("rules").unwrap();
    assert_eq!(features::status()[0].state, features::State::Fallback);
    assert_eq!(rules::describe(), "smart-reminders 1.0.0 (native)");
}

fn package_with_world(component: &[u8], key: &ed25519_dalek::SigningKey, world_version: &str) -> Vec<u8> {
    let m = Manifest {
        name: "smart-reminders".into(),
        version: "2.0.0".into(),
        world: "acme:feature/rules-engine".into(),
        world_version: world_version.into(),
        min_app_version: None,
        size: 0,
        sha256: String::new(),
        precompiled: BTreeMap::new(),
        wasmtime: String::new(),
    };
    package::build(m, component, &BTreeMap::new(), key).unwrap()
}
