use core::{cell::RefCell, future::ready};

use aes::cipher::{KeyIvInit, StreamCipher};
use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Key, Nonce,
};
use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use framework_macros::include_bytes_gz;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use picoserve::{
    extract::{FromRequest, State},
    io::Read,
    request::{RequestBody, RequestParts},
    response::{IntoResponse, Redirect, StatusCode},
    routing::{get, get_service, post, PathRouter},
    AppWithStateBuilder, ResponseSent,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{framework::Framework, ota::OtaRequest};

#[derive(Clone, Copy)]
pub struct Encryption(pub &'static RefCell<Vec<u8>>);

pub struct WebAppState {
    pub encryption: Encryption,
}
impl WebAppState {
    pub fn new(key: &'static RefCell<Vec<u8>>) -> Self {
        Self {
            encryption: Encryption(key),
        }
    }
}

impl picoserve::extract::FromRef<WebAppState> for Encryption {
    fn from_ref(state: &WebAppState) -> Self {
        state.encryption
    }
}

pub trait NestedAppWithWebAppStateBuilder: AppWithStateBuilder<State = WebAppState> {
    fn path_description(&self) -> &'static str;
}

pub struct WebAppBuilder<NestedMainAppBuilder: NestedAppWithWebAppStateBuilder> {
    pub app_builder: NestedMainAppBuilder,
    pub framework: Rc<RefCell<Framework>>,
    pub captive_html_gz: &'static [u8],
    pub web_app_html_gz: &'static [u8],
}

impl<NestedMainAppBuilder: NestedAppWithWebAppStateBuilder> AppWithStateBuilder
    for WebAppBuilder<NestedMainAppBuilder>
{
    type State = WebAppState;
    type PathRouter = impl PathRouter<WebAppState>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        let framework = self.framework;

        let router = picoserve::Router::new();
        let router = router.nest(
            self.app_builder.path_description(),
            self.app_builder.build_app(),
        );

        // Captive portal parts ///////////////////////////////////////////////////////////////////////////////////////

        let router = router
            .route(
                "/crypto-js-4.2.0.min.js",
                get_service(picoserve::response::File::with_content_type_and_headers(
                    "application/javascript; charset=utf-8",
                    include_bytes_gz!("src/static/crypto-js-4.2.0.min.js"),
                    &[("Content-Encoding", "gzip")],
                )),
            )
            .route(
                "/captive",
                get_service(picoserve::response::File::with_content_type_and_headers(
                    "text/html",
                    self.captive_html_gz,
                    &[("Content-Encoding", "gzip")],
                )),
            );
        let router = router.route(
            "/captive/api/test-key",
            post(
                async move |State(Encryption(key)): State<Encryption>, body: String| {
                    // Order matter, state first, post data last
                    if let Ok(_decrypted) = ctr_decrypt(&key.borrow(), body.as_bytes()) {
                        (StatusCode::OK, "")
                    } else {
                        (StatusCode::FORBIDDEN, "")
                    }
                },
            ),
        );

        let framework_clone_post = framework.clone();
        let router = router.route(
            "/captive/api/fixed-key-config",
            post(
                move |State(Encryption(key)): State<Encryption>, body: String| {
                    ready(match ctr_decrypt(&key.borrow(), body.as_bytes()) {
                        Ok(decrypted) => (StatusCode::OK, {
                            match serde_json::from_str::<FixedKeyConfigDTO>(&decrypted) {
                                Ok(fixed_key_config) => {
                                    match framework_clone_post
                                        .borrow_mut()
                                        .set_fixed_key(&fixed_key_config.key)
                                    {
                                        Ok(_) => SetConfigResponseDTO { error_text: None }
                                            .ctr_encrypt(&key.borrow()),
                                        Err(e) => SetConfigResponseDTO {
                                            error_text: Some(format!("{e:?}")),
                                        }
                                        .ctr_encrypt(&key.borrow()),
                                    }
                                }
                                Err(e) => SetConfigResponseDTO {
                                    error_text: Some(format!("{e:?}")),
                                }
                                .ctr_encrypt(&key.borrow()),
                            }
                        }),
                        Err(e) => (StatusCode::FORBIDDEN, format!("Decryption Error: {e}")),
                    })
                },
            ),
        );

        let framework_clone_post = framework.clone();
        let framework_clone_get = framework.clone();
        let router = router.route(
            "/captive/api/wifi-config",
            post(
                move |State(Encryption(key)): State<Encryption>, body: String| {
                    ready(match ctr_decrypt(&key.borrow(), body.as_bytes()) {
                        Ok(decrypted) => (StatusCode::OK, {
                            match serde_json::from_str::<WifiConfigDTO>(&decrypted) {
                                Ok(wifi_config) => {
                                    match framework_clone_post.borrow_mut().set_wifi_credentials(
                                        &wifi_config.ssid,
                                        &wifi_config.password,
                                    ) {
                                        Ok(_) => SetConfigResponseDTO { error_text: None }
                                            .ctr_encrypt(&key.borrow()),
                                        Err(e) => SetConfigResponseDTO {
                                            error_text: Some(format!("{e:?}")),
                                        }
                                        .ctr_encrypt(&key.borrow()),
                                    }
                                }
                                Err(e) => SetConfigResponseDTO {
                                    error_text: Some(format!("{e:?}")),
                                }
                                .ctr_encrypt(&key.borrow()),
                            }
                        }),
                        Err(e) => (StatusCode::FORBIDDEN, format!("Decryption Error: {e}")),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready(
                    WifiConfigDTO {
                        ssid: framework_clone_get
                            .borrow()
                            .wifi_ssid
                            .as_ref()
                            .unwrap_or(&String::from(""))
                            .clone(),
                        password: framework_clone_get
                            .borrow()
                            .wifi_password
                            .as_ref()
                            .unwrap_or(&String::from(""))
                            .clone(),
                    }
                    .ctr_encrypt(&key.borrow()),
                )
            }),
        );

        let framework_clone_post = framework.clone();
        let framework_clone_get = framework.clone();
        let router = router.route(
            "/captive/api/device-name-config",
            post(
                move |State(Encryption(key)): State<Encryption>, body: String| {
                    ready(match ctr_decrypt(&key.borrow(), body.as_bytes()) {
                        Ok(decrypted) => (StatusCode::OK, {
                            match serde_json::from_str::<DeviceNameDTO>(&decrypted) {
                                Ok(device_name_config) => {
                                    match framework_clone_post
                                        .borrow_mut()
                                        .set_device_name(&device_name_config.name)
                                    {
                                        Ok(_) => SetConfigResponseDTO { error_text: None }
                                            .ctr_encrypt(&key.borrow()),
                                        Err(e) => SetConfigResponseDTO {
                                            error_text: Some(format!("{e:?}")),
                                        }
                                        .ctr_encrypt(&key.borrow()),
                                    }
                                }
                                Err(e) => SetConfigResponseDTO {
                                    error_text: Some(format!("{e:?}")),
                                }
                                .ctr_encrypt(&key.borrow()),
                            }
                        }),
                        Err(e) => (StatusCode::FORBIDDEN, format!("Decryption Error: {e}")),
                    })
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready(
                    DeviceNameDTO {
                        name: framework_clone_get
                            .borrow()
                            .device_name
                            .as_ref()
                            .unwrap_or(&String::from(""))
                            .clone(),
                    }
                    .ctr_encrypt(&key.borrow()),
                )
            }),
        );

        let framework_clone = framework.clone();
        let router = router.route(
            "/captive/api/reset-device",
            post(
                move |State(Encryption(key)): State<Encryption>, body: String| {
                    ready(match ctr_decrypt(&key.borrow(), body.as_bytes()) {
                        Ok(_) => {
                            framework_clone.borrow_mut().reset_device();
                            (
                                StatusCode::OK,
                                SetConfigResponseDTO { error_text: None }
                                    .ctr_encrypt(&key.borrow()),
                            )
                        }
                        Err(e) => (StatusCode::FORBIDDEN, format!("Decryption Error: {e}")),
                    })
                },
            ),
        );

        // Standard config parts //////////////////////////////////////////////////////////////////////////////////////
        let router = router.route(
            "/config",
            get_service(picoserve::response::File::with_content_type_and_headers(
                "text/html",
                self.web_app_html_gz,
                &[("Content-Encoding", "gzip")],
            )),
        ); // main config page

        let router = router
            .route(
                // wasm (for encrypt/decrypt)
                "/pkg/device_wasm_bg.wasm",
                get_service(picoserve::response::File::with_content_type_and_headers(
                    "application/wasm",
                    include_bytes_gz!("src/static/device_wasm_bg.wasm"),
                    &[("Content-Encoding", "gzip")],
                )),
            )
            .route(
                "/pkg/device_wasm.js",
                get_service(picoserve::response::File::with_content_type_and_headers(
                    "application/javascript; charset=utf-8",
                    include_bytes_gz!("src/static/device_wasm.js"),
                    &[("Content-Encoding", "gzip")],
                )),
            );

        let framework_clone_post = framework.clone();
        let framework_clone_get = framework.clone();
        let router = router.route(
            "/api/wifi-config",
            post(
                move |State(Encryption(key)): State<Encryption>,
                      WifiConfigDTO { ssid, password }| {
                    // NOTE: ready is used here, I'm not fully clear why it's required but it is.
                    // It has to do with the method not being async and th need to borrow together.
                    // If I do async then I get issue with borrowing moved data.
                    // If I don't do async no the result (which is not future) then I have issue with borrow.
                    // Could be that if key will not be borrowed, or if like with picoserve Json there will be
                    //   an impl of future to the result (then need something other than String),
                    // it will be solved.
                    // So if need async here, need to search for proper solution
                    ready(
                        match framework_clone_post
                            .borrow_mut()
                            .set_wifi_credentials(&ssid, &password)
                        {
                            Ok(_) => {
                                SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow())
                            }
                            Err(e) => SetConfigResponseDTO {
                                error_text: Some(format!("{e:?}")),
                            }
                            .encrypt(&key.borrow()),
                        },
                    )
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready(
                    WifiConfigDTO {
                        ssid: framework_clone_get
                            .borrow()
                            .wifi_ssid
                            .as_ref()
                            .unwrap_or(&String::from(""))
                            .clone(),
                        password: framework_clone_get
                            .borrow()
                            .wifi_password
                            .as_ref()
                            .unwrap_or(&String::from(""))
                            .clone(),
                    }
                    .encrypt(&key.borrow()),
                )
            }),
        );

        let framework_clone_post = framework.clone();
        let framework_clone_get = framework.clone();
        let router = router.route(
            "/api/device-name-config",
            post(
                move |State(Encryption(key)): State<Encryption>, DeviceNameDTO { name }| {
                    ready(
                        match framework_clone_post.borrow_mut().set_device_name(&name) {
                            Ok(_) => {
                                SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow())
                            }
                            Err(e) => SetConfigResponseDTO {
                                error_text: Some(format!("{e:?}")),
                            }
                            .encrypt(&key.borrow()),
                        },
                    )
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                ready(
                    DeviceNameDTO {
                        name: framework_clone_get
                            .borrow()
                            .device_name
                            .as_ref()
                            .unwrap_or(&String::from(""))
                            .clone(),
                    }
                    .encrypt(&key.borrow()),
                )
            }),
        );

        let framework_clone = framework.clone();
        let router = router.route(
            "/api/reset-device",
            post(
                move |State(Encryption(key)): State<Encryption>, ResetDeviceDTO {}| {
                    framework_clone.borrow_mut().reset_device();
                    ready(SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()))
                },
            ),
        );

        let framework_clone_post = framework.clone();
        let framework_clone_get = framework.clone();
        let router = router.route(
            "/api/display-config",
            post(
                move |State(Encryption(key)): State<Encryption>,
                      DisplayConfigDTO {
                          dimming_timeout,
                          dimming_percent,
                          blackout_timeout,
                      }| {
                    ready(
                        match framework_clone_post.borrow_mut().set_display_settings(
                            dimming_timeout,
                            dimming_percent,
                            blackout_timeout,
                        ) {
                            Ok(_) => {
                                SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow())
                            }
                            Err(e) => SetConfigResponseDTO {
                                error_text: Some(format!("{e:?}")),
                            }
                            .encrypt(&key.borrow()),
                        },
                    )
                },
            )
            .get(move |State(Encryption(key)): State<Encryption>| {
                let framework = framework_clone_get.borrow();
                ready(
                    DisplayConfigDTO {
                        dimming_timeout: framework.display_dimming_timeout,
                        dimming_percent: framework.display_dimming_percent,
                        blackout_timeout: framework.display_blackout_timeout,
                    }
                    .encrypt(&key.borrow()),
                )
            }),
        );

        let router = router.route(
            "/api/test-key",
            post(
                async move |State(Encryption(key)): State<Encryption>,
                            TestKeyDTO { test: _test }| {
                    // Order matter, state first, post data last
                    TestKeyResponseDTO { error_text: None }.encrypt(&key.borrow())
                },
            ),
        );

        let framework_clone_post = framework.clone();
        let router = router.route(
            "/api/fixed-key-config",
            post(
                move |State(Encryption(key)): State<Encryption>,
                      FixedKeyConfigDTO { key: fixed_key }| {
                    ready(
                        match framework_clone_post.borrow_mut().set_fixed_key(&fixed_key) {
                            Ok(_) => {
                                SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow())
                            }
                            Err(e) => SetConfigResponseDTO {
                                error_text: Some(format!("{e:?}")),
                            }
                            .encrypt(&key.borrow()),
                        },
                    )
                },
            ),
        );

        let framework_clone_post = framework.clone();
        let router = router.route(
            "/api/ota-request",
            post(
                move |State(Encryption(key)): State<Encryption>, OtaRequestDTO { request }| {
                    ready({
                        framework_clone_post.borrow().submit_ota_request(request);
                        SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow())
                    })
                },
            ),
        );

        let framework_clone_get = framework.clone();
        let router = router.route(
            "/api/ota-config",
            get(move |State(Encryption(key)): State<Encryption>| {
                let framework = framework_clone_get.borrow();
                ready(
                    OtaStatusDTO {
                        status: framework
                            .ota_state
                            .as_ref()
                            .map_or(String::new(), |s| s.to_string()),
                        curr_ver: framework.settings.app_cargo_pkg_version.to_string(),
                    }
                    .encrypt(&key.borrow()),
                )
            }),
        );

        router
    }
}

