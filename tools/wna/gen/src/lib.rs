//! WNA binding generators. Each backend turns a [`wna_abi::Wit`] into source
//! files; the CLI (`wna-gen`) and the `wna::generate!` macro drive them.

pub mod c;
pub mod kotlin;
pub mod rust;

use std::collections::BTreeMap;
use std::path::Path;

/// Generated files: relative path → contents.
pub type Files = BTreeMap<String, String>;

/// Writes files under `dir`, leaving unchanged files untouched so build
/// systems don't see spurious changes.
pub fn write_files(dir: &Path, files: &Files) -> anyhow::Result<()> {
    for (name, contents) in files {
        let path = dir.join(name);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(contents) {
            std::fs::write(&path, contents)?;
        }
    }
    Ok(())
}
