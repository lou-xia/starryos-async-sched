#![no_std]

pub use vsched2::*;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    panic_loop();
}

/// 导出此符号，从而确认当在vdso中panic时，会在哪个地址循环。
#[no_mangle]
pub fn panic_loop() -> ! {
    loop {}
}

