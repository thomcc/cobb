use std::sync::{Arc, RwLock};
use std::{
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};
use thread::JoinHandle;

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct CachePad<T> {
    _pre: MaybeUninit<[u8; 64]>,
    value: T,
    _post: MaybeUninit<[u8; 64]>,
}
impl<T> CachePad<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            _pre: MaybeUninit::uninit(),
            value,
            _post: MaybeUninit::uninit(),
        }
    }
}
impl<T> core::ops::Deref for CachePad<T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> core::ops::DerefMut for CachePad<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

pub struct TestCfg<T> {
    pub threads: usize,
    pub iterations: usize,
    pub sub_iterations: usize,
    pub groups: usize,
    pub setup: fn() -> T,
    pub teardown: fn(&mut T),
    pub test: fn(&T, &TestCtx),
    pub before_each: fn(&T),
    pub after_each: fn(&T),
    pub name: Option<&'static str>,
    pub reprioritize: Option<PrioritizeMode>,
    // TODO: flag for mucking with thread suspend/resume
    // so that the os reorders too.
}

impl<T> Clone for TestCfg<T> {
    fn clone(&self) -> Self {
        Self {
            threads: self.threads,
            iterations: self.iterations,
            sub_iterations: self.sub_iterations,
            groups: self.groups,
            teardown: self.teardown,
            test: self.test,
            setup: self.setup,
            name: self.name,
            before_each: self.before_each,
            after_each: self.after_each,
            reprioritize: self.reprioritize,
        }
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Copy)]
pub enum PrioritizeMode {
    Random,
    MostlyLo,
    MostlyHi,
    Count(usize),
}

impl<T> Default for TestCfg<T> {
    fn default() -> Self {
        Self {
            // threads can't be configured since it's how
            // many logical threads the
            threads: 4,
            sub_iterations: 1,
            iterations: match option_env!("COBB_ITERATIONS") {
                None | Some("0") | Some("") => 1000,
                Some(n) => n.parse::<usize>().unwrap_or_else(|_| {
                    eprintln!("couldn't parse COBB_ITERATIONS");
                    1000
                }),
            },
            groups: match option_env!("COBB_GROUPS") {
                None | Some("0") | Some("") => 1,
                Some(n) => n.parse::<usize>().unwrap_or_else(|_| {
                    eprintln!("couldn't parse COBB_GROUPS");
                    1
                }),
            },
            setup: || panic!("please provide setup"),
            teardown: |_| {},
            before_each: |_| {},
            after_each: |_| {},
            test: |_, _| {},
            name: None,
            reprioritize: match option_env!("COBB_REPRIORITIZE") {
                None | Some("") | Some("0") => None,
                Some(s) if s.eq_ignore_ascii_case("random") => Some(PrioritizeMode::Random),
                Some(s) if s.eq_ignore_ascii_case("mostly-high") => Some(PrioritizeMode::MostlyHi),
                Some(s) if s.eq_ignore_ascii_case("mostly-low") => Some(PrioritizeMode::MostlyLo),
                Some(s) if s.parse::<usize>().is_ok() => {
                    Some(PrioritizeMode::Count(s.parse::<usize>().unwrap()))
                }
                Some(s) => panic!(
                    "unknown mode {:?}, must be random|mostly-high|mostly-low",
                    s
                ),
            },
        }
    }
}

pub fn run_test<T: Send + Sync + 'static>(test: TestCfg<T>) {
    if test.groups <= 1 || cfg!(miri) {
        run_group(test, 0);
    } else {
        let name = test.name.unwrap_or("cobb");
        let join_handles = (0..test.groups)
            .map(|tg| {
                let test_for_group = test.clone();
                let jh = std::thread::Builder::new()
                    .name(format!("{} group {} driver", name, tg))
                    .spawn(move || run_group(test_for_group, tg))
                    .unwrap_or_else(|e| {
                        panic!("Failed to launch driver for test group {}: {:?}", tg, e)
                    });
                (jh, tg)
            })
            .collect::<Vec<_>>();

        let mut failed = vec![];
        for (jh, group_idx) in join_handles {
            jh.join().unwrap_or_else(|e| {
                eprintln!(
                    "{}: Group {} failed with error: {}",
                    name,
                    group_idx,
                    extract_msg(&e)
                );
                failed.push((e, group_idx));
            });
        }
        if !failed.is_empty() {
            eprintln!(
                "{}: {} groups failed: {:?}",
                name,
                failed.len(),
                failed.iter().map(|f| f.1).collect::<Vec<_>>()
            );
            std::panic::resume_unwind(failed.pop().unwrap().0);
        }
    }
}