pub struct CustomNotFound {
    pub web_server_captive: bool,
}

impl picoserve::routing::PathRouterService<WebAppState> for CustomNotFound {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &WebAppState,
        _path_parameters: (),
        path: picoserve::request::Path<'_>,
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        debug!("Redirecting request from '{}' to: '/'", path);
        if self.web_server_captive {
            // TODO: Theoretically, this should be only when in AP mode
            // TODO: probably best to handle only captive urls, for now everything. Could be confusing in case of not captive/AP
            Redirect::to("/captive")
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await
        } else {
            (StatusCode::NOT_FOUND, "")
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await
        }
    }
}

// Macro has to be used prior to usage, it is for encryption reasons (encryption code comes later)
#[macro_export]
macro_rules! encrypted_input {
    ($type:ident) => {
        impl<'r> FromRequest<'r, WebAppState> for $type {
            type Rejection = EncryptedRejection;

            async fn from_request<R: Read>(
                state: &'r WebAppState,
                _request_parts: RequestParts<'r>,
                request_body: RequestBody<'r, R>,
            ) -> Result<Self, Self::Rejection> {
                let encrypted_data = request_body
                    .read_all()
                    .await
                    .map_err(|_| EncryptedRejection::IoError)?;
                let key = state.encryption.0;
                let decrypted_data = decrypt(&key.borrow(), encrypted_data)
                    .map_err(|e| EncryptedRejection::DecryptionError(e))?;

                (serde_json::from_str(&decrypted_data) as Result<$type, _>)
                    .map_err(|e| EncryptedRejection::DeserializationError(e))
            }
        }
    };
}

