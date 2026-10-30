//! The app's native core (`acme:app/core`): key-value storage, persisted to
//! the data directory the shell's config provides.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

wna::generate!({ world: "acme:app/core", path: "../../wit", name: "acme:core-rs" });

use acme::shell::config;
use exports::acme::core::storage::{Error, Guest, Item};
use wasi::logging::logging::{Level, log};

#[derive(Default, Serialize, Deserialize)]
struct Db {
    items: BTreeMap<String, (Vec<u8>, u64)>,
}

struct Store {
    db: Mutex<Db>,
    file: Option<PathBuf>,
}

static STORE: OnceLock<Store> = OnceLock::new();

fn store() -> &'static Store {
    STORE.get_or_init(|| {
        let file = config::get("data-dir").map(|d| PathBuf::from(d).join("store.json"));
        let db = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        log(Level::Info, "core", &format!("storage opened ({})", file.as_ref().map_or("in memory".into(), |f| f.display().to_string())));
        Store { db: Mutex::new(db), file }
    })
}

impl Store {
    fn write(&self, f: impl FnOnce(&mut Db) -> Result<(), Error>) -> Result<(), Error> {
        let mut db = self.db.lock().unwrap();
        f(&mut db)?;
        if let Some(path) = &self.file {
            let bytes = serde_json::to_vec(&*db).map_err(|e| Error::Io(e.to_string()))?;
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| Error::Io(e.to_string()))?;
        }
        Ok(())
    }
}

fn check_key(key: &str) -> Result<(), Error> {
    if key.is_empty() || key.len() > 256 || key.contains('\0') {
        Err(Error::InvalidKey(format!("keys are 1-256 bytes without NUL, got {} bytes", key.len())))
    } else {
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub struct Core;

impl Guest for Core {
    fn get(key: String) -> Result<Vec<u8>, Error> {
        check_key(&key)?;
        store().db.lock().unwrap().items.get(&key).map(|(v, _)| v.clone()).ok_or(Error::NotFound)
    }

    fn put(key: String, value: Vec<u8>) -> Result<(), Error> {
        check_key(&key)?;
        store().write(|db| {
            db.items.insert(key, (value, now_ms()));
            Ok(())
        })
    }

    fn delete(key: String) -> Result<(), Error> {
        check_key(&key)?;
        store().write(|db| db.items.remove(&key).map(|_| ()).ok_or(Error::NotFound))
    }

    fn scan(prefix: String) -> Vec<Item> {
        let db = store().db.lock().unwrap();
        db.items
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, (v, t))| Item { key: k.clone(), value: v.clone(), updated_ms: *t })
            .collect()
    }

    fn count(prefix: String) -> u32 {
        Self::scan(prefix).len() as u32
    }
}

export!(Core);
