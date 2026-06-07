use core::{cell::RefCell, ffi::CStr};

use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use embassy_futures::select::select;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, pubsub::WaitResult};
use embassy_time::Duration;
use embedded_io_async::Write;
use esp_mbedtls::TlsReference;
use picoserve::{routing, AppRouter, AppWithStateBuilder, Config, LogDisplay, Router};

use embassy_net::tcp::TcpSocket;
use embassy_sync::mutex::Mutex;
use esp_mbedtls::{
    Certificate, Credentials, PrivateKey, ServerSessionConfig, Session, SessionConfig,
    SessionError, X509,
};

use super::{
    framework::{Framework, WebServerCommands, WebServerSubscriber},
    framework_web_app::{
        NestedAppWithWebAppStateBuilder, WebAppBuilder, WebAppState, LOG_WEB_MEASUREMENTS,
    },
};

//////////////////////////////////////////////////////////////////////////////////////////////////////////////
// Specific Web Application Runner for the Config App which is part of the Framework
//////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub struct WebAppRunner<
    MoreState: 'static,
    NestedMainAppBuilder: NestedAppWithWebAppStateBuilder<MoreState> + 'static,
> {
    framework: Rc<RefCell<Framework>>,
    generic_runner:
        GenericRunner<WebAppBuilder<MoreState, NestedMainAppBuilder>, WebAppState<MoreState>>,
}

