//! Converting between WNA memory layouts and Wasmtime's dynamic `Val`s,
//! driven by the WIT types. This is what lets one bridge serve any world.

use crate::bridge::State;
use wasmtime::error::Context;
use wasmtime::{Result, bail, format_err as anyhow};
use std::sync::Arc;
use wasmtime::StoreContextMut;
use wasmtime::component::{ResourceAny, ResourceDynamic, Val};
use wna::abi::WnaList;
use wna::handle::HandleTable;
use wna::lift::AllocCx;
use wna_abi::wit_parser::{Handle, Type, TypeDefKind, TypeId};
use wna_abi::{Wit, flags_size};

/// How a resource type crosses the bridge.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ResKind {
    /// Implemented natively and imported by the guest; `u32` = the payload
    /// tag of its `ResourceType::host_dynamic`.
    Host(u32),
    /// Implemented by the guest; native code holds bridge handles to it.
    Guest,
}

pub struct Cx<'a, 's> {
    pub wit: &'a Wit,
    pub store: StoreContextMut<'s, State>,
    pub kinds: &'a dyn Fn(TypeId) -> Option<ResKind>,
    pub guest_handles: Arc<HandleTable<ResourceAny>>,
    /// Handles lent to native code for one call (borrows of guest resources).
    pub lent: Vec<u32>,
}

/// Where buffers for strings and lists come from when writing.
pub enum Mem<'m, 'a> {
    /// Importer side (guest → native parameters): kept alive for the call.
    Scratch(&'m mut Vec<Vec<u64>>),
    /// Exporter side (results to a native caller): the caller's allocator.
    Alloc(&'m mut AllocCx<'a>),
}

impl Mem<'_, '_> {
    fn alloc(&mut self, size: usize, align: usize) -> Result<*mut u8> {
        if size == 0 {
            return Ok(std::ptr::null_mut());
        }
        match self {
            Mem::Scratch(bufs) => {
                debug_assert!(align <= 8);
                let mut v = vec![0u64; size.div_ceil(8)];
                let p = v.as_mut_ptr() as *mut u8;
                bufs.push(v);
                Ok(p)
            }
            Mem::Alloc(cx) => cx.alloc(size, align).map_err(wasmtime::Error::new),
        }
    }
}

unsafe fn rd<T: Copy>(p: *const u8) -> T {
    unsafe { std::ptr::read(p as *const T) }
}
unsafe fn wr<T>(p: *mut u8, v: T) {
    unsafe { std::ptr::write(p as *mut T, v) }
}

/// Frees a buffer a native callee allocated through `wna::lift::RUST_ALLOC`.
unsafe fn free(ptr: *mut u8, size: usize, align: usize) {
    if !ptr.is_null() && size != 0 {
        unsafe { std::alloc::dealloc(ptr, std::alloc::Layout::from_size_align_unchecked(size, align)) }
    }
}

fn disc_read(p: *const u8, size: usize) -> u32 {
    unsafe {
        match size {
            1 => rd::<u8>(p) as u32,
            2 => rd::<u16>(p) as u32,
            _ => rd::<u32>(p),
        }
    }
}

fn disc_write(p: *mut u8, size: usize, v: u32) {
    unsafe {
        match size {
            1 => wr(p, v as u8),
            2 => wr(p, v as u16),
            _ => wr(p, v),
        }
    }
}

impl Cx<'_, '_> {
    /// Reads a value of type `ty` at `p`. `owned`: buffers were allocated for
    /// us with `RUST_ALLOC` (a native callee's result) and are freed here.
    ///
    /// # Safety
    /// `p` must point to a valid value of `ty` in WNA layout.
    pub unsafe fn read(&mut self, ty: Type, p: *const u8, owned: bool) -> Result<Val> {
        unsafe {
            Ok(match ty {
                Type::Bool => Val::Bool(rd::<u8>(p) != 0),
                Type::U8 => Val::U8(rd(p)),
                Type::U16 => Val::U16(rd(p)),
                Type::U32 => Val::U32(rd(p)),
                Type::U64 => Val::U64(rd(p)),
                Type::S8 => Val::S8(rd(p)),
                Type::S16 => Val::S16(rd(p)),
                Type::S32 => Val::S32(rd(p)),
                Type::S64 => Val::S64(rd(p)),
                Type::F32 => Val::Float32(rd(p)),
                Type::F64 => Val::Float64(rd(p)),
                Type::Char => {
                    let c: u32 = rd(p);
                    Val::Char(char::from_u32(c).with_context(|| format!("invalid char {c:#x}"))?)
                }
                Type::String => {
                    let l: WnaList<u8> = rd(p);
                    let s = std::str::from_utf8(l.as_slice()).map(str::to_owned);
                    if owned {
                        free(l.ptr, l.len, 1);
                    }
                    Val::String(s.context("string is not valid UTF-8")?)
                }
                Type::ErrorContext => bail!("error-context is not supported"),
                Type::Id(id) => self.read_id(id, p, owned)?,
            })
        }
    }

