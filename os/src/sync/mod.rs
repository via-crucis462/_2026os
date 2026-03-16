//! Synchronization and interior mutability primitives

mod mp;
mod semaphore;

pub use mp::{MPSafeCell, MPSafeGuard};
pub use semaphore::*;