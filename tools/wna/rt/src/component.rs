//! Instances and the per-component "current instance" context.
//!
//! A component is a function from imports to exports, so the same Rust
//! component can be instantiated several times with different imports. Free
//! import functions (the wit-bindgen shape) find their tables through the
//! instance that is currently running an export on this thread; code running
//! outside any export call (a background thread) uses the component's first
//! instance.

use crate::abi::Iface;
use crate::handle::HandleTable;
use std::any::Any;
use std::sync::Arc;

pub type AnyArc = Arc<dyn Any + Send + Sync>;

pub struct Instance {
    pub imports: Box<[Iface]>,
    /// One table per resource type the component exports.
    pub resources: Box<[HandleTable<dyn Any + Send + Sync>]>,
}

impl Instance {
    pub fn new(imports: &[Iface], n_resources: usize) -> Self {
        Instance { imports: imports.into(), resources: (0..n_resources).map(|_| HandleTable::new()).collect() }
    }
}

/// Expands to the context of one component: `instance()`, `enter()`,
/// `instantiate()`. Generated bindings invoke it once per world.
#[doc(hidden)]
#[macro_export]
macro_rules! __component_context {
    ($name:expr) => {
        ::std::thread_local! {
            static CURRENT: ::std::cell::Cell<*const $crate::component::Instance> =
                const { ::std::cell::Cell::new(::std::ptr::null()) };
        }
        static DEFAULT: ::std::sync::atomic::AtomicPtr<$crate::component::Instance> =
            ::std::sync::atomic::AtomicPtr::new(::std::ptr::null_mut());

        /// The instance whose export is running on this thread, else the first one.
        pub fn instance() -> &'static $crate::component::Instance {
            let p = CURRENT.with(|c| c.get());
            let p = if p.is_null() { DEFAULT.load(::std::sync::atomic::Ordering::Acquire) as *const _ } else { p };
            match unsafe { p.as_ref() } {
                Some(i) => i,
                None => ::std::panic::panic_any($crate::Trap::new(
                    $crate::abi::TRAP_UNAVAILABLE,
                    ::std::format!("component `{}` has no instance; compose it first", $name),
                )),
            }
        }

        /// Runs `f` with `inst` as the current instance.
        pub fn enter<R>(inst: *mut ::std::ffi::c_void, f: impl FnOnce() -> R) -> R {
            struct Restore(*const $crate::component::Instance);
            impl Drop for Restore {
                fn drop(&mut self) {
                    CURRENT.with(|c| c.set(self.0));
                }
            }
            let _restore = Restore(CURRENT.with(|c| c.replace(inst as *const _)));
            f()
        }

        pub fn instantiate(imports: &[$crate::abi::Iface], n_resources: usize) -> *mut $crate::component::Instance {
            // Native instances live for the process: never freed.
            let p = ::std::boxed::Box::into_raw(::std::boxed::Box::new($crate::component::Instance::new(imports, n_resources)));
            let _ = DEFAULT.compare_exchange(
                ::std::ptr::null_mut(),
                p,
                ::std::sync::atomic::Ordering::AcqRel,
                ::std::sync::atomic::Ordering::Acquire,
            );
            p
        }
    };
}

/// Does `requested` ("ns:pkg/name" or "ns:pkg/name@x.y.z") name the export
/// `provided` ("ns:pkg/name@x.y.z")? With a version, majors must match (minor
/// for 0.x) and the provided minor must be at least the requested one.
pub fn iface_matches(requested: &str, provided: &str) -> bool {
    let (rn, rv) = split_version(requested);
    let (pn, pv) = split_version(provided);
    if rn != pn {
        return false;
    }
    match (rv, pv) {
        (None, _) => true,
        (Some(r), Some(p)) => version_compatible(r, p),
        (Some(_), None) => false,
    }
}

fn split_version(s: &str) -> (&str, Option<&str>) {
    match s.split_once('@') {
        Some((n, v)) => (n, Some(v)),
        None => (s, None),
    }
}

/// Can an exporter at version `provided` serve an importer built against `required`?
pub fn version_compatible(required: &str, provided: &str) -> bool {
    let parse = |v: &str| -> (u64, u64, u64) {
        let core = v.split(['-', '+']).next().unwrap_or("");
        let mut it = core.split('.').map(|x| x.parse().unwrap_or(0));
        (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
    };
    let (r, p) = (parse(required), parse(provided));
    if r.0 != p.0 {
        return false;
    }
    if r.0 == 0 {
        return r.1 == p.1;
    }
    (p.1, p.2) >= (r.1, r.2) || p.1 > r.1
}
