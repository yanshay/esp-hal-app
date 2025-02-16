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
pub mod display;
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
}
