use core::cell::RefCell;

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use picoserve::{
    extract::FromRequest,
    io::Read,
    request::{Request, RequestBody, RequestParts},
    response::{IntoResponse, ResponseWriter, StatusCode},
    ResponseSent,
};
use serde::{Deserialize, Serialize};

use crate::{framework::Framework, ota::OtaRequest};

use super::{
    runtime::{
        extract_web_app_request, write_rejection, BodyReadRejection, LimitedBodyString, WebAppState,
    },
    security::{ctr_decrypt, Encryptable, EncryptableCTR, EncryptedRejection},
};

pub(super) async fn handle_framework_get<MoreState, R, W, Handler>(
    request: Request<'_, R>,
    response_writer: W,
    state: &WebAppState<MoreState>,
    handler: Handler,
) -> Result<ResponseSent, W::Error>
where
    R: Read,
    W: ResponseWriter<Error = R::Error>,
    Handler: FnOnce(&'static RefCell<Vec<u8>>, Rc<RefCell<Framework>>) -> String,
{
    let payload = handler(state.encryption.0, state.framework.0.clone());
    payload
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

pub(super) async fn handle_framework_post<T, MoreState, R, W, Handler>(
    state: &WebAppState<MoreState>,
    mut request: Request<'_, R>,
    response_writer: W,
    handler: Handler,
) -> Result<ResponseSent, W::Error>
where
    T: for<'r> FromRequest<'r, WebAppState<MoreState>, Rejection = EncryptedRejection>,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
    Handler: FnOnce(&'static RefCell<Vec<u8>>, Rc<RefCell<Framework>>, T) -> String,
{
    let input = match extract_web_app_request(state, request.parts, request.body_connection.body())
        .await
    {
        Ok(value) => value,
        Err(err) => return write_rejection(request.body_connection, response_writer, err).await,
    };

    let payload = handler(state.encryption.0, state.framework.0.clone(), input);
    payload
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

pub(super) fn handle_framework_wifi_config_post(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
    WifiConfigDTO { ssid, password }: WifiConfigDTO,
) -> String {
    match framework
        .borrow_mut()
        .set_wifi_credentials(&ssid, &password)
    {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

pub(super) fn handle_framework_wifi_config_get(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
) -> String {
    WifiConfigDTO {
        ssid: framework
            .borrow()
            .wifi_ssid
            .as_ref()
            .unwrap_or(&String::from(""))
            .clone(),
        password: framework
            .borrow()
            .wifi_password
            .as_ref()
            .unwrap_or(&String::from(""))
            .clone(),
    }
    .encrypt(&key.borrow())
}

pub(super) fn handle_framework_device_name_config_post(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
    DeviceNameDTO { name }: DeviceNameDTO,
) -> String {
    match framework.borrow_mut().set_device_name(&name) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

pub(super) fn handle_framework_device_name_config_get(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
) -> String {
    DeviceNameDTO {
        name: framework
            .borrow()
            .device_name
            .as_ref()
            .unwrap_or(&String::from(""))
            .clone(),
    }
    .encrypt(&key.borrow())
}

pub(super) fn handle_framework_reset_device(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
    ResetDeviceDTO {}: ResetDeviceDTO,
) -> String {
    framework.borrow_mut().reset_device_safer(None);
    SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow())
}

pub(super) fn handle_framework_display_config_post(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
    DisplayConfigDTO {
        dimming_timeout,
        dimming_percent,
        blackout_timeout,
    }: DisplayConfigDTO,
) -> String {
    match framework.borrow_mut().set_display_settings(
        dimming_timeout,
        dimming_percent,
        blackout_timeout,
    ) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

pub(super) fn handle_framework_display_config_get(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
) -> String {
    let framework = framework.borrow();
    DisplayConfigDTO {
        dimming_timeout: framework.display_dimming_timeout,
        dimming_percent: framework.display_dimming_percent,
        blackout_timeout: framework.display_blackout_timeout,
    }
    .encrypt(&key.borrow())
}

pub(super) fn handle_framework_test_key(
    key: &'static RefCell<Vec<u8>>,
    _framework: Rc<RefCell<Framework>>,
    TestKeyDTO { test: _test }: TestKeyDTO,
) -> String {
    TestKeyResponseDTO { error_text: None }.encrypt(&key.borrow())
}

pub(super) fn handle_framework_fixed_key_config(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
    FixedKeyConfigDTO { key: fixed_key }: FixedKeyConfigDTO,
) -> String {
    match framework.borrow_mut().set_fixed_key(&fixed_key) {
        Ok(_) => SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow()),
        Err(e) => SetConfigResponseDTO {
            error_text: Some(format!("{e:?}")),
        }
        .encrypt(&key.borrow()),
    }
}

pub(super) fn handle_framework_ota_request(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
    OtaRequestDTO { request }: OtaRequestDTO,
) -> String {
    framework.borrow().submit_ota_request(request);
    SetConfigResponseDTO { error_text: None }.encrypt(&key.borrow())
}

pub(super) fn handle_framework_ota_config_get(
    key: &'static RefCell<Vec<u8>>,
    framework: Rc<RefCell<Framework>>,
) -> String {
    let framework = framework.borrow();
    OtaStatusDTO {
        status: framework
            .ota_state
            .as_ref()
            .map_or(String::new(), |s| s.to_string()),
        curr_ver: framework.settings.app_cargo_pkg_version.to_string(),
    }
    .encrypt(&key.borrow())
}

async fn extract_limited_body_string<'r, MoreState, R: Read>(
    state: &'r WebAppState<MoreState>,
    request: &'r mut Request<'_, R>,
) -> Result<LimitedBodyString, BodyReadRejection> {
    LimitedBodyString::from_request(state, request.parts, request.body_connection.body()).await
}

pub(super) async fn handle_captive_test_key<
    MoreState,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
>(
    state: &WebAppState<MoreState>,
    mut request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let key = state.encryption.0;
    let LimitedBodyString(body) = match extract_limited_body_string(state, &mut request).await {
        Ok(value) => value,
        Err(err) => {
            return err
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }
    };

    let response = if ctr_decrypt(&key.borrow(), body.as_bytes()).is_ok() {
        (StatusCode::OK, "")
    } else {
        (StatusCode::FORBIDDEN, "")
    };
    response
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

pub(super) async fn handle_captive_fixed_key_config<
    MoreState,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
>(
    state: &WebAppState<MoreState>,
    mut request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let key = state.encryption.0;
    let framework = state.framework.0.clone();
    let LimitedBodyString(body) = match extract_limited_body_string(state, &mut request).await {
        Ok(value) => value,
        Err(err) => {
            return err
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }
    };

    let response = match ctr_decrypt(&key.borrow(), body.as_bytes()) {
        Ok(decrypted) => (
            StatusCode::OK,
            match serde_json::from_str::<FixedKeyConfigDTO>(&decrypted) {
                Ok(fixed_key_config) => match framework
                    .borrow_mut()
                    .set_fixed_key(&fixed_key_config.key)
                {
                    Ok(_) => SetConfigResponseDTO { error_text: None }.ctr_encrypt(&key.borrow()),
                    Err(e) => SetConfigResponseDTO {
                        error_text: Some(format!("{e:?}")),
                    }
                    .ctr_encrypt(&key.borrow()),
                },
                Err(e) => SetConfigResponseDTO {
                    error_text: Some(format!("{e:?}")),
                }
                .ctr_encrypt(&key.borrow()),
            },
        ),
        Err(e) => (StatusCode::FORBIDDEN, format!("Decryption Error: {e}")),
    };
    response
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

pub(super) async fn handle_captive_wifi_config_post<
    MoreState,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
>(
    state: &WebAppState<MoreState>,
    mut request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let key = state.encryption.0;
    let framework = state.framework.0.clone();
    let LimitedBodyString(body) = match extract_limited_body_string(state, &mut request).await {
        Ok(value) => value,
        Err(err) => {
            return err
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }
    };

    let response = match ctr_decrypt(&key.borrow(), body.as_bytes()) {
        Ok(decrypted) => (
            StatusCode::OK,
            match serde_json::from_str::<WifiConfigDTO>(&decrypted) {
                Ok(wifi_config) => match framework
                    .borrow_mut()
                    .set_wifi_credentials(&wifi_config.ssid, &wifi_config.password)
                {
                    Ok(_) => SetConfigResponseDTO { error_text: None }.ctr_encrypt(&key.borrow()),
                    Err(e) => SetConfigResponseDTO {
                        error_text: Some(format!("{e:?}")),
                    }
                    .ctr_encrypt(&key.borrow()),
                },
                Err(e) => SetConfigResponseDTO {
                    error_text: Some(format!("{e:?}")),
                }
                .ctr_encrypt(&key.borrow()),
            },
        ),
        Err(e) => (StatusCode::FORBIDDEN, format!("Decryption Error: {e}")),
    };
    response
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

pub(super) async fn handle_captive_wifi_config_get<
    MoreState,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
>(
    state: &WebAppState<MoreState>,
    request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let key = state.encryption.0;
    let framework = state.framework.0.borrow();
    let payload = WifiConfigDTO {
        ssid: framework
            .wifi_ssid
            .as_ref()
            .unwrap_or(&String::from(""))
            .clone(),
        password: framework
            .wifi_password
            .as_ref()
            .unwrap_or(&String::from(""))
            .clone(),
    }
    .ctr_encrypt(&key.borrow());
    payload
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

pub(super) async fn handle_captive_device_name_config_post<
    MoreState,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
>(
    state: &WebAppState<MoreState>,
    mut request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let key = state.encryption.0;
    let framework = state.framework.0.clone();
    let LimitedBodyString(body) = match extract_limited_body_string(state, &mut request).await {
        Ok(value) => value,
        Err(err) => {
            return err
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }
    };

    let response = match ctr_decrypt(&key.borrow(), body.as_bytes()) {
        Ok(decrypted) => (
            StatusCode::OK,
            match serde_json::from_str::<DeviceNameDTO>(&decrypted) {
                Ok(device_name_config) => match framework
                    .borrow_mut()
                    .set_device_name(&device_name_config.name)
                {
                    Ok(_) => SetConfigResponseDTO { error_text: None }.ctr_encrypt(&key.borrow()),
                    Err(e) => SetConfigResponseDTO {
                        error_text: Some(format!("{e:?}")),
                    }
                    .ctr_encrypt(&key.borrow()),
                },
                Err(e) => SetConfigResponseDTO {
                    error_text: Some(format!("{e:?}")),
                }
                .ctr_encrypt(&key.borrow()),
            },
        ),
        Err(e) => (StatusCode::FORBIDDEN, format!("Decryption Error: {e}")),
    };
    response
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

pub(super) async fn handle_captive_device_name_config_get<
    MoreState,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
>(
    state: &WebAppState<MoreState>,
    request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let key = state.encryption.0;
    let framework = state.framework.0.borrow();
    let payload = DeviceNameDTO {
        name: framework
            .device_name
            .as_ref()
            .unwrap_or(&String::from(""))
            .clone(),
    }
    .ctr_encrypt(&key.borrow());
    payload
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

pub(super) async fn handle_captive_reset_device<
    MoreState,
    R: Read,
    W: ResponseWriter<Error = R::Error>,
>(
    state: &WebAppState<MoreState>,
    mut request: Request<'_, R>,
    response_writer: W,
) -> Result<ResponseSent, W::Error> {
    let key = state.encryption.0;
    let framework = state.framework.0.clone();
    let LimitedBodyString(body) = match extract_limited_body_string(state, &mut request).await {
        Ok(value) => value,
        Err(err) => {
            return err
                .write_to(request.body_connection.finalize().await?, response_writer)
                .await;
        }
    };

    let response = match ctr_decrypt(&key.borrow(), body.as_bytes()) {
        Ok(_) => {
            framework.borrow_mut().reset_device_safer(None);
            (
                StatusCode::OK,
                SetConfigResponseDTO { error_text: None }.ctr_encrypt(&key.borrow()),
            )
        }
        Err(e) => (StatusCode::FORBIDDEN, format!("Decryption Error: {e}")),
    };
    response
        .write_to(request.body_connection.finalize().await?, response_writer)
        .await
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct WifiConfigDTO {
    ssid: String,
    password: String,
}
crate::encrypted_input!(WifiConfigDTO);
impl EncryptableCTR for WifiConfigDTO {}

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct DeviceNameDTO {
    name: String,
}
crate::encrypted_input!(DeviceNameDTO);
impl EncryptableCTR for DeviceNameDTO {}

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct ResetDeviceDTO {}
crate::encrypted_input!(ResetDeviceDTO);

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct DisplayConfigDTO {
    dimming_timeout: u64,
    dimming_percent: u8,
    blackout_timeout: u64,
}
crate::encrypted_input!(DisplayConfigDTO);

#[derive(serde::Serialize)]
pub struct SetConfigResponseDTO {
    pub error_text: Option<String>,
}
impl EncryptableCTR for SetConfigResponseDTO {}

#[derive(Deserialize)]
pub(super) struct TestKeyDTO {
    test: String,
}
crate::encrypted_input!(TestKeyDTO);

#[derive(Deserialize)]
pub(super) struct FixedKeyConfigDTO {
    key: String,
}
crate::encrypted_input!(FixedKeyConfigDTO);

#[derive(Serialize)]
struct TestKeyResponseDTO {
    error_text: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct OtaRequestDTO {
    request: OtaRequest,
}
crate::encrypted_input!(OtaRequestDTO);

#[derive(Serialize)]
struct OtaStatusDTO {
    status: String,
    curr_ver: String,
}
