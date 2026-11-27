use net_guard::{
    NetGuardError, PrivateEndpointException, TargetPolicy, is_cloud_metadata_ip, normalize_url,
};
use std::net::{IpAddr, SocketAddr};

#[test]
fn nat64_6to4_and_teredo_block_self_listen_unspecified_and_cloud_metadata() {
    // 1. Teredo client/server cloud metadata IPs must be unconditionally blocked
    for teredo_meta in [
        "2001:0000:4136:e378:8000:63bf:5601:5601", // client 169.254.169.254
        "2001:0000:a9fe:a9fe::1",                  // server 169.254.169.254
        "2001:0000:4136:e378:8000:63bf:9b9b:9b37", // client 100.100.100.200
    ] {
        let ip: IpAddr = teredo_meta.parse().expect("parse ip");
        assert!(
            is_cloud_metadata_ip(ip),
            "expected Teredo metadata IP `{teredo_meta}` to be detected"
        );
        let url = format!("http://[{teredo_meta}]/latest/meta-data/");
        assert!(
            matches!(
                normalize_url(&url, true),
                Err(NetGuardError::BlockedAddress(_))
            ),
            "normalize_url must reject Teredo metadata URL `{url}`"
        );
    }

    // 2. NAT64 / 6to4 / Teredo must NOT bypass `blocked_self` or `unspecified` even with PrivateEndpointException
    let target = normalize_url("http://internal.lan:18765/api/v1/health", true).expect("target");
    for bad_ip_str in [
        "64:ff9b::7f00:1",                         // NAT64 127.0.0.1
        "2002:7f00:1::",                           // 6to4 127.0.0.1
        "64:ff9b::c0a8:10a",                       // NAT64 192.168.1.10
        "2002:c0a8:10a::",                         // 6to4 192.168.1.10
        "64:ff9b::",                               // NAT64 0.0.0.0 (unspecified)
        "2002::",                                  // 6to4 0.0.0.0 (unspecified)
        "2001:0000:4136:e378:8000:63bf:80ff:fffe", // Teredo client 127.0.0.1 (0x7f00 ^ 0xffff = 0x80ff, 0x0001 ^ 0xffff = 0xfffe)
        "2001:0000:4136:e378:8000:63bf:ffff:ffff", // Teredo client 0.0.0.0
    ] {
        let ip: IpAddr = bad_ip_str.parse().expect("parse v6");
        let sock = SocketAddr::new(ip, 18765);
        let policy = TargetPolicy::production()
            .with_blocked_self("127.0.0.1:18765".parse().unwrap())
            .with_blocked_self("192.168.1.10:18765".parse().unwrap())
            .with_private_exception(PrivateEndpointException {
                host: "internal.lan".into(),
                port: 18765,
                allowed_ips: vec![ip],
                path_prefixes: vec![],
            });
        assert!(
            matches!(
                policy.validate_socket_addr(&target, sock),
                Err(NetGuardError::BlockedAddress(_))
            ),
            "validate_socket_addr must block `{sock}`"
        );
    }
}

#[test]
fn rfc8215_subnetted_nat64_blocks_cloud_metadata_self_listen_and_unspecified() {
    for nat64_meta in ["64:ff9b:1:0808::a9fe:a9fe", "64:ff9b:1:1:100::a9fe:a9fe"] {
        let ip: IpAddr = nat64_meta.parse().expect("parse ip");
        assert!(
            is_cloud_metadata_ip(ip),
            "expected RFC 8215 subnetted NAT64 metadata IP `{nat64_meta}` to be detected"
        );
        let url = format!("http://[{nat64_meta}]/latest/meta-data/");
        assert!(
            matches!(
                normalize_url(&url, true),
                Err(NetGuardError::BlockedAddress(_))
            ),
            "normalize_url must reject RFC 8215 metadata URL `{url}`"
        );
    }

    let target = normalize_url("http://internal.lan:18765/api/v1/health", true).expect("target");
    for bad_ip_str in [
        "64:ff9b:1:0808::7f00:1",
        "64:ff9b:1:1:100::7f00:1",
        "64:ff9b:1:0808::",
    ] {
        let ip: IpAddr = bad_ip_str.parse().expect("parse v6");
        let sock = SocketAddr::new(ip, 18765);
        let policy = TargetPolicy::production()
            .with_blocked_self("127.0.0.1:18765".parse().unwrap())
            .with_private_exception(PrivateEndpointException {
                host: "internal.lan".into(),
                port: 18765,
                allowed_ips: vec![ip],
                path_prefixes: vec![],
            });
        assert!(
            matches!(
                policy.validate_socket_addr(&target, sock),
                Err(NetGuardError::BlockedAddress(_))
            ),
            "validate_socket_addr must block RFC 8215 `{sock}`"
        );
    }
}