impl<MoreState, NestedMainAppBuilder: NestedAppWithWebAppStateBuilder<MoreState>>
    WebAppRunner<MoreState, NestedMainAppBuilder>
{
    pub fn new(
        framework: Rc<RefCell<Framework>>,
        app_router: &'static AppRouter<WebAppBuilder<MoreState, NestedMainAppBuilder>>,
        app_state: &'static WebAppState<MoreState>,
        config: Config,
        buffer_sizes: WebServerBufferSizes,
    ) -> Self {
        let web_server_config = WebServerConfig {
            web_app_name: "Web-App",
            port: framework.borrow().settings.web_server_port,
            tls: framework.borrow().settings.web_server_https,
            tls_certificate: framework.borrow().settings.web_server_tls_certificate,
            tls_private_key: framework.borrow().settings.web_server_tls_private_key,
            buffer_sizes,
        };
        let generic_runner = GenericRunner::<
            WebAppBuilder<MoreState, NestedMainAppBuilder>,
            WebAppState<MoreState>,
        >::new(
            framework.clone(),
            web_server_config,
            app_router,
            app_state,
            framework.borrow().web_server_commands,
            config.clone(),
        );

        let myself = Self {
            framework: framework.clone(),
            generic_runner,
        };

        myself.start_captive_if_needed(); // TODO: why is it here and not in run()?

        myself
    }

    pub async fn run(&self, id: usize) {
        self.generic_runner.run(id).await;
    }

    fn start_captive_if_needed(&self) {
        debug!("runner::start called");
        // Need a standalone captive task if on https, or port that isn't 80 and if setting require captive in the first place
        #[allow(unused_assignments)]
        let mut need_standalone_captive = false;
        if self.framework.borrow().settings.web_server_https
            || (self.framework.borrow().settings.web_server_port != 80)
        {
            need_standalone_captive = true;
        }

        if !self.framework.borrow().settings.web_server_captive {
            need_standalone_captive = false;
        }

        let spawner = self.framework.borrow().spawner;
        let web_server_commands = self.framework.borrow().web_server_commands;
        let web_app_domain = self.framework.borrow().settings.web_app_domain;

        if need_standalone_captive {
            spawner
                .spawn(standalone_captive_redirect_listen_and_serve_task(
                    web_server_commands.subscriber().unwrap(),
                    web_app_domain.to_string(),
                ))
                .unwrap();
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////
// Generic Web Application Runner - To be used for generic web applications (on unconflicting ports with Web Config)
//////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub struct GenericRunner<GenericAppProps, GenericAppState>
where
    // GenericAppBuilder : AppWithStateBuilder + 'static,
    GenericAppProps: AppWithStateBuilder + 'static,
    GenericAppState: 'static,
{
    web_server_config: WebServerConfig,
    app_router: &'static AppRouter<GenericAppProps>,
    app_state: &'static GenericAppState,
    config: Config,
    web_server_commands: &'static WebServerCommands,
    tls: TlsReference<'static>,
    tls_credentials: Option<Credentials<'static>>,
    http_buffer_pool: HttpBufferPool,
}

impl<GenericAppProps, GenericAppState> GenericRunner<GenericAppProps, GenericAppState>
where
    GenericAppProps: AppWithStateBuilder<State = GenericAppState> + 'static,
    GenericAppState: 'static,
{
    pub fn new(
        framework: Rc<RefCell<Framework>>,
        web_server_config: WebServerConfig,
        app_router: &'static AppRouter<GenericAppProps>,
        app_state: &'static GenericAppState,
        web_server_commands: &'static WebServerCommands,
        config: Config,
    ) -> Self {
        let tls_credentials = if web_server_config.tls {
            let certificate =
                CStr::from_bytes_with_nul(web_server_config.tls_certificate.as_bytes()).unwrap();
            let private_key =
                CStr::from_bytes_with_nul(web_server_config.tls_private_key.as_bytes()).unwrap();

            Some(Credentials {
                certificate: Certificate::new(X509::PEM(certificate)).unwrap(),
                private_key: PrivateKey::new(X509::PEM(private_key), None).unwrap(),
            })
        } else {
            None
        };

        let http_buffer_pool = HttpBufferPool::new(
            web_server_config.buffer_sizes.http,
            web_server_config.buffer_sizes.http_retained,
        );

        let myself = Self {
            web_server_config,
            app_router,
            app_state,
            config,
            web_server_commands,
            tls: framework.borrow().tls,
            tls_credentials,
            http_buffer_pool,
        };

        myself
    }

    pub async fn run(&self, id: usize) {
        web_task::<GenericAppProps, GenericAppState>(
            self.web_server_config.clone(),
            id,
            self.app_router,
            &self.config,
            self.web_server_commands.subscriber().unwrap(),
            self.tls,
            self.tls_credentials.as_ref(),
            self.app_state,
            self.http_buffer_pool.clone(),
        )
        .await;
    }
}

#[derive(Clone)]
struct HttpBufferPool {
    buffers: Rc<RefCell<Vec<Box<[u8]>>>>,
    buffer_size: usize,
}

impl HttpBufferPool {
    fn new(buffer_size: usize, retained_buffers: usize) -> Self {
        let mut buffers = Vec::new();
        for _ in 0..retained_buffers {
            buffers.push(vec![0u8; buffer_size].into_boxed_slice());
        }

        Self {
            buffers: Rc::new(RefCell::new(buffers)),
            buffer_size,
        }
    }

    fn take(&self) -> HttpBufferLease {
        if let Some(buffer) = self.buffers.borrow_mut().pop() {
            HttpBufferLease::Pooled {
                pool: self.buffers.clone(),
                buffer: Some(buffer),
            }
        } else {
            HttpBufferLease::Overflow {
                buffer: vec![0u8; self.buffer_size].into_boxed_slice(),
            }
        }
    }
}

enum HttpBufferLease {
    Pooled {
        pool: Rc<RefCell<Vec<Box<[u8]>>>>,
        buffer: Option<Box<[u8]>>,
    },
    Overflow {
        buffer: Box<[u8]>,
    },
}

impl HttpBufferLease {
    fn as_mut_slice(&mut self) -> &mut [u8] {
        match self {
            Self::Pooled { buffer, .. } => buffer.as_mut().unwrap().as_mut(),
            Self::Overflow { buffer } => buffer.as_mut(),
        }
    }
}

impl Drop for HttpBufferLease {
    fn drop(&mut self) {
        if let Self::Pooled { pool, buffer } = self {
            if let Some(buffer) = buffer.take() {
                pool.borrow_mut().push(buffer);
            }
        }
    }
}

#[derive(Clone)]
pub enum WebServerCommand {
    Start(Stack<'static>),
    Stop,
}

#[derive(Clone, Debug)]
pub struct WebServerConfig {
    pub web_app_name: &'static str,
    pub port: u16,
    pub tls: bool,
    pub tls_certificate: &'static str,
    pub tls_private_key: &'static str,
    pub buffer_sizes: WebServerBufferSizes,
}

#[derive(Clone, Copy, Debug)]
pub struct WebServerBufferSizes {
    pub tcp_rx: usize,
    pub tcp_tx: usize,
    pub http: usize,
    pub http_retained: usize,
}

impl Default for WebServerBufferSizes {
    fn default() -> Self {
        Self {
            tcp_rx: 2048,
            tcp_tx: 2048,
            http: 16 * 1024,
            http_retained: 1,
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////
// Actual functions implementing all web server aspects
//////////////////////////////////////////////////////////////////////////////////////////////////////////////
#[allow(clippy::too_many_arguments)]
async fn web_task<GenericAppProps, GenericAppState>(
    web_server_config: WebServerConfig,
    task_id: usize,
    // DHCP
    app: &'static AppRouter<GenericAppProps>,
    config: &picoserve::Config,
    mut web_server_commands: WebServerSubscriber,
    tls: TlsReference<'static>,
    tls_credentials: Option<&Credentials<'static>>,
    state: &'static GenericAppState,
    http_buffer_pool: HttpBufferPool,
) where
    GenericAppProps: AppWithStateBuilder<State = GenericAppState> + 'static,
    GenericAppState: 'static,
{
    debug!(
        "[{task_id}] {} web task started",
        web_server_config.web_app_name
    );
    let mut command = None;

    loop {
        if command.is_none() {
            command = Some(web_server_commands.next_message().await);
        }
        match command {
            Some(embassy_sync::pubsub::WaitResult::Lagged(_)) => command = None,
            Some(embassy_sync::pubsub::WaitResult::Message(WebServerCommand::Stop)) => {
                command = None;
            }
            Some(embassy_sync::pubsub::WaitResult::Message(WebServerCommand::Start(stack))) => {
                let res = select(
                    my_listen_and_serve(
                        web_server_config.clone(),
                        task_id,
                        app,
                        config,
                        stack,
                        tls,
                        tls_credentials,
                        state,
                        http_buffer_pool.clone(),
                    ),
                    web_server_commands.next_message_pure(),
                )
                .await;
                command = match res {
                    embassy_futures::select::Either::First(_) => None,
                    embassy_futures::select::Either::Second(command) => {
                        Some(WaitResult::Message(command))
                    }
                };
            }
            None => (),
        }
    }
}

#[embassy_executor::task]
async fn standalone_captive_redirect_listen_and_serve_task(
    mut web_server_commands: WebServerSubscriber,
    web_app_domain: String,
) {
    debug!("/// Captive started");
    let mut command = None;

    loop {
        if command.is_none() {
            command = Some(web_server_commands.next_message().await);
        }
        match command {
            Some(embassy_sync::pubsub::WaitResult::Lagged(_)) => command = None,
            Some(embassy_sync::pubsub::WaitResult::Message(WebServerCommand::Stop)) => {
                command = None;
            }
            Some(embassy_sync::pubsub::WaitResult::Message(WebServerCommand::Start(stack))) => {
                let res = select(
                    standalone_captive_redirect_listen_and_serve(stack, web_app_domain.clone()),
                    web_server_commands.next_message_pure(),
                )
                .await;
                command = match res {
                    embassy_futures::select::Either::First(_) => None,
                    embassy_futures::select::Either::Second(command) => {
                        Some(WaitResult::Message(command))
                    }
                };
            }
            None => (),
        }
    }
}

async fn standalone_captive_redirect_listen_and_serve(
    stack: embassy_net::Stack<'static>,
    web_app_domain: String,
) {
    let port = 80;
    let mut tcp_rx_buffer = Box::new([0; 512]);
    let mut tcp_tx_buffer = Box::new([0; 512]);
    let mut socket =
        embassy_net::tcp::TcpSocket::new(stack, &mut *tcp_rx_buffer, &mut *tcp_tx_buffer);

    loop {
        debug!("Captive: listening on TCP:{}...", port);

        if let Err(err) = socket.accept(port).await {
            warn!("Captive: accept error: {:?}", err);
            continue;
        }

        let _remote_endpoint = socket.remote_endpoint();

        let redirect_response = format!(
            "HTTP/1.1 302 Found\r\nLocation: https://{web_app_domain}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let r = socket.write_all(redirect_response.as_bytes()).await;
        if let Err(e) = r {
            error!("Captive write error: {:?}", e);
            socket.close();
            socket.abort();
            continue;
        }

        let r = socket.flush().await;
        if let Err(e) = r {
            error!("Captive flush error: {:?}", e);
            socket.close();
            socket.abort();
            continue;
        }

        socket.close();
        socket.abort();
    }
}

#[allow(clippy::too_many_arguments)]
async fn my_listen_and_serve<P: routing::PathRouter<GenericAppState>, GenericAppState>(
    web_server_config: WebServerConfig,
    task_id: impl LogDisplay,
    app: &Router<P, GenericAppState>,
    config: &Config,
    stack: embassy_net::Stack<'static>,
    tls: TlsReference<'static>,
    tls_credentials: Option<&Credentials<'static>>,
    state: &GenericAppState,
    http_buffer_pool: HttpBufferPool,
) -> ! {
    let port = web_server_config.port;
    let buffer_sizes = web_server_config.buffer_sizes;
    let mut tcp_rx_buffer = vec![0u8; buffer_sizes.tcp_rx].into_boxed_slice();
    let mut tcp_tx_buffer = vec![0u8; buffer_sizes.tcp_tx].into_boxed_slice();

    loop {
        let mut socket =
            embassy_net::tcp::TcpSocket::new(stack, &mut *tcp_rx_buffer, &mut *tcp_tx_buffer);

        debug!(
            "[{task_id}] {}: listening TCP:{port}",
            web_server_config.web_app_name
        );

        if let Err(err) = socket.accept(port).await {
            warn!("[{task_id}]: accept error: {:?}", err);
            continue;
        }

        let mut http_buffer = http_buffer_pool.take();

        if web_server_config.tls {
            let serve = Box::pin(serve_tls_connection(
                web_server_config.web_app_name,
                &task_id,
                app,
                config,
                http_buffer.as_mut_slice(),
                socket,
                tls,
                tls_credentials.unwrap(),
                state,
            ));
            if LOG_WEB_MEASUREMENTS {
                info!(
                    "[{task_id}] {} HTTPS boxed serve future state={} bytes",
                    web_server_config.web_app_name,
                    core::mem::size_of_val(serve.as_ref().get_ref())
                );
            }
            serve.await;
        } else {
            let serve = Box::pin(serve_http_connection(
                web_server_config.web_app_name,
                &task_id,
                app,
                config,
                http_buffer.as_mut_slice(),
                socket,
                state,
            ));
            if LOG_WEB_MEASUREMENTS {
                info!(
                    "[{task_id}] {} HTTP boxed serve future state={} bytes",
                    web_server_config.web_app_name,
                    core::mem::size_of_val(serve.as_ref().get_ref())
                );
            }
            serve.await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn serve_http_connection<P: routing::PathRouter<GenericAppState>, GenericAppState>(
    web_app_name: &'static str,
    task_id: impl LogDisplay,
    app: &Router<P, GenericAppState>,
    config: &Config,
    http_buffer: &mut [u8],
    mut socket: TcpSocket<'_>,
    state: &GenericAppState,
) {
    socket.set_keep_alive(Some(Duration::from_secs(30)));
    socket.set_timeout(Some(Duration::from_secs(45)));

    let remote_endpoint = format_remote_endpoint(&socket);
    debug!("[{task_id}] {web_app_name}: accepted {remote_endpoint}");

    let app_with_state = app.shared().with_state(state);
    match picoserve::Server::new(&app_with_state, config, http_buffer)
        .serve(socket)
        .await
    {
        Ok(_) => {}
        Err(err) => match err {
            picoserve::Error::ReadTimeout(_) => (),
            _ => {
                error!("[{task_id}] Error handling request : {:?}", &err);
            }
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn serve_tls_connection<P: routing::PathRouter<GenericAppState>, GenericAppState>(
    web_app_name: &'static str,
    task_id: impl LogDisplay,
    app: &Router<P, GenericAppState>,
    config: &Config,
    http_buffer: &mut [u8],
    mut socket: TcpSocket<'_>,
    tls: TlsReference<'static>,
    tls_credentials: &Credentials<'static>,
    state: &GenericAppState,
) {
    socket.set_keep_alive(Some(Duration::from_secs(30)));
    socket.set_timeout(Some(Duration::from_secs(45)));

    let remote_endpoint = format_remote_endpoint(&socket);
    debug!("[{task_id}] {web_app_name}: accepted {remote_endpoint}");

    let tls_config = ServerSessionConfig::new(tls_credentials.clone());
    let session = Session::new(tls, socket, &SessionConfig::Server(tls_config)).unwrap();
    let wrapper = SessionWrapper::new(session);
    let app_with_state = app.shared().with_state(state);

    match picoserve::Server::new(&app_with_state, config, http_buffer)
        .serve(wrapper)
        .await
    {
        Ok(_) => {}
        Err(err) => error!("[{task_id}] Error handling request: {:?}", &err),
    }
}

fn format_remote_endpoint(socket: &TcpSocket<'_>) -> String {
    match socket.remote_endpoint() {
        Some(endpoint) => match endpoint.addr {
            embassy_net::IpAddress::Ipv4(addr) => {
                let octets = addr.octets();
                format!(
                    "{}.{}.{}.{}:{}",
                    octets[0], octets[1], octets[2], octets[3], endpoint.port
                )
            }
            embassy_net::IpAddress::Ipv6(addr) => format!("{:?}:{}", addr, endpoint.port),
        },
        None => "unknown".to_string(),
    }
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////
// esp-mbedtls implementation for use with picoserve /////////////////////////////////////////////////////////
//////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub struct SessionWrapper<'a> {
    session: Rc<Mutex<NoopRawMutex, Session<'a, TcpSocket<'a>>>>,
}

#[derive(Debug)]
pub enum TlsSocketError {
    Session(SessionError),
    Tcp(embassy_net::tcp::Error),
}

impl core::fmt::Display for TlsSocketError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Session(error) => error.fmt(f),
            Self::Tcp(error) => write!(f, "TCP error: {:?}", error),
        }
    }
}

impl core::error::Error for TlsSocketError {}

impl embedded_io::Error for TlsSocketError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

impl<'s> SessionWrapper<'s> {
    pub fn new(session: Session<'s, TcpSocket<'s>>) -> Self {
        Self {
            session: Rc::new(Mutex::new(session)),
        }
    }
    // Must be called under a timeout: TLS close-notify and TCP flush can wait on the peer.
    async fn graceful_close(&mut self) -> Result<(), TlsSocketError> {
        let mut session = self.session.lock().await;
        session.close().await.map_err(TlsSocketError::Session)?;
        session.stream().close();
        session.stream().flush().await.map_err(TlsSocketError::Tcp)
    }

    async fn abort_tcp<Timer: picoserve::Timer<picoserve::EmbassyRuntime>>(
        &mut self,
        timeouts: &picoserve::Timeouts,
        timer: &mut Timer,
    ) -> Result<(), picoserve::Error<TlsSocketError>> {
        debug!("TLS TCP abort started");
        let mut session = self.session.lock().await;
        session.stream().abort();
        timer
            .run_with_timeout(timeouts.write, session.stream().flush())
            .await
            .map_err(picoserve::Error::WriteTimeout)?
            .map_err(|error| picoserve::Error::Write(TlsSocketError::Tcp(error)))?;
        debug!("TLS TCP abort completed");
        Ok(())
    }
}

// Reader

pub struct SessionReader<'a> {
    session: Rc<Mutex<NoopRawMutex, Session<'a, TcpSocket<'a>>>>,
}

impl embedded_io_async::ErrorType for SessionReader<'_> {
    type Error = TlsSocketError;
}

impl embedded_io_async::Read for SessionReader<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut session = self.session.lock().await;
        session.read(buf).await.map_err(TlsSocketError::Session)
    }
}

pub struct SessionWriter<'a> {
    session: Rc<Mutex<NoopRawMutex, Session<'a, TcpSocket<'a>>>>,
}

impl embedded_io_async::ErrorType for SessionWriter<'_> {
    type Error = TlsSocketError;
}

impl embedded_io_async::Write for SessionWriter<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut session = self.session.lock().await;
        session.write(buf).await.map_err(TlsSocketError::Session)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let mut session = self.session.lock().await;
        session.flush().await.map_err(TlsSocketError::Session)
    }
}

// Implement picoserve Socket on SessionWrapper
impl<'s> picoserve::io::Socket<picoserve::EmbassyRuntime> for SessionWrapper<'s> {
    type Error = TlsSocketError;
    type ReadHalf<'a>
        = SessionReader<'s>
    where
        's: 'a;
    type WriteHalf<'a>
        = SessionWriter<'s>
    where
        's: 'a;

    fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
        (
            SessionReader {
                session: self.session.clone(),
            },
            SessionWriter {
                session: self.session.clone(),
            },
        )
    }

    async fn abort<Timer: picoserve::Timer<picoserve::EmbassyRuntime>>(
        mut self,
        timeouts: &picoserve::Timeouts,
        timer: &mut Timer,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        self.abort_tcp(timeouts, timer).await
    }

    async fn shutdown<Timer: picoserve::Timer<picoserve::EmbassyRuntime>>(
        mut self,
        timeouts: &picoserve::Timeouts,
        timer: &mut Timer,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        match timer
            .run_with_timeout(timeouts.write, self.graceful_close())
            .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                warn!("TLS shutdown failed, aborting TCP: {:?}", error);
                self.abort_tcp(timeouts, timer).await
            }
            Err(error) => {
                warn!("TLS shutdown timed out, aborting TCP");
                self.abort_tcp(timeouts, timer).await?;
                Err(picoserve::Error::WriteTimeout(error))
            }
        }
    }
}
