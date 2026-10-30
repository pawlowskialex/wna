//! wna-pkg: the publish side of downloadable features.
//!
//! ```text
//! wna-pkg componentize <core.wasm> -o <component.wasm>
//! wna-pkg keygen -o <key.hex>                 # prints the public key to pin in the app
//! wna-pkg build --component <c.wasm> --wit <dir> --world <ns:pkg/world> --name <n>
//!               --version <v> --key <key.hex> [--min-app <v>] [--precompile <key>]... -o <pkg>
//! wna-pkg inspect <pkg> [--pubkey <hex>]
//! ```

use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;
use std::path::PathBuf;
use wna_host::package::{self, Manifest};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else { bail!("usage: wna-pkg <componentize|keygen|build|inspect> ...") };
    let mut flags: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut positional = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if let Some(f) = a.strip_prefix("--").or_else(|| a.strip_prefix('-')) {
            flags.entry(f.to_string()).or_default().push(it.next().with_context(|| format!("{a} needs a value"))?.clone());
        } else {
            positional.push(a.clone());
        }
    }
    let one = |k: &str| -> Result<String> { flags.get(k).and_then(|v| v.last().cloned()).with_context(|| format!("missing --{k}")) };
    let out = || -> Result<PathBuf> { Ok(PathBuf::from(one("o").or_else(|_| one("out"))?)) };

    match cmd.as_str() {
        "componentize" => {
            let core = std::fs::read(positional.first().context("missing core module")?)?;
            // The world comes from wit-bindgen's `component-type` custom
            // section. No WASI adapter: the component imports only its world.
            let component = wit_component::ComponentEncoder::default().module(&core)?.validate(true).encode()?;
            std::fs::write(out()?, component)?;
        }
        "keygen" => {
            let key = SigningKey::generate(&mut rand::rngs::OsRng);
            std::fs::write(out()?, package::key_hex(&key.to_bytes()))?;
            println!("{}", package::key_hex(key.verifying_key().as_bytes()));
        }
        "build" => {
            let component = std::fs::read(one("component")?)?;
            let wit = wna_abi::Wit::load(one("wit")?)?;
            let world_spec = one("world")?;
            let world = wit.world(&world_spec)?;
            let w = &wit.resolve.worlds[world];
            let pkg_name = &wit.resolve.packages[w.package.unwrap()].name;
            let key = package::signing_key_from_hex(&std::fs::read_to_string(one("key")?)?)?;
            let mut precompiled = BTreeMap::new();
            for k in flags.get("precompile").cloned().unwrap_or_default() {
                let engine = wna_host::engine_for_artifact(&k)?;
                precompiled.insert(k.clone(), engine.precompile_component(&component).map_err(anyhow::Error::from).with_context(|| format!("precompiling for {k}"))?);
            }
            let manifest = Manifest {
                name: one("name")?,
                version: one("version")?,
                world: format!("{}:{}/{}", pkg_name.namespace, pkg_name.name, w.name),
                world_version: pkg_name.version.as_ref().map(|v| v.to_string()).unwrap_or_default(),
                min_app_version: one("min-app").ok(),
                size: 0,
                sha256: String::new(),
                precompiled: BTreeMap::new(),
                wasmtime: if precompiled.is_empty() { String::new() } else { wasmtime_version() },
            };
            let bytes = package::build(manifest, &component, &precompiled, &key)?;
            std::fs::write(out()?, bytes)?;
        }
        "inspect" => {
            let bytes = std::fs::read(positional.first().context("missing package")?)?;
            let trusted = match one("pubkey") {
                Ok(k) => vec![package::verifying_key_from_hex(&k)?],
                Err(_) => bail!("--pubkey is required: nothing in a package is trusted before verification"),
            };
            let p = package::open(&bytes, &trusted)?;
            println!("{}", serde_json::to_string_pretty(&p.manifest)?);
        }
        c => bail!("unknown command {c}"),
    }
    Ok(())
}

fn wasmtime_version() -> String {
    // Precompiled artifacts are only valid for the exact Wasmtime that made them.
    format!("wasmtime-{}", env!("CARGO_PKG_VERSION_MAJOR"))
}
