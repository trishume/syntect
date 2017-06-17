use std::cell::UnsafeCell;
use std::sync::{Mutex, Condvar};

use num_cpus;

use super::SyntaxSet;

/// This is intentionally not public. The `Send` implementation makes
/// this unsafe to use outside this module.
struct LazyInit<T> {
    inner: UnsafeCell<Option<T>>
}

/// A pool for parallelizing syntax parsing/highlighting. Constructs
/// a lazily-initialized pool of `SyntaxSet`s, each initialized the
/// same way. The first time each `SyntaxSet` gets used, the thread
/// using it runs its setup code.
///
/// Use syntax sets by passing a closure to `with_syntax_set()`. A
/// `SyntaxSet` will be removed from the pool, and a reference to it
/// passed to the given closure. Afterwards, the set will automatically
/// be returned to the pool.
pub struct SyntaxSetPool<F: Fn() -> SyntaxSet> {
    /// We intentionally use a *stack* of `SyntaxSet`s so that
    /// already-initialized syntaxes get reused as much as possible.
    syntaxes: Mutex<Vec<LazyInit<SyntaxSet>>>,
    has_syntax: Condvar,
    init_fn: F
}

// The unsafety boundary for this `unsafe` is not *this* module, but the
// *parent* module. With the way we're using this, `SyntaxSet` must not
// implement either `Clone` or `Copy`.
unsafe impl<T> Send for LazyInit<T> {}

impl<T> LazyInit<T> {
    /// Create a `LazyInit` with contents uninitialized.
    fn new() -> Self {
        LazyInit { inner: UnsafeCell::new(None) }
    }

    fn maybe_init<F>(&self, init_fn: F) -> *mut T where F: Fn() -> T {
        let inner_ptr = self.inner.get();
        unsafe {
            if let None = *inner_ptr {
                *inner_ptr = Some(init_fn());
            }

            match *inner_ptr {
                Some(ref mut inner) => inner as *mut T,
                None => unreachable!()
            }
        }
    }

    #[allow(dead_code)]
    fn get_or<F>(&self, init_fn: F) -> &T where F: Fn() -> T {
        unsafe { &*self.maybe_init(init_fn) }
    }

    fn get_mut_or<F>(&self, init_fn: F) -> &mut T where F: Fn() -> T {
        unsafe { &mut *self.maybe_init(init_fn) }
    }
}

struct Repeating<F, T> where F: Fn() -> T {
    item_fn: F
}

impl<F, T> Iterator for Repeating<F, T> where F: Fn() -> T {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        Some((self.item_fn)())
    }
}

fn repeating<F, T>(item_fn: F) -> Repeating<F, T> where F: Fn() -> T {
    Repeating { item_fn: item_fn }
}

impl<F> SyntaxSetPool<F> where F: Fn() -> SyntaxSet + Sync {
    /// Creates a pool which will initialize `SyntaxSet`s as needed by
    /// worker threads, and will only initialize, at maximum, an amount
    /// of sets equal to the number of CPUs on the current machine. All
    /// `SyntaxSet`s in the pool are initially uninitialized.
    pub fn new(init_fn: F) -> Self {
        Self::with_pool_size(init_fn, num_cpus::get())
    }
  
    /// Same as `new`, but with a defined maximum pool size.
    pub fn with_pool_size(init_fn: F, pool_size: usize) -> Self {
        SyntaxSetPool {
            syntaxes: Mutex::new(repeating(LazyInit::new).take(pool_size).collect()),
            has_syntax: Condvar::new(),
            init_fn: init_fn
        }
    }

    /// Run some code with a `SyntaxSet` available. Passes a `SyntaxSet`
    /// reference to the closure given. Said reference cannot escape the
    /// closure.
    ///
    /// Attempts to reuse an already-initialized `SyntaxSet` if possible;
    /// otherwise, if there are uninitialized `SyntaxSet`s in the pool,
    /// initializes one. If there simply aren't any `SyntaxSet`s available,
    /// blocks until one is available.
    pub fn with_syntax_set<G, R>(&self, go: G) -> R where G: FnOnce(&mut SyntaxSet) -> R {
        let syntax_init;

        {
            let mut syntaxes = self.syntaxes.lock().unwrap();
            loop {  // catch spurious wakeups on condvar
                match syntaxes.pop() {
                    Some(init) => {
                        syntax_init = init;
                        break;
                    },
                    None => {
                        syntaxes = self.has_syntax.wait(syntaxes).unwrap();
                    }
                }
            }
        }

        let result = go(syntax_init.get_mut_or(|| (self.init_fn)()));

        {
            let mut syntaxes = self.syntaxes.lock().unwrap();
            syntaxes.push(syntax_init);
            self.has_syntax.notify_one();
        }

        result
    }
}
