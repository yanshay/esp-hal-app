use core::cell::RefCell;
use core::net::SocketAddr;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::ToString;
use alloc::{ffi::CString, format};
use edge_http::io::client::Connection;
use edge_nal_embassy::{Tcp, TcpBuffers};
use embassy_net::IpAddress;
use embassy_time::Timer;
use embedded_io_async::Read;
use esp_hal_ota::Ota;
use esp_mbedtls::{Certificates, TlsVersion, X509};
use esp_storage::FlashStorage;
use semver::{Version, VersionReq};
use serde::Deserialize;

use super::framework::Framework;

enum Report<'a> {
    Status,
    Failure,
    Complete,
    Success,
    Version(&'a str, bool),
}

#[derive(Clone, Copy, PartialEq, Deserialize)]
pub enum OtaRequest {
    CheckVersion,
    Update,
}

pub trait OtaObserver {
    fn on_ota_start(&mut self);
    fn on_ota_status(&mut self, text: &str);
    fn on_ota_failed(&mut self, text: &str);
    fn on_ota_completed(&mut self, text: &str);
    fn on_ota_version_available(&mut self, version: &str, newer: bool);
}

struct FrameworkOtaObserver {
    framework: Rc<RefCell<Framework>>,
    update: bool,
}

impl OtaObserver for FrameworkOtaObserver {
    fn on_ota_start(&mut self) {
        if self.update {
            self.framework.borrow_mut().notify_ota_start();
        }
    }

    fn on_ota_status(&mut self, text: &str) {
        if self.update {
            self.framework.borrow_mut().notify_ota_status(text);
        }
    }

    fn on_ota_failed(&mut self, text: &str) {
        if self.update {
            self.framework.borrow_mut().notify_ota_failed(text);
        }
    }

    fn on_ota_completed(&mut self, text: &str) {
        if self.update {
            self.framework.borrow_mut().notify_ota_completed(text);
        }
    }

    fn on_ota_version_available(&mut self, version: &str, newer: bool) {
        self.framework
            .borrow_mut()
            .notify_ota_version_available(version, newer);
    }
}

