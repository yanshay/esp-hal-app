use core::cell::RefCell;

use alloc::{rc::Rc, string::String, vec::Vec};
use picoserve::{
    extract::FromRequest,
    io::Read,
    request::{RequestBody, RequestBodyConnection, RequestParts},
    response::{IntoResponse, ResponseWriter, StatusCode},
    ResponseSent,
};

use crate::framework::Framework;

use super::security::EncryptedRejection;

#[derive(Clone, Copy)]
pub struct Encryption(pub &'static RefCell<Vec<u8>>);

#[derive(Clone)]
pub struct FrameworkState(pub Rc<RefCell<Framework>>);

pub struct WebAppState<MoreState> {
    pub encryption: Encryption,
    pub framework: FrameworkState,
    pub more_state: MoreState,
    pub request_body_max_bytes: usize,
}
impl<MoreState> WebAppState<MoreState> {
    pub fn new(
        key: &'static RefCell<Vec<u8>>,
        framework: Rc<RefCell<Framework>>,
        _more_state: MoreState,
        request_body_max_bytes: usize,
    ) -> Self {
        Self {
            encryption: Encryption(key),
            framework: FrameworkState(framework.clone()),
            more_state: _more_state,
            request_body_max_bytes,
        }
    }
}

impl<MoreState> picoserve::extract::FromRef<WebAppState<MoreState>> for Encryption {
    fn from_ref(state: &WebAppState<MoreState>) -> Self {
        state.encryption
    }
}

impl<MoreState> picoserve::extract::FromRef<WebAppState<MoreState>> for FrameworkState {
    fn from_ref(state: &WebAppState<MoreState>) -> Self {
        state.framework.clone()
    }
}

#[derive(Debug)]
pub enum BodyReadRejection {
    IoError,
    PayloadTooLarge {
        content_length: usize,
        max_length: usize,
    },
    BodyIsNotUtf8,
}

impl IntoResponse for BodyReadRejection {
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
            Self::PayloadTooLarge {
                content_length,
                max_length,
            } => {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format_args!(
                        "Payload too large: {content_length} bytes, max {max_length} bytes"
                    ),
                )
                    .write_to(connection, response_writer)
                    .await
            }
            Self::BodyIsNotUtf8 => {
                (StatusCode::BAD_REQUEST, "Body is not UTF-8")
                    .write_to(connection, response_writer)
                    .await
            }
        }
    }
}

pub async fn read_limited_body<R: Read>(
    request_body: RequestBody<'_, R>,
    max_length: usize,
) -> Result<Vec<u8>, BodyReadRejection> {
    let content_length = request_body.content_length();
    if content_length > max_length {
        return Err(BodyReadRejection::PayloadTooLarge {
            content_length,
            max_length,
        });
    }

    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(content_length)
        .map_err(|_| BodyReadRejection::PayloadTooLarge {
            content_length,
            max_length,
        })?;
    buffer.resize(content_length, 0);
    request_body
        .reader()
        .read_exact(buffer.as_mut_slice())
        .await
        .map_err(|_| BodyReadRejection::IoError)?;
    Ok(buffer)
}

pub struct LimitedBodyString(pub String);

impl<'r, MoreState> FromRequest<'r, WebAppState<MoreState>> for LimitedBodyString {
    type Rejection = BodyReadRejection;

    async fn from_request<R: Read>(
        state: &'r WebAppState<MoreState>,
        _request_parts: RequestParts<'r>,
        request_body: RequestBody<'r, R>,
    ) -> Result<Self, Self::Rejection> {
        String::from_utf8(read_limited_body(request_body, state.request_body_max_bytes).await?)
            .map(Self)
            .map_err(|_| BodyReadRejection::BodyIsNotUtf8)
    }
}

pub async fn extract_web_app_request<T, MoreState, R: Read>(
    state: &WebAppState<MoreState>,
    request_parts: RequestParts<'_>,
    request_body: RequestBody<'_, R>,
) -> Result<T, EncryptedRejection>
where
    T: for<'r> FromRequest<'r, WebAppState<MoreState>, Rejection = EncryptedRejection>,
{
    T::from_request(state, request_parts, request_body).await
}

pub async fn write_rejection<
    R: Read,
    W: ResponseWriter<Error = R::Error>,
    Rejection: IntoResponse,
>(
    body_connection: RequestBodyConnection<'_, R>,
    response_writer: W,
    rejection: Rejection,
) -> Result<ResponseSent, W::Error> {
    rejection
        .write_to(body_connection.finalize().await?, response_writer)
        .await
}

#[derive(Clone, Copy)]
pub enum ApiMethod {
    Get,
    Post,
    Other,
}

impl From<&str> for ApiMethod {
    fn from(method: &str) -> Self {
        match method {
            "GET" => Self::Get,
            "POST" => Self::Post,
            _ => Self::Other,
        }
    }
}