    unsafe fn read_id(&mut self, id: TypeId, p: *const u8, owned: bool) -> Result<Val> {
        let wit = self.wit;
        let kind = &wit.resolve.types[id].kind;
        unsafe {
            Ok(match kind {
                TypeDefKind::Type(t) => self.read(*t, p, owned)?,
                TypeDefKind::Record(r) => {
                    let l = wit.struct_layout(r.fields.iter().map(|f| f.ty));
                    let mut out = Vec::with_capacity(r.fields.len());
                    for (f, off) in r.fields.iter().zip(l.offsets) {
                        out.push((f.name.clone(), self.read(f.ty, p.add(off), owned)?));
                    }
                    Val::Record(out)
                }
                TypeDefKind::Tuple(t) => {
                    let l = wit.struct_layout(t.types.iter().copied());
                    let mut out = Vec::with_capacity(t.types.len());
                    for (ty, off) in t.types.iter().zip(l.offsets) {
                        out.push(self.read(*ty, p.add(off), owned)?);
                    }
                    Val::Tuple(out)
                }
                TypeDefKind::List(e) => {
                    let l: WnaList<u8> = rd(p);
                    let el = wit.layout(*e);
                    let mut out = Vec::with_capacity(l.len);
                    for i in 0..l.len {
                        out.push(self.read(*e, l.ptr.add(i * el.size), owned)?);
                    }
                    if owned {
                        free(l.ptr, l.len * el.size, el.align);
                    }
                    Val::List(out)
                }
                TypeDefKind::Flags(f) => {
                    let bits = disc_read(p, flags_size(f.flags.len()));
                    Val::Flags(f.flags.iter().enumerate().filter(|(i, _)| bits & (1 << i) != 0).map(|(_, f)| f.name.clone()).collect())
                }
                TypeDefKind::Enum(e) => {
                    let d = disc_read(p, wna_abi::discriminant_size(e.cases.len()));
                    Val::Enum(e.cases.get(d as usize).with_context(|| format!("enum case {d}"))?.name.clone())
                }
                TypeDefKind::Variant(v) => {
                    let sl = wit.sum_layout(v.cases.iter().map(|c| c.ty));
                    let d = disc_read(p, sl.disc_size) as usize;
                    let c = v.cases.get(d).with_context(|| format!("variant case {d}"))?;
                    let payload = match c.ty {
                        Some(t) => Some(Box::new(self.read(t, p.add(sl.payload_offset), owned)?)),
                        None => None,
                    };
                    Val::Variant(c.name.clone(), payload)
                }
                TypeDefKind::Option(t) => {
                    let sl = wit.sum_layout([None, Some(*t)].into_iter());
                    match rd::<u8>(p) {
                        0 => Val::Option(None),
                        1 => Val::Option(Some(Box::new(self.read(*t, p.add(sl.payload_offset), owned)?))),
                        d => bail!("option discriminant {d}"),
                    }
                }
                TypeDefKind::Result(r) => {
                    let sl = wit.sum_layout([r.ok, r.err].into_iter());
                    let q = p.add(sl.payload_offset);
                    match rd::<u8>(p) {
                        0 => Val::Result(Ok(match r.ok {
                            Some(t) => Some(Box::new(self.read(t, q, owned)?)),
                            None => None,
                        })),
                        1 => Val::Result(Err(match r.err {
                            Some(t) => Some(Box::new(self.read(t, q, owned)?)),
                            None => None,
                        })),
                        d => bail!("result discriminant {d}"),
                    }
                }
                TypeDefKind::Handle(h) => {
                    let handle: u32 = rd(p);
                    let r = wit.resource_of(h);
                    let own = matches!(h, Handle::Own(_));
                    match (self.kinds)(r).context("resource outside the world")? {
                        ResKind::Host(tag) => {
                            let d = if own { ResourceDynamic::new_own(handle, tag) } else { ResourceDynamic::new_borrow(handle, tag) };
                            Val::Resource(d.try_into_resource_any(&mut self.store)?)
                        }
                        ResKind::Guest => {
                            let any = if own { self.guest_handles.remove(handle) } else { self.guest_handles.get(handle) }
                                .map_err(wasmtime::Error::new)?;
                            Val::Resource(*any)
                        }
                    }
                }
                k => bail!("unsupported type {k:?}"),
            })
        }
    }

