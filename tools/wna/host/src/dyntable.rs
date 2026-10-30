//! Function tables built at runtime. Every WNA slot has the same C shape, so
//! a const-generic trampoline per slot index can forward any interface to a
//! Rust [`Dispatch`] object: the Wasm bridge and the hot-swappable feature
//! slots both use this instead of generated code.

use std::ffi::c_void;
use std::sync::Arc;
use wna::Trap;
use wna::abi::*;

/// The most slots a runtime-built table can have.
pub const MAX_SLOTS: usize = 256;

pub trait Dispatch: Send + Sync + 'static {
    /// Handles a call to slot `slot` (0-based, in table order).
    ///
    /// # Safety
    /// `args`, `ret` and `alloc` are as the slot's C signature describes.
    unsafe fn call(&self, slot: usize, args: *const c_void, ret: *mut c_void, alloc: *const Alloc) -> Result<(), Trap>;
}

unsafe extern "C" fn tramp<const I: usize>(inst: *mut c_void, args: *const c_void, ret: *mut c_void, alloc: *const Alloc) -> i32 {
    let d = unsafe { &*(inst as *const Arc<dyn Dispatch>) };
    wna::trap::guard(|| unsafe { d.call(I, args, ret, alloc) })
}

macro_rules! tramps { ($($i:literal)*) => { static TRAMPS: [RawFn; MAX_SLOTS] = [$(tramp::<$i>),*]; } }
tramps!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79 80 81 82 83 84 85 86 87 88 89 90 91 92 93 94 95 96 97 98 99 100 101 102 103 104 105 106 107 108 109 110 111 112 113 114 115 116 117 118 119 120 121 122 123 124 125 126 127 128 129 130 131 132 133 134 135 136 137 138 139 140 141 142 143 144 145 146 147 148 149 150 151 152 153 154 155 156 157 158 159 160 161 162 163 164 165 166 167 168 169 170 171 172 173 174 175 176 177 178 179 180 181 182 183 184 185 186 187 188 189 190 191 192 193 194 195 196 197 198 199 200 201 202 203 204 205 206 207 208 209 210 211 212 213 214 215 216 217 218 219 220 221 222 223 224 225 226 227 228 229 230 231 232 233 234 235 236 237 238 239 240 241 242 243 244 245 246 247 248 249 250 251 252 253 254 255);

/// A table of `n` slots forwarding to `target`. The returned [`Iface`] is
/// valid while the `DynTable` lives.
pub struct DynTable {
    words: Box<[usize]>,
    inst: Box<Arc<dyn Dispatch>>,
}

unsafe impl Send for DynTable {}
unsafe impl Sync for DynTable {}

impl DynTable {
    pub fn new(n: usize, target: Arc<dyn Dispatch>) -> Self {
        assert!(n <= MAX_SLOTS, "interface has more than {MAX_SLOTS} functions");
        let mut words = vec![0usize; 2 + n];
        // Header: `uint32_t size; uint32_t reserved;` then `last_trap`.
        let header = TableHeader { size: (TABLE_HEADER_SIZE + 8 * n) as u32, reserved: 0, last_trap: wna::trap::last_trap };
        unsafe { std::ptr::write(words.as_mut_ptr() as *mut TableHeader, header) };
        for (i, w) in words[2..].iter_mut().enumerate() {
            *w = TRAMPS[i] as usize;
        }
        DynTable { words: words.into_boxed_slice(), inst: Box::new(target) }
    }

    pub fn iface(&self) -> Iface {
        Iface { table: self.words.as_ptr() as *const c_void, inst: &*self.inst as *const Arc<dyn Dispatch> as *mut c_void }
    }
}
