//! Lock-free coalescing wakeup notifications for libmpv's event and render callbacks.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn monotonic_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Monotonic timestamp in nanoseconds since process epoch, guaranteed non-zero
/// (`0` is reserved to mean "no pending notification").
#[inline]
fn now_ns() -> u64 {
    monotonic_epoch()
        .elapsed()
        .as_nanos()
        .clamp(1, u128::from(u64::MAX)) as u64
}

#[derive(Default)]
pub(super) struct EventNotification {
    pub(super) pending: AtomicBool,
    pub(super) wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl EventNotification {
    pub(super) fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            if let Some(wake) = self.wake.get() {
                wake();
            }
        }
    }

    pub(super) fn clear_pending(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

#[derive(Default)]
pub(super) struct RenderNotification {
    pub(super) pending: AtomicBool,
    pub(super) wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// Nanoseconds since [`monotonic_epoch`], or `0` when no notification is pending.
    pending_since_ns: AtomicU64,
}

impl RenderNotification {
    /// Completely lock-free: safe to invoke from libmpv's internal VO/decoder
    /// threads while libmpv holds internal locks.
    pub(super) fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.pending_since_ns.store(now_ns(), Ordering::Release);
            if let Some(wake) = self.wake.get() {
                wake();
            }
        }
    }

    pub(super) fn take(&self) -> Option<Duration> {
        if self.pending.swap(false, Ordering::AcqRel) {
            let since = self.pending_since_ns.swap(0, Ordering::AcqRel);
            let delay = if since == 0 {
                Duration::ZERO
            } else {
                Duration::from_nanos(now_ns().saturating_sub(since))
            };
            Some(delay)
        } else {
            None
        }
    }

    pub(super) fn pending_delay(&self) -> Duration {
        if !self.pending.load(Ordering::Acquire) {
            return Duration::ZERO;
        }
        let since = self.pending_since_ns.load(Ordering::Acquire);
        if since == 0 {
            Duration::ZERO
        } else {
            Duration::from_nanos(now_ns().saturating_sub(since))
        }
    }

    /// Re-invoke the registered wakeup callback without modifying `pending`.
    /// Used when `take_render_update` fails `try_lock` while `pending` is true,
    /// preventing a lost-wakeup freeze when mpv coalesces subsequent callbacks.
    pub(super) fn trigger_wake(&self) {
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }
}

pub(super) unsafe extern "C" fn on_render_update(userdata: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !userdata.is_null() {
            unsafe { &*(userdata as *const RenderNotification) }.notify();
        }
    }));
}

pub(super) unsafe extern "C" fn on_mpv_wakeup(userdata: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !userdata.is_null() {
            unsafe { &*(userdata as *const EventNotification) }.notify();
        }
    }));
}
