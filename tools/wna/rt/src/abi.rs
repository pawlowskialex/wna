//! `#[repr(C)]` mirrors of the WNA memory layouts. Generic types here stand
//! in for the anonymous types (`list<T>`, `option<T>`, `result<T, E>`,
//! tuples) that the C header names structurally.

use std::ffi::{c_char, c_void};
use std::mem::MaybeUninit;

pub const OK: i32 = 0;
pub const TRAP_PANIC: i32 = 1;
pub const TRAP_BAD_HANDLE: i32 = 2;
pub const TRAP_OOM: i32 = 3;
pub const TRAP_INVALID: i32 = 4;
pub const TRAP_REENTRANCE: i32 = 5;
pub const TRAP_WRONG_THREAD: i32 = 6;
pub const TRAP_UNAVAILABLE: i32 = 7;
pub const CANCELLED: i32 = 8;

pub const ABI_VERSION: u32 = 1;
pub const TABLE_HEADER_SIZE: usize = 16;

pub type ReallocFn = unsafe extern "C" fn(ctx: *mut c_void, old: *mut c_void, old_size: usize, align: usize, new_size: usize) -> *mut c_void;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Alloc {
    pub ctx: *mut c_void,
    pub realloc: ReallocFn,
}

unsafe impl Sync for Alloc {}
unsafe impl Send for Alloc {}

/// A slot: `int32_t f(void *inst, const void *args, void *ret, const wna_alloc *alloc)`.
pub type RawFn = unsafe extern "C" fn(inst: *mut c_void, args: *const c_void, ret: *mut c_void, alloc: *const Alloc) -> i32;

pub type LastTrapFn = unsafe extern "C" fn(inst: *mut c_void, msg: *mut WnaList<u8>);

#[repr(C)]
#[derive(Copy, Clone)]
pub struct TableHeader {
    pub size: u32,
    pub reserved: u32,
    pub last_trap: LastTrapFn,
}

/// A table with `N` slots.
#[repr(C)]
pub struct Table<const N: usize> {
    pub header: TableHeader,
    pub slots: [RawFn; N],
}

impl<const N: usize> Table<N> {
    pub const fn new(slots: [RawFn; N]) -> Self {
        Table {
            header: TableHeader { size: (TABLE_HEADER_SIZE + 8 * N) as u32, reserved: 0, last_trap: crate::trap::last_trap },
            slots,
        }
    }
}

/// `wna_iface`: a table and the exporter's instance.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Iface {
    pub table: *const c_void,
    pub inst: *mut c_void,
}

unsafe impl Send for Iface {}
unsafe impl Sync for Iface {}

impl Iface {
    pub const NULL: Iface = Iface { table: std::ptr::null(), inst: std::ptr::null_mut() };

    pub fn header(&self) -> Option<&TableHeader> {
        unsafe { (self.table as *const TableHeader).as_ref() }
    }

    /// Calls the slot at byte `offset`. A table that predates the slot (an
    /// exporter built against an older minor) yields `TRAP_UNAVAILABLE`.
    ///
    /// # Safety
    /// `args`/`ret` must match the slot's signature.
    #[inline]
    pub unsafe fn invoke(&self, offset: usize, args: *const c_void, ret: *mut c_void, alloc: *const Alloc) -> Result<(), crate::Trap> {
        unsafe {
            match self.header() {
                Some(h) if h.size as usize >= offset + 8 => {
                    let f = *((self.table as *const u8).add(offset) as *const RawFn);
                    match f(self.inst, args, ret, alloc) {
                        OK => Ok(()),
                        code => Err(crate::Trap::new(code, self.last_trap())),
                    }
                }
                Some(_) => Err(crate::Trap::new(TRAP_UNAVAILABLE, "the exporter's table predates this function")),
                None => Err(crate::Trap::new(TRAP_UNAVAILABLE, "interface not bound")),
            }
        }
    }

    /// The exporter's message for the last trap on this thread.
    pub fn last_trap(&self) -> String {
        let mut msg = WnaList::<u8>::EMPTY;
        unsafe {
            if let Some(h) = self.header() {
                (h.last_trap)(self.inst, &mut msg);
            }
            String::from_utf8_lossy(msg.as_slice()).into_owned()
        }
    }
}

#[repr(C)]
pub struct Component {
    pub abi_version: u32,
    pub name: *const c_char,
    pub n_imports: u32,
    pub imports: *const *const c_char,
    pub n_exports: u32,
    pub exports: *const *const c_char,
    pub instantiate: unsafe extern "C" fn(imports: *const Iface, inst: *mut *mut c_void) -> i32,
    pub get_export: unsafe extern "C" fn(inst: *mut c_void, iface: *const c_char, out: *mut Iface) -> i32,
    pub destroy: unsafe extern "C" fn(inst: *mut c_void) -> i32,
}

unsafe impl Sync for Component {}

/// `{ptr, len}`: strings (as `WnaList<u8>`) and lists.
#[repr(C)]
#[derive(Debug)]
pub struct WnaList<T> {
    pub ptr: *mut T,
    pub len: usize,
}

impl<T> Clone for WnaList<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for WnaList<T> {}

impl<T> WnaList<T> {
    pub const EMPTY: Self = WnaList { ptr: std::ptr::null_mut(), len: 0 };

    /// # Safety
    /// `ptr` must be valid for `len` elements unless `len == 0`.
    pub unsafe fn as_slice<'a>(&self) -> &'a [T] {
        if self.len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(self.ptr, self.len) } }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct WnaOption<T: Copy> {
    pub is_some: u8,
    pub val: MaybeUninit<T>,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct WnaResult<T: Copy, E: Copy> {
    pub is_err: u8,
    pub val: WnaResultVal<T, E>,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union WnaResultVal<T: Copy, E: Copy> {
    pub ok: T,
    pub err: E,
}

macro_rules! tuples {
    ($($name:ident($($t:ident $i:tt),*);)*) => {$(
        #[repr(C)]
        #[derive(Copy, Clone)]
        pub struct $name<$($t: Copy),*>($(pub $t),*);
    )*};
}

tuples! {
    WnaTuple1(A 0);
    WnaTuple2(A 0, B 1);
    WnaTuple3(A 0, B 1, C 2);
    WnaTuple4(A 0, B 1, C 2, D 3);
    WnaTuple5(A 0, B 1, C 2, D 3, E 4);
    WnaTuple6(A 0, B 1, C 2, D 3, E 4, F 5);
    WnaTuple7(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
    WnaTuple8(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
}
