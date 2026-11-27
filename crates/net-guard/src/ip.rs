use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn ipv4_from_nat64_exact_96(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let segs = v6.segments();
    // RFC 6052 well-known prefix 64:ff9b::/96 or exact zero-subnet RFC 8215 64:ff9b:1::/96
    if segs[0] == 0x0064
        && segs[1] == 0xff9b
        && (segs[2..6] == [0, 0, 0, 0] || (segs[2] == 0x0001 && segs[3..6] == [0, 0, 0]))
    {
        return Some(Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            segs[6] as u8,
            (segs[7] >> 8) as u8,
            segs[7] as u8,
        ));
    }
    None
}

fn ipv4s_from_nat64(v6: Ipv6Addr) -> [Option<Ipv4Addr>; 4] {
    let segs = v6.segments();
    if segs[0] == 0x0064 && segs[1] == 0xff9b && segs[2..6] == [0, 0, 0, 0] {
        return [
            Some(Ipv4Addr::new(
                (segs[6] >> 8) as u8,
                segs[6] as u8,
                (segs[7] >> 8) as u8,
                segs[7] as u8,
            )),
            None,
            None,
            None,
        ];
    }
    // RFC 8215 local-use NAT64 64:ff9b:1::/48 can be subnetted as /96, /64, /56, or /48 per RFC 6052 §2.2
    if segs[0] == 0x0064 && segs[1] == 0xff9b && segs[2] == 0x0001 {
        let p96 = Some(Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            segs[6] as u8,
            (segs[7] >> 8) as u8,
            segs[7] as u8,
        ));
        let u_zero = (segs[4] >> 8) == 0;
        let p64 = u_zero.then(|| {
            Ipv4Addr::new(
                segs[4] as u8,
                (segs[5] >> 8) as u8,
                segs[5] as u8,
                (segs[6] >> 8) as u8,
            )
        });
        let p56 = u_zero.then(|| {
            Ipv4Addr::new(
                segs[3] as u8,
                segs[4] as u8,
                (segs[5] >> 8) as u8,
                segs[5] as u8,
            )
        });
        let p48 = u_zero.then(|| {
            Ipv4Addr::new(
                (segs[3] >> 8) as u8,
                segs[3] as u8,
                segs[4] as u8,
                (segs[5] >> 8) as u8,
            )
        });
        return [p96, p64, p56, p48];
    }
    [None; 4]
}

fn ipv4_from_6to4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let segs = v6.segments();
    if segs[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (segs[1] >> 8) as u8,
            segs[1] as u8,
            (segs[2] >> 8) as u8,
            segs[2] as u8,
        ));
    }
    None
}

fn ipv4s_from_teredo(v6: Ipv6Addr) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let segs = v6.segments();
    (segs[0] == 0x2001 && segs[1] == 0x0000).then(|| {
        let srv = Ipv4Addr::new(
            (segs[2] >> 8) as u8,
            segs[2] as u8,
            (segs[3] >> 8) as u8,
            segs[3] as u8,
        );
        let c6 = !segs[6];
        let c7 = !segs[7];
        let cli = Ipv4Addr::new((c6 >> 8) as u8, c6 as u8, (c7 >> 8) as u8, c7 as u8);
        (srv, cli)
    })
}

pub(crate) fn embedded_ipv4s(v6: Ipv6Addr) -> [Option<Ipv4Addr>; 9] {
    let nat64 = ipv4s_from_nat64(v6);
    let teredo = ipv4s_from_teredo(v6);
    [
        v6.to_ipv4_mapped(),
        if v6.is_loopback() || v6.is_unspecified() {
            None
        } else {
            v6.to_ipv4()
        },
        nat64[0],
        nat64[1],
        nat64[2],
        nat64[3],
        ipv4_from_6to4(v6),
        teredo.map(|(srv, _)| srv),
        teredo.map(|(_, cli)| cli),
    ]
}

pub fn has_unspecified_or_zero_ip(ip: IpAddr) -> bool {
    let canon = canonicalize_ip(ip);
    if canon.is_unspecified() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => v4.octets()[0] == 0,
        IpAddr::V6(v6) => embedded_ipv4s(v6)
            .into_iter()
            .flatten()
            .any(|v4| v4.is_unspecified() || v4.octets()[0] == 0),
    }
}

