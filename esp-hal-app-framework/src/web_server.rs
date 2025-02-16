use core::cell::RefCell;

use alloc::{format, rc::Rc};
use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_net::Stack;
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    pubsub::WaitResult,
};
use embassy_time::Duration;
use embedded_io_async::Write;
use esp_mbedtls::TlsReference;
use picoserve::{ routing, serve_with_state, AppRouter, Config, LogDisplay, Router };

use embassy_net::tcp::TcpSocket;
use embassy_sync::mutex::Mutex;
use esp_mbedtls::{asynch::Session, Certificates, Mode, TlsError, TlsVersion, X509};

// use crate::WEB_SERVER_COMMANDS_LISTENERS;
use super::{framework::{Framework, WebServerCommands, WebServerSubscriber}, framework_web_app::{NestedAppWithWebAppStateBuilder, WebAppProps, WebAppState}};

#[derive(Clone)]
pub enum WebConfigCommand {
    Start(Stack<'static>),
    Stop,
}

pub struct Runner<NestedMainAppBuilder: NestedAppWithWebAppStateBuilder + 'static> {
    framework: Rc<RefCell<Framework>>,
    app_props: &'static AppRouter<WebAppProps<NestedMainAppBuilder>>,
    app_state: &'static WebAppState,
    config: &'static Config<Duration>,
    spawner: Spawner,
    web_server_commands: &'static WebServerCommands,
    tls: TlsReference<'static>,
}

impl<NestedMainAppBuilder: NestedAppWithWebAppStateBuilder> Runner<NestedMainAppBuilder> {
    pub fn new(framework: Rc<RefCell<Framework>>, app_props: &'static AppRouter<WebAppProps<NestedMainAppBuilder>>, app_state: &'static WebAppState, spawner: Spawner, web_server_commands: &'static WebServerCommands, tls: TlsReference<'static>,) -> Self {
        let config = crate::mk_static!(
            picoserve::Config<Duration>,
            picoserve::Config::new(picoserve::Timeouts {
                start_read_request: Some(Duration::from_secs(5)),
                read_request: Some(Duration::from_millis(5000)),
                write: Some(Duration::from_millis(5000)),
            })
            .keep_connection_alive()
        );

        let myself = Self {
            framework,
            app_props,
            app_state,
            config,
            spawner,
            web_server_commands,
            tls,
        };

        myself.start();

        myself
    }

    pub async fn run(&self, id:usize) {
        web_task(self.framework.clone(), id, self.app_props, self.config, self.web_server_commands.subscriber().unwrap(), self.tls, self.app_state).await;
    }

    fn start(&self) {
        debug!("runner::start called");
        // Need a standalone captive task if on https, or port that isn't 80 and if setting require captive in the first place
        #[allow(unused_assignments)]
        let mut need_standalone_captive = false;
        if self.framework.borrow().settings.web_server_https || (self.framework.borrow().settings.web_server_port != 80) {
            need_standalone_captive = true;
        }

        if !self.framework.borrow().settings.web_server_captive {
            need_standalone_captive = false;
        }

        if need_standalone_captive {
            self.spawner
                .spawn(standalone_captive_redirect_listen_and_serve_task(
                    self.web_server_commands.subscriber().unwrap(),
                    self.framework.borrow().settings.web_app_domain,
                ))
                .unwrap();
        }
    }
}

async fn web_task<NestedMainAppBuilder: NestedAppWithWebAppStateBuilder>(
    framework: Rc<RefCell<Framework>>, // TODO: Can move tasks into runner and have access to it's framework member
    task_id: usize,
    // DHCP
    app: &'static AppRouter<WebAppProps<NestedMainAppBuilder>>,
    config: &'static picoserve::Config<Duration>,
    mut web_server_commands: WebServerSubscriber,
    tls: TlsReference<'static>,
    state: &'static WebAppState,
) {
    let mut command = None;

    debug!("//// web_task {task_id} started");

    loop {
        if command.is_none() {
            command = Some(web_server_commands.next_message().await);
        }
        match command {
            Some(embassy_sync::pubsub::WaitResult::Lagged(_)) => command = None,
            Some(embassy_sync::pubsub::WaitResult::Message(WebConfigCommand::Stop)) => {
                command = None;
            }
            Some(embassy_sync::pubsub::WaitResult::Message(WebConfigCommand::Start(stack))) => {
                let res = select(
                    my_listen_and_serve(framework.clone(), task_id, app, config, stack, tls, state),
                    web_server_commands.next_message_pure(),
                )
                .await;
                command = match res {
                    embassy_futures::select::Either::First(_) => None,
                    embassy_futures::select::Either::Second(command) => Some(WaitResult::Message(command)),
                };
            }
            None => (),
        }
    }
}

#[embassy_executor::task]
async fn standalone_captive_redirect_listen_and_serve_task(
    mut web_server_commands: WebServerSubscriber,
    web_app_domain: &'static str,
) {
    debug!("/// Captive started");
    let mut command = None;

    loop {
        if command.is_none() {
            command = Some(web_server_commands.next_message().await);
        }
        match command {
            Some(embassy_sync::pubsub::WaitResult::Lagged(_)) => command = None,
            Some(embassy_sync::pubsub::WaitResult::Message(WebConfigCommand::Stop)) => {
                command = None;
            }
            Some(embassy_sync::pubsub::WaitResult::Message(WebConfigCommand::Start(stack))) => {
                let res = select(
                    standalone_captive_redirect_listen_and_serve(stack, web_app_domain),
                    web_server_commands.next_message_pure(),
                )
                .await;
                command = match res {
                    embassy_futures::select::Either::First(_) => None,
                    embassy_futures::select::Either::Second(command) => Some(WaitResult::Message(command)),
                };
            }
            None => (),
        }
    }
}

