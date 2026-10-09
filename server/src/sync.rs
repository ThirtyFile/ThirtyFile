//! Locks for what the server keeps in memory (settings, counters, maps of what is running) that a panic doesn't make
//! unusable. The standard library's locks are "poisoned" when a thread panics while holding one, and every later
//! `lock().unwrap()` panics too: one caught panic in a request would turn every request after it into an error until
//! the server restarts. What these locks guard is never left half written in a way that matters (a setting, a count, a
//! map entry), so a poisoned lock is simply used as it is.
//!
//! `clippy.toml` refuses `std::sync::Mutex` and `std::sync::RwLock` elsewhere.

#![allow(clippy::disallowed_types, reason = "the one place that wraps the standard library's locks")]

use std::sync::{MutexGuard, PoisonError, RwLockReadGuard, RwLockWriteGuard, TryLockError};

#[derive(Default, Debug)]
pub struct Mutex<T: ?Sized>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Mutex(std::sync::Mutex::new(value))
    }

    pub fn into_inner(self) -> T {
        self.0.into_inner().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// None when another thread holds it
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        match self.0.try_lock() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Default, Debug)]
pub struct RwLock<T: ?Sized>(std::sync::RwLock<T>);

impl<T> RwLock<T> {
    pub const fn new(value: T) -> Self {
        RwLock(std::sync::RwLock::new(value))
    }
}

impl<T: ?Sized> RwLock<T> {
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.0.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_while_holding_a_lock_leaves_it_usable() {
        let m = std::sync::Arc::new(Mutex::new(1));
        let r = std::sync::Arc::new(RwLock::new(1));
        let (m2, r2) = (m.clone(), r.clone());
        let _ = std::thread::spawn(move || {
            let _held = m2.lock();
            let mut w = r2.write();
            *w = 2;
            panic!("a request fails while holding the locks");
        })
        .join();
        *m.lock() += 1;
        assert_eq!(*m.lock(), 2);
        assert_eq!(*r.read(), 2, "what was written stays");
        assert!(m.try_lock().is_some());
    }
}