fn is_cloud_metadata_ipv4(v4: Ipv4Addr) -> bool {
    let octets = v4.octets();
    matches!(
        octets,
        [169, 254, 169, 254] | [169, 254, 170, 2] | [100, 100, 100, 200] | [168, 63, 129, 16]
    )
}

pub fn is_cloud_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_cloud_metadata_ipv4(v4),
        IpAddr::V6(v6) => {
            let segs = v6.segments();
            let is_aws_imds_v6 = segs[0] == 0xfd00 && segs[1] == 0x0ec2;
            is_aws_imds_v6
                || embedded_ipv4s(v6)
                    .into_iter()
                    .flatten()
                    .any(is_cloud_metadata_ipv4)
        }
    }
}

pub fn is_cloud_metadata_host(host: &str) -> bool {
    let clean = host.trim_end_matches('.').to_ascii_lowercase();
    matches!(
        clean.as_str(),
        "metadata"
            | "instance-data"
            | "metadata.google.internal"
            | "metadata.goog"
            | "metadata.azure.internal"
    ) || clean.ends_with(".metadata.google.internal")
        || clean.ends_with(".metadata.goog")
        || clean.ends_with(".metadata.azure.internal")
}

fn is_global_unicast_v4(v4: Ipv4Addr) -> bool {
    if is_cloud_metadata_ipv4(v4) {
        return false;
    }
    let o = v4.octets();
    let is_this_network = o[0] == 0;
    let is_cgnat = o[0] == 100 && (o[1] & 0b1100_0000) == 64;
    let is_ietf_protocol = o[0] == 192 && o[1] == 0 && o[2] == 0;
    let is_6to4_relay = o[0] == 192 && o[1] == 88 && o[2] == 99;
    let is_benchmark = o[0] == 198 && (o[1] & 0xfe) == 18;
    let is_test_net_2 = o[0] == 198 && o[1] == 51 && o[2] == 100;
    let is_test_net_3 = o[0] == 203 && o[1] == 0 && o[2] == 113;
    let is_reserved_high = o[0] >= 240;

    !(v4.is_unspecified()
        || is_this_network
        || v4.is_loopback()
        || v4.is_private()
        || is_cgnat
        || v4.is_link_local()
        || is_ietf_protocol
        || v4.is_documentation()
        || is_6to4_relay
        || is_benchmark
        || is_test_net_2
        || is_test_net_3
        || v4.is_multicast()
        || v4.is_broadcast()
        || is_reserved_high)
}

fn is_global_unicast_v6(v6: Ipv6Addr) -> bool {
    if is_cloud_metadata_ip(IpAddr::V6(v6)) {
        return false;
    }
    let segs = v6.segments();
    // Only 2000::/3 global unicast space is eligible, excluding special-purpose allocations.
    if (segs[0] & 0xe000) != 0x2000 {
        return false;
    }
    let is_teredo = segs[0] == 0x2001 && segs[1] == 0x0000;
    let is_benchmark = segs[0] == 0x2001 && segs[1] == 0x0002 && segs[2] == 0x0000;
    let is_orchid = segs[0] == 0x2001 && (segs[1] & 0xfff0) == 0x0010;
    let is_orchid_v2 = segs[0] == 0x2001 && (segs[1] & 0xfff0) == 0x0020;
    let is_doc = segs[0] == 0x2001 && segs[1] == 0x0db8;
    let is_6to4 = segs[0] == 0x2002;

    !(v6.is_unspecified()
        || v6.is_loopback()
        || v6.is_multicast()
        || v6.is_unicast_link_local()
        || v6.is_unique_local()
        || is_teredo
        || is_benchmark
        || is_orchid
        || is_orchid_v2
        || is_doc
        || is_6to4)
}

pub fn canonicalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                IpAddr::V6(v6)
            } else {
                v6.to_ipv4_mapped()
                    .or_else(|| v6.to_ipv4())
                    .or_else(|| ipv4_from_nat64_exact_96(v6))
                    .or_else(|| ipv4_from_6to4(v6))
                    .map(IpAddr::V4)
                    .unwrap_or(IpAddr::V6(v6))
            }
        }
    }
}

pub fn is_global_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_global_unicast_v4(v4),
        IpAddr::V6(v6) => is_global_unicast_v6(v6),
    }
}

/// RFC 2544 benchmarking range (`198.18.0.0/15`), commonly assigned by router Fake-IP DNS proxies.
pub fn is_benchmark_fake_ip(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 198 && (v4.octets()[1] & 0xfe) == 18)
}
