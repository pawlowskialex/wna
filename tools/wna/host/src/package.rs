//! Feature packages: one file per feature version.
//!
//! A package is an uncompressed tar with `manifest.json`, `component.wasm`,
//! `signature` (Ed25519, 64 bytes) and optional `precompiled/<key>.cwasm`.
//! The signature covers the manifest and the component; the manifest lists
//! the sha256 of every precompiled artifact, so those are covered too.

use anyhow::{Context, Result, bail, ensure};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    /// Version of the feature itself.
    pub version: String,
    /// The world it targets, `ns:pkg/world`.
    pub world: String,
    /// The world version it was built against.
    pub world_version: String,
    pub min_app_version: Option<String>,
    pub size: u64,
    /// sha256 of `component.wasm`, hex.
    pub sha256: String,
    /// Precompiled artifact key → sha256, hex.
    #[serde(default)]
    pub precompiled: BTreeMap<String, String>,
    /// Wasmtime version that produced the precompiled artifacts.
    #[serde(default)]
    pub wasmtime: String,
}

pub struct Package {
    pub manifest: Manifest,
    pub component: Vec<u8>,
    pub precompiled: BTreeMap<String, Vec<u8>>,
}

const DOMAIN: &[u8] = b"wna-feature-package-v1\0";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn message(manifest: &[u8], component: &[u8]) -> Vec<u8> {
    let mut m = DOMAIN.to_vec();
    m.extend_from_slice(&Sha256::digest(manifest));
    m.extend_from_slice(&Sha256::digest(component));
    m
}

/// Builds a signed package. `manifest.size`, `sha256` and `precompiled` are
/// filled in from the contents.
pub fn build(mut manifest: Manifest, component: &[u8], precompiled: &BTreeMap<String, Vec<u8>>, key: &SigningKey) -> Result<Vec<u8>> {
    manifest.size = component.len() as u64;
    manifest.sha256 = sha256_hex(component);
    manifest.precompiled = precompiled.iter().map(|(k, v)| (k.clone(), sha256_hex(v))).collect();
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let sig = key.sign(&message(&manifest_bytes, component));

    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |name: &str, data: &[u8]| -> Result<()> {
        let mut h = tar::Header::new_ustar();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(0);
        h.set_cksum();
        tar.append_data(&mut h, name, data)?;
        Ok(())
    };
    add("manifest.json", &manifest_bytes)?;
    add("component.wasm", component)?;
    add("signature", &sig.to_bytes())?;
    for (k, v) in precompiled {
        add(&format!("precompiled/{k}.cwasm"), v)?;
    }
    Ok(tar.into_inner()?)
}

/// Parses a package and verifies it against any of `trusted`. Nothing in an
/// unverified package is used.
pub fn open(bytes: &[u8], trusted: &[VerifyingKey]) -> Result<Package> {
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut ar = tar::Archive::new(bytes);
    for e in ar.entries()? {
        let mut e = e?;
        let name = e.path()?.to_string_lossy().into_owned();
        let mut data = Vec::new();
        e.read_to_end(&mut data)?;
        ensure!(entries.insert(name.clone(), data).is_none(), "duplicate entry {name}");
    }
    let manifest_bytes = entries.remove("manifest.json").context("missing manifest.json")?;
    let component = entries.remove("component.wasm").context("missing component.wasm")?;
    let sig = entries.remove("signature").context("missing signature")?;
    let sig = Signature::from_slice(&sig).context("malformed signature")?;
    let msg = message(&manifest_bytes, &component);
    if !trusted.iter().any(|k| k.verify(&msg, &sig).is_ok()) {
        bail!("package signature does not match any trusted key");
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes).context("parsing manifest")?;
    ensure!(manifest.sha256 == sha256_hex(&component) && manifest.size == component.len() as u64, "component does not match its manifest");
    let mut precompiled = BTreeMap::new();
    for (name, data) in entries {
        let Some(key) = name.strip_prefix("precompiled/").and_then(|n| n.strip_suffix(".cwasm")) else { bail!("unexpected entry {name}") };
        ensure!(manifest.precompiled.get(key) == Some(&sha256_hex(&data)), "precompiled {key} does not match its manifest");
        precompiled.insert(key.to_string(), data);
    }
    Ok(Package { manifest, component, precompiled })
}

pub fn signing_key_from_hex(s: &str) -> Result<SigningKey> {
    let b = unhex(s.trim())?;
    Ok(SigningKey::from_bytes(&b.try_into().map_err(|_| anyhow::anyhow!("signing key must be 32 bytes"))?))
}

pub fn verifying_key_from_hex(s: &str) -> Result<VerifyingKey> {
    let b = unhex(s.trim())?;
    Ok(VerifyingKey::from_bytes(&b.try_into().map_err(|_| anyhow::anyhow!("verifying key must be 32 bytes"))?)?)
}

pub fn key_hex(k: &[u8]) -> String {
    hex(k)
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    ensure!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("bad hex")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            name: "x".into(),
            version: "1.0.0".into(),
            world: "acme:feature/rules-engine".into(),
            world_version: "1.2.0".into(),
            min_app_version: None,
            size: 0,
            sha256: String::new(),
            precompiled: BTreeMap::new(),
            wasmtime: String::new(),
        }
    }

    #[test]
    fn sign_verify_tamper() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let mut pre = BTreeMap::new();
        pre.insert("pulley64".to_string(), vec![1, 2, 3]);
        let bytes = build(manifest(), b"\0asm-component", &pre, &key).unwrap();
        let p = open(&bytes, &[key.verifying_key()]).unwrap();
        assert_eq!(p.component, b"\0asm-component");
        assert_eq!(p.precompiled["pulley64"], vec![1, 2, 3]);
        assert!(open(&bytes, &[other.verifying_key()]).is_err(), "untrusted key");

        // Flip one byte of the component inside the tar.
        let pos = bytes.windows(14).position(|w| w == b"\0asm-component").unwrap();
        let mut bad = bytes.clone();
        bad[pos + 3] ^= 1;
        assert!(open(&bad, &[key.verifying_key()]).is_err(), "tampered component");
    }
}
