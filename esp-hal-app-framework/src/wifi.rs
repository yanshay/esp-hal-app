use core::{
    cell::RefCell,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr as _,
};

use alloc::vec;
use alloc::{format, rc::Rc, vec::Vec};
use edge_dhcp::io::{self, DEFAULT_SERVER_PORT};
use edge_nal::UdpBind;
use embassy_net::{Runner, Stack};
use embassy_time::{with_timeout, Duration, Timer};
use embedded_io_async::{Read as _, Write as _};
use esp_wifi::wifi::{
    AccessPointConfiguration, AccessPointInfo, Configuration, WifiApDevice, WifiDevice,
    WifiStaDevice,
};

// use deku::DekuContainerRead as _;

use crate::utils::SpawnerHeapExt;

use super::{
    framework::{Framework, WebConfigMode},
    improv_wifi::*,
};

#[embassy_executor::task]
#[allow(clippy::too_many_arguments)]
pub async fn connection_task(
    controller: esp_wifi::wifi::WifiController<'static>,
    sta_stack: Stack<'static>,
    ap_stack: Stack<'static>,
    #[cfg(feature = "improv-jtag-serial")] rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<
        'static,
        esp_hal::Async,
    >,
    #[cfg(feature = "improv-jtag-serial")] tx: esp_hal::usb_serial_jtag::UsbSerialJtagTx<
        'static,
        esp_hal::Async,
    >,
    #[cfg(feature = "improv-uart")] mut rx: esp_hal::uart::UartRx<'static, esp_hal::Async>,
    #[cfg(feature = "improv-uart")] mut tx: esp_hal::uart::UartTx<'static, esp_hal::Async>,
    framework: Rc<RefCell<Framework>>,
) {
    connection_task_inner(controller, sta_stack, ap_stack, rx, tx, framework).await
}

