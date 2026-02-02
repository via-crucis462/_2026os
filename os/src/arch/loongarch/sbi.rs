/// LoongArch64 SBI/firmware stubs.
/// Replace with real firmware calls once loongarch64 platform is wired up.
#[allow(dead_code)]
pub fn console_putchar(_c: usize) {}

#[allow(dead_code)]
pub fn console_getchar() -> usize {
    0
}

#[allow(dead_code)]
pub fn set_timer(_timer: usize) {}

#[allow(dead_code)]
pub fn shutdown() -> ! {
    loop {}
}
