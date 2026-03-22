#![no_std]
#![feature(asm_experimental_arch)]
#![feature(type_alias_impl_trait)]
#![feature(trait_alias)]
#![feature(impl_trait_in_assoc_type)]
#![no_main]
#![feature(associated_type_defaults)]
#![cfg_attr(feature = "jc8048w550c", feature(generic_const_exprs))]
#![cfg_attr(feature = "jc8048w550c", allow(incomplete_features))]

#[macro_use]
pub mod log_ext;

pub mod terminal;

pub mod backlight;
pub mod flash_map;
pub mod framework;
pub mod ui_loop;
#[cfg(feature = "wt32-sc01-plus")]
pub mod wt32_sc01_plus;
#[cfg(feature = "jc8048w550c")]
pub mod jc8048w550c;

#[macro_use]
pub mod framework_web_app;
pub mod improv_wifi;
pub mod license;
// pub mod sdcard;
pub mod ota;
pub mod sdcard_spi;
pub mod sdcard_store;
pub mod slint_ext;
pub mod touch;
#[cfg(feature = "wt32-sc01-plus")]
pub mod ft6x36_adapter;
#[cfg(feature = "jc8048w550c")]
pub mod gt9x_adapter;
#[cfg(feature = "jc8048w550c")]
#[path = "rgb-display.rs"]
pub mod rgb_display;
pub mod web_server;
pub mod wifi;
#[macro_use]
pub mod utils;
pub mod settings;
pub mod ntp;
pub mod mdns;

extern crate alloc;

#[cfg(all(feature = "wt32-sc01-plus", feature = "jc8048w550c"))]
compile_error!("Only one board feature can be enabled at a time");

#[cfg(any(
    all(feature = "log_none", any(feature = "log_trace", feature = "log_debug", feature = "log_info", feature = "log_warn", feature = "log_error", feature = "log_fatal")),
    all(feature = "log_trace", any(feature = "log_debug", feature = "log_info", feature = "log_warn", feature = "log_error", feature = "log_fatal")),
    all(feature = "log_debug", any(feature = "log_info", feature = "log_warn", feature = "log_error", feature = "log_fatal")),
    all(feature = "log_info", any(feature = "log_warn", feature = "log_error", feature = "log_fatal")),
    all(feature = "log_warn", any(feature = "log_error", feature = "log_fatal")),
    all(feature = "log_error", feature = "log_fatal"),
))]
compile_error!("Only one log level feature can be enabled at a time");

pub mod prelude {
    pub use crate::flash_map::FlashMap;
    pub use crate::framework::Framework;
    pub use crate::framework::FrameworkSettings;
    pub use crate::license::LicenseManager;
    pub use crate::warn;
    pub use crate::sdcard_store::{SDCardStore, SDCardStoreErrorSource};
    pub use dbg;
    pub use dbgt;
    pub use debug;
    pub use error;
    pub use info;
    pub use mk_static;
    pub use term_error;
    pub use term_info;
    pub use trace;
    pub const FRAMEWORK_STA_STACK_RESOURCES: usize = 5; // potentially https captive +  ota + captive dns + ? initial firmware check if doen't complete + mDNS
    pub const FRAMEWORK_AP_STACK_RESOURCES: usize = 5;
    pub use crate::utils::AwaitHeap;
    pub use crate::utils::SpawnerHeapExt;
}

#[cfg(feature = "extern-random")]
pub static mut RNG: once_cell::sync::OnceCell<esp_hal::rng::Rng> = once_cell::sync::OnceCell::new();
#[cfg(feature = "extern-random")]
use rand::RngCore;
#[cfg(feature = "extern-random")]
#[no_mangle]
unsafe extern "Rust" fn __getrandom_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    let buf = unsafe {
        // fill the buffer with zeros
        core::ptr::write_bytes(dest, 0, len);
        // create mutable byte slice
        core::slice::from_raw_parts_mut(dest, len)
    };
    #[allow(static_mut_refs)]
    RNG.get_mut().unwrap().fill_bytes(buf);
    Ok(())
}