    /// Writes `val` as `ty` at `p`, allocating nested buffers from `mem`.
    ///
    /// # Safety
    /// `p` must be writable for the layout of `ty`.
    pub unsafe fn write(&mut self, ty: Type, val: &Val, p: *mut u8, mem: &mut Mem) -> Result<()> {
        unsafe {
            match (ty, val) {
                (Type::Bool, Val::Bool(v)) => wr(p, *v as u8),
                (Type::U8, Val::U8(v)) => wr(p, *v),
                (Type::U16, Val::U16(v)) => wr(p, *v),
                (Type::U32, Val::U32(v)) => wr(p, *v),
                (Type::U64, Val::U64(v)) => wr(p, *v),
                (Type::S8, Val::S8(v)) => wr(p, *v),
                (Type::S16, Val::S16(v)) => wr(p, *v),
                (Type::S32, Val::S32(v)) => wr(p, *v),
                (Type::S64, Val::S64(v)) => wr(p, *v),
                (Type::F32, Val::Float32(v)) => wr(p, *v),
                (Type::F64, Val::Float64(v)) => wr(p, *v),
                (Type::Char, Val::Char(v)) => wr(p, *v as u32),
                (Type::String, Val::String(s)) => {
                    let buf = mem.alloc(s.len(), 1)?;
                    std::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
                    wr(p, WnaList { ptr: buf, len: s.len() });
                }
                (Type::Id(id), v) => self.write_id(id, v, p, mem)?,
                (t, v) => bail!("value {v:?} does not match type {t:?}"),
            }
        }
        Ok(())
    }

