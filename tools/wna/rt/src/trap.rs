//! Traps: the status channel for calls that failed as calls. Boundaries never
//! unwind; panics become statuses at the exporter's edge and statuses become
//! panics (carrying a [`Trap`]) at the importer's.

use crate::abi::*;
use std::cell::RefCell;
use std::ffi::c_void;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trap {
    pub code: i32,
    pub message: String,
}

impl Trap {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Trap { code, message: message.into() }
    }
    pub fn invalid(what: impl Into<String>) -> Self {
        Trap::new(TRAP_INVALID, what)
    }
    pub fn bad_handle(h: u32) -> Self {
        Trap::new(TRAP_BAD_HANDLE, format!("stale or foreign handle {h:#x}"))
    }
    pub fn oom() -> Self {
        Trap::new(TRAP_OOM, "out of memory")
    }
    pub fn name(code: i32) -> &'static str {
        match code {
            OK => "ok",
            TRAP_PANIC => "panic",
            TRAP_BAD_HANDLE => "bad handle",
            TRAP_OOM => "out of memory",
            TRAP_INVALID => "invalid value",
            TRAP_REENTRANCE => "reentrance",
            TRAP_WRONG_THREAD => "wrong thread",
            TRAP_UNAVAILABLE => "unavailable",
            CANCELLED => "cancelled",
            _ => "unknown trap",
        }
    }
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "wna trap: {}", Trap::name(self.code))
        } else {
            write!(f, "wna trap ({}): {}", Trap::name(self.code), self.message)
        }
    }
}

impl std::error::Error for Trap {}

thread_local! {
    static LAST_TRAP: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_last_trap(msg: String) {
    LAST_TRAP.with(|m| *m.borrow_mut() = msg);
}

/// The `last_trap` entry of every table a Rust component exports.
///
/// # Safety
/// `msg` must be writable.
pub unsafe extern "C" fn last_trap(_inst: *mut c_void, msg: *mut WnaList<u8>) {
    LAST_TRAP.with(|m| {
        let m = m.borrow();
        unsafe { *msg = WnaList { ptr: m.as_ptr() as *mut u8, len: m.len() } };
    });
}

/// Runs an export's body: catches panics and turns them, or an `Err`, into
/// a status with a message for `last_trap`.
pub fn guard(f: impl FnOnce() -> Result<(), Trap>) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => OK,
        Ok(Err(t)) => {
            let code = t.code;
            set_last_trap(t.message);
            code
        }
        Err(payload) => {
            let (code, msg) = if let Some(t) = payload.downcast_ref::<Trap>() {
                (t.code, t.to_string())
            } else if let Some(s) = payload.downcast_ref::<&str>() {
                (TRAP_PANIC, s.to_string())
            } else if let Some(s) = payload.downcast_ref::<String>() {
                (TRAP_PANIC, s.clone())
            } else {
                (TRAP_PANIC, "panic".to_string())
            };
            set_last_trap(msg);
            code
        }
    }
}

/// Importer side: raises a failed call's [`Trap`] as a panic, which the
/// importer's own export edge reports with the same status.
#[track_caller]
pub fn raise(t: Trap, what: &str) -> ! {
    std::panic::panic_any(Trap::new(t.code, format!("{what}: {}", t.message)))
}

/// Runs `f`, catching a trap raised by an import call inside it. Useful for
/// app code that wants to handle traps (e.g. show "loading" on
/// `TRAP_UNAVAILABLE`) instead of propagating them.
pub fn catch<R>(f: impl FnOnce() -> R) -> Result<R, Trap> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => Ok(r),
        Err(p) => match p.downcast::<Trap>() {
            Ok(t) => Err(*t),
            Err(p) => std::panic::resume_unwind(p),
        },
    }
}
