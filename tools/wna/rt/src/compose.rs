//! Calling component descriptors: what a composition root does.

use crate::Trap;
use crate::abi::*;
use std::ffi::{CStr, CString, c_void};

/// A component's descriptor, as returned by its `<name>__wna_component` symbol.
#[derive(Copy, Clone)]
pub struct Descriptor(pub &'static Component);

unsafe impl Send for Descriptor {}
unsafe impl Sync for Descriptor {}

impl Descriptor {
    /// # Safety
    /// `p` must point to a valid, 'static descriptor.
    pub unsafe fn from_ptr(p: *const Component) -> Self {
        Descriptor(unsafe { &*p })
    }

    pub fn name(&self) -> &'static str {
        unsafe { CStr::from_ptr(self.0.name) }.to_str().unwrap_or("?")
    }

    fn strs(n: u32, p: *const *const std::ffi::c_char) -> Vec<&'static str> {
        (0..n as usize).map(|i| unsafe { CStr::from_ptr(*p.add(i)) }.to_str().unwrap_or("?")).collect()
    }

    pub fn imports(&self) -> Vec<&'static str> {
        Self::strs(self.0.n_imports, self.0.imports)
    }

    pub fn exports(&self) -> Vec<&'static str> {
        Self::strs(self.0.n_exports, self.0.exports)
    }

    /// Instantiates with `imports` in the descriptor's import order.
    pub fn instantiate(&self, imports: &[Iface]) -> Result<Instance, Trap> {
        if self.0.abi_version != ABI_VERSION {
            return Err(Trap::new(TRAP_UNAVAILABLE, format!("{}: ABI version {} (want {ABI_VERSION})", self.name(), self.0.abi_version)));
        }
        if imports.len() != self.0.n_imports as usize {
            return Err(Trap::new(TRAP_UNAVAILABLE, format!("{}: {} imports supplied, {} declared", self.name(), imports.len(), self.0.n_imports)));
        }
        let mut inst = std::ptr::null_mut();
        let st = unsafe { (self.0.instantiate)(imports.as_ptr(), &mut inst) };
        if st != OK {
            return Err(Trap::new(st, format!("instantiating {}", self.name())));
        }
        Ok(Instance { desc: *self, inst })
    }
}

#[derive(Copy, Clone)]
pub struct Instance {
    pub desc: Descriptor,
    pub inst: *mut c_void,
}

unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

impl Instance {
    /// The table for an export: `"ns:pkg/iface"` or `"ns:pkg/iface@x.y.z"`.
    pub fn export(&self, iface: &str) -> Result<Iface, Trap> {
        let name = CString::new(iface).map_err(|_| Trap::invalid("interface name"))?;
        let mut out = Iface::NULL;
        match unsafe { (self.desc.0.get_export)(self.inst, name.as_ptr(), &mut out) } {
            OK => Ok(out),
            st => Err(Trap::new(st, format!("{} does not export {iface}", self.desc.name()))),
        }
    }
}

/// Orders `available` exports to match `wanted` (a descriptor's import
/// list), matching by interface name and version compatibility.
pub fn order_imports(wanted: &[&str], available: &[(&str, Iface)]) -> Result<Vec<Iface>, Trap> {
    wanted
        .iter()
        .map(|w| {
            available
                .iter()
                .find(|(name, _)| crate::component::iface_matches(name, w) || crate::component::iface_matches(w, name))
                .map(|(_, i)| *i)
                .ok_or_else(|| Trap::new(TRAP_UNAVAILABLE, format!("no export satisfies import {w}")))
        })
        .collect()
}
