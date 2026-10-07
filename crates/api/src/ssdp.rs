//! SSDP 局域网发现（Jellyfin 兼容）：监听 UPnP 组播 239.255.255.250:1900，
//! 响应 Infuse / Jellyfin 客户端的 M-SEARCH，让它们在同一局域网自动发现
//! 本实例的 Jellyfin 兼容面（docs/adr/0005-jellyfin-compatible-playback.md）。
//!
//! 绑定失败（权限/端口占用）时静默退出 —— 发现是便利功能，不是服务主链路。

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

const SSDP_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaServer:1";

/// 监听并响应 M-SEARCH；`port` 是 Jellyfin 兼容面的 HTTP 端口，LOCATION 用它。
pub async fn run(port: u16, device_id: String) {
    let socket = match tokio::net::UdpSocket::bind(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        SSDP_PORT,
    ))
    .await
    {
        Ok(socket) => socket,
        Err(_) => return, // 权限不足 / 端口被占：静默跳过
    };
    if socket
        .join_multicast_v4(SSDP_ADDR, Ipv4Addr::UNSPECIFIED)
        .is_err()
    {
        return;
    }
    if socket.set_multicast_loop_v4(true).is_err() {
        return;
    }
    let host = local_ip().unwrap_or_else(|| Ipv4Addr::LOCALHOST);
    let location = format!("http://{host}:{port}/jellyfin");
    let mut buf = [0u8; 4096];
    loop {
        let Ok((len, from)) = socket.recv_from(&mut buf).await else {
            break;
        };
        let request = String::from_utf8_lossy(&buf[..len]).to_ascii_lowercase();
        if !request.contains("m-search") {
            continue;
        }
        if !(request.contains("st: urn:schemas-upnp-org:device:mediaserver:1")
            || request.contains("st: ssdp:all")
            || request.contains("st: upnp:rootdevice"))
        {
            continue;
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             CACHE-CONTROL: max-age=86400\r\n\
             EXT:\r\n\
             LOCATION: {location}\r\n\
             SERVER: crawler-media/0.1 UPnP/1.0\r\n\
             ST: {DEVICE_TYPE}\r\n\
             USN: uuid:{device_id}::{DEVICE_TYPE}\r\n\
             \r\n"
        );
        let _ = socket.send_to(response.as_bytes(), from).await;
        // 组播源请求也回组播，方便多播客户端。
        let multicast = SocketAddrV4::new(SSDP_ADDR, SSDP_PORT);
        let _ = socket.send_to(response.as_bytes(), multicast).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 取本机局域网 IP：建一个不发送数据的 UDP 连接探出默认路由网段。
fn local_ip() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:53").ok()?;
    let addr = socket.local_addr().ok()?;
    match addr.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}
