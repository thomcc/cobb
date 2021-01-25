use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};

// A mutex that uses Relaxed instead of Release when unlocking (allowing a data race)
struct BuggyMutex<T>(AtomicBool, UnsafeCell<T>);

unsafe impl<T: Send> Send for BuggyMutex<T> {}
unsafe impl<T: Send> Sync for BuggyMutex<T> {}
impl<T> BuggyMutex<T> {
    pub const fn new(v: T) -> Self {
        Self(AtomicBool::new(false), UnsafeCell::new(v))
    }
    pub fn lock(&self) -> Guard<'_, T> {
        self.raw_lock();
        Guard(self)
    }
    fn raw_lock(&self) {
        while self.0.swap(true, Ordering::Acquire) {
            std::thread::yield_now();
        }
    }
    fn raw_unlock(&self) {
        // this more or less the bug spin::RwLock used to have
        self.0.store(false, Ordering::Relaxed);
    }
}
pub struct Guard<'a, T: 'a>(&'a BuggyMutex<T>);
impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.0.raw_unlock();
    }
}
impl<T> std::ops::Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*(self.0).1.get() }
    }
}
impl<T> std::ops::DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *(self.0).1.get() }
    }
}

fn main() {
    cobb::run_test(cobb::TestCfg::<BuggyMutex<usize>> {
        threads: 16,
        iterations: 1000,
        setup: || BuggyMutex::new(0),
        test: |mutex, tctx| {
            *mutex.lock() += tctx.thread_index();
        },
        before_each: |m| {
            *m.lock() = 0;
        },
        after_each: |m| {
            assert_eq!((0..16usize).sum::<usize>(), *m.lock());
        },
        ..Default::default()
    });
}