#[allow(clippy::too_many_arguments)]
pub async fn connection_task_inner(
    mut controller: esp_wifi::wifi::WifiController<'static>,
    sta_stack: Stack<'static>,
    ap_stack: Stack<'static>,
    #[cfg(feature = "improv-jtag-serial")] mut rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<
        'static,
        esp_hal::Async,
    >,
    #[cfg(feature = "improv-jtag-serial")] mut tx: esp_hal::usb_serial_jtag::UsbSerialJtagTx<
        'static,
        esp_hal::Async,
    >,
    #[cfg(feature = "improv-uart")] mut rx: esp_hal::uart::UartRx<'static, esp_hal::Async>,
    #[cfg(feature = "improv-uart")] mut tx: esp_hal::uart::UartTx<'static, esp_hal::Async>,
    framework: Rc<RefCell<Framework>>,
) {
    let ap_addr = framework.borrow().settings.ap_addr;
    let app_cargo_pkg_name = framework.borrow().settings.app_cargo_pkg_name;
    let app_cargo_pkg_version = framework.borrow().settings.app_cargo_pkg_version;
    let prefix = if framework.borrow().settings.web_server_https {
        "https"
    } else {
        "http"
    };

    let spawner = embassy_executor::Spawner::for_current_executor().await;

    let mut send_packet = async |packet: ImprovWifiPacket, flush: bool| {
        let data = packet.to_bytes().unwrap();
        tx.write(&data).await.unwrap();
        if flush {
            #[cfg(feature = "improv-jtag-serial")]
            tx.flush().await.unwrap();
            #[cfg(feature = "improv-uart")]
            tx.flush_async().await.unwrap();
        }
        // embedded_io_async usage if needed:
        // embedded_io_async::Write::write(&mut tx, &data).await.unwrap();
        // embedded_io_async::Write::flush(&mut tx).await.unwrap();
    };

    trace!("Connection task started");
    //  TODO: improve on this flow, handle case of not getting IP due to disconnect, or handle
    //  timeout of not getting IP

    // ssid and password initialize either from configuration data received or if not received using improv wifi
    // only once these are availble will continue to actual wifi connectivity
    let mut ssid = heapless::String::<32>::new();
    let mut password = heapless::String::<64>::new();
    let mut improv_wifi_bootstrap = false;
    let mut ap_active;
    let mut credentials_available = false;

    if framework.borrow().wifi_ssid.is_some() {
        ssid = heapless::String::<32>::from_str(framework.borrow().wifi_ssid.as_ref().unwrap())
            .unwrap_or_default();
        password =
            heapless::String::<64>::from_str(framework.borrow().wifi_password.as_ref().unwrap())
                .unwrap_or_default();
        credentials_available = true;
    }

    // Improv Wifi and AccessPoint
    if !credentials_available {
        let client_config = Configuration::AccessPoint(AccessPointConfiguration {
            ssid: app_cargo_pkg_name.try_into().unwrap(),
            ..Default::default()
        });
        controller.set_configuration(&client_config).unwrap();
        controller.start_async().await.unwrap();
        // spawner.spawn(crate::framework::wifi::ap_net_task(ap_runner)).ok();
        spawner.spawn_heap(dhcp_server(ap_stack, framework.clone())).ok();
        if framework.borrow().settings.web_server_captive {
            spawner
                .spawn_heap(captive_portal(ap_stack, framework.clone()))
                .ok();
        }
        Timer::after(Duration::from_millis(1000)).await; // why wait (in original example)
        {
            // Important: Don't remove: block to drop framework_borrow
            let mut framework_borrow = framework.borrow_mut();
            framework_borrow.start_web_app(ap_stack, WebConfigMode::AP);
            drop(framework_borrow); // adding explicit drop, just in case
        }
        framework.borrow_mut().report_wifi(
            Some(Ipv4Addr::new(ap_addr.0, ap_addr.1, ap_addr.2, ap_addr.3)),
            true,
            app_cargo_pkg_name,
        );

        term_info!("WiFi Credentions not Configured.");
        term_info!("Provide WiFi credentials using either:");
        term_info!("- WiFi SSID: {}", app_cargo_pkg_name);
        term_info!(
            "  URL: {}://{}.{}.{}.{} or {}://config",
            { prefix },
            ap_addr.0,
            ap_addr.1,
            ap_addr.2,
            ap_addr.3,
            { prefix },
        );
        term_info!("- Continue web flash process in browser");
        // run Improv Wifi to get ssid/password

        ap_active = true;
        improv_wifi_bootstrap = true;

        // using  async closures which is unstable and seems like quite recent,
        // if there are issues, move to the function below with additional param

        // async fn send_packet(tx: &mut esp_hal::usb_serial_jtag::UsbSerialJtagTx<'static, esp_hal::Async>, packet: ImprovWifiPacket) {
        //     let data = packet.to_bytes().unwrap();
        //     embedded_io_async::Write::write(tx, &data).await.unwrap();
        //     embedded_io_async::Write::flush(tx).await.unwrap();
        // }

        // When using esp-flash web installer we miss the request for status that comes right after
        //   installation completes, therefore we send status w/o being asked.
        // Also, if we send too early, data doesn't arrive properly, therefore the wait before.
        // If there is no one on the other side of the serial this will hang, but it doesn't matter much,
        //   since if there's no one on the other side no point in this anyway, but need to be aware of that
        //   in case of future code changes
        // Howevere, some edge cases were seen where I suspect were caused by this code hanging, so I added a timeout.

        Timer::after(Duration::from_millis(2000)).await;
        let response = ImprovWifiPacket::new_current_state(CurrentStateOption::Ready);
        let _ = with_timeout(Duration::from_millis(1000), send_packet(response, false)).await;

        let mut buffer = Vec::with_capacity(100);
        let mut temp_buf = [0u8; 40];

        'improv_loop: loop {
            let r = rx.read(&mut temp_buf).await;

            match r {
                Ok(len) => {
                    if len == 0 {
                        // need to display something to use and exit,
                        // no point continuing to wifi section
                        return;
                    } // Append the new data to our growing buffer

                    buffer.extend_from_slice(&temp_buf[..len]);

                    // Try to parse packets from the buffer as long as data is available
                    'process_data: while !buffer.is_empty() {
                        // Attempt to parse a packet from the buffer
                        match ImprovWifiPacket::from_bytes((buffer.as_ref(), 0)) {
                            Ok((rest, packet)) => {
                                // Update the buffer by removing the parsed data (do it now to save time after send later)
                                let parsed_len = buffer.len() - rest.0.len();
                                buffer.drain(..parsed_len);
                                // Successfully parsed a packet
                                match packet.data {
                                    ImprovWifiPacketData::RPC(RPCCommandStruct {
                                        data: RPCCommand::RequestCurrentState,
                                        ..
                                    }) => {
                                        // TODO: check wifi state and respond accordingly
                                        let response = ImprovWifiPacket::new_current_state(
                                            CurrentStateOption::Ready,
                                        );
                                        send_packet(response, false).await;
                                    }
                                    ImprovWifiPacketData::RPC(RPCCommandStruct {
                                        data: RPCCommand::RequestDeviceInformation,
                                        ..
                                    }) => {
                                        let response = ImprovWifiPacket::new_rpc_result(RPCResultStruct::new_response_to_request_device_information(
                                            app_cargo_pkg_name,
                                            app_cargo_pkg_version,
                                            "ESP32S3",
                                            "WT32-SC01-Plus",
                                        ));
                                        send_packet(response, false).await;
                                    }
                                    ImprovWifiPacketData::RPC(RPCCommandStruct {
                                        data: RPCCommand::RequestScannedWifiNetworks,
                                        ..
                                    }) => {
                                        let cfg = esp_wifi::wifi::ScanConfig {
                                            ssid: None,
                                            bssid: None,
                                            channel: None,
                                            show_hidden: false,
                                            scan_type: esp_wifi::wifi::ScanTypeConfig::default(),
                                        };
                                        info!("Scanning for available WiFi networks");
                                        let scan_res =
                                            controller.scan_with_config_async::<50>(cfg).await;

                                        if let Ok(scan_results) = scan_res {
                                            let mut seen = hashbrown::HashSet::new();
                                            let unique_aps: Vec<AccessPointInfo> = scan_results
                                                .0
                                                .into_iter()
                                                .filter(|item| seen.insert(item.ssid.clone()))
                                                .collect();
                                            for ap_info in unique_aps {
                                                let response =
                                                    ImprovWifiPacket::new_rpc_result(RPCResultStruct::new_response_to_request_scanned_wifi_networks(
                                                        &ap_info.ssid,
                                                        &format!("{}", ap_info.signal_strength),
                                                        ap_info.auth_method.is_some(),
                                                    ));
                                                send_packet(response, true).await;
                                            }
                                        } else {
                                            term_error!(
                                                "Error scanning wifi networks {:?}",
                                                scan_res
                                            );
                                        }
                                        let response =
                                            ImprovWifiPacket::new_rpc_result(RPCResultStruct::new_response_to_request_scanned_wifi_networks_end());
                                        send_packet(response, true).await;
                                    }

                                    ImprovWifiPacketData::RPC(RPCCommandStruct {
                                        data:
                                            RPCCommand::SendWifiSettings(SendWifiSettingsStruct {
                                                ssid: improv_ssid,
                                                password: improv_password,
                                            }),
                                        ..
                                    }) => {
                                        let response = ImprovWifiPacket::new_current_state(
                                            CurrentStateOption::Provisioning,
                                        );
                                        send_packet(response, true).await;
                                        // If Acess Point is active stop it from now on,
                                        // For now to activate back need to restart device
                                        if ap_active {
                                            term_info!("ImprovWiFi setup: Stopping Acess Point");
                                            framework.borrow().stop_web_app(); // disable because it was started for Access Point mode configuration
                                            let _ = controller.disconnect_async().await;
                                            let _ = controller.stop_async().await;
                                            ap_active = false;
                                        }
                                        let client_config = esp_wifi::wifi::Configuration::Client(
                                            esp_wifi::wifi::ClientConfiguration {
                                                ssid: heapless::String::<32>::from_str(
                                                    <&str>::from(&improv_ssid),
                                                )
                                                .unwrap(),
                                                password: heapless::String::<64>::from_str(
                                                    <&str>::from(&improv_password),
                                                )
                                                .unwrap(),
                                                ..Default::default()
                                            },
                                        );
                                        term_info!(
                                            "ImprovWiFi: Credentials check - WiFi '{}'",
                                            <&str>::from(&improv_ssid)
                                        );
                                        controller.set_configuration(&client_config).unwrap();
                                        let _ = controller.start_async().await;
                                        let connect_res = controller.connect_async().await;
                                        let _ = controller.stop_async().await;
                                        if connect_res.is_ok() {
                                            ssid = heapless::String::<32>::from_str(<&str>::from(
                                                &improv_ssid,
                                            ))
                                            .unwrap();
                                            password = heapless::String::<64>::from_str(
                                                <&str>::from(&improv_password),
                                            )
                                            .unwrap();
                                            term_info!("ImprovWifi: Credentials Ok");
                                            break 'improv_loop;
                                        } else {
                                            let response = ImprovWifiPacket::new_error_state(
                                                ErrorStateOption::UnableToConnect,
                                            );
                                            send_packet(response, true).await;
                                            term_info!("ImprovWiFi: Credentials incorrect");
                                        }
                                    }
                                    _ => (),
                                }

                                if buffer.is_empty() {
                                    break 'process_data; // skips one empty iteration over no data to speed things up
                                }
                            }
                            Err(ParseError::Incomplete) => {
                                // debug!("Incomplete Deku data, will get more");
                                break 'process_data;
                            }
                            Err(_e) => {
                                term_error!("recv_err: {:x?}", buffer);
                                // let response = ImprovWifiPacket::new_error_state(ErrorStateOption::InvalidRPCPacket);
                                // send_packet(response).await;

                                // esp-web-tools doen't deal well with error messages.
                                // usually errors take place at the beginning of interaction and a few bytes are missed when it wants to send RequestCurrentState
                                // So let's send it, shouldn't hurt, and would probably help

                                // check that byte before last, checksum is 0xe6
                                if buffer.len() > 1 && buffer[buffer.len() - 2] == 0xe6 {
                                    let response = ImprovWifiPacket::new_rpc_result(
                                        RPCResultStruct::new_response_to_request_device_information(
                                            app_cargo_pkg_name,
                                            app_cargo_pkg_version,
                                            "ESP32S3",
                                            "WT32-SC01-Plus",
                                        ),
                                    );
                                    send_packet(response, false).await;
                                }
                                if let Some(pos) = buffer.iter().position(|&x| x == 10) {
                                    buffer.drain(..=pos); // Remove everything up to and including the first 10
                                } else {
                                    buffer.clear(); // If no 10 is found, clear the vector
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    term_error!("Error reading serial: {:?}", e);
                }
            }
        }
    }
    // Now WiFi credtneials available

    term_info!("About to connect to WiFi SSID '{}'", ssid);
    // trace!("About to connect Wifi using '{}', '{}'", password, ssid);

    let mut first_connect = true;
    let mut is_connected = false;
    loop {
        #[allow(clippy::single_match)]
        // TODO: Things are not working here as it should and code is also (in addition) incorrect.
        //       wifi_state() is always Invalid.
        //       and this loop is always 'stuck' in the connect_async() when connected.
        //       https://github.com/esp-rs/esp-hal/discussions/4261
        match esp_wifi::wifi::wifi_state() {
            esp_wifi::wifi::WifiState::StaConnected => {
                // wait until we're no longer connected
                // controller.wait_for_event(esp_wifi::wifi::WifiEvent::StaDisconnected).await;
                loop {
                    // trace!("Scanning");
                    // let cfg = esp_wifi::wifi::ScanConfig{
                    //     ssid:Some("DEV"),
                    //     bssid: None,
                    //     channel: None,
                    //     show_hidden: false,
                    //     scan_type: esp_wifi::wifi::ScanTypeConfig::Passive(core::time::Duration::from_secs(5))
                    // };
                    // let res = controller.scan_with_config::<1>(cfg).await;
                    // dbg!(res);
                    Timer::after(Duration::from_millis(1000)).await // why wait (in original example)
                }
            }
            _ => {
                // if !first_connect {
                //     term_error!("WiFi disconnected, reconnecting...");
                // }
            }
        }

        if !matches!(controller.is_started(), Ok(true)) {
            let client_config =
                esp_wifi::wifi::Configuration::Client(esp_wifi::wifi::ClientConfiguration {
                    ssid: ssid.clone(),
                    password: password.clone(),
                    ..Default::default()
                });
            controller.set_configuration(&client_config).unwrap();
            trace!("Starting wifi");
            controller.start_async().await.unwrap();
            trace!("Wifi started!");
        }

        match controller.connect_async().await {
            Ok(_) => {
                term_info!("Connected to WiFi");

                loop {
                    info!("Waiting for link to be up");
                    if sta_stack.is_link_up() {
                        break;
                    }
                    Timer::after(Duration::from_millis(500)).await;
                }
                term_info!("Waiting for an IP");

                let mut wait_counter = 24;
                const SKIP_CHECKS: i32 = 0;
                loop {
                    if let Some(config) = sta_stack.config_v4() {
                        term_info!("Received IP: {}", config.address);
                        framework.borrow_mut().report_wifi(
                            Some(config.address.address()),
                            false,
                            &ssid,
                        );
                        if improv_wifi_bootstrap {
                            // ignore warning, it's wrong, there's a drop below
                            let res = framework
                                .borrow_mut()
                                .set_wifi_credentials(&ssid, &password); // need to be on separate line (due to borrowing)
                            match res {
                                Ok(_) => {
                                    let response = ImprovWifiPacket::new_current_state(
                                        CurrentStateOption::Provisioned,
                                    );
                                    send_packet(response, true).await;

                                    framework
                                        .borrow_mut()
                                        .start_web_app(sta_stack, WebConfigMode::STA);

                                    let response = ImprovWifiPacket::new_rpc_result(
                                        RPCResultStruct::new_response_to_send_wifi_settings(
                                            &format!("{prefix}://{}", config.address.address()),
                                        ),
                                    );
                                    term_info!("Stored credentials in flash");
                                    send_packet(response, true).await;
                                }
                                Err(e) => {
                                    term_error!(format!("Error storing credentials in flash, WiFi initialization halted {e:?}"));
                                    return;
                                }
                            }
                        }
                        framework.borrow().notify_wifi_sta_connected();
                        first_connect = false;
                        is_connected = true;
                        break;
                    } else {
                        if wait_counter >= SKIP_CHECKS {
                            if (wait_counter - SKIP_CHECKS) % 90 == 0 {
                                term_info!("");
                            }
                            term_info_same_line!(".");
                        }
                        wait_counter += 1;
                    }
                    Timer::after(Duration::from_millis(250)).await;
                    info!("Still waiting for an IP address");
                }
            }
            Err(e) => {
                if is_connected && !first_connect {
                    framework.borrow_mut().report_wifi(None, false, &ssid);
                    framework.borrow().notify_wifi_sta_disconnected();
                }
                is_connected = false;
                term_error!("Error while trying to connect to wifi: {:?}", e);
                Timer::after(Duration::from_millis(1000)).await
            }
        }
    }
}

// #[embassy_executor::task]
async fn dhcp_server(stack: Stack<'static>, framework: Rc<RefCell<Framework>>) {
    let ap_addr = framework.borrow().settings.ap_addr;
    let mut server: edge_dhcp::server::Server<fn() -> u64, 3> =
        edge_dhcp::server::Server::new_with_et(Ipv4Addr::new(
            ap_addr.0, ap_addr.1, ap_addr.2, ap_addr.3,
        ));
    let mut gw = [Ipv4Addr::new(ap_addr.0, ap_addr.1, ap_addr.2, ap_addr.3)];
    let mut server_options = edge_dhcp::server::ServerOptions::new(
        Ipv4Addr::new(ap_addr.0, ap_addr.1, ap_addr.2, ap_addr.3),
        Some(&mut gw),
    );
    let dnss = [Ipv4Addr::new(ap_addr.0, ap_addr.1, ap_addr.2, ap_addr.3)];
    server_options.dns = &dnss;
    // server_options.lease_duration_secs = 5;

    let mut buf = vec![0; 512];
    let udp_buffers: edge_nal_embassy::UdpBuffers<1, 512, 512, 1> =
        edge_nal_embassy::UdpBuffers::new();
    let udp = edge_nal_embassy::Udp::new(stack, &udp_buffers);
    let addr = core::net::SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DEFAULT_SERVER_PORT);
    let mut socket = udp.bind(core::net::SocketAddr::V4(addr)).await.unwrap();
    io::server::server::run(&mut server, &server_options, &mut socket, &mut buf)
        .await
        .unwrap();
}

// #[embassy_executor::task]
async fn captive_portal(stack: Stack<'static>, framework: Rc<RefCell<Framework>>) {
    let ap_addr = framework.borrow().settings.ap_addr;
    let udp_buffers: edge_nal_embassy::UdpBuffers<1, 512, 512, 1> =
        edge_nal_embassy::UdpBuffers::new();
    let udp = edge_nal_embassy::Udp::new(stack, &udp_buffers);

    let mut tx_buf = vec![0; 512];
    let mut rx_buf = vec![0; 512];
    edge_captive::io::run(
        &udp,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 53),
        &mut tx_buf,
        &mut rx_buf,
        Ipv4Addr::new(ap_addr.0, ap_addr.1, ap_addr.2, ap_addr.3),
        core::time::Duration::from_secs(60),
    )
    .await
    .unwrap();
}

#[embassy_executor::task]
pub async fn sta_net_task(mut runner: Runner<'static, WifiDevice<'static, WifiStaDevice>>) {
    runner.run().await
}

#[embassy_executor::task]
pub async fn ap_net_task(mut runner: Runner<'static, WifiDevice<'static, WifiApDevice>>) {
    runner.run().await
}
