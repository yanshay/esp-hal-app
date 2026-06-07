use core::{cell::RefCell, marker::PhantomData};

use alloc::{rc::Rc, string::ToString};
use framework_macros::include_bytes_gz;
use picoserve::{
    io::Read,
    request::{Path, Request},
    response::{IntoResponse, Redirect, ResponseWriter},
    routing::{PathRouter, PathRouterService, RequestHandlerService},
    AppWithStateBuilder, ResponseSent,
};

use crate::framework::{Framework, WebConfigMode};

use super::{
    api::{
        handle_captive_device_name_config_get, handle_captive_device_name_config_post,
        handle_captive_fixed_key_config, handle_captive_reset_device, handle_captive_test_key,
        handle_captive_wifi_config_get, handle_captive_wifi_config_post,
        handle_framework_device_name_config_get, handle_framework_device_name_config_post,
        handle_framework_display_config_get, handle_framework_display_config_post,
        handle_framework_fixed_key_config, handle_framework_get, handle_framework_ota_config_get,
        handle_framework_ota_request, handle_framework_post, handle_framework_reset_device,
        handle_framework_test_key, handle_framework_wifi_config_get,
        handle_framework_wifi_config_post,
    },
    runtime::{ApiMethod, WebAppState},
};

pub enum RouteAttempt<'p, 'r, R, W>
where
    R: Read,
    W: ResponseWriter<Error = R::Error>,
{
    Handled(Result<ResponseSent, W::Error>),
    NotMatched {
        path: Path<'p>,
        request: Request<'r, R>,
        response_writer: W,
    },
}

#[allow(async_fn_in_trait)]
pub trait NestedAppWithWebAppState<MoreState> {
    async fn try_handle_route<'p, 'r, R, W>(
        &self,
        state: &WebAppState<MoreState>,
        path: Path<'p>,
        request: Request<'r, R>,
        response_writer: W,
    ) -> RouteAttempt<'p, 'r, R, W>
    where
        R: Read,
        W: ResponseWriter<Error = R::Error>;

    async fn handle_fallback<'p, 'r, R, W>(
        &self,
        state: &WebAppState<MoreState>,
        path: Path<'p>,
        request: Request<'r, R>,
        response_writer: W,
        default_fallback: &CustomNotFound,
    ) -> Result<ResponseSent, W::Error>
    where
        R: Read,
        W: ResponseWriter<Error = R::Error>,
    {
        default_fallback
            .call_path_router_service(state, (), path, request, response_writer)
            .await
    }
}

pub trait NestedAppWithWebAppStateBuilder<MoreState> {
    type WebApp: NestedAppWithWebAppState<MoreState>;

    fn build_web_app(self) -> Self::WebApp;
}

pub struct WebAppBuilder<
    MoreState,
    NestedMainAppBuilder: NestedAppWithWebAppStateBuilder<MoreState>,
> {
    pub app_builder: NestedMainAppBuilder,
    pub framework: Rc<RefCell<Framework>>,
    pub captive_html_gz: &'static [u8],
    pub web_app_html_gz: &'static [u8],
    pub _phantom: PhantomData<MoreState>,
}

impl<MoreState, NestedMainAppBuilder: NestedAppWithWebAppStateBuilder<MoreState>>
    AppWithStateBuilder for WebAppBuilder<MoreState, NestedMainAppBuilder>
{
    type State = WebAppState<MoreState>;
    type PathRouter = impl PathRouter<WebAppState<MoreState>>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        let nested_app = self.app_builder.build_web_app();
        let default_fallback = CustomNotFound {
            web_server_captive: self.framework.borrow().settings.web_server_captive,
        };
        picoserve::Router::from_service(FrameworkWebService {
            nested_app,
            default_fallback,
            captive_html_gz: self.captive_html_gz,
            web_app_html_gz: self.web_app_html_gz,
            _phantom: PhantomData,
        })
    }
}

pub struct CustomNotFound {
    pub web_server_captive: bool,
}

struct FrameworkWebService<MoreState, NestedApp>
where
    NestedApp: NestedAppWithWebAppState<MoreState>,
{
    nested_app: NestedApp,
    default_fallback: CustomNotFound,
    captive_html_gz: &'static [u8],
    web_app_html_gz: &'static [u8],
    _phantom: PhantomData<MoreState>,
}

