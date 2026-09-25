//! Acceptance: `Navigator::update_location` performs no heap allocations once
//! warmed up. Uses a counting global allocator.
#![cfg(feature = "serde")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use navcore::fixtures::{fixtures_dir, Fixture};
use navcore::{Navigator, NavigatorConfig};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call verbatim to the system allocator; the counter
// is a relaxed atomic and never touches allocator state.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn update_location_does_not_allocate_after_warmup() {
    for name in ["route_simple", "route_uturn", "route_detour"] {
        let fx = Fixture::load(fixtures_dir().join(format!("{name}.json"))).unwrap();
        let mut nav = Navigator::new(fx.route.clone(), NavigatorConfig::default()).unwrap();

        // Warm up: the first update may lazily size internal buffers.
        let (warm, rest) = fx.trace.split_at(2);
        for raw in warm {
            nav.update_location(*raw).unwrap();
        }

        let before = ALLOCS.load(Ordering::SeqCst);
        for raw in rest {
            let state = nav.update_location(*raw).unwrap();
            std::hint::black_box(state);
        }
        let after = ALLOCS.load(Ordering::SeqCst);
        assert_eq!(
            after - before,
            0,
            "{name}: {} allocations in {} updates",
            after - before,
            rest.len()
        );
    }
}
