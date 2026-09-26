//! Finding devices a host can carry: mDNS for Apple TVs, SSDP for Samsung and LG TVs, and
//! `devicectl` for iPhones and iPads on this Mac.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use bridge_protocol::DeviceOs;
use serde::Serialize;

use crate::http;

/// A device found on the network (or attached to this Mac).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Found {
    /// The name the device goes by ("Living Room", "Shubham's iPhone").
    pub name: String,
    /// What to put in `HostedDevice::address`: an IP address for TVs, the UDID for iPhone and iPad.
    pub address: String,
    pub model: Option<String>,
}

/// Looks for devices of this kind for up to `timeout`. Never fails; finds nothing instead.
pub async fn discover(os: DeviceOs, timeout: Duration) -> Vec<Found> {
    let mut found = match os {
        DeviceOs::Tvos => apple_tvs(timeout).await,
        DeviceOs::SamsungTv => samsung_tvs(timeout).await,
        DeviceOs::LgTv => lg_tvs(timeout).await,
        DeviceOs::Ios | DeviceOs::Ipados => crate::ios::attached_devices(os).await,
        _ => vec![],
    };
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));
    found.dedup_by(|a, b| a.address == b.address);
    found
}

/// The device named `name` (case-insensitive), or the only one found.
pub(crate) fn pick<'a>(found: &'a [Found], name: &str) -> Option<&'a Found> {
    let n = name.trim().to_lowercase();
    found
        .iter()
        .find(|f| f.name.to_lowercase() == n)
        .or(if found.len() == 1 {
            found.first()
        } else {
            None
        })
}

// ───────────── mDNS ─────────────

/// One resolved mDNS service instance.
#[derive(Debug, Clone, Default)]
pub(crate) struct MdnsService {
    /// Instance name ("Living Room").
    pub instance: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub txt: HashMap<String, String>,
}

impl MdnsService {
    /// Prefers IPv4, which is what TVs are normally reached on.
    pub fn best_address(&self) -> Option<IpAddr> {
        self.addresses
            .iter()
            .find(|a| a.is_ipv4())
            .or(self.addresses.first())
            .copied()
    }
}

/// Browses one service type (`_companion-link._tcp.local.`) for `timeout`.
pub(crate) async fn browse_mdns(service_type: &str, timeout: Duration) -> Vec<MdnsService> {
    let service_type = service_type.to_owned();
    tokio::task::spawn_blocking(move || browse_blocking(&service_type, timeout))
        .await
        .unwrap_or_default()
}

fn browse_blocking(service_type: &str, timeout: Duration) -> Vec<MdnsService> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    let Ok(daemon) = ServiceDaemon::new() else {
        return vec![];
    };
    let Ok(rx) = daemon.browse(service_type) else {
        let _ = daemon.shutdown();
        return vec![];
    };
    let deadline = std::time::Instant::now() + timeout;
    let mut out: Vec<MdnsService> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let suffix = format!(".{service_type}");
                let full = info.get_fullname();
                let instance = full
                    .strip_suffix(&suffix)
                    .unwrap_or(full)
                    .replace("\\032", " ");
                let txt = info
                    .get_properties()
                    .iter()
                    .map(|p| (p.key().to_owned(), p.val_str().to_owned()))
                    .collect();
                let svc = MdnsService {
                    instance,
                    addresses: info.get_addresses().iter().copied().collect(),
                    port: info.get_port(),
                    txt,
                };
                if let Some(existing) = out.iter_mut().find(|s| s.instance == svc.instance) {
                    *existing = svc;
                } else {
                    out.push(svc);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse(service_type);
    let _ = daemon.shutdown();
    out
}

async fn apple_tvs(timeout: Duration) -> Vec<Found> {
    browse_mdns("_companion-link._tcp.local.", timeout)
        .await
        .into_iter()
        .filter(|s| s.txt.get("rpMd").is_some_and(|m| m.starts_with("AppleTV")))
        .filter_map(|s| {
            Some(Found {
                name: s.instance.clone(),
                address: s.best_address()?.to_string(),
                model: s.txt.get("rpMd").cloned(),
            })
        })
        .collect()
}

// ───────────── SSDP ─────────────

const SAMSUNG_ST: &str = "urn:samsung.com:device:RemoteControlReceiver:1";
const LG_ST: &str = "urn:lge-com:service:webos-second-screen:1";

pub(crate) fn m_search(st: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
    )
}

/// One SSDP answer: who answered and their headers (lower-cased names).
#[derive(Debug, Clone)]
pub(crate) struct SsdpReply {
    pub from: IpAddr,
    pub headers: HashMap<String, String>,
}

pub(crate) fn parse_ssdp(from: IpAddr, text: &str) -> Option<SsdpReply> {
    let mut lines = text.split("\r\n");
    if !lines
        .next()?
        .to_ascii_uppercase()
        .starts_with("HTTP/1.1 200")
    {
        return None;
    }
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Some(SsdpReply { from, headers })
}

