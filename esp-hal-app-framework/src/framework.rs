use alloc::{format, rc::Rc, string::String, vec, vec::Vec};
use embassy_time::Timer;
use esp_hal::gpio::{AnyPin, GpioPin, Input, Pull};
use core::cell::RefCell;
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_executor::Spawner;
use embassy_futures::block_on;
use embassy_net::Stack;
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    pubsub::{PubSubChannel, Publisher, Subscriber},
};
use esp_mbedtls::TlsReference;
use esp_storage::FlashStorage;

use super::{
    flash_map::FlashMap, framework_web_app::derive_key, ota::ota_task, terminal::Terminal,
};
use crate::{ota::OtaRequest, web_server::WebServerCommand};

const WIFI_CONFIG_KEY: &str = "__wifi__";
const FIXED_CONFIG_KEY: &str = "__fixed_key__";
const DISPLAY_CONFIG_KEY: &str = "__display_";
// const WEB_SERVER_COMMANDS_LISTENERS: usize = WEB_SERVER_NUM_LISTENERS + 1 + 1; // web_server listeners + potentially https captive if on https + 1 for use by app_config to monitor if required to behave accordingly

// calculation is as above, but to avoid generics going into embassy tasks, use here a number large enough, at very little cost in memory
// Should be enough for the largest number per web application, since they use different instances, but this is the max number of listeners to control
// Not nice, but good enough for now
const WEB_SERVER_COMMANDS_LISTENERS: usize = 20;

