use smoltcp::wire::{IpCidr, IpAddress, Ipv4Address};

#[derive(Clone, Copy, PartialEq)]
pub enum NetMode {
    Dhcp,
    Static,
}

pub struct NetConfig {
    pub mode: NetMode,
    /// Static IP address + prefix (e.g. 10.0.2.15/24). Only used when mode == Static.
    pub address: Option<IpCidr>,
    /// Default gateway. Only used when mode == Static.
    pub gateway: Option<Ipv4Address>,
    /// DNS server. Only used when mode == Static.
    pub dns: Option<Ipv4Address>,
    /// Whether to send a test ping on startup.
    pub ping_enabled: bool,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            mode: NetMode::Dhcp,
            address: None,
            gateway: None,
            dns: None,
            ping_enabled: true,
        }
    }
}

impl NetConfig {
    pub fn parse(bytes: &[u8]) -> Self {
        let text = match core::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };

        let mut cfg = Self::default();
        let mut section = "";

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if line.starts_with('[') && line.ends_with(']') {
                section = &line[1..line.len() - 1];
                continue;
            }

            let Some((key, val)) = line.split_once('=') else { continue };
            let key = key.trim();
            let val = val.trim();

            match section {
                "general" => match key {
                    "mode" => {
                        cfg.mode = match val {
                            "static" => NetMode::Static,
                            _ => NetMode::Dhcp,
                        };
                    }
                    _ => {}
                },
                "static" => match key {
                    "address" => cfg.address = parse_cidr(val),
                    "gateway" => cfg.gateway = parse_ipv4(val),
                    "dns"     => cfg.dns = parse_ipv4(val),
                    _ => {}
                },
                "ping" => match key {
                    "enabled" => cfg.ping_enabled = val == "true",
                    _ => {}
                },
                _ => {}
            }
        }

        cfg
    }
}

fn parse_ipv4(s: &str) -> Option<Ipv4Address> {
    let mut parts = s.splitn(4, '.');
    let a = parts.next()?.parse::<u8>().ok()?;
    let b = parts.next()?.parse::<u8>().ok()?;
    let c = parts.next()?.parse::<u8>().ok()?;
    let d = parts.next()?.parse::<u8>().ok()?;
    Some(Ipv4Address::new(a, b, c, d))
}

fn parse_cidr(s: &str) -> Option<IpCidr> {
    let (addr_str, prefix_str) = s.split_once('/')?;
    let addr = parse_ipv4(addr_str.trim())?;
    let prefix = prefix_str.trim().parse::<u8>().ok()?;
    Some(IpCidr::new(IpAddress::Ipv4(addr), prefix))
}
