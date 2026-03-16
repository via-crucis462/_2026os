//! Synchronization and interior mutability primitives

mod mp;
mod semaphore;

pub use mp::MPSafeCell;
pub use semaphore::Semaphore;