use std::ptr::null_mut;
use std::sync::atomic::{Ordering::*, *};
// this stack uses the wrong orderings in some places and has ABA issues leading
// to the possibility of UAF and other bugs
pub struct BuggyStack<T> {
    head: AtomicPtr<BuggyNode<T>>,
    _boo: core::marker::PhantomData<T>,
}

struct BuggyNode<T> {
    data: T,
    next: AtomicPtr<BuggyNode<T>>,
}
impl<T> Drop for BuggyStack<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}

impl<T> BuggyStack<T> {
    pub const fn new() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
            _boo: core::marker::PhantomData,
        }
    }
}
impl<T> BuggyStack<T> {
    pub fn push(&self, data: T) {
        let n = Box::into_raw(Box::new(BuggyNode {
            next: AtomicPtr::new(null_mut()),
            data,
        }));
        let mut next = self.head.load(Relaxed);
        loop {
            unsafe {
                (*n).next.store(next, Relaxed);
            }
            match self.head.compare_exchange_weak(next, n, Release, Relaxed) {
                Ok(_) => break,
                Err(new) => next = new,
            }
        }
    }
    pub fn pop(&self) -> Option<T> {
        let mut n = self.head.load(Acquire);
        loop {
            if n.is_null() {
                return None;
            }
            let next = unsafe { (*n).next.load(Relaxed) };
            match self.head.compare_exchange_weak(n, next, Acquire, Acquire) {
                Ok(_) => break,
                Err(h) => n = h,
            }
        }
        debug_assert!(!n.is_null());
        let n = unsafe { Box::from_raw(n) };
        Some(n.data)
    }
}
// send+sync for sendable data.
unsafe impl<T> Send for BuggyStack<T> where T: Send + 'static {}
unsafe impl<T> Sync for BuggyStack<T> where T: Send + 'static {}

fn main() {
    cobb::run_test(cobb::TestCfg::<BuggyStack<usize>> {
        threads: if cfg!(miri) { 8 } else { 16 },
        iterations: if cfg!(miri) { 100 } else { 1000 },
        sub_iterations: if cfg!(miri) { 10 } else { 20 },
        setup: || BuggyStack::new(),
        test: |stk, tctx| {
            stk.push(tctx.thread_index());
            let _ = stk.pop();
        },
        ..Default::default()
    });
}
