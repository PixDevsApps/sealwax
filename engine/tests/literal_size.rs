//! A literal's announced size isn't allocated before its data arrives (async-imap as patched in
//! `vendor/async-imap`). Measured with a global allocator of the test's own, so this file holds
//! one test and no other test's allocations happen in its process.

mod fake;

use fake::{Fake, Flavor};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// The bytes allocated now, and the most since the test set it.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
            PEAK.fetch_max(live, Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[tokio::test]
async fn a_literals_announced_size_isnt_allocated_before_its_data() {
    let fake = Fake::start(Flavor::Rev1).await;
    let mut conn = fake.connect().await;
    let data = "x".repeat(1 << 20);
    let reply = format!("* 1 FETCH (UID 1 BODY[] {{400000000}}\r\n{data}");
    fake.world().cut_with("NOOP", &reply);
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let error = conn.noop().await.unwrap_err();
    let grew = PEAK.load(Relaxed) - before;
    assert!(error.is_connection(), "{error}");
    assert_eq!(fake.world().cuts, 1);
    assert!(
        grew < 32 << 20,
        "{grew} bytes allocated for a literal of which 1 MiB arrived"
    );
}