#[derive(serde::Deserialize, serde::Serialize)]
struct WifiConfigDTO {
    ssid: String,
    password: String,
}
encrypted_input!(WifiConfigDTO);
impl EncryptableCTR for WifiConfigDTO {}

#[derive(serde::Deserialize, serde::Serialize)]
struct DeviceNameDTO {
    name: String,
}
encrypted_input!(DeviceNameDTO);
impl EncryptableCTR for DeviceNameDTO {}

#[derive(serde::Deserialize, serde::Serialize)]
struct ResetDeviceDTO {}
encrypted_input!(ResetDeviceDTO);

#[derive(serde::Deserialize, serde::Serialize)]
struct DisplayConfigDTO {
    dimming_timeout: u64,
    dimming_percent: u8,
    blackout_timeout: u64,
}
encrypted_input!(DisplayConfigDTO);

#[derive(serde::Deserialize, serde::Serialize)]
struct PrinterConfigDTO {
    ip: String,
    // name: String,
    serial: String,
    access_code: String,
}
encrypted_input!(PrinterConfigDTO);

#[derive(serde::Deserialize, serde::Serialize)]
struct TagConfigDTO {
    tag_scan_timeout: u64,
}
encrypted_input!(TagConfigDTO);

#[derive(serde::Serialize)]
pub struct SetConfigResponseDTO {
    pub error_text: Option<String>,
}
impl EncryptableCTR for SetConfigResponseDTO {}