#[allow(clippy::too_many_arguments)]
#[embassy_executor::task]
pub async fn ota_task(
    ota_domain: &'static str,
    ota_path: &'static str,
    ota_toml_filename: &'static str,
    cert: &'static str,
    ota_request: OtaRequest,
    framework: Rc<RefCell<Framework>>,
) {
    let mut framework_observer = FrameworkOtaObserver {
        framework: framework.clone(),
        update: matches!(ota_request, OtaRequest::Update),
    };

    let curr_ver = framework
        .borrow()
        .settings
        .app_cargo_pkg_version
        .to_string();
    run_ota(
        ota_domain,
        ota_path,
        ota_toml_filename,
        &curr_ver,
        cert,
        ota_request,
        framework,
        &mut framework_observer,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn run_ota(
    ota_domain: &'static str,
    ota_path: &'static str,
    ota_toml_filename: &'static str,
    cur_version: &str,
    cert: &'static str,
    ota_request: OtaRequest,
    framework: Rc<RefCell<Framework>>,
    observer: &mut dyn OtaObserver,
) {
    let stack = framework.borrow().stack;
    let tls = framework.borrow().tls;

    if ota_request == OtaRequest::Update {
        observer.on_ota_start();
    }

    let mut report = move |report: Report, text: &str| match report {
        Report::Status => {
            // if ota_request == OtaRequest::Update {
            observer.on_ota_status(text);
            // }
            info!("{text}");
        }
        Report::Failure => {
            // if ota_request == OtaRequest::Update {
            observer.on_ota_failed(text);
            // }
            warn!("{text}");
        }
        Report::Complete => {
            // if ota_request == OtaRequest::Update {
            observer.on_ota_completed(text);
            // }
            info!("{text}");
        }
        Report::Success => {
            // if ota_request == OtaRequest::Update {
            observer.on_ota_completed(text);
            // }
            info!("{text}");
        }
        Report::Version(version, newer) => {
            observer.on_ota_version_available(version, newer);
        }
    };
    report(Report::Status, "Resolving Dns");
    let Ok(ips) = stack
        .dns_query(ota_domain, embassy_net::dns::DnsQueryType::A)
        .await
    else {
        report(
            Report::Failure,
            "Failed to resolve Dns, Internet accessible?",
        );
        return;
    };

    info!("Resolved DNS for {ota_domain} {:?}", ips);

    if ips.is_empty() {
        report(
            Report::Status,
            "Failed to resolve Dns for {ota_domain}, Internet accessible?",
        );
        return;
    }

    let certificates = Certificates {
        ca_chain: X509::pem(cert.as_bytes()).ok(),
        ..Default::default()
    };

    let mut tcp_buffers_boxed = Box::new(TcpBuffers::<1, 1024, 16384>::new());
    let tcp_buffers = &mut *tcp_buffers_boxed;
    let tcp = Tcp::new(stack, tcp_buffers);

    let servername = CString::new(ota_domain).unwrap();
    let tls_connector = Box::new(esp_mbedtls::asynch::TlsConnector::new(
        tcp,
        &servername,
        TlsVersion::Tls1_2,
        certificates,
        tls,
    ));

    let IpAddress::Ipv4(addr) = ips[0] else {
        report(Report::Failure, "Unsupported reply from Dns");
        return;
    };

    let mut conn_buf_boxed = Box::new([0_u8; 4096]);
    let conn_buf = &mut *conn_buf_boxed;
    let mut data_buf_boxed = Box::new([0_u8; 4096]);
    let data_buf = &mut *data_buf_boxed;

    let mut conn: Box<Connection<_, 32>> = Box::new(Connection::new(
        &mut *conn_buf,
        &*tls_connector,
        SocketAddr::new(core::net::IpAddr::V4(addr), 443),
    ));

    // Get ota.toml

    let toml_filename = format!("{ota_path}{ota_toml_filename}");

    info!("Fetching OTA metadata from {toml_filename}");
    report(Report::Status, "Fetching firmware metadata");
    if let Err(err) = conn
        .initiate_request(
            true,
            edge_http::Method::Get,
            &toml_filename,
            &[("Host", ota_domain)],
        )
        .await
    {
        report(Report::Failure, "Failed to initiate request for metadata");
        error!("Error: {err:?}");
        return;
    }

    if let Err(err) = conn.initiate_response().await {
        report(Report::Failure, "Failed to fetch response for metadata");
        error!("Error: {err:?}");
        return;
    };

    let headers = match conn.headers() {
        Ok(headers) => headers,
        Err(err) => {
            report(Report::Failure, "Failed to read resopnse headers");
            info!("Error: {err}");
            return;
        }
    };

    let status_code = headers.code;
    if status_code != 200 {
        report(Report::Failure, "Failed to fetch firmware metadata");
        return;
    }

    // TODO - loop to read until buffer full or nothing to read
    let Ok(len) = conn.read(&mut *data_buf).await else {
        report(Report::Failure, "Failed to read response");
        return;
    };

    let toml = core::str::from_utf8(&data_buf[..len]).unwrap_or_default();
    info!("Firmware metadata:\n{}", toml);

    let mut filename = None;
    let mut crc32 = None;
    let mut version = None;
    let mut filesize = None;

    for line in toml.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "filename" => filename = Some(value.trim().trim_matches('"')),
                "crc32" => crc32 = Some(u32::from_str_radix(value.trim().trim_matches('"'), 16)),
                "filesize" => filesize = Some(value.trim().trim_matches('"').parse::<u32>()),
                "version" => version = Some(value.trim().trim_matches('"')),
                _ => (), // Ignore unknown keys
            }
        }
    }
    let (Some(filename), Some(Ok(crc32)), Some(version), Some(Ok(filesize))) =
        (filename, crc32, version, filesize)
    else {
        report(Report::Failure, "Something is wrong with firmware metadata");
        return;
    };

    let new_semver = match Version::parse(version) {
        Ok(v) => v,
        Err(_) => {
            report(
                Report::Failure,
                "Version number in firmware metadata is invalid",
            );
            return;
        }
    };

    let newer = {
        if let Ok(mut curr_req) =
            VersionReq::parse(cur_version)
        {
            curr_req.comparators[0].op = semver::Op::Greater;
            curr_req.matches(&new_semver)
        } else {
            false
        }
    };

    if !newer {
        report(
            Report::Complete,
            &format!(
                "Current firmware version {} is up to date",
                framework.borrow().settings.app_cargo_pkg_version
            ),
        );
        report(Report::Version(version, false), "Version is up to date");
        return;
    } else {
        report(Report::Version(version, true), "Version is behind");
    }

    if ota_request == OtaRequest::CheckVersion {
        return;
    }

    // Fetch the bin file

    report(Report::Status, "Downloading firmware");
    let bin_filename = format!("{}{}", ota_path, filename);
    if let Err(e) = conn
        .initiate_request(
            true,
            edge_http::Method::Get,
            &bin_filename,
            &[("Host", ota_domain)],
        )
        .await
    {
        report(
            Report::Failure,
            &format!("Failed to initiate request for firmware {e:?}"),
        );
        return;
    }

    if let Err(e) = conn.initiate_response().await {
        report(
            Report::Failure,
            &format!("Failed to fetch response for metadata {e:?}"),
        );
        return;
    }

    let status_code = conn.headers().unwrap().code;
    info!("Response code {}", status_code);
    if status_code != 200 {
        report(Report::Failure, "Failed downloading firmware");
        return;
    }

    // start OTA

    let mut ota = match Ota::new(FlashStorage::new()) {
        Ok(v) => v,
        Err(_) => {
            report(Report::Failure, "Error initializing flashing");
            return;
        }
    };
    ota.ota_begin(filesize, crc32).unwrap_or_else(|e| {
        report(Report::Failure, &format!("Failed to start OTA: {e:?}"));
    });

    debug!("Starting firmware download");
    let mut bytes_read = 0;
    let start_time = embassy_time::Instant::now();
    let mut reported_on_sec_since_start = 0;
    let mut x = 0;
    let mut sec_since_start;
    loop {
        let bytes_to_read = data_buf
            .len()
            .min((filesize - bytes_read).try_into().unwrap());

        if conn
            .read_exact(&mut data_buf[..bytes_to_read])
            .await
            .is_ok()
        {
            bytes_read += bytes_to_read as u32;

            if bytes_to_read == 0 {
                error!("Binary File smaller than expected");
                break;
            }

            let res = ota.ota_write_chunk(&data_buf[..bytes_to_read]);

            match res {
                Ok(true) => {
//                     let res = ota.ota_flush(false, true);
                    sec_since_start = start_time.elapsed().as_secs();
                    debug!(
                        "Finished: {x}: {sec_since_start} secs, {bytes_read} {bytes_read} {:.0}%",
                        100.0
                    );
                    info!(
                        "Download & Flash time: {}ms",
                        start_time.elapsed().as_millis()
                    );
                    if let Err(e) = res {
                        report(Report::Failure, &format!("Ota flush error: {e:?}"));
                        break;
                    }

                    for countdown in 0..5 {
                        report(
                            Report::Success,
                            &format!(
                                "Firmware version {} flashed successfully\nRestarting {} in {} seconds",
                                framework.borrow().settings.app_cargo_pkg_name,
                                new_semver,
                                5 - countdown
                            ),
                        );
                        Timer::after_millis(1000).await;
                    }
                    esp_hal::reset::software_reset();
                    break;
                }
                Err(e) => {
                    report(Report::Failure, &format!("Flashing error: {e:?}"));
                    break;
                }
                _ => {}
            }
            sec_since_start = start_time.elapsed().as_secs();
            if sec_since_start >= reported_on_sec_since_start {
                let progress_percent = ota.get_ota_progress() * 100.0;
                report(
                    Report::Status,
                    &format!(
                        "Downloading/Flashing {} version {}\n{sec_since_start} secs, {:.0}%",
                        framework.borrow().settings.app_cargo_pkg_name,
                        new_semver,
                        progress_percent
                    ),
                );
                reported_on_sec_since_start = sec_since_start + 1;
            }
            x += 1;
        }
    }
    conn.close().await.ok();
}
