//! `wna:test/sink-provider` in Rust: listeners that count events.

use std::sync::Mutex;

::wna::generate!({ world: "wna:test/sink-provider", path: "../../../wit", name: "wna-test-rust-sink" });

use exports::wna::test::sink::{Guest, GuestListener};

pub struct Sink;

pub struct Recorder(Mutex<Vec<String>>);

impl Guest for Sink {
    type Listener = Recorder;
}

impl GuestListener for Recorder {
    fn new() -> Self {
        Recorder(Mutex::new(Vec::new()))
    }
    fn on_event(&self, e: String) {
        self.0.lock().unwrap().push(e);
    }
    fn count(&self) -> u32 {
        self.0.lock().unwrap().len() as u32
    }
}

export!(Sink);