#[derive(Deserialize)]
struct TestKeyDTO {
    test: String,
}
encrypted_input!(TestKeyDTO);

#[derive(Deserialize)]
struct FixedKeyConfigDTO {
    key: String,
}
encrypted_input!(FixedKeyConfigDTO);

#[derive(Serialize)]
struct TestKeyResponseDTO {
    error_text: Option<String>,
}

#[derive(Deserialize)]
struct OtaRequestDTO {
    request: OtaRequest,
}
encrypted_input!(OtaRequestDTO);

#[derive(Serialize)]
struct OtaStatusDTO {
    status: String,
    curr_ver: String,
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// AES-GCM Encryption ///////////////////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub fn derive_key(key: &str, salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut key_bytes = vec![0u8; 32]; // 32-byte key for AES-256
    pbkdf2_hmac::<Sha256>(key.as_bytes(), salt, iterations, &mut key_bytes);
    key_bytes
}

pub fn encrypt(key_bytes: &[u8], data: &str) -> String {
    // Derive key (32 bytes from a user-provided key)

    // let key_bytes = derive_key(key);
    let key = Key::<Aes256Gcm>::from_slice(key_bytes);

    let cipher = Aes256Gcm::new(key);

    // Generate random IV (12 bytes for AES-GCM)
    let mut iv_bytes = [0u8; 12];
    getrandom::getrandom(&mut iv_bytes).expect("Random should not fail");
    let iv = Nonce::from_slice(&iv_bytes);

    // Encrypt the data
    let ciphertext = cipher
        .encrypt(iv, Payload::from(data.as_bytes()))
        .expect("Encryption here should not fail"); // only memory issue?
    let res = format!(
        "{}{}",
        STANDARD_NO_PAD.encode(iv),
        STANDARD_NO_PAD.encode(ciphertext)
    );

    res
}

