//! The app's native half, one library per platform: the Rust core, the
//! built-in features, the feature host, and the composition root. Shells
//! call `wna_compose` once at startup.

pub use acme_core;
pub use smart_reminders;

wna::compose!("../wna.compose.toml");
