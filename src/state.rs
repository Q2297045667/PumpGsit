//! Plugin-instance state shared by WASI 0.3 callbacks, which have separate task-local storage.
use std::{mem, sync::Mutex};

/// Limits access to a short synchronous operation so a guard cannot be held across an await.
#[derive(Default)]
pub(crate) struct State<T>(Mutex<T>);

impl<T> State<T> {
    /// Creates a state cell, including during static initialization.
    pub(crate) const fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    /// Reads the current value without exposing its lock guard.
    pub(crate) fn with_borrow<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.0.lock().unwrap())
    }

    /// Mutates the value without exposing its lock guard.
    pub(crate) fn with_borrow_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// Copies a scalar or other small Copy value out of the cell.
    pub(crate) fn get(&self) -> T
    where
        T: Copy,
    {
        self.with_borrow(|value| *value)
    }

    /// Installs a value and drops its predecessor after releasing the lock.
    pub(crate) fn set(&self, value: T) {
        self.with_borrow_mut(|old| mem::replace(old, value));
    }

    /// Moves out the value and leaves its default in the cell.
    pub(crate) fn take(&self) -> T
    where
        T: Default,
    {
        self.with_borrow_mut(mem::take)
    }
}