pub fn decrypt(key_bytes: &[u8], encrypted: &[u8]) -> Result<String, String> {
    //Derive key (32 bytes from a user-provided key)
    // let key_bytes = derive_key(key);
    let key = Key::<Aes256Gcm>::from_slice(key_bytes);

    let cipher = Aes256Gcm::new(key);

    // Decode IV and ciphertext
    let iv_bytes = STANDARD_NO_PAD
        .decode(&encrypted[..16])
        .map_err(|_| "Failed to decode IV".to_string())?;
    let iv = Nonce::from_slice(&iv_bytes);

    let ciphertext = STANDARD_NO_PAD
        .decode(&encrypted[16..])
        .map_err(|_| "Failed to decode ciphertext".to_string())?;

    // Decrypt the data
    let plaintext = cipher
        .decrypt(iv, Payload::from(&ciphertext[..])) // Use `&ciphertext[..]` here
        .map_err(|e| format!("Decryption failed : {e}"))?;

    String::from_utf8(plaintext).map_err(|_| "Failed to convert plaintext to string".to_string())
}

pub trait Encryptable<T: Serialize> {
    // fn encrypt(&self, key: &[u8], rng: Rng) -> EncryptedData;
    fn encrypt(&self, key: &[u8]) -> String;
}

impl<T> Encryptable<T> for T
where
    T: Serialize,
{
    fn encrypt(&self, key: &[u8]) -> String {
        let serialized = serde_json::to_string(self).expect("Serialization failed");
        encrypt(key, &serialized)
    }
}

#[derive(Debug)]
pub enum EncryptedRejection {
    IoError,
    DecryptionError(String),
    DeserializationError(serde_json::Error),
}

