use super::epoch::{AtomicEpoch, Epoch};
use crate::alloc::ObjectAllocator;
use crate::utils::{
    shim::sync::atomic::{fence, AtomicUsize, Ordering},
    wyrng::WyRng,
};
use std::cell::UnsafeCell;
use std::marker::PhantomData;

const COLLECT_CHANCE: u32 = 4;

/// The interface we need in order to work with the main GC state.
pub trait EbrState {
    type T;
    type A: ObjectAllocator<Self::T>;

    fn load_epoch(&self) -> Epoch;
    fn should_advance(&self) -> bool;
    fn try_cycle(&self);
}

/// Per thread state needed for the GC.
/// We store a local epoch, an active flag and a number generator used
/// for reducing the frequency of some operations.
pub struct ThreadState<G> {
    active: AtomicUsize,
    epoch: AtomicEpoch,
    rng: UnsafeCell<WyRng>,
    _m0: PhantomData<G>,
}

impl<G: EbrState> ThreadState<G> {
    pub fn new(state: &G, thread_id: u32) -> Self {
        let global_epoch = state.load_epoch();

        Self {
            active: AtomicUsize::new(0),
            epoch: AtomicEpoch::new(global_epoch),
            rng: UnsafeCell::new(WyRng::new(thread_id)),
            _m0: PhantomData,
        }
    }

    /// Check if we should try to advance the global epoch.
    ///
    /// We use random numbers here to reduce the frequency of this returning true.
    /// We do this because advancing the epoch is a rather expensive operation.
    fn should_advance(&self, state: &G) -> bool {
        let rng = unsafe { &mut *self.rng.get() };
        (rng.generate() % COLLECT_CHANCE == 0) && state.should_advance()
    }

    /// Check if the given thread is in a critical section.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst) == 0
    }

    /// Get the local epoch of the given thread.
    pub fn load_epoch(&self) -> Epoch {
        self.epoch.load()
    }

    /// Enter a critical section with the given thread.
    pub fn enter(&self, state: &G) {
        // since `active` is a counter we only need to
        // update the local epoch when we go from 0 to something else
        if self.active.fetch_add(1, Ordering::SeqCst) == 0 {
            let global_epoch = state.load_epoch();
            self.epoch.store(global_epoch);
            fence(Ordering::SeqCst);
        }
    }

    /// Exit a critical section with the given thread.
    pub fn exit(&self, state: &G) {
        // decrement the `active` counter and fetch the previous value
        let prev_active = self.active.fetch_sub(1, Ordering::SeqCst);

        // if the counter wraps we've called exit more than enter which is not allowed
        debug_assert!(prev_active != 0);

        // check if we should try to advance the epoch if it reaches 0
        if prev_active == 1 {
            if self.should_advance(state) {
                state.try_cycle();
            }
        }
    }
}

unsafe impl<G: Sync> Sync for ThreadState<G> {}
