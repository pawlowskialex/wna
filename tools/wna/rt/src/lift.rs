//! Moving values across the boundary. Four directions, one trait each:
//!
//! | trait          | side     | what                                          |
//! |----------------|----------|-----------------------------------------------|
//! | [`LowerParam`] | importer | borrow a value as a view, valid for the call  |
//! | [`LiftParam`]  | exporter | copy a caller's view into an owned value      |
//! | [`LowerResult`]| exporter | write a result, allocating via caller's alloc |
//! | [`LiftResult`] | importer | adopt (or copy and free) a result's buffers   |
//!
//! Importers always pass [`RUST_ALLOC`] (the global allocator), so result
//! strings and scalar lists are adopted without copying.

use crate::abi::*;
use crate::trap::Trap;
use std::alloc::Layout;
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::{MaybeUninit, align_of, size_of};

/// A type with a WNA memory representation `Self::Abi`.
///
/// # Safety
/// `Abi` must have the layout the WNA model assigns to the WIT type.
pub unsafe trait Abi {
    type Abi: Copy;
    /// `Self` and `Self::Abi` are the same plain type (integers, floats), so
    /// lists of it cross without element conversion.
    const IDENTITY: bool = false;
}

pub trait LowerParam: Abi {
    fn lower_param(&self, s: &Scratch) -> Self::Abi;
}

pub trait LiftParam: Abi + Sized {
    /// # Safety
    /// `abi` must be a valid view for the duration of the call.
    unsafe fn lift_param(abi: &Self::Abi) -> Result<Self, Trap>;
}

pub trait LowerResult: Abi + Sized {
    fn lower_result(self, cx: &mut AllocCx) -> Result<Self::Abi, Trap>;
}

pub trait LiftResult: Abi + Sized {
    /// # Safety
    /// `abi` must have been produced by a callee allocating with [`RUST_ALLOC`].
    unsafe fn lift_result(abi: Self::Abi) -> Result<Self, Trap>;
}

// ---------------------------------------------------------------------------
// Allocation
// ---------------------------------------------------------------------------

unsafe extern "C" fn rust_realloc(_ctx: *mut c_void, old: *mut c_void, old_size: usize, align: usize, new_size: usize) -> *mut c_void {
    unsafe {
        let Ok(new_layout) = Layout::from_size_align(new_size, align) else { return std::ptr::null_mut() };
        if old.is_null() {
            return if new_size == 0 { std::ptr::null_mut() } else { std::alloc::alloc(new_layout) as *mut c_void };
        }
        let old_layout = Layout::from_size_align_unchecked(old_size, align);
        if new_size == 0 {
            std::alloc::dealloc(old as *mut u8, old_layout);
            return std::ptr::null_mut();
        }
        std::alloc::realloc(old as *mut u8, old_layout, new_size) as *mut c_void
    }
}

/// The allocator Rust importers pass: Rust's global allocator.
pub static RUST_ALLOC: Alloc = Alloc { ctx: std::ptr::null_mut(), realloc: rust_realloc };

/// Frees a buffer allocated through [`RUST_ALLOC`].
unsafe fn rust_free(ptr: *mut u8, size: usize, align: usize) {
    if !ptr.is_null() && size != 0 {
        unsafe { std::alloc::dealloc(ptr, Layout::from_size_align_unchecked(size, align)) }
    }
}

/// Exporter-side allocation through the caller's allocator. Remembers every
/// buffer so a failed lowering (OOM) releases what it already allocated.
pub struct AllocCx<'a> {
    alloc: &'a Alloc,
    live: Vec<(*mut c_void, usize, usize)>,
}

impl<'a> AllocCx<'a> {
    pub fn new(alloc: &'a Alloc) -> Self {
        AllocCx { alloc, live: Vec::new() }
    }

    pub fn alloc(&mut self, size: usize, align: usize) -> Result<*mut u8, Trap> {
        if size == 0 {
            return Ok(std::ptr::null_mut());
        }
        let p = unsafe { (self.alloc.realloc)(self.alloc.ctx, std::ptr::null_mut(), 0, align, size) };
        if p.is_null() {
            return Err(Trap::oom());
        }
        self.live.push((p, size, align));
        Ok(p as *mut u8)
    }

    /// Ownership of everything allocated passes to the caller.
    pub fn commit(mut self) {
        self.live.clear();
    }
}

impl Drop for AllocCx<'_> {
    fn drop(&mut self) {
        for (p, size, align) in self.live.drain(..) {
            unsafe { (self.alloc.realloc)(self.alloc.ctx, p, size, align, 0) };
        }
    }
}

