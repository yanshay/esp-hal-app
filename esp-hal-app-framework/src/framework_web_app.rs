mod api;
mod runtime;
mod security;

pub const LOG_WEB_MEASUREMENTS: bool = false;

pub fn log_boxed_route_future_state(size: usize) {
    if LOG_WEB_MEASUREMENTS {
        crate::info!("boxed route future state={} bytes", size);
    }
}

#[macro_export]
macro_rules! box_route_future {
    ($body:expr) => {{
        let route = alloc::boxed::Box::pin(async move { $body });
        $crate::framework_web_app::log_boxed_route_future_state(core::mem::size_of_val(
            route.as_ref().get_ref(),
        ));
        route.await
    }};
}

#[macro_export]
macro_rules! handled_route_future {
    ($body:expr) => {
        $crate::framework_web_app::RouteAttempt::Handled($crate::box_route_future!($body))
    };
}

mod routing;

pub use api::SetConfigResponseDTO;
pub use routing::{
    CustomNotFound, NestedAppWithWebAppState, NestedAppWithWebAppStateBuilder, RouteAttempt,
    WebAppBuilder,
};
pub use runtime::{
    extract_web_app_request, read_limited_body, write_rejection, ApiMethod, BodyReadRejection,
    Encryption, FrameworkState, LimitedBodyString, WebAppState,
};
pub use security::{
    decrypt, decrypt_compact, derive_key, encrypt, encrypt_bytes, encrypt_bytes_compact,
    Encryptable, EncryptableCTR, EncryptedRejection,
};