fn run_group<T: Send + Sync + 'static>(test: TestCfg<T>, group_idx: usize) {
    let threads = test.threads;
    let iterations = if cfg!(miri) {
        test.iterations.max(100)
    } else {
        test.iterations
    };
    let verbose = matches!(option_env!("COBB_VERBOSE"), Some(s) if s != "" && s != "0");
    let test_name = test.name.unwrap_or("cobb");
    let after_events = (0..threads)
        .map(|_| Event::new_shared())
        .collect::<Vec<_>>();
    let before_evts = (0..threads)
        .map(|_| Event::new_shared())
        .collect::<Vec<_>>();
    let mut order = (0..threads).collect::<Vec<_>>();
    let pri_states = (0..threads)
        .map(|_| Arc::new(AtomicBool::new(true)))
        .collect::<Vec<_>>();
    let state = Arc::new(RwLock::new(CachePad::new((test.setup)())));
    // let mut thread_controllers = Vec::with_capacity(threads);
    let join_handles = (0..threads)
        .map(|thread_index| {
            let thread_control = TestThread {
                index: thread_index,
                sub_iterations: test.sub_iterations,
                iters: iterations,
                test_fn: test.test,
                test_state: Arc::clone(&state),
                before_event: Arc::clone(&before_evts[thread_index]),
                after_event: Arc::clone(&after_events[thread_index]),
                pri: Arc::clone(&pri_states[thread_index]),
            };
            let jh = std::thread::Builder::new()
                .name(format!(
                    "{} group {} runner {}",
                    test_name, group_idx, thread_index
                ))
                .spawn(move || run_test_thread(thread_control))
                .unwrap_or_else(|e| {
                    panic!(
                        "Cobb: failed to launch thread {} for group {}: {:?}",
                        thread_index, group_idx, e
                    )
                });
            (jh, thread_index)
        })
        .collect::<Vec<(JoinHandle<()>, usize)>>();
    let mut rng = Rng::new();
    for rep in 0..iterations {
        if verbose && group_idx == 0 {
            eprintln!("{}/{}:", rep, iterations);
        }
        if test.reprioritize.is_some() && rep != 0 && (rep % 200) == 0 && !cfg!(miri) {
            if verbose && group_idx == 0 {
                eprintln!("reprioritize");
            }
            let pris = match test.reprioritize.unwrap() {
                PrioritizeMode::Random => rng.between(1..threads - 1),
                PrioritizeMode::MostlyHi => 1,
                PrioritizeMode::MostlyLo => threads - 1,
                PrioritizeMode::Count(n) => n,
            };
            for i in (0..threads).map(|i| order[i]) {
                pri_states[i].store(i < pris, Ordering::Relaxed);
            }
        }
        rng.shuffle(&mut order);
        if rep == 0 {
            if verbose && group_idx == 0 {
                eprintln!("first iteration setup:");
            }
            let testv = (test.setup)();
            **state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = testv;
        }

        if verbose && group_idx == 0 {
            eprintln!("before_each:");
        }
        {
            (test.before_each)(
                &**state
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }

        if verbose && group_idx == 0 {
            eprintln!("running threads:");
        }

        for i in (0..threads).map(|i| order[i]) {
            // starting threads 1 at a time gives extra instruction scrambling.
            before_evts[i].notify();
        }

        // this one could be a WFMO if we had such a thing
        for i in (0..threads).map(|i| order[i]) {
            after_events[i].wait();
        }
        if verbose && group_idx == 0 {
            eprintln!("after_each:");
        }

        {
            (test.after_each)(
                &**state
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }
    // last kick to get threads out of iteratoin loop
    for i in (0..threads).map(|i| order[i]) {
        before_evts[i].notify();
    }
    let mut failed = vec![];
    for (jh, thread_index) in join_handles {
        jh.join().unwrap_or_else(|e| {
            eprintln!(
                "{}:Thread {} in group {} failed with error: {}",
                test_name,
                thread_index,
                group_idx,
                extract_msg(&e)
            );
            failed.push((e, thread_index));
        });
    }
    if !failed.is_empty() {
        eprintln!(
            "{}: {} threads in group {} failed: {:?}",
            test_name,
            failed.len(),
            group_idx,
            failed.iter().map(|f| f.1).collect::<Vec<_>>()
        );
        std::panic::resume_unwind(failed.pop().unwrap().0);
    }
    {
        (test.teardown)(
            &mut **state
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}
fn extract_msg(e: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = e.downcast_ref::<&'static str>() {
        s.to_string()
    } else if let Some(e) = e.downcast_ref::<String>() {
        e.clone()
    } else {
        format!("Unknown Any")
    }
}
#[derive(Copy, Clone)]
pub struct Rng(u64);
impl Rng {
    pub fn new() -> Self {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        Self(RandomState::new().build_hasher().finish() | 1)
    }
    // fn spawn(&mut self) -> Self {
    //     Self((!self.gen()).wrapping_mul(0xc0bb_15_c001))
    // }
    fn gen(&mut self) -> u64 {
        let x = self.0 ^ (self.0 >> 12);
        let x = x ^ (x << 25);
        self.0 = x ^ (x >> 27);
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn upto(&mut self, top: usize) -> usize {
        self.gen() as usize % top // todo: biased
    }
    fn between(&mut self, r: core::ops::Range<usize>) -> usize {
        self.upto(r.end - r.start) + r.start
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in 0..(v.len() - 1) {
            v.swap(i, self.between(i..v.len()));
        }
    }
}

#[repr(align(64))]
struct TestThread<T> {
    index: usize,
    iters: usize,
    sub_iterations: usize,
    test_state: Arc<RwLock<CachePad<T>>>,
    test_fn: fn(&T, &TestCtx),
    before_event: Arc<Event>,
    after_event: Arc<Event>,
    pri: Arc<AtomicBool>,
}

pub struct TestCtx {
    thread_index: usize,
    sub_iter: usize,
    rng: std::cell::Cell<Rng>,
}
impl TestCtx {
    /// The index of your thread, in the range between 0 and the specified
    /// TestCfg::threads value.
    pub fn thread_index(&self) -> usize {
        self.thread_index
    }
    /// Which iteration you're on between 0 and `TestCfg::sub_iterations` (which
    /// is usually 1).
    pub fn sub_iteration(&self) -> usize {
        self.sub_iter
    }
    /// Hint that if your thread got scheduled at this point, it may help expose
    /// bugs.
    pub fn sp(&self) {
        // self.sub_iter
        let mut rng = self.rng.get();
        let val = rng.gen();
        self.rng.set(rng);
        // if (val % 100) < 50
        {
            schedule_point((val >> 24) as u8);
        }
    }
}

fn set_own_priority(_high: bool) {
    /*
    #[cfg(all(target_vendor = "apple", not(miri)))]
    {
        const PRIO_DARWIN_THREAD: i32 = 3;
        const PRIO_DARWIN_BG: i32 = 4096;
        extern "C" {
            fn setpriority(which: i32, who: u32, prio: i32) -> i32;
        }
        let pri = if _high { 0 } else { PRIO_DARWIN_BG };
        unsafe { setpriority(PRIO_DARWIN_THREAD, 0, pri) };
    }*/
}

fn run_test_thread<T: Send + Sync + 'static>(t: TestThread<T>) {
    let TestThread {
        index: thread_index,
        sub_iterations,
        iters,
        test_state,
        test_fn,
        before_event,
        after_event,
        pri,
    } = t;
    let want_pri = pri.load(Ordering::Relaxed);
    set_own_priority(want_pri);
    let mut cur_pri = want_pri;
    before_event.wait(); //.unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut tctx = TestCtx {
        thread_index,
        sub_iter: 0,
        rng: std::cell::Cell::new(Rng::new()),
    };
    for _ in 0..iters {
        {
            let guard = test_state.read().unwrap();
            let state: &T = &*guard;
            for sub_iter in 0..sub_iterations.max(1) {
                tctx.sub_iter = sub_iter;
                (test_fn)(state, &tctx);
            }
        }
        after_event.notify();
        let want_pri = pri.load(Ordering::Relaxed);
        if want_pri != cur_pri {
            set_own_priority(want_pri);
            cur_pri = want_pri;
        }
        before_event.wait();
    }
}
#[derive(Default)]
pub struct Event {
    cv: std::sync::Condvar,
    mtx: std::sync::Mutex<bool>,
}
impl Event {
    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn wait(&self) {
        let g = self
            .mtx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut g = self
            .cv
            .wait_while(g, |stopped| !*stopped)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *g = false;
    }
    pub fn notify(&self) {
        let mut g = self
            .mtx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *g = true;
        self.cv.notify_one();
    }
}
fn schedule_point(r: u8) {
    use std::time::Duration;
    match r {
        0..=10 => thread::sleep(Duration::from_nanos(0)),
        // 6..=10 => thread::sleep(Duration::from_micros(1)),
        11..=15 => thread::sleep(Duration::from_millis(1)),
        16..=75 => thread::yield_now(),
        76..=125 => {
            for _ in 0..50usize {
                core::hint::spin_loop();
            }
        }
        225..=255 => {
            for _ in 0..=5 {
                thread::yield_now()
            }
        }
        // #[cfg(target_vendor = "apple")]
        // n @ 225..=255 => {
        //     extern "C" {
        //         // fn pthread_mach_thread_np(pthread: *core::ffi::c_void) -> u32;
        //         fn thread_switch(p: u32, o: i32, t: u32) -> i32;
        //     }
        //     unsafe {
        //         thread_switch(0, 1, (n > 240) as u32);
        //     }
        // }
        n => unsafe {
            for i in 0..(n as usize) {
                let mut g = 0;
                core::ptr::write_volatile(&mut g, i);
                let _ = core::ptr::read_volatile(&g);
            }
        },
    }
}
