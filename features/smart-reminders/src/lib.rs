//! Smart reminders: for every stored reminder, schedule a notification
//! `lead-ms` before it is due (once), and expire reminders that have passed.
//!
//! One source, two substrates: linked into the app it is the built-in
//! fallback; compiled to Wasm it ships as a signed, sandboxed package whose
//! only capabilities are the `rules-engine` world's imports.
//!
//! Storage layout: `reminder/<id>` = `"<due-ms>|<title>"`,
//! `scheduled/<id>` = `"<at-ms>"`.

wna::generate!({ world: "acme:feature/rules-engine", path: "../../wit", name: "acme:smart-reminders" });

use acme::core::storage;
use acme::shell::notify::{self, Notification};
use exports::acme::feature::rules::{Action, Fact, Guest};
use wasi::logging::logging::{Level, log};

const DEFAULT_LEAD_MS: u64 = 15 * 60 * 1000;

pub struct SmartReminders;

fn fact(facts: &[Fact], key: &str) -> Option<u64> {
    facts.iter().find(|f| f.key == key).and_then(|f| f.value.parse().ok())
}

impl Guest for SmartReminders {
    fn evaluate(facts: Vec<Fact>) -> Result<Vec<Action>, String> {
        let now = fact(&facts, "now").ok_or("missing fact `now`")?;
        let lead = fact(&facts, "lead-ms").unwrap_or(DEFAULT_LEAD_MS);
        let mut actions = Vec::new();
        for item in storage::scan("reminder/") {
            let id = &item.key["reminder/".len()..];
            let text = String::from_utf8_lossy(&item.value);
            let Some((due, title)) = text.split_once('|').and_then(|(d, t)| Some((d.parse::<u64>().ok()?, t))) else {
                log(Level::Warn, "smart-reminders", &format!("skipping malformed reminder {id}"));
                continue;
            };
            let marker = format!("scheduled/{id}");
            if due <= now {
                let _ = storage::delete(&item.key);
                let _ = storage::delete(&marker);
                notify::cancel(id);
                actions.push(Action { kind: "expired".into(), target: id.into(), at_ms: None });
                continue;
            }
            if storage::get(&marker).is_ok() {
                continue;
            }
            let at = due.saturating_sub(lead).max(now);
            notify::schedule(&Notification { id: id.into(), title: title.into(), body: format!("Due in {} min", (due - at) / 60_000), at_ms: at })?;
            storage::put(&marker, at.to_string().as_bytes()).map_err(|e| format!("{e:?}"))?;
            actions.push(Action { kind: "notify".into(), target: id.into(), at_ms: Some(at) });
        }
        log(Level::Debug, "smart-reminders", &format!("{} actions", actions.len()));
        Ok(actions)
    }

    fn describe() -> String {
        let substrate = if cfg!(target_family = "wasm") { "wasm" } else { "native" };
        format!("smart-reminders {} ({substrate})", env!("CARGO_PKG_VERSION"))
    }
}

export!(SmartReminders);
