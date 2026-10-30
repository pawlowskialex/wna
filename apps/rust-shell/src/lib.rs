//! A shell implements `acme:app/shell`: it exports platform services
//! (secrets, notifications, config, logging) and imports the app
//! (storage, rules, feature control). This one runs on a desktop and keeps
//! everything in memory; the iOS and Android shells do the same with the
//! Keychain/Keystore and the OS notification APIs.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

::wna::generate!({ world: "acme:app/shell", path: "../../wit", name: "acme:rust-shell" });

pub use acme::core::storage;
pub use acme::feature::rules;
pub use self::wna::host::features;

use exports::acme::shell::notify::Notification;
use exports::acme::shell::{config, notify, secrets};
use exports::wasi::logging::logging::{self, Level};

#[derive(Default)]
pub struct Platform {
    pub secrets: Mutex<HashMap<String, String>>,
    pub scheduled: Mutex<Vec<Notification>>,
    pub config: Mutex<HashMap<String, String>>,
    pub logs: Mutex<Vec<String>>,
}

pub fn platform() -> &'static Platform {
    static P: OnceLock<Platform> = OnceLock::new();
    P.get_or_init(Platform::default)
}

pub struct Shell;

impl secrets::Guest for Shell {
    fn get(name: String) -> Option<String> {
        platform().secrets.lock().unwrap().get(&name).cloned()
    }
    fn set(name: String, value: String) {
        platform().secrets.lock().unwrap().insert(name, value);
    }
}

impl notify::Guest for Shell {
    fn schedule(n: Notification) -> Result<(), String> {
        let mut s = platform().scheduled.lock().unwrap();
        s.retain(|x| x.id != n.id);
        s.push(n);
        Ok(())
    }
    fn cancel(id: String) {
        platform().scheduled.lock().unwrap().retain(|x| x.id != id);
    }
}

impl config::Guest for Shell {
    fn get(key: String) -> Option<String> {
        platform().config.lock().unwrap().get(&key).cloned()
    }
}

impl logging::Guest for Shell {
    fn log(level: Level, context: String, message: String) {
        let line = format!("[{level:?}] {context}: {message}");
        if std::env::var_os("ACME_LOG").is_some() {
            eprintln!("{line}");
        }
        platform().logs.lock().unwrap().push(line);
    }
}

export!(Shell);

/// Starts the app: hands the shell's exports to the composition root (through
/// the same C entry point Swift and Kotlin use) and binds the app's tables.
pub fn start(config: &[(&str, &str)]) -> Result<(), ::wna::Trap> {
    platform().config.lock().unwrap().extend(config.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let (inst, exports) = __wna_world::host::<Shell>();
    let mut imports = vec![::wna::abi::Iface::NULL; __wna_world::IMPORTS.len()];
    let st = unsafe { acme_app::wna_compose(exports.as_ptr(), exports.len() as u32, imports.as_mut_ptr(), imports.len() as u32) };
    if st != ::wna::abi::OK {
        return Err(::wna::Trap::new(st, "composing the app"));
    }
    inst.bind(&imports);
    Ok(())
}
