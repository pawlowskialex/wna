//! Resource handle tables. A handle is a nonzero `u32`: the low 24 bits are a
//! slot index, the high 8 bits a generation (1..=255). A stale handle is
//! detected and reported as `TRAP_BAD_HANDLE`, never undefined behavior.
//!
//! Objects are stored as `Arc`s: a method call clones the `Arc`, so dropping
//! the handle while a call is in flight on another thread frees the object
//! when the last call returns.

use crate::trap::Trap;
use std::sync::{Arc, Mutex};

const INDEX_BITS: u32 = 24;
const INDEX_MASK: u32 = (1 << INDEX_BITS) - 1;

struct Slot<T: ?Sized> {
    generation: u8,
    value: Option<Arc<T>>,
}

pub struct HandleTable<T: ?Sized> {
    inner: Mutex<Inner<T>>,
}

struct Inner<T: ?Sized> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T: ?Sized> Default for HandleTable<T> {
    fn default() -> Self {
        HandleTable { inner: Mutex::new(Inner { slots: Vec::new(), free: Vec::new() }) }
    }
}

impl<T: ?Sized> HandleTable<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, value: Arc<T>) -> u32 {
        let mut t = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let index = match t.free.pop() {
            Some(i) => i,
            None => {
                let i = t.slots.len() as u32;
                assert!(i <= INDEX_MASK, "handle table full");
                t.slots.push(Slot { generation: 0, value: None });
                i
            }
        };
        let slot = &mut t.slots[index as usize];
        slot.generation = if slot.generation == u8::MAX { 1 } else { slot.generation + 1 };
        slot.value = Some(value);
        ((slot.generation as u32) << INDEX_BITS) | index
    }

    pub fn get(&self, handle: u32) -> Result<Arc<T>, Trap> {
        let t = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (index, generation) = split(handle);
        match t.slots.get(index as usize) {
            Some(Slot { generation: g, value: Some(v) }) if *g == generation => Ok(v.clone()),
            _ => Err(Trap::bad_handle(handle)),
        }
    }

    pub fn remove(&self, handle: u32) -> Result<Arc<T>, Trap> {
        let mut t = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (index, generation) = split(handle);
        let v = match t.slots.get_mut(index as usize) {
            Some(slot) if slot.generation == generation && slot.value.is_some() => slot.value.take().unwrap(),
            _ => return Err(Trap::bad_handle(handle)),
        };
        t.free.push(index);
        Ok(v)
    }

    pub fn len(&self) -> usize {
        let t = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        t.slots.len() - t.free.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn split(handle: u32) -> (u32, u8) {
    (handle & INDEX_MASK, (handle >> INDEX_BITS) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generations_catch_stale_handles() {
        let t = HandleTable::<u32>::new();
        let a = t.insert(Arc::new(1));
        assert_eq!(*t.get(a).unwrap(), 1);
        t.remove(a).unwrap();
        let b = t.insert(Arc::new(2));
        assert_eq!(a & INDEX_MASK, b & INDEX_MASK, "slot reused");
        assert!(t.get(a).is_err(), "stale handle rejected");
        assert!(t.remove(a).is_err());
        assert!(t.get(0).is_err(), "zero is never a handle");
        assert_eq!(*t.get(b).unwrap(), 2);
    }
}
