use core::{
    cell::RefCell,
    net::{IpAddr, SocketAddr},
};

use alloc::{boxed::Box, rc::Rc};
use chrono::{DateTime, Utc};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Instant, Timer};
use smoltcp::wire::DnsQueryType;
use sntpc::{get_time, NtpContext, NtpTimestampGenerator};

use crate::prelude::Framework;

const NTP_SERVERS: [&str; 6] = [
    "pool.ntp.org",
    "time.aws.com",
    "time.windows.com",
    "time.apple.com",
    "cn.pool.ntp.org",
    "time.google.com",
];

#[derive(Copy, Clone)]
struct TimestampGen {
    instant: Instant,
}

impl TimestampGen {
    pub fn new() -> Self {
        Self {
            instant: Instant::from_micros(0),
        }
    }
}

impl NtpTimestampGenerator for TimestampGen {
    fn init(&mut self) {
        self.instant = Instant::now();
    }

    fn timestamp_sec(&self) -> u64 {
        self.instant.as_secs()
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        let subsec_micros = self.instant.as_micros() - self.instant.as_secs() * 1_000_000;
        subsec_micros as u32
    }
}

#[embassy_executor::task]
#[allow(clippy::too_many_arguments)]

pub async fn ntp_task(framework: Rc<RefCell<Framework>>) {
    info!("ntp_task started (not yet functional, need IP)");

    Framework::wait_for_wifi(&framework).await;

    let stack = framework.borrow().stack;

    let mut resolved = false;
    let mut ntp_address = None;
    term_info!("Requesting to get NTP Time");
    'global_loop: for ntp_server in NTP_SERVERS.iter().cycle() {
        for trial in 0..2 {
            let ntp_addrs = match stack.dns_query(ntp_server, DnsQueryType::A).await {
                Ok(v) => v,
                Err(err) => {
                    error!("Failed try {trial} to resolve NTP server {ntp_server} DNS : {err:?}, retrying");
                    Timer::after_secs(1).await;
                    continue;
                }
            };
            if ntp_addrs.is_empty() {
                error!("Resolved DNS using {ntp_server} but received empty result, retrying");
                Timer::after_secs(1).await;
                continue;
            } else {
                resolved = true;
                ntp_address = Some(ntp_addrs[0]);
                term_info!("Using NTP server {ntp_server} at address: {}", ntp_addrs[0]);
                break;
            }
        }
        if resolved {
            let addr: IpAddr = if let Some(ntp_address) = ntp_address {
                ntp_address.into()
            } else {
                error!("Failed to resolve any ntp server");
                return;
            };

            let timestamp_gen = TimestampGen::new();
            let context = NtpContext::new(timestamp_gen);

            // Create UDP socket

            let mut rx_meta = Box::new([PacketMetadata::EMPTY; 16]);
            let mut rx_buffer = Box::new([0; 512]);
            let mut tx_meta = Box::new([PacketMetadata::EMPTY; 16]);
            let mut tx_buffer = Box::new([0; 512]);

            let mut socket = UdpSocket::new(
                stack,
                &mut *rx_meta,
                &mut *rx_buffer,
                &mut *tx_meta,
                &mut *tx_buffer,
            );
            socket.bind(123).unwrap();
            let trials = 10;
            for trial in 0..trials {
                info!("Issuing NTP query to {addr}");
                match get_time(SocketAddr::from((addr, 123)), &socket, context).await {
                    Ok(time) => {
                        let query_time_micros_since_epoch =
                            time.sec() as u64 * 1_000_000 + time.roundtrip() / 2;
                        let query_time_micros_instant_now = Instant::now().as_micros();
                        let offset_micros =
                            query_time_micros_since_epoch - query_time_micros_instant_now;
                        let offset_duration_micros = Duration::from_micros(offset_micros);
                        set_time_offset(offset_duration_micros);

                        debug!(
                            "NTP Time: {time:?} -> {}",
                            DateTime::from_timestamp(time.sec() as i64, 0).unwrap()
                        );

                        term_info!("Received NTP Time : {}", DateTime::from_timestamp(time.sec() as i64, 0).unwrap());
                        // info!("Complete NTP information: {:?}", time);
                        // Timer::after_secs(10).await;
                        // info!(">>>> After 5 seconds time is {:?}", Instant::now().to_date_time());
                        break 'global_loop;
                    }
                    Err(err) => {
                        error!("NTP error: {err:?}");
                        if trial == trials-1 {
                            term_error!("Failed to receive NTP time, retrying another server");
                        }
                        Timer::after_secs(1).await; // and continue the loop
                    }
                }
            }
            // Note: Can't get NTP more than once with current implementation since relies on global once_cell
            // Need to change to something that can be modified many time
        }
    }
    info!("ntp_task Exited");
}

pub static mut TIME_OFFSET: once_cell::sync::OnceCell<Duration> = once_cell::sync::OnceCell::new();

pub fn set_time_offset(offset_duration_micros: Duration) {
    unsafe {
        #[allow(static_mut_refs)]
        TIME_OFFSET.set(offset_duration_micros).unwrap();
    }
}

pub trait InstantExt {
    fn to_date_time(&self) -> Option<DateTime<Utc>>;
}

impl InstantExt for Instant {
    fn to_date_time(&self) -> Option<DateTime<Utc>> {
        #[allow(static_mut_refs)]
        if let Some(offset_duration_micros) = unsafe { TIME_OFFSET.get() } {
            let real_world_instant_now = Instant::now() + *offset_duration_micros;
            let micros_since_epoch_now = real_world_instant_now.as_micros();
            DateTime::from_timestamp_micros(micros_since_epoch_now as i64)
        } else {
            None
        }
    }
}
