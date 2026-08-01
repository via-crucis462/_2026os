//! The panic handler

use crate::arch::sbi::shutdown;
use core::hint::spin_loop;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

static PANICKING: AtomicBool = AtomicBool::new(false);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if PANICKING.swap(true, Ordering::SeqCst) {
        // Avoid panic -> shutdown -> panic recursion storms.
        loop {
            spin_loop();
        }
    }

    if let Some(location) = info.location() {
        println!(
            "[kernel] Panicked at {}:{} {}",
            location.file(),
            location.line(),
            info.message()
        );
    } else {
        println!("[kernel] Panicked: {}", info.message());
    }

    #[cfg(debug_assertions)]
    loop {
        spin_loop();
    }

    #[cfg(not(debug_assertions))]
    {
        println!("syncing disks...");
        crate::ext4fs::block_cache_sync_all();
        println!("shutting down...");
        shutdown();
        loop {
            spin_loop();
        }
    }
}
