//! Interface discovery and explicitly routed IPv4 connections.
use crate::{Error, Result};
use serde::Serialize;
use std::{
    ffi::{CStr, CString},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    num::NonZeroU32,
};

#[derive(Clone, Debug)]
pub struct LocalBind {
    pub interface: String,
    pub index: NonZeroU32,
    pub local_ip: Ipv4Addr,
}
impl LocalBind {
    /// Both absent means normal routing; otherwise both values are required.
    /// The interface may be a name or a positive numeric index.
    pub fn parse(interface: Option<&str>, local_ip: Option<&str>) -> Result<Option<Self>> {
        let (interface, ip) = match (interface, local_ip) {
            (None, None) => return Ok(None),
            (Some(interface), Some(ip)) => (interface, ip),
            _ => {
                return Err(Error::InvalidArgument(
                    "binding requires interface and local IPv4 together".into(),
                ));
            }
        };
        let local_ip = ip
            .parse::<Ipv4Addr>()
            .map_err(|e| Error::InvalidArgument(format!("local IPv4: {e}")))?;
        let index = if interface.bytes().all(|b| b.is_ascii_digit()) && !interface.is_empty() {
            interface
                .parse::<u32>()
                .map_err(|e| Error::InvalidArgument(format!("interface index: {e}")))?
        } else {
            let name =
                CString::new(interface).map_err(|e| Error::InvalidArgument(e.to_string()))?;
            unsafe { libc::if_nametoindex(name.as_ptr()) }
        };
        let index = NonZeroU32::new(index).ok_or_else(|| {
            Error::InvalidArgument(format!("unknown/zero interface: {interface}"))
        })?;
        Ok(Some(Self {
            interface: interface.into(),
            index,
            local_ip,
        }))
    }
}
pub(crate) async fn connect(
    addr: SocketAddr,
    binding: Option<&LocalBind>,
) -> Result<tokio::net::TcpStream> {
    let Some(binding) = binding else {
        return Ok(tokio::net::TcpStream::connect(addr).await?);
    };
    if !addr.is_ipv4() {
        return Err(Error::InvalidArgument(
            "IPv4 binding requires IPv4 peer".into(),
        ));
    }
    let socket = tokio::net::TcpSocket::new_v4()?;
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    socket2::SockRef::from(&socket).bind_device_by_index_v4(Some(binding.index))?;
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    return Err(Error::InvalidArgument(
        "interface binding unsupported on this platform".into(),
    ));
    socket.bind(SocketAddr::new(binding.local_ip.into(), 0))?;
    Ok(socket.connect(addr).await?)
}
#[derive(Debug, Serialize)]
pub struct Interface {
    pub name: String,
    pub index: u32,
    pub address: String,
    pub flags: Vec<&'static str>,
}
pub fn list_interfaces() -> Result<Vec<Interface>> {
    let mut head = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    struct Guard(*mut libc::ifaddrs);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let _guard = Guard(head);
    let mut entries = Vec::new();
    let mut current = head;
    while let Some(item) = unsafe { current.as_ref() } {
        current = item.ifa_next;
        if item.ifa_addr.is_null() || item.ifa_name.is_null() {
            continue;
        }
        let family = unsafe { (*item.ifa_addr).sa_family } as i32;
        let address = match family {
            libc::AF_INET => {
                let addr = unsafe { &*item.ifa_addr.cast::<libc::sockaddr_in>() };
                Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes()).to_string()
            }
            libc::AF_INET6 => {
                let addr = unsafe { &*item.ifa_addr.cast::<libc::sockaddr_in6>() };
                Ipv6Addr::from(addr.sin6_addr.s6_addr).to_string()
            }
            _ => continue,
        };
        let flags = [
            (libc::IFF_UP, "UP"),
            (libc::IFF_RUNNING, "RUNNING"),
            (libc::IFF_POINTOPOINT, "POINTOPOINT"),
        ]
        .into_iter()
        .filter_map(|(bit, name)| (item.ifa_flags & bit as u32 != 0).then_some(name))
        .collect();
        entries.push(Interface {
            name: unsafe { CStr::from_ptr(item.ifa_name) }
                .to_string_lossy()
                .into_owned(),
            index: unsafe { libc::if_nametoindex(item.ifa_name) },
            address,
            flags,
        });
    }
    Ok(entries)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binding_argument_validation() {
        assert!(LocalBind::parse(None, None).unwrap().is_none());
        for (iface, ip) in [
            (Some("0"), Some("127.0.0.1")),
            (None, Some("127.0.0.1")),
            (Some("lo0"), None),
            (Some("1"), Some("::1")),
            (Some("ipu-no-such-interface"), Some("127.0.0.1")),
        ] {
            assert!(LocalBind::parse(iface, ip).is_err());
        }
        let binding = LocalBind::parse(Some("1"), Some("127.0.0.1"))
            .unwrap()
            .unwrap();
        assert_eq!(binding.index.get(), 1);
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn bound_loopback_connect_and_interface_json() {
        let entries = list_interfaces().unwrap();
        let json = serde_json::to_value(&entries).unwrap();
        assert!(
            json.as_array()
                .unwrap()
                .iter()
                .any(|i| i["name"] == "lo0" && i["address"] == "127.0.0.1")
        );
        let binding = LocalBind::parse(Some("lo0"), Some("127.0.0.1"))
            .unwrap()
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            connect(listener.local_addr().unwrap(), Some(&binding)),
        )
        .await
        .unwrap()
        .unwrap();
        let (_, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.ip(), binding.local_ip);
        assert_eq!(stream.local_addr().unwrap().ip(), binding.local_ip);
    }
}