impl<MoreState, NestedApp> FrameworkWebService<MoreState, NestedApp>
where
    NestedApp: NestedAppWithWebAppState<MoreState>,
{
    async fn try_handle_framework_route<'p, 'r, R, W>(
        &self,
        state: &WebAppState<MoreState>,
        path: Path<'p>,
        request: Request<'r, R>,
        response_writer: W,
    ) -> RouteAttempt<'p, 'r, R, W>
    where
        R: Read,
        W: ResponseWriter<Error = R::Error>,
    {
        let method = ApiMethod::from(request.parts.method());
        match (method, path.encoded()) {
            (ApiMethod::Get, "/crypto-js-4.2.0.min.js") => handled_route_future!({
                picoserve::response::File::with_content_type_and_headers(
                    "application/javascript; charset=utf-8",
                    include_bytes_gz!("src/static/crypto-js-4.2.0.min.js"),
                    &[("Content-Encoding", "gzip")],
                )
                .call_request_handler_service(state, (), request, response_writer)
                .await
            }),
            (ApiMethod::Get, "/captive") => handled_route_future!({
                picoserve::response::File::with_content_type_and_headers(
                    "text/html",
                    self.captive_html_gz,
                    &[("Content-Encoding", "gzip")],
                )
                .call_request_handler_service(state, (), request, response_writer)
                .await
            }),
            (ApiMethod::Post, "/captive/api/test-key") => {
                handled_route_future!(
                    handle_captive_test_key(state, request, response_writer).await
                )
            }
            (ApiMethod::Post, "/captive/api/fixed-key-config") => {
                handled_route_future!(
                    handle_captive_fixed_key_config(state, request, response_writer).await
                )
            }
            (ApiMethod::Post, "/captive/api/wifi-config") => {
                handled_route_future!(
                    handle_captive_wifi_config_post(state, request, response_writer).await
                )
            }
            (ApiMethod::Get, "/captive/api/wifi-config") => {
                handled_route_future!(
                    handle_captive_wifi_config_get(state, request, response_writer).await
                )
            }
            (ApiMethod::Post, "/captive/api/device-name-config") => {
                handled_route_future!(
                    handle_captive_device_name_config_post(state, request, response_writer).await
                )
            }
            (ApiMethod::Get, "/captive/api/device-name-config") => {
                handled_route_future!(
                    handle_captive_device_name_config_get(state, request, response_writer).await
                )
            }
            (ApiMethod::Post, "/captive/api/reset-device") => {
                handled_route_future!(
                    handle_captive_reset_device(state, request, response_writer).await
                )
            }
            (ApiMethod::Get, "/config") => handled_route_future!({
                picoserve::response::File::with_content_type_and_headers(
                    "text/html",
                    self.web_app_html_gz,
                    &[("Content-Encoding", "gzip")],
                )
                .call_request_handler_service(state, (), request, response_writer)
                .await
            }),
            (ApiMethod::Get, "/pkg/device_wasm_bg.wasm") => handled_route_future!({
                picoserve::response::File::with_content_type_and_headers(
                    "application/wasm",
                    include_bytes_gz!("src/static/device_wasm_bg.wasm"),
                    &[("Content-Encoding", "gzip")],
                )
                .call_request_handler_service(state, (), request, response_writer)
                .await
            }),
            (ApiMethod::Get, "/pkg/device_wasm.js") => handled_route_future!({
                picoserve::response::File::with_content_type_and_headers(
                    "application/javascript; charset=utf-8",
                    include_bytes_gz!("src/static/device_wasm.js"),
                    &[("Content-Encoding", "gzip")],
                )
                .call_request_handler_service(state, (), request, response_writer)
                .await
            }),
            (ApiMethod::Post, "/api/wifi-config") => {
                handled_route_future!(
                    handle_framework_post(
                        state,
                        request,
                        response_writer,
                        handle_framework_wifi_config_post,
                    )
                    .await
                )
            }
            (ApiMethod::Get, "/api/wifi-config") => {
                handled_route_future!(
                    handle_framework_get(
                        request,
                        response_writer,
                        state,
                        handle_framework_wifi_config_get,
                    )
                    .await
                )
            }
            (ApiMethod::Post, "/api/device-name-config") => {
                handled_route_future!(
                    handle_framework_post(
                        state,
                        request,
                        response_writer,
                        handle_framework_device_name_config_post,
                    )
                    .await
                )
            }
            (ApiMethod::Get, "/api/device-name-config") => {
                handled_route_future!(
                    handle_framework_get(
                        request,
                        response_writer,
                        state,
                        handle_framework_device_name_config_get,
                    )
                    .await
                )
            }
            (ApiMethod::Post, "/api/reset-device") => {
                handled_route_future!(
                    handle_framework_post(
                        state,
                        request,
                        response_writer,
                        handle_framework_reset_device,
                    )
                    .await
                )
            }
            (ApiMethod::Post, "/api/display-config") => {
                handled_route_future!(
                    handle_framework_post(
                        state,
                        request,
                        response_writer,
                        handle_framework_display_config_post,
                    )
                    .await
                )
            }
            (ApiMethod::Get, "/api/display-config") => {
                handled_route_future!(
                    handle_framework_get(
                        request,
                        response_writer,
                        state,
                        handle_framework_display_config_get,
                    )
                    .await
                )
            }
            (ApiMethod::Post, "/api/test-key") => {
                handled_route_future!(
                    handle_framework_post(
                        state,
                        request,
                        response_writer,
                        handle_framework_test_key
                    )
                    .await
                )
            }
            (ApiMethod::Post, "/api/fixed-key-config") => {
                handled_route_future!(
                    handle_framework_post(
                        state,
                        request,
                        response_writer,
                        handle_framework_fixed_key_config,
                    )
                    .await
                )
            }
            (ApiMethod::Post, "/api/ota-request") => {
                handled_route_future!(
                    handle_framework_post(
                        state,
                        request,
                        response_writer,
                        handle_framework_ota_request,
                    )
                    .await
                )
            }
            (ApiMethod::Get, "/api/ota-config") => {
                handled_route_future!(
                    handle_framework_get(
                        request,
                        response_writer,
                        state,
                        handle_framework_ota_config_get,
                    )
                    .await
                )
            }
            _ => RouteAttempt::NotMatched {
                path,
                request,
                response_writer,
            },
        }
    }
}

