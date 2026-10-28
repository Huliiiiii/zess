use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
};

pub struct Allocator;

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && ENABLED.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            add_bytes(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if ENABLED.load(Relaxed) {
            LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
        }
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        if !pointer.is_null() && ENABLED.load(Relaxed) {
            REALLOCATIONS.fetch_add(1, Relaxed);
            LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
            add_bytes(size);
        }
        pointer
    }
}

fn add_bytes(bytes: usize) {
    ALLOCATED_BYTES.fetch_add(bytes, Relaxed);
    let live = LIVE_BYTES.fetch_add(bytes, Relaxed) + bytes;
    PEAK_BYTES.fetch_max(live, Relaxed);
}

// The closure owns its result; its drop happens after counters are disabled.
// Fixtures must be constructed outside this window, and not freed inside it.
pub fn report<T>(size: usize, name: &str, build: impl FnOnce() -> T) {
    for counter in [
        &ALLOCATIONS,
        &REALLOCATIONS,
        &ALLOCATED_BYTES,
        &LIVE_BYTES,
        &PEAK_BYTES,
    ] {
        counter.store(0, Relaxed);
    }
    ENABLED.store(true, Relaxed);
    let value = build();
    ENABLED.store(false, Relaxed);
    println!(
        "{size},{name},{},{},{},{},{}",
        ALLOCATIONS.load(Relaxed),
        REALLOCATIONS.load(Relaxed),
        ALLOCATED_BYTES.load(Relaxed),
        LIVE_BYTES.load(Relaxed),
        PEAK_BYTES.load(Relaxed)
    );
    std::hint::black_box(value);
}