async fn standalone_captive_redirect_listen_and_serve(stack: embassy_net::Stack<'static>, web_app_domain: &'static str) {
    let port = 80;
    let mut tcp_rx_buffer = [0; 512];
    let mut tcp_tx_buffer = [0; 512];
    let mut socket = embassy_net::tcp::TcpSocket::new(stack, &mut tcp_rx_buffer, &mut tcp_tx_buffer);
    loop {
        info!("Captive: listening on TCP:{}...", port);

        if let Err(err) = socket.accept(port).await {
            warn!("Captive: accept error: {:?}", err);
            continue;
        }

        let _remote_endpoint = socket.remote_endpoint();

        let redirect_response =
            format!("HTTP/1.1 302 Found\r\nLocation: https://{web_app_domain}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
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

async fn my_listen_and_serve<P: routing::PathRouter<WebAppState>>(
    framework: Rc<RefCell<Framework>>,
    task_id: impl LogDisplay,
    app: &Router<P, WebAppState>,
    config: &Config<embassy_time::Duration>,
    stack: embassy_net::Stack<'static>,
    tls: TlsReference<'static>,
    state: &WebAppState,
) -> ! {
    let port = framework.borrow().settings.web_server_port;
    let mut tcp_rx_buffer = [0u8; 1024];
    let mut tcp_tx_buffer = [0u8; 1024];
    let mut http_buffer = [0u8; 1024];
    loop {
        let mut socket = embassy_net::tcp::TcpSocket::new(stack, &mut tcp_rx_buffer, &mut tcp_tx_buffer);

        info!("{}: Listening on TCP:{}...", task_id, port);

        if let Err(err) = socket.accept(port).await {
            warn!("{}: accept error: {:?}", task_id, err);
            continue;
        }

        let remote_endpoint = socket.remote_endpoint();

        info!("{}: Received connection from {:?}", task_id, remote_endpoint);
        let certificate = framework.borrow().settings.web_server_tls_certificate;
        let private_key = framework.borrow().settings.web_server_tls_private_key;

        if framework.borrow().settings.web_server_https {
            let session = esp_mbedtls::asynch::Session::new(
                socket,
                Mode::Server,
                TlsVersion::Tls1_2,
                Certificates {
                    // Use self-signed certificates
                    certificate: X509::pem(certificate.as_bytes()).ok(),
                    private_key: X509::pem(private_key.as_bytes()).ok(),
                    ..Default::default()
                },
                tls,
            )
            .unwrap();

            let wrapper = SessionWrapper::new(session);

            match serve_with_state(app, config, &mut http_buffer, wrapper, state).await {
                Ok(handled_requests_count) => {
                    info!("{} requests handled from {:?}", handled_requests_count, remote_endpoint);
                }
                Err(err) => error!("{:?}", &err),
            }
        } else {
            match serve_with_state(app, config, &mut http_buffer, socket, state).await {
                Ok(handled_requests_count) => {
                    info!("{} requests handled from {:?}", handled_requests_count, remote_endpoint);
                }
                Err(err) => error!("{:?}", &err),
            }
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////////////////////////
// esp-mbedtls implementation for use with picoserve /////////////////////////////////////////////////////////
//////////////////////////////////////////////////////////////////////////////////////////////////////////////

pub struct SessionWrapper<'a> {
    session: Rc<Mutex<NoopRawMutex, Session<'a, TcpSocket<'a>>>>,
}

impl<'a, 's> SessionWrapper<'s>
where
    's: 'a,
{
    pub fn new(session: Session<'s, TcpSocket<'s>>) -> Self {
        Self {
            session: Rc::new(Mutex::new(session)),
        }
    }
    pub async fn close(&mut self) -> Result<(), TlsError> {
        let mut session = self.session.lock().await;
        session.close().await
    }
}

// Reader

pub struct SessionReader<'a> {
    session: Rc<Mutex<NoopRawMutex, Session<'a, TcpSocket<'a>>>>,
}

impl<'a> embedded_io_async::ErrorType for SessionReader<'a> {
    type Error = TlsError;
}

impl<'a> embedded_io_async::Read for SessionReader<'a> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut session = self.session.lock().await;
        let res = session.read(buf).await;
        res
    }
}

pub struct SessionWriter<'a> {
    session: Rc<Mutex<NoopRawMutex, Session<'a, TcpSocket<'a>>>>,
}

impl<'a> embedded_io_async::ErrorType for SessionWriter<'a> {
    type Error = TlsError;
}

impl<'a> embedded_io_async::Write for SessionWriter<'a> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut session = self.session.lock().await;
        let res = session.write(buf).await;
        res
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let mut session = self.session.lock().await;
        let res = session.flush().await;
        res
    }
}

// Implement picoserve Socket on SessionWrapper
impl<'s> picoserve::io::Socket for SessionWrapper<'s> {
    type Error = TlsError;
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

    async fn shutdown<Timer: picoserve::Timer>(
        mut self,
        _timeouts: &picoserve::Timeouts<Timer::Duration>,
        _timer: &mut Timer,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        self.close().await.map_err(|e| picoserve::Error::Write(e))
    }
}
