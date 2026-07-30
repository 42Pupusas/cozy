//! Test-only helper for temporarily overriding process-wide environment
//! variables, shared by `app_index`'s and `launch`'s test modules.
//!
//! Both used to hand-roll their own `EnvGuard` + `ENV_MUTEX` pair around
//! an identical `unsafe { std::env::set_var/remove_var }` call -- same
//! justification (serialize `cargo test`'s multithreaded runner so
//! concurrent tests don't stomp each other's env override), same
//! restore-on-drop shape, just copy-pasted. One shared guard means one
//! audited pair of `unsafe` blocks instead of two.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Serializes every test in this crate that touches a process-wide env
/// var (`TERMINAL`, `XDG_DATA_HOME`, `XDG_DATA_DIRS`, ...): `cargo test`'s
/// default multithreading means concurrent tests would otherwise stomp
/// each other's override mid-check.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Scoped env-var override: restores every touched variable to its prior
/// value (or removes it, if it was previously unset) on drop, and holds
/// [`ENV_MUTEX`] for its whole lifetime so overlapping tests can't
/// interleave their env var changes.
pub struct EnvGuard {
    vars: Vec<(&'static str, Option<String>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// Lock `ENV_MUTEX`, recovering from poisoning rather than cascading
    /// one panicking test's failure into every other test that shares
    /// this guard.
    fn lock() -> MutexGuard<'static, ()> {
        ENV_MUTEX.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Set each `(name, value)` pair, remembering whatever was there
    /// before so [`Drop`] can restore it.
    #[must_use]
    pub fn set(pairs: &[(&'static str, &str)]) -> Self {
        let lock = Self::lock();
        let vars = pairs
            .iter()
            .map(|(k, v)| {
                let prev = std::env::var(k).ok();
                // SAFETY: serialized by `_lock` above; no other thread
                // reads/writes these vars while this guard is alive.
                unsafe { std::env::set_var(k, v) };
                (*k, prev)
            })
            .collect();
        Self { vars, _lock: lock }
    }

    /// Remove each named var, remembering whatever was there before so
    /// [`Drop`] can restore it.
    #[must_use]
    pub fn unset(names: &[&'static str]) -> Self {
        let lock = Self::lock();
        let vars = names
            .iter()
            .map(|k| {
                let prev = std::env::var(k).ok();
                // SAFETY: see `set` above.
                unsafe { std::env::remove_var(k) };
                (*k, prev)
            })
            .collect();
        Self { vars, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, prev) in &self.vars {
            match prev {
                // SAFETY: see `set` above.
                Some(v) => unsafe { std::env::set_var(k, v) },
                // SAFETY: see `set` above.
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}
