use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind, IoSliceMut};
use std::mem::MaybeUninit;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::io::{AsRawFd, RawFd};
use std::{io, mem, ptr};

use socket2::{Domain, Protocol, SockAddr, SockAddrStorage, Socket, Type};

use crate::PktInfo;

// BSDs do not implement IP_PKTINFO / struct in_pktinfo. Instead they split
// the same information across IP_RECVDSTADDR (destination address, delivered
// as struct in_addr) and IP_RECVIF (receive interface, delivered as struct
// sockaddr_dl carrying the interface index in sdl_index). IPv6 is uniform
// across platforms and uses IPV6_(RECV)PKTINFO / struct in6_pktinfo.

unsafe fn setsockopt<T>(
    socket: libc::c_int,
    level: libc::c_int,
    name: libc::c_int,
    value: T,
) -> io::Result<()>
where
    T: Copy,
{
    let value = &value as *const T as *const libc::c_void;
    if libc::setsockopt(
        socket,
        level,
        name,
        value,
        mem::size_of::<T>() as libc::socklen_t,
    ) == 0
    {
        Ok(())
    } else {
        Err(Error::last_os_error())
    }
}

//
pub struct PktInfoUdpSocket {
    socket: Socket,
    domain: Domain,
}

impl Debug for PktInfoUdpSocket {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.socket.fmt(f)
    }
}

impl AsRawFd for PktInfoUdpSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}