/// Temporary memory for lowering parameters that need it (lists whose
/// elements change representation). Lives for one call.
#[derive(Default)]
pub struct Scratch {
    bufs: RefCell<Vec<Box<dyn std::any::Any>>>,
}

impl Scratch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Keeps `v` alive until the scratch is dropped and returns its pointer.
    pub fn keep<T: 'static>(&self, mut v: Vec<T>) -> *mut T {
        let p = v.as_mut_ptr();
        self.bufs.borrow_mut().push(Box::new(v));
        p
    }
}

// ---------------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------------

macro_rules! identity {
    ($($t:ty),*) => {$(
        unsafe impl Abi for $t { type Abi = $t; const IDENTITY: bool = true; }
        impl LowerParam for $t { #[inline] fn lower_param(&self, _: &Scratch) -> $t { *self } }
        impl LiftParam for $t { #[inline] unsafe fn lift_param(a: &$t) -> Result<$t, Trap> { Ok(*a) } }
        impl LowerResult for $t { #[inline] fn lower_result(self, _: &mut AllocCx) -> Result<$t, Trap> { Ok(self) } }
        impl LiftResult for $t { #[inline] unsafe fn lift_result(a: $t) -> Result<$t, Trap> { Ok(a) } }
    )*};
}
identity!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);

unsafe impl Abi for bool {
    type Abi = u8;
}
impl LowerParam for bool {
    fn lower_param(&self, _: &Scratch) -> u8 {
        *self as u8
    }
}
impl LiftParam for bool {
    unsafe fn lift_param(a: &u8) -> Result<bool, Trap> {
        Ok(*a != 0)
    }
}
impl LowerResult for bool {
    fn lower_result(self, _: &mut AllocCx) -> Result<u8, Trap> {
        Ok(self as u8)
    }
}
impl LiftResult for bool {
    unsafe fn lift_result(a: u8) -> Result<bool, Trap> {
        Ok(a != 0)
    }
}

unsafe impl Abi for char {
    type Abi = u32;
}
fn char_from(c: u32) -> Result<char, Trap> {
    char::from_u32(c).ok_or_else(|| Trap::invalid(format!("invalid char {c:#x}")))
}
impl LowerParam for char {
    fn lower_param(&self, _: &Scratch) -> u32 {
        *self as u32
    }
}
impl LiftParam for char {
    unsafe fn lift_param(a: &u32) -> Result<char, Trap> {
        char_from(*a)
    }
}
impl LowerResult for char {
    fn lower_result(self, _: &mut AllocCx) -> Result<u32, Trap> {
        Ok(self as u32)
    }
}
impl LiftResult for char {
    unsafe fn lift_result(a: u32) -> Result<char, Trap> {
        char_from(a)
    }
}

unsafe impl Abi for () {
    type Abi = ();
}
impl LowerParam for () {
    fn lower_param(&self, _: &Scratch) {}
}
impl LiftParam for () {
    unsafe fn lift_param(_: &()) -> Result<(), Trap> {
        Ok(())
    }
}
impl LowerResult for () {
    fn lower_result(self, _: &mut AllocCx) -> Result<(), Trap> {
        Ok(())
    }
}
impl LiftResult for () {
    unsafe fn lift_result(_: ()) -> Result<(), Trap> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// References: `&T` lowers like `T` (import parameters are borrowed forms).
// ---------------------------------------------------------------------------

unsafe impl<T: Abi + ?Sized> Abi for &T {
    type Abi = T::Abi;
}
impl<T: LowerParam + ?Sized> LowerParam for &T {
    #[inline]
    fn lower_param(&self, s: &Scratch) -> T::Abi {
        (**self).lower_param(s)
    }
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

unsafe impl Abi for str {
    type Abi = WnaList<u8>;
}
unsafe impl Abi for String {
    type Abi = WnaList<u8>;
}
impl LowerParam for str {
    #[inline]
    fn lower_param(&self, _: &Scratch) -> WnaList<u8> {
        WnaList { ptr: self.as_ptr() as *mut u8, len: self.len() }
    }
}
impl LowerParam for String {
    #[inline]
    fn lower_param(&self, s: &Scratch) -> WnaList<u8> {
        self.as_str().lower_param(s)
    }
}
impl LiftParam for String {
    unsafe fn lift_param(a: &WnaList<u8>) -> Result<String, Trap> {
        let bytes = unsafe { a.as_slice() };
        match std::str::from_utf8(bytes) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => Err(Trap::invalid("string is not valid UTF-8")),
        }
    }
}
impl LowerResult for String {
    fn lower_result(self, cx: &mut AllocCx) -> Result<WnaList<u8>, Trap> {
        let p = cx.alloc(self.len(), 1)?;
        unsafe { std::ptr::copy_nonoverlapping(self.as_ptr(), p, self.len()) };
        Ok(WnaList { ptr: p, len: self.len() })
    }
}
impl LiftResult for String {
    unsafe fn lift_result(a: WnaList<u8>) -> Result<String, Trap> {
        if a.len == 0 {
            return Ok(String::new());
        }
        let v = unsafe { Vec::from_raw_parts(a.ptr, a.len, a.len) };
        String::from_utf8(v).map_err(|_| Trap::invalid("string is not valid UTF-8"))
    }
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

unsafe impl<T: Abi> Abi for [T] {
    type Abi = WnaList<T::Abi>;
}
unsafe impl<T: Abi> Abi for Vec<T> {
    type Abi = WnaList<T::Abi>;
}

impl<T: LowerParam> LowerParam for [T]
where
    T::Abi: 'static,
{
    fn lower_param(&self, s: &Scratch) -> WnaList<T::Abi> {
        if T::IDENTITY {
            return WnaList { ptr: self.as_ptr() as *mut T::Abi, len: self.len() };
        }
        let v: Vec<T::Abi> = self.iter().map(|x| x.lower_param(s)).collect();
        let len = v.len();
        WnaList { ptr: s.keep(v), len }
    }
}
impl<T: LowerParam> LowerParam for Vec<T>
where
    T::Abi: 'static,
{
    fn lower_param(&self, s: &Scratch) -> WnaList<T::Abi> {
        self.as_slice().lower_param(s)
    }
}
impl<T: LiftParam> LiftParam for Vec<T> {
    unsafe fn lift_param(a: &WnaList<T::Abi>) -> Result<Vec<T>, Trap> {
        unsafe { a.as_slice() }.iter().map(|x| unsafe { T::lift_param(x) }).collect()
    }
}
impl<T: LowerResult> LowerResult for Vec<T> {
    fn lower_result(self, cx: &mut AllocCx) -> Result<WnaList<T::Abi>, Trap> {
        let len = self.len();
        let p = cx.alloc(len * size_of::<T::Abi>(), align_of::<T::Abi>())? as *mut T::Abi;
        for (i, x) in self.into_iter().enumerate() {
            let v = x.lower_result(cx)?;
            unsafe { p.add(i).write(v) };
        }
        Ok(WnaList { ptr: p, len })
    }
}
impl<T: LiftResult> LiftResult for Vec<T> {
    unsafe fn lift_result(a: WnaList<T::Abi>) -> Result<Vec<T>, Trap> {
        if a.len == 0 {
            return Ok(Vec::new());
        }
        unsafe {
            if T::IDENTITY {
                return Ok(Vec::from_raw_parts(a.ptr as *mut T, a.len, a.len));
            }
            // Lift every element even after a failure so no buffer leaks.
            let mut out = Vec::with_capacity(a.len);
            let mut err = None;
            for x in a.as_slice() {
                match T::lift_result(*x) {
                    Ok(v) => out.push(v),
                    Err(e) => err = err.or(Some(e)),
                }
            }
            rust_free(a.ptr as *mut u8, a.len * size_of::<T::Abi>(), align_of::<T::Abi>());
            match err {
                None => Ok(out),
                Some(e) => Err(e),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Option / Result
// ---------------------------------------------------------------------------

unsafe impl<T: Abi> Abi for Option<T> {
    type Abi = WnaOption<T::Abi>;
}
impl<T: LowerParam> LowerParam for Option<T> {
    fn lower_param(&self, s: &Scratch) -> WnaOption<T::Abi> {
        match self {
            None => WnaOption { is_some: 0, val: MaybeUninit::uninit() },
            Some(v) => WnaOption { is_some: 1, val: MaybeUninit::new(v.lower_param(s)) },
        }
    }
}
impl<T: LiftParam> LiftParam for Option<T> {
    unsafe fn lift_param(a: &WnaOption<T::Abi>) -> Result<Option<T>, Trap> {
        match a.is_some {
            0 => Ok(None),
            1 => Ok(Some(unsafe { T::lift_param(a.val.assume_init_ref())? })),
            d => Err(Trap::invalid(format!("option discriminant {d}"))),
        }
    }
}
impl<T: LowerResult> LowerResult for Option<T> {
    fn lower_result(self, cx: &mut AllocCx) -> Result<WnaOption<T::Abi>, Trap> {
        Ok(match self {
            None => WnaOption { is_some: 0, val: MaybeUninit::uninit() },
            Some(v) => WnaOption { is_some: 1, val: MaybeUninit::new(v.lower_result(cx)?) },
        })
    }
}
impl<T: LiftResult> LiftResult for Option<T> {
    unsafe fn lift_result(a: WnaOption<T::Abi>) -> Result<Option<T>, Trap> {
        match a.is_some {
            0 => Ok(None),
            1 => Ok(Some(unsafe { T::lift_result(a.val.assume_init())? })),
            d => Err(Trap::invalid(format!("option discriminant {d}"))),
        }
    }
}

unsafe impl<T: Abi, E: Abi> Abi for Result<T, E> {
    type Abi = WnaResult<T::Abi, E::Abi>;
}
impl<T: LowerParam, E: LowerParam> LowerParam for Result<T, E> {
    fn lower_param(&self, s: &Scratch) -> Self::Abi {
        match self {
            Ok(v) => WnaResult { is_err: 0, val: WnaResultVal { ok: v.lower_param(s) } },
            Err(e) => WnaResult { is_err: 1, val: WnaResultVal { err: e.lower_param(s) } },
        }
    }
}
impl<T: LiftParam, E: LiftParam> LiftParam for Result<T, E> {
    unsafe fn lift_param(a: &Self::Abi) -> Result<Self, Trap> {
        unsafe {
            match a.is_err {
                0 => Ok(Ok(T::lift_param(&a.val.ok)?)),
                1 => Ok(Err(E::lift_param(&a.val.err)?)),
                d => Err(Trap::invalid(format!("result discriminant {d}"))),
            }
        }
    }
}
impl<T: LowerResult, E: LowerResult> LowerResult for Result<T, E> {
    fn lower_result(self, cx: &mut AllocCx) -> Result<Self::Abi, Trap> {
        Ok(match self {
            Ok(v) => WnaResult { is_err: 0, val: WnaResultVal { ok: v.lower_result(cx)? } },
            Err(e) => WnaResult { is_err: 1, val: WnaResultVal { err: e.lower_result(cx)? } },
        })
    }
}
impl<T: LiftResult, E: LiftResult> LiftResult for Result<T, E> {
    unsafe fn lift_result(a: Self::Abi) -> Result<Self, Trap> {
        unsafe {
            match a.is_err {
                0 => Ok(Ok(T::lift_result(a.val.ok)?)),
                1 => Ok(Err(E::lift_result(a.val.err)?)),
                d => Err(Trap::invalid(format!("result discriminant {d}"))),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tuples
// ---------------------------------------------------------------------------

macro_rules! tuple_impls {
    ($($name:ident($($t:ident $i:tt),*);)*) => {$(
        unsafe impl<$($t: Abi),*> Abi for ($($t,)*) {
            type Abi = $name<$($t::Abi),*>;
        }
        impl<$($t: LowerParam),*> LowerParam for ($($t,)*) {
            fn lower_param(&self, s: &Scratch) -> Self::Abi { $name($(self.$i.lower_param(s)),*) }
        }
        impl<$($t: LiftParam),*> LiftParam for ($($t,)*) {
            unsafe fn lift_param(a: &Self::Abi) -> Result<Self, Trap> { unsafe { Ok(($($t::lift_param(&a.$i)?,)*)) } }
        }
        impl<$($t: LowerResult),*> LowerResult for ($($t,)*) {
            fn lower_result(self, cx: &mut AllocCx) -> Result<Self::Abi, Trap> { Ok($name($(self.$i.lower_result(cx)?),*)) }
        }
        impl<$($t: LiftResult),*> LiftResult for ($($t,)*) {
            unsafe fn lift_result(a: Self::Abi) -> Result<Self, Trap> {
                // Lift all fields so a failure in one doesn't leak the others.
                let r = ($(unsafe { $t::lift_result(a.$i) },)*);
                Ok(($(r.$i?,)*))
            }
        }
    )*};
}

tuple_impls! {
    WnaTuple1(A 0);
    WnaTuple2(A 0, B 1);
    WnaTuple3(A 0, B 1, C 2);
    WnaTuple4(A 0, B 1, C 2, D 3);
    WnaTuple5(A 0, B 1, C 2, D 3, E 4);
    WnaTuple6(A 0, B 1, C 2, D 3, E 4, F 5);
    WnaTuple7(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
    WnaTuple8(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
}
