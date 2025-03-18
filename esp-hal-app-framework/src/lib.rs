#![no_std]
#![feature(asm_experimental_arch)]
#![feature(type_alias_impl_trait)]
#![feature(trait_alias)]
#![feature(impl_trait_in_assoc_type)]
#![feature(async_closure)]
#![no_main]
#![feature(associated_type_defaults)]

#[macro_use]
pub mod log_ext;

pub mod terminal;

#[cfg(feature="wt32-sc01-plus")]
pub mod wt32_sc01_plus;
pub mod flash_map;
pub mod framework;

#[macro_use]
pub mod framework_web_app;
pub mod improv_wifi;
pub mod license;
pub mod sdcard;
pub mod slint_ext;
pub mod touch;
pub mod web_server;
pub mod wifi;
pub mod ota;
#[macro_use]
pub mod utils;

extern crate alloc;

pub mod prelude {
    pub use debug;
    pub use dbg;
    pub use dbgt;
    pub use info;
    pub use trace;
    pub use crate::warn;
    pub use error;
    pub use term_info;
    pub use term_error;
    pub use mk_static;
    pub use crate::framework::Framework;
    pub use crate::framework::FrameworkSettings;
    pub use crate::license::LicenseManager;
    pub use crate::flash_map::FlashMap;
    pub const FRAMEWORK_STA_STACK_RESOURCES: usize = 4;  // potentially https captive +  ota + captive dns + ? initial firmware check if doen't complete 
    pub const FRAMEWORK_AP_STACK_RESOURCES: usize = 4;
}


#[cfg(feature="extern-random")]
pub static mut RNG: once_cell::sync::OnceCell<esp_hal::rng::Rng> = once_cell::sync::OnceCell::new();
#[cfg(feature="extern-random")]
use rand::RngCore;
#[cfg(feature="extern-random")]
#[no_mangle]
unsafe extern "Rust" fn __getrandom_custom(dest: *mut u8, len: usize) -> Result<(), getrandom::Error> {
    let mut buf = unsafe {
        // fill the buffer with zeros
        core::ptr::write_bytes(dest, 0, len);
        // create mutable byte slice
        core::slice::from_raw_parts_mut(dest, len)
    };
    #[allow(static_mut_refs)]
    RNG.get_mut().unwrap().fill_bytes(&mut buf);
    Ok(())
}