impl IntoResponse for EncryptedRejection {
    async fn write_to<R: Read, W: picoserve::response::ResponseWriter<Error = R::Error>>(
        self,
        connection: picoserve::response::Connection<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        match self {
            Self::IoError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "IO Error")
                    .write_to(connection, response_writer)
                    .await
            }
            Self::DeserializationError(error) => {
                (
                    StatusCode::BAD_REQUEST,
                    format_args!("Failed to parse JSON body: {error}"),
                )
                    .write_to(connection, response_writer)
                    .await
            }
            Self::DecryptionError(error) => {
                (
                    StatusCode::BAD_REQUEST,
                    format_args!("Failed to decrypt data: {error}"),
                )
                    .write_to(connection, response_writer)
                    .await
            }
        }
    }
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// AES-CTR Encryption ///////////////////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

type Aes256Ctr32BE = ctr::Ctr32BE<aes::Aes256>; // The 32 and BE are important for compatibility with CryptoJS

fn ctr_encrypt(key_bytes: &[u8], data: &str) -> String {
    let mut key = [0u8; 32];
    key.copy_from_slice(key_bytes);

    let mut iv = [0x24; 16]; // random, sent with data
    getrandom::getrandom(&mut iv).unwrap();

    let mut cipher = Aes256Ctr32BE::new(&key.into(), &iv.into());

    let mut dest = data.as_bytes().to_vec();
    cipher.apply_keystream(&mut dest);

    let encrypted_content = format!(
        "{}{}",
        STANDARD_NO_PAD.encode(iv).trim_end_matches('='),
        STANDARD_NO_PAD.encode(dest).trim_end_matches('=')
    );

    // calculate hmac tag prefix
    let mut hmac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key).expect("Invalid key length");
    hmac.update(encrypted_content.as_bytes());
    let hmac_tag = STANDARD_NO_PAD.encode(hmac.finalize().into_bytes().as_slice()); // sha 256: 32 bytes -> 43 base64 no padding
    format!("{hmac_tag}{encrypted_content}")
}

fn ctr_decrypt(key_bytes: &[u8], encrypted: &[u8]) -> Result<String, String> {
    // start verifying the hmac tag

    let hmac_base64 = core::str::from_utf8(&encrypted[..43])
        .map_err(|e| format!("Failed UTF8 decoding hmac {e}"))?;
    let received_hmac = STANDARD_NO_PAD
        .decode(hmac_base64)
        .map_err(|e| format!("Failed BASE64 decoding hmac {e}"))?;

    let encrypted_content = &encrypted[43..];

    let mut hmac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key_bytes).expect("Invalid key length");
    hmac.update(encrypted_content);
    let calced_hmac = hmac.finalize().into_bytes();
    let calced_hmac = calced_hmac.as_slice(); // sha 256: 32 bytes -> 43 base64 no padding

    if received_hmac != calced_hmac {
        return Err("Failed hmac validation".to_string());
    }

    let encrypted = encrypted_content;

    // decrypt

    let mut key = [0u8; 32];
    key.copy_from_slice(key_bytes);

    // Decode IV and ciphertext
    let iv_vec = STANDARD_NO_PAD
        .decode(&encrypted[..22])
        .map_err(|e| format!("Failed to decode IV: {e}"))?;
    let iv: &[u8; 16] = iv_vec.as_slice().try_into().unwrap();

    let mut cipher = Aes256Ctr32BE::new(&key.into(), iv.into());

    let mut dest = STANDARD_NO_PAD
        .decode(&encrypted[22..])
        .map_err(|_| "Failed to decode data".to_string())?;

    for chunk in dest.chunks_mut(1) {
        cipher
            .try_apply_keystream(chunk)
            .map_err(|e| format!("Decryption error {e}"))?;
    }
    String::from_utf8(dest).map_err(|_| "Failed to convert plaintext to string".to_string())
}

pub trait EncryptableCTR {
    // fn encrypt(&self, key: &[u8], rng: Rng) -> EncryptedData;
    // fn encrypt(&self, key: &[u8]) -> String;
    fn ctr_encrypt(&self, key: &[u8]) -> String
    where
        Self: Serialize,
    {
        let serialized = serde_json::to_string(self).expect("Serialization failed");
        ctr_encrypt(key, &serialized)
    }
}