impl PktInfoUdpSocket {
    pub fn new(domain: Domain) -> io::Result<PktInfoUdpSocket> {
        let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

        match domain {
            Domain::IPV4 => unsafe {
                #[cfg(any(
                    target_os = "freebsd",
                    target_os = "dragonfly",
                    target_os = "netbsd",
                    target_os = "openbsd",
                ))]
                {
                    // BSDs split IP_PKTINFO into two sockopts.
                    setsockopt(socket.as_raw_fd(), libc::IPPROTO_IP, libc::IP_RECVDSTADDR, 1)?;
                    setsockopt(socket.as_raw_fd(), libc::IPPROTO_IP, libc::IP_RECVIF, 1)?;
                }
                #[cfg(not(any(
                    target_os = "freebsd",
                    target_os = "dragonfly",
                    target_os = "netbsd",
                    target_os = "openbsd",
                )))]
                {
                    setsockopt(socket.as_raw_fd(), libc::IPPROTO_IP, libc::IP_PKTINFO, 1)?;
                }
            },
            Domain::IPV6 => unsafe {
                setsockopt(
                    socket.as_raw_fd(),
                    libc::IPPROTO_IPV6,
                    libc::IPV6_RECVPKTINFO,
                    1,
                )?;
            },
            _ => return Err(Error::from(ErrorKind::Unsupported)),
        }

        Ok(PktInfoUdpSocket { socket, domain })
    }

    pub fn domain(&self) -> Domain {
        self.domain
    }
    pub fn set_reuse_address(&self, reuse: bool) -> io::Result<()> {
        self.socket.set_reuse_address(reuse)
    }

    pub fn set_reuse_port(&self, reuse: bool) -> io::Result<()> {
        self.socket.set_reuse_port(reuse)
    }

    pub fn join_multicast_v4(&self, addr: &Ipv4Addr, interface: &Ipv4Addr) -> io::Result<()> {
        self.socket.join_multicast_v4(addr, interface)
    }

    /// Drop membership in a multicast group for IPv4.
    pub fn leave_multicast_v4(&self, addr: &Ipv4Addr, interface: &Ipv4Addr) -> io::Result<()> {
        self.socket.leave_multicast_v4(addr, interface)
    }

    pub fn set_multicast_if_v4(&self, interface: &Ipv4Addr) -> io::Result<()> {
        self.socket.set_multicast_if_v4(interface)
    }

    pub fn set_multicast_loop_v4(&self, loop_v4: bool) -> io::Result<()> {
        self.socket.set_multicast_loop_v4(loop_v4)
    }

    pub fn set_multicast_ttl_v4(&self, ttl: u32) -> io::Result<()> {
        self.socket.set_multicast_ttl_v4(ttl)
    }

    pub fn join_multicast_v6(&self, addr: &Ipv6Addr, interface: u32) -> io::Result<()> {
        self.socket.join_multicast_v6(addr, interface)
    }

    /// Drop membership in a multicast group for IPv6.
    pub fn leave_multicast_v6(&self, addr: &Ipv6Addr, interface: u32) -> io::Result<()> {
        self.socket.leave_multicast_v6(addr, interface)
    }

    pub fn set_multicast_if_v6(&self, interface: u32) -> io::Result<()> {
        self.socket.set_multicast_if_v6(interface)
    }

    pub fn set_multicast_loop_v6(&self, loop_v6: bool) -> io::Result<()> {
        self.socket.set_multicast_loop_v6(loop_v6)
    }

    pub fn set_multicast_hops_v6(&self, hops: u32) -> io::Result<()> {
        self.socket.set_multicast_hops_v6(hops)
    }

    pub fn set_nonblocking(&self, reuse: bool) -> io::Result<()> {
        self.socket.set_nonblocking(reuse)
    }

    pub fn bind(&self, addr: &SockAddr) -> io::Result<()> {
        self.socket.bind(addr)
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.socket.send(buf)
    }

    pub fn send_to(&self, buf: &[u8], addr: &SockAddr) -> io::Result<usize> {
        self.socket.send_to(buf, addr)
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, PktInfo)> {
        let mut addr_src = SockAddrStorage::zeroed();
        let mut msg_iov = IoSliceMut::new(buf);
        let mut cmsg = {
            let space = if self.domain == Domain::IPV4 {
                // BSDs deliver two cmsgs for IPv4 (IP_RECVDSTADDR + IP_RECVIF);
                // Linux/others deliver one (IP_PKTINFO). Compute cmsg-space
                // for whichever the platform will actually populate.
                #[cfg(any(
                    target_os = "freebsd",
                    target_os = "dragonfly",
                    target_os = "netbsd",
                    target_os = "openbsd",
                ))]
                unsafe {
                    libc::CMSG_SPACE(mem::size_of::<libc::in_addr>() as libc::c_uint) as usize
                        + libc::CMSG_SPACE(mem::size_of::<libc::sockaddr_dl>() as libc::c_uint)
                            as usize
                }
                #[cfg(not(any(
                    target_os = "freebsd",
                    target_os = "dragonfly",
                    target_os = "netbsd",
                    target_os = "openbsd",
                )))]
                unsafe {
                    libc::CMSG_SPACE(mem::size_of::<libc::in_pktinfo>() as libc::c_uint) as usize
                }
            } else {
                unsafe {
                    libc::CMSG_SPACE(mem::size_of::<libc::in6_pktinfo>() as libc::c_uint) as usize
                }
            };
            Vec::<u8>::with_capacity(space)
        };

        let mut mhdr = unsafe {
            let mut mhdr = MaybeUninit::<libc::msghdr>::zeroed();
            let p = mhdr.as_mut_ptr();
            (*p).msg_name = addr_src.view_as::<libc::c_void>();
            (*p).msg_namelen = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            (*p).msg_iov = &mut msg_iov as *mut IoSliceMut as *mut libc::iovec;
            (*p).msg_iovlen = 1;
            (*p).msg_control = cmsg.as_mut_ptr() as *mut libc::c_void;
            (*p).msg_controllen = cmsg.capacity() as _;
            (*p).msg_flags = 0;
            mhdr.assume_init()
        };

        let bytes_recv =
            unsafe { libc::recvmsg(self.socket.as_raw_fd(), &mut mhdr as *mut libc::msghdr, 0) };
        if bytes_recv <= 0 {
            return Err(Error::last_os_error());
        }

        let len = addr_src.size_of();
        let addr_src = unsafe { SockAddr::new(addr_src, len) }.as_socket().unwrap();

        let mut header = if mhdr.msg_controllen > 0 {
            debug_assert!(!mhdr.msg_control.is_null());
            debug_assert!(cmsg.capacity() >= mhdr.msg_controllen as usize);

            Some(unsafe {
                libc::CMSG_FIRSTHDR(&mhdr as *const libc::msghdr)
                    .as_ref()
                    .unwrap()
            })
        } else {
            None
        };

        // On non-BSD platforms the single IP_PKTINFO / IPV6_PKTINFO cmsg is
        // sufficient. On BSDs the IPv4 destination address and interface
        // arrive in two separate cmsgs (IP_RECVDSTADDR + IP_RECVIF), which
        // must be combined into one PktInfo — so we track them separately
        // and merge after the walk.
        let mut info: Option<PktInfo> = None;
        #[cfg(any(
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
        ))]
        let mut bsd_dst: Option<Ipv4Addr> = None;
        #[cfg(any(
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
        ))]
        let mut bsd_if_index: Option<u32> = None;

        while info.is_none() && header.is_some() {
            let h = header.unwrap();
            let p = unsafe { libc::CMSG_DATA(h) };

            match (h.cmsg_level, h.cmsg_type) {
                #[cfg(not(any(
                    target_os = "freebsd",
                    target_os = "dragonfly",
                    target_os = "netbsd",
                    target_os = "openbsd",
                )))]
                (libc::IPPROTO_IP, libc::IP_PKTINFO) => {
                    let pktinfo = unsafe { ptr::read_unaligned(p as *const libc::in_pktinfo) };
                    info = Some(PktInfo {
                        if_index: pktinfo.ipi_ifindex as _,
                        addr_src,
                        addr_dst: IpAddr::V4(Ipv4Addr::from(u32::from_be(pktinfo.ipi_addr.s_addr))),
                    })
                }
                #[cfg(any(
                    target_os = "freebsd",
                    target_os = "dragonfly",
                    target_os = "netbsd",
                    target_os = "openbsd",
                ))]
                (libc::IPPROTO_IP, libc::IP_RECVDSTADDR) => {
                    let addr = unsafe { ptr::read_unaligned(p as *const libc::in_addr) };
                    bsd_dst = Some(Ipv4Addr::from(u32::from_be(addr.s_addr)));
                }
                #[cfg(any(
                    target_os = "freebsd",
                    target_os = "dragonfly",
                    target_os = "netbsd",
                    target_os = "openbsd",
                ))]
                (libc::IPPROTO_IP, libc::IP_RECVIF) => {
                    let sdl = unsafe { ptr::read_unaligned(p as *const libc::sockaddr_dl) };
                    bsd_if_index = Some(sdl.sdl_index as u32);
                }
                (libc::IPPROTO_IPV6, libc::IPV6_PKTINFO) => {
                    let pktinfo = unsafe { ptr::read_unaligned(p as *const libc::in6_pktinfo) };

                    info = Some(PktInfo {
                        if_index: pktinfo.ipi6_ifindex as _,
                        addr_src,
                        addr_dst: IpAddr::V6(Ipv6Addr::from(pktinfo.ipi6_addr.s6_addr)),
                    })
                }
                _ => {}
            }

            // Advance to the next cmsg unconditionally: on BSD IPv4 both
            // cmsgs must be visited to collect the destination address AND
            // interface index. The outer `info.is_none()` guard preserves
            // the early-exit on non-BSD after the single PKTINFO cmsg.
            header = unsafe {
                let p = libc::CMSG_NXTHDR(&mhdr as *const _, h as *const _);
                p.as_ref()
            };
        }

        // BSD IPv4 path: fold the two accumulated cmsgs into a PktInfo.
        // Interface index may legitimately be unknown (0) if IP_RECVIF was
        // not delivered by the kernel for this packet.
        #[cfg(any(
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
        ))]
        {
            if info.is_none() {
                if let Some(dst) = bsd_dst {
                    info = Some(PktInfo {
                        if_index: bsd_if_index.unwrap_or(0) as u64,
                        addr_src,
                        addr_dst: IpAddr::V4(dst),
                    });
                }
            }
        }

        match info {
            None => Err(Error::new(
                ErrorKind::NotFound,
                "Failed to read PKTINFO from socket",
            )),
            Some(info) => Ok((bytes_recv as _, info)),
        }
    }

    /// Creates a new independently owned std UdpSocket from this PktInfoUdpSocket.
    ///
    /// This is useful to mix and match functionality from this crate with stdlib or other crates.
    pub fn try_clone_std(&self) -> io::Result<std::net::UdpSocket> {
        Ok(self.socket.try_clone()?.into())
    }
}