    unsafe fn write_id(&mut self, id: TypeId, val: &Val, p: *mut u8, mem: &mut Mem) -> Result<()> {
        let wit = self.wit;
        let kind = &wit.resolve.types[id].kind;
        unsafe {
            match (kind, val) {
                (TypeDefKind::Type(t), v) => self.write(*t, v, p, mem)?,
                (TypeDefKind::Record(r), Val::Record(fields)) => {
                    let l = wit.struct_layout(r.fields.iter().map(|f| f.ty));
                    for ((f, off), (_, v)) in r.fields.iter().zip(l.offsets).zip(fields) {
                        self.write(f.ty, v, p.add(off), mem)?;
                    }
                }
                (TypeDefKind::Tuple(t), Val::Tuple(vals)) => {
                    let l = wit.struct_layout(t.types.iter().copied());
                    for ((ty, off), v) in t.types.iter().zip(l.offsets).zip(vals) {
                        self.write(*ty, v, p.add(off), mem)?;
                    }
                }
                (TypeDefKind::List(e), Val::List(vals)) => {
                    let el = wit.layout(*e);
                    let buf = mem.alloc(vals.len() * el.size, el.align)?;
                    for (i, v) in vals.iter().enumerate() {
                        self.write(*e, v, buf.add(i * el.size), mem)?;
                    }
                    wr(p, WnaList { ptr: buf, len: vals.len() });
                }
                (TypeDefKind::Flags(f), Val::Flags(names)) => {
                    let mut bits = 0u32;
                    for n in names {
                        bits |= 1 << f.flags.iter().position(|x| &x.name == n).with_context(|| format!("unknown flag {n}"))?;
                    }
                    disc_write(p, flags_size(f.flags.len()), bits);
                }
                (TypeDefKind::Enum(e), Val::Enum(name)) => {
                    let d = e.cases.iter().position(|c| &c.name == name).with_context(|| format!("unknown case {name}"))?;
                    disc_write(p, wna_abi::discriminant_size(e.cases.len()), d as u32);
                }
                (TypeDefKind::Variant(v), Val::Variant(name, payload)) => {
                    let sl = wit.sum_layout(v.cases.iter().map(|c| c.ty));
                    let d = v.cases.iter().position(|c| &c.name == name).with_context(|| format!("unknown case {name}"))?;
                    disc_write(p, sl.disc_size, d as u32);
                    if let (Some(t), Some(pv)) = (v.cases[d].ty, payload) {
                        self.write(t, pv, p.add(sl.payload_offset), mem)?;
                    }
                }
                (TypeDefKind::Option(t), Val::Option(v)) => {
                    let sl = wit.sum_layout([None, Some(*t)].into_iter());
                    match v {
                        None => wr(p, 0u8),
                        Some(v) => {
                            wr(p, 1u8);
                            self.write(*t, v, p.add(sl.payload_offset), mem)?;
                        }
                    }
                }
                (TypeDefKind::Result(r), Val::Result(v)) => {
                    let sl = wit.sum_layout([r.ok, r.err].into_iter());
                    let q = p.add(sl.payload_offset);
                    match v {
                        Ok(v) => {
                            wr(p, 0u8);
                            if let (Some(t), Some(v)) = (r.ok, v) {
                                self.write(t, v, q, mem)?;
                            }
                        }
                        Err(v) => {
                            wr(p, 1u8);
                            if let (Some(t), Some(v)) = (r.err, v) {
                                self.write(t, v, q, mem)?;
                            }
                        }
                    }
                }
                (TypeDefKind::Handle(h), Val::Resource(any)) => {
                    let r = wit.resource_of(h);
                    let handle = match (self.kinds)(r).context("resource outside the world")? {
                        ResKind::Host(_) => ResourceDynamic::try_from_resource_any(*any, &mut self.store)?.rep(),
                        ResKind::Guest => {
                            let h2 = self.guest_handles.insert(Arc::new(*any));
                            if matches!(h, Handle::Borrow(_)) {
                                self.lent.push(h2);
                            }
                            h2
                        }
                    };
                    wr(p, handle);
                }
                (k, v) => bail!("value {v:?} does not match type {k:?}"),
            }
        }
        Ok(())
    }

    /// Ends a call: borrows of guest resources lent to native code expire.
    pub fn end_call(&mut self) {
        for h in self.lent.drain(..) {
            let _ = self.guest_handles.remove(h);
        }
    }
}

/// A zero value of a type, used as a placeholder for `Func::call` results.
pub fn placeholder() -> Val {
    Val::Bool(false)
}