#[derive(Clone, Copy)]
pub enum WebConfigMode {
    AP,
    STA,
}
#[derive(serde::Deserialize, serde::Serialize)]
pub struct WifiConfig {
    pub ssid: Option<String>,
    pub password: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct FixedKeyConfig {
    pub key: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct DisplayConfig {
    pub dimming_timeout: Option<u64>,
    pub dimming_percent: Option<u8>,
    pub blackout_timeout: Option<u64>,
}

pub struct FrameworkSettings {
    pub ota_domain: &'static str,
    pub ota_path: &'static str,
    pub ota_toml_filename: &'static str,
    pub ota_certs: &'static str,

    pub ap_addr: (u8, u8, u8, u8),

    pub web_server_https: bool,
    pub web_server_port: u16,
    pub web_server_captive: bool,
    #[allow(dead_code)]
    pub web_server_num_listeners: usize,
    pub web_server_tls_certificate: &'static str,
    pub web_server_tls_private_key: &'static str,

    pub web_app_domain: &'static str,
    pub web_app_security_key_length: usize,
    pub web_app_salt: &'static str,
    pub web_app_key_derivation_iterations: u32,

    pub app_cargo_pkg_name: &'static str,
    pub app_cargo_pkg_version: &'static str,

    pub default_fixed_security_key: Option<String>,
}

pub type WebServerCommands =
    PubSubChannel<NoopRawMutex, WebServerCommand, 2, WEB_SERVER_COMMANDS_LISTENERS, 1>;
#[allow(dead_code)]
pub type WebServerPublisher =
    Publisher<'static, NoopRawMutex, WebServerCommand, 2, WEB_SERVER_COMMANDS_LISTENERS, 1>;
pub type WebServerSubscriber =
    Subscriber<'static, NoopRawMutex, WebServerCommand, 2, WEB_SERVER_COMMANDS_LISTENERS, 1>;

pub struct Framework {
    pub settings: FrameworkSettings,
    observers: Vec<alloc::rc::Weak<RefCell<dyn FrameworkObserver>>>,
    framework: Option<Rc<RefCell<Framework>>>,
    flash_map: Rc<RefCell<FlashMap<BlockingAsync<FlashStorage>>>>,
    pub web_server_commands: &'static WebServerCommands,
    pub wifi_ssid: Option<String>,
    pub wifi_password: Option<String>,
    pub fixed_key: Option<String>,

    pub display_dimming_timeout: u64,
    pub display_dimming_percent: u8,
    pub display_blackout_timeout: u64,
    pub undim_display:
        &'static embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, ()>,

    pub spawner: Spawner,
    pub stack: Stack<'static>,
    pub tls: TlsReference<'static>,
    pub encryption_key: &'static RefCell<Vec<u8>>,

    config_processed_ok: Option<bool>,
    wifi_ok: Option<bool>,
}

impl Framework {
    pub fn new(
        settings: FrameworkSettings,
        flash_map: Rc<RefCell<FlashMap<BlockingAsync<FlashStorage>>>>,
        spawner: Spawner,
        stack: Stack<'static>,
        tls: TlsReference<'static>,
        erase_wifi_key_settings_and_restart_gpio: Option<AnyPin>,
    ) -> Rc<RefCell<Self>> {
        Terminal::initialize();

        let web_server_commands = crate::mk_static!(WebServerCommands, WebServerCommands::new());

        let undim_display = crate::mk_static!(
            embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::NoopRawMutex, ()>,
            embassy_sync::signal::Signal::<embassy_sync::blocking_mutex::raw::NoopRawMutex, ()>::new()
        );

        let framework = Self {
            fixed_key: settings.default_fixed_security_key.clone(),
            observers: Vec::new(),
            framework: None,
            flash_map,
            web_server_commands,
            wifi_ssid: None,
            wifi_password: None,
            display_dimming_timeout: 60 * 2,
            display_dimming_percent: 10,
            display_blackout_timeout: 60 * 5,
            spawner,
            stack,
            tls,
            encryption_key: crate::mk_static!(RefCell<Vec<u8>>, RefCell::new(vec![])),
            undim_display,
            config_processed_ok: None,
            wifi_ok: None,
            settings,
        };
        let framework = Rc::new(RefCell::new(framework));

        if let Some(gpio) = erase_wifi_key_settings_and_restart_gpio {
            spawner.spawn(button_erase_wifi_key_and_restart_handler(gpio, framework.clone())).ok();
        }

        framework.borrow_mut().framework = Some(framework.clone());
        framework
    }

    pub fn load_config_flash_then_toml(&mut self, toml_str: &str) -> Result<(), String> {
        // Start by lading from flash, SDCard if exist will override
        if let Ok(Some(wifi_store)) = block_on(
            self.flash_map
                .borrow_mut()
                .fetch(String::from(WIFI_CONFIG_KEY)),
        ) {
            if let Ok(wifi_config) = serde_json::from_str::<WifiConfig>(&wifi_store) {
                self.wifi_ssid = wifi_config.ssid;
                self.wifi_password = wifi_config.password;
            }
        }

        if let Ok(Some(fixed_key_store)) = block_on(
            self.flash_map
                .borrow_mut()
                .fetch(String::from(FIXED_CONFIG_KEY)),
        ) {
            if let Ok(fixed_key_config) = serde_json::from_str::<FixedKeyConfig>(&fixed_key_store) {
                self.fixed_key = fixed_key_config.key;
            }
        }

        if let Ok(Some(display_store)) = block_on(
            self.flash_map
                .borrow_mut()
                .fetch(String::from(DISPLAY_CONFIG_KEY)),
        ) {
            if let Ok(display_config) = serde_json::from_str::<DisplayConfig>(&display_store) {
                self.display_dimming_timeout = display_config
                    .dimming_timeout
                    .unwrap_or(self.display_dimming_timeout);
                self.display_dimming_percent = display_config
                    .dimming_percent
                    .unwrap_or(self.display_dimming_percent);
                self.display_blackout_timeout = display_config
                    .blackout_timeout
                    .unwrap_or(self.display_blackout_timeout);
            }
        }

        let mut section = String::from("");

        let mut parse_errors = false;

        for (line_num, line) in toml_str.lines().enumerate() {
            // Trim whitespace and ignore empty lines or comments
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with("[") && line.ends_with("]") {
                section = String::from(&line[1..line.len() - 1]);
                continue;
            }

            // Check if the line contains a key-value pair
            if let Some((key, value)) = line.split_once('=') {
                // Trim key and value to remove any surrounding whitespace
                let key = key.trim();
                let value = value.trim().trim_matches('"'); // Remove surrounding quotes if present

                // Match the key and assign the value to the corresponding field
                let expanded_key = format!("{}_{}", &section, &key);
                match expanded_key.as_str() {
                    "wifi_ssid" => {
                        self.wifi_ssid = Some(String::from(value));
                        term_info!("Loaded WiFi credentials from SDCard (overriding Flash)");
                    }
                    "wifi_password" => self.wifi_password = Some(String::from(value)),
                    "fixed_key" => {
                        self.fixed_key = Some(String::from(value));
                    }
                    "display_dimming_timeout" => {
                        if let Ok(display_dimming_timeout) = value.parse::<u64>() {
                            self.display_dimming_timeout = display_dimming_timeout;
                        } else {
                            parse_errors = true;
                            term_error!(
                                "config file format error at display dimming_timeout at line {}",
                                line_num
                            );
                        }
                    }
                    "display_dimming_percent" => {
                        if let Ok(display_dimming_percent) = value.parse::<u8>() {
                            self.display_dimming_percent = display_dimming_percent;
                        } else {
                            parse_errors = true;
                            term_error!(
                                "config file format error at display dimming_percent at line {}",
                                line_num
                            );
                        }
                    }
                    "display_blackout_timeout" => {
                        if let Ok(display_blackout_timeout) = value.parse::<u64>() {
                            self.display_blackout_timeout = display_blackout_timeout;
                        } else {
                            parse_errors = true;
                            term_error!(
                                "config file format error at display blackout_timeout at line {}",
                                line_num
                            );
                        }
                    }
                    _ => {
                        // allow unknown rows because app_config might use them
                    }
                }
            } else {
                // Error(warning) on general syntax error in line will be reported by app_config
            }

            // TODO: add error handling with notification on missing mandatory selfs
            if parse_errors {
                self.config_processed_ok = Some(false);
                return Err(String::from("Parse Error"));
            }
        }
        self.config_processed_ok = Some(true);
        Ok(())
    }

    pub fn report_wifi(&mut self, status: bool, url: &str, ssid: &str) {
        let web_config_url = if status {
            url
        } else {
            "N/A - WiFi not connected"
        };
        self.wifi_ok = Some(status);
        self.notify_webapp_url_update(web_config_url, ssid);
        // self.check_status_so_far();
    }

    // not on self, since async across borrow on framework would most probably panic
    pub async fn wait_for_wifi(framework: &Rc<RefCell<Self>>) {
        let stack = framework.borrow().stack;
        loop {
            if let Some(_config) = stack.config_v4() {
                break;
            }
            Timer::after_millis(250).await;
        }
    }

    pub fn initialization_ok(&self) -> bool {
        matches!(self.config_processed_ok, Some(true))
            && self.wifi_ssid != None
            && self.wifi_password != None
    }

    #[allow(dead_code)]
    pub fn boot_completed(&self) -> bool {
        matches!(self.wifi_ok, Some(true))
    }

    // General
    pub fn reset_device(&self) {
        esp_hal::reset::software_reset();
    }

    // Fixed Security Key
    pub fn set_fixed_key(
        &mut self,
        key: &str,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        if key.is_empty() {
            self.fixed_key = None;
            return embassy_futures::block_on(
                self.flash_map
                    .borrow_mut()
                    .remove(String::from(FIXED_CONFIG_KEY)),
            );
        } else {
            self.fixed_key = Some(String::from(key));
            let fixed_key_config = FixedKeyConfig {
                key: Some(String::from(key)),
            };
            let fixed_key_store = serde_json::to_string(&fixed_key_config).unwrap();
            return self.store(String::from(FIXED_CONFIG_KEY), fixed_key_store);
        }
    }
    pub fn erase_stored_fixed_key(&mut self) {
        let _ = embassy_futures::block_on(
            self.flash_map
                .borrow_mut()
                .remove(String::from(FIXED_CONFIG_KEY)),
        );
        self.fixed_key = self.settings.default_fixed_security_key.clone();
    }

    // Wifi
    pub fn erase_stored_wifi_credentials(&mut self) {
        let _ = embassy_futures::block_on(
            self.flash_map
                .borrow_mut()
                .remove(String::from(WIFI_CONFIG_KEY)),
        );
        self.wifi_ssid = None;
        self.wifi_password = None;
    }

    pub fn set_wifi_credentials(
        &mut self,
        ssid: &str,
        password: &str,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        self.wifi_ssid = Some(String::from(ssid));
        self.wifi_password = Some(String::from(password));

        let wifi_config = WifiConfig {
            ssid: Some(String::from(ssid)),
            password: Some(String::from(password)),
        };

        let wifi_store = serde_json::to_string(&wifi_config).unwrap();

        self.store(String::from(WIFI_CONFIG_KEY), wifi_store)
    }

    // OTA
    pub fn update_firmware_ota(&self) {
        info!("Starting Firmware Upgrade Over the Air");
        self.spawner
            .spawn(ota_task(
                self.settings.ota_domain,
                self.settings.ota_path,
                self.settings.ota_toml_filename,
                self.settings.ota_certs,
                self.stack,
                self.tls,
                OtaRequest::Update,
                self.framework.as_ref().unwrap().clone(),
            ))
            .ok();
    }
    pub fn check_firmware_ota(&self) {
        info!("Checking Firmware Version Over the Air");
        self.spawner
            .spawn(ota_task(
                self.settings.ota_domain,
                self.settings.ota_path,
                self.settings.ota_toml_filename,
                self.settings.ota_certs,
                self.stack,
                self.tls,
                OtaRequest::CheckVersion,
                self.framework.as_ref().unwrap().clone(),
            ))
            .ok();
    }

    // Web App
    pub fn start_web_app(&self, stack: Stack<'static>, mode: WebConfigMode) {
        let salt: &[u8] = self.settings.web_app_salt.as_bytes();
        let iterations = self.settings.web_app_key_derivation_iterations;

        let mut buf_vec = vec![0; self.settings.web_app_security_key_length];
        let mut buf = buf_vec.as_mut_slice();

        let key_to_use;
        if let Some(key) = self.fixed_key.as_ref() {
            key_to_use = key.as_str();
        } else {
            fn number_to_ascii_from_list(n: u8) -> u8 {
                // characters to used, removed a few that are unclear/similar (iI0Oo)
                let charset = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz123456789-/$@?!";

                // Make sure the number is within the 0..255 range and map it to the charset
                let index = (n % 62) as usize; // % 62 ensures it stays in the 0..61 range
                charset[index]
            }

            getrandom::getrandom(&mut buf).unwrap();
            for x in buf.iter_mut() {
                *x = number_to_ascii_from_list(*x);
            }
            buf[0] = buf[0].to_ascii_uppercase(); // to make it easier to type in iPhone that starts with capital lette
            let key = core::str::from_utf8(&buf).unwrap();
            key_to_use = key;
        }
        self.encryption_key
            .replace(derive_key(key_to_use, salt, iterations));
        self.web_server_commands
            .publisher()
            .unwrap()
            .publish_immediate(WebServerCommand::Start(stack));
        self.notify_web_config_started(key_to_use, mode);
    }
    pub fn stop_web_app(&self) {
        self.web_server_commands
            .publisher()
            .unwrap()
            .publish_immediate(WebServerCommand::Stop);
        self.notify_web_config_stopped();
    }

    // Flash Storage
    pub fn store(
        &self,
        key: String,
        value: String,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        block_on(self.flash_map.borrow_mut().store(key, value))
    }
    pub fn fetch(
        &self,
        key: String,
    ) -> Result<Option<String>, sequential_storage::Error<esp_storage::FlashStorageError>> {
        block_on(self.flash_map.borrow_mut().fetch(key))
    }

    // Display
    pub fn set_display_settings(
        &mut self,
        dimming_timeout: u64,
        dimming_percent: u8,
        blackout_timeout: u64,
    ) -> Result<(), sequential_storage::Error<esp_storage::FlashStorageError>> {
        self.display_dimming_timeout = dimming_timeout;
        self.display_dimming_percent = dimming_percent;
        self.display_blackout_timeout = blackout_timeout;

        let display_config = DisplayConfig {
            dimming_timeout: Some(dimming_timeout),
            dimming_percent: Some(dimming_percent),
            blackout_timeout: Some(blackout_timeout),
        };

        let display_store = serde_json::to_string(&display_config).unwrap();

        self.store(String::from(DISPLAY_CONFIG_KEY), display_store)
    }
    pub fn undim_display(&self) {
        self.undim_display.signal(());
    }

    // Observers support
    pub fn subscribe(&mut self, observer: alloc::rc::Weak<RefCell<dyn FrameworkObserver>>) {
        self.observers.push(observer);
    }
    pub fn notify_web_config_started(&self, key: &str, mode: WebConfigMode) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_web_config_started(key, mode);
        }
    }
    pub fn notify_web_config_stopped(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_web_config_stopped();
        }
    }

    pub fn notify_ota_version_available(&self, version: &str, newer: bool) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer
                .borrow_mut()
                .on_ota_version_available(version, newer);
        }
    }
    pub fn notify_ota_start(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_ota_start();
        }
    }
    pub fn notify_ota_status(&self, text: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_ota_status(text);
        }
    }
    pub fn notify_ota_failed(&self, text: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_ota_failed(text);
        }
    }
    pub fn notify_ota_completed(&self, text: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_ota_completed(text);
        }
    }
    pub fn notify_wifi_sta_connected(&self) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_wifi_sta_connected();
        }
    }
    pub fn notify_initialization_completed(&self, status: bool) {
        debug!(
            "Notified on Initialization Completed {}",
            self.observers.len()
        );
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_initialization_completed(status);
        }
    }
    pub fn notify_webapp_url_update(&self, url: &str, ssid: &str) {
        for weak_observer in self.observers.iter() {
            let observer = weak_observer.upgrade().unwrap();
            observer.borrow_mut().on_webapp_url_update(url, ssid);
        }
    }
}

pub trait FrameworkObserver {
    fn on_webapp_url_update(&self, url: &str, ssid: &str);
    fn on_initialization_completed(&self, status: bool);
    fn on_ota_version_available(&self, version: &str, newer: bool);
    fn on_ota_start(&self);
    fn on_ota_status(&self, text: &str);
    fn on_ota_failed(&self, text: &str);
    fn on_ota_completed(&self, text: &str);
    fn on_web_config_started(&self, key: &str, mode: WebConfigMode);
    fn on_web_config_stopped(&self);
    fn on_wifi_sta_connected(&self);
}

#[embassy_executor::task]
pub async fn button_erase_wifi_key_and_restart_handler(boot_gpio: AnyPin, framework: Rc<RefCell<Framework>>) {
    info!("Boot button handler to reset wifi & security key settings installed");
    let mut boot_pin = Input::new(boot_gpio, Pull::None);
    loop {
        boot_pin.wait_for_low().await;
        boot_pin.wait_for_high().await;
        debug!("Boot Pin pressed");
        framework.borrow_mut().erase_stored_wifi_credentials();
        framework.borrow_mut().erase_stored_fixed_key();
        framework.borrow().reset_device();
    }
}
