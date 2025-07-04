use core::{cell::RefCell, net::{Ipv4Addr, Ipv6Addr}};

use alloc::{boxed::Box, rc::Rc};
use edge_mdns::io::{Mdns, DEFAULT_SOCKET};
use edge_nal::UdpSplit;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};

use crate::prelude::Framework;

#[embassy_executor::task]
pub async fn mdns_task(framework: Rc<RefCell<Framework>>) {
    if framework.borrow().device_name.is_none() {
        return;
    }
    info!("mdns_task started (not yet functional, need IP)");
    let stack = framework.borrow().stack;
    let (recv_buf, send_buf) = (
        Box::new(edge_mdns::buf::VecBufAccess::<NoopRawMutex, 512>::new()),
        Box::new(edge_mdns::buf::VecBufAccess::<NoopRawMutex, 512>::new()),
    );
    let udp_buffers: Box<edge_nal_embassy::UdpBuffers<1, 512, 512, 1>> =
        Box::new(edge_nal_embassy::UdpBuffers::new());
    let udp = edge_nal_embassy::Udp::new(stack, &udp_buffers);
    let mut socket =
        edge_mdns::io::bind(&udp, DEFAULT_SOCKET, Some(Ipv4Addr::UNSPECIFIED), Some(0))
            .await
            .unwrap();
    let (recv, send) = socket.split();
    let signal = Signal::<NoopRawMutex, ()>::new();
    let mdns = Mdns::new(
        Some(Ipv4Addr::UNSPECIFIED),
        Some(0),
        recv,
        send,
        *recv_buf,
        *send_buf,
        |buf| getrandom::getrandom(buf).unwrap(),
        &signal,
    );
    let device_name = framework.borrow().device_name.as_ref().unwrap().clone();

    Framework::wait_for_wifi(&framework).await;
    let address = stack.config_v4().unwrap().address.address();

    let host = edge_mdns::host::Host {
        hostname: &device_name,
        ipv4: address,
        ipv6: Ipv6Addr::UNSPECIFIED,
        ttl: edge_mdns::domain::base::Ttl::from_secs(60),
    };
    info!("mDNS active with HOST {}, IP: {}", host.hostname, host.ipv4);
    mdns.run(edge_mdns::HostAnswersMdnsHandler::new(&host))
        .await
        .unwrap();
}