async fn ssdp(st: &str, timeout: Duration) -> Vec<SsdpReply> {
    let Ok(sock) = tokio::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await
    else {
        return vec![];
    };
    let _ = sock.set_multicast_ttl_v4(2);
    let target = SocketAddr::from((Ipv4Addr::new(239, 255, 255, 250), 1900));
    let msg = m_search(st);
    for _ in 0..2 {
        let _ = sock.send_to(msg.as_bytes(), target).await;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut out: Vec<SsdpReply> = Vec::new();
    let mut buf = vec![0u8; 4096];
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await
    {
        if let Some(r) = parse_ssdp(from.ip(), &String::from_utf8_lossy(&buf[..n])) {
            let matches_st = r
                .headers
                .get("st")
                .is_some_and(|s| s.eq_ignore_ascii_case(st));
            if matches_st && !out.iter().any(|o| o.from == r.from) {
                out.push(r);
            }
        }
    }
    out
}

/// `<tag>value</tag>` from a small XML document (UPnP device descriptions).
pub(crate) fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    let v = xml[start..end].trim();
    (!v.is_empty()).then(|| {
        v.replace("&amp;", "&")
            .replace("&apos;", "'")
            .replace("&quot;", "\"")
    })
}

async fn samsung_tvs(timeout: Duration) -> Vec<Found> {
    let replies = ssdp(SAMSUNG_ST, timeout).await;
    let lookups = replies.into_iter().map(|r| async move {
        let ip = r.from.to_string();
        let info = http::request(
            "GET",
            &format!("http://{}:8001/api/v2/", crate::common::url_host(&ip)),
            &[],
            b"",
            Duration::from_secs(2),
            true,
        )
        .await
        .ok()
        .and_then(|m| m.json())
        .map(|v| crate::samsung::parse_device_info(&v));
        Found {
            name: info
                .as_ref()
                .and_then(|i| i.name.clone())
                .unwrap_or_else(|| format!("Samsung TV ({ip})")),
            address: ip,
            model: info.and_then(|i| i.model),
        }
    });
    futures_util::future::join_all(lookups).await
}

async fn lg_tvs(timeout: Duration) -> Vec<Found> {
    let replies = ssdp(LG_ST, timeout).await;
    let lookups = replies.into_iter().map(|r| async move {
        let ip = r.from.to_string();
        let desc = match r.headers.get("location") {
            Some(loc) => http::request("GET", loc, &[], b"", Duration::from_secs(2), true)
                .await
                .ok()
                .map(|m| m.text()),
            None => None,
        };
        Found {
            name: desc
                .as_deref()
                .and_then(|d| xml_tag(d, "friendlyName"))
                .unwrap_or_else(|| format!("LG TV ({ip})")),
            address: ip,
            model: desc.as_deref().and_then(|d| xml_tag(d, "modelName")),
        }
    });
    futures_util::future::join_all(lookups).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssdp_messages() {
        assert_eq!(
            m_search(LG_ST),
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:lge-com:service:webos-second-screen:1\r\n\r\n"
        );
        let r = parse_ssdp(
            "192.168.1.9".parse().unwrap(),
            "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLOCATION: http://192.168.1.9:1990/desc.xml\r\nST: urn:lge-com:service:webos-second-screen:1\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.headers["location"], "http://192.168.1.9:1990/desc.xml");
        assert!(parse_ssdp("1.2.3.4".parse().unwrap(), "NOTIFY * HTTP/1.1\r\n\r\n").is_none());
    }

    #[test]
    fn xml_tags() {
        let d = "<root><device><friendlyName>[LG] webOS TV OLED55C1</friendlyName><modelName>OLED55C1PUB</modelName></device></root>";
        assert_eq!(
            xml_tag(d, "friendlyName").as_deref(),
            Some("[LG] webOS TV OLED55C1")
        );
        assert_eq!(xml_tag(d, "modelName").as_deref(), Some("OLED55C1PUB"));
        assert_eq!(xml_tag(d, "serialNumber"), None);
    }

    #[test]
    fn picking() {
        let f = |n: &str, a: &str| Found {
            name: n.into(),
            address: a.into(),
            model: None,
        };
        let list = vec![f("Bedroom", "10.0.0.2"), f("Living Room", "10.0.0.3")];
        assert_eq!(pick(&list, "living room").unwrap().address, "10.0.0.3");
        assert!(pick(&list, "Kitchen").is_none());
        assert_eq!(pick(&list[..1], "Kitchen").unwrap().address, "10.0.0.2");
    }

    #[tokio::test]
    async fn discovery_never_fails() {
        // No TVs on a CI network: an empty list, quickly, not an error.
        let t = std::time::Instant::now();
        let _ = discover(DeviceOs::LgTv, Duration::from_millis(300)).await;
        let _ = discover(DeviceOs::Android, Duration::from_millis(300)).await;
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