impl<MoreState, NestedApp> PathRouterService<WebAppState<MoreState>>
    for FrameworkWebService<MoreState, NestedApp>
where
    NestedApp: NestedAppWithWebAppState<MoreState>,
{
    async fn call_path_router_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        state: &WebAppState<MoreState>,
        (): (),
        path: Path<'_>,
        request: Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        let method_log = request.parts.method().to_string();
        let path_log = path.encoded().to_string();
        debug!("Web-App request started: {method_log} {path_log}");

        let result = match self
            .nested_app
            .try_handle_route(state, path, request, response_writer)
            .await
        {
            RouteAttempt::Handled(result) => result,
            RouteAttempt::NotMatched {
                path,
                request,
                response_writer,
            } => match self
                .try_handle_framework_route(state, path, request, response_writer)
                .await
            {
                RouteAttempt::Handled(result) => result,
                RouteAttempt::NotMatched {
                    path,
                    request,
                    response_writer,
                } => {
                    box_route_future!(
                        self.nested_app
                            .handle_fallback(
                                state,
                                path,
                                request,
                                response_writer,
                                &self.default_fallback,
                            )
                            .await
                    )
                }
            },
        };

        debug!(
            "Web-App request completed: {method_log} {path_log} {}",
            if result.is_ok() { "ok" } else { "error" }
        );
        result
    }
}

impl<MoreState> picoserve::routing::PathRouterService<WebAppState<MoreState>> for CustomNotFound {
    async fn call_path_router_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        state: &WebAppState<MoreState>,
        _path_parameters: (),
        path: picoserve::request::Path<'_>,
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let redirect_target = {
            let framework = state.framework.0.borrow();
            if matches!(framework.wifi_ok, Some(true)) {
                "/"
            } else if self.web_server_captive
                && matches!(framework.web_config_mode, Some(WebConfigMode::AP))
            {
                "/captive"
            } else {
                "/config"
            }
        };

        debug!(
            "Redirecting request from '{}' to: '{}'",
            path, redirect_target
        );
        Redirect::to(redirect_target)
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}
