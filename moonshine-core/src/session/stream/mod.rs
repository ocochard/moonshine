use std::net::SocketAddr;

use serde::Deserialize;
use serde::Serialize;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use crate::session::stream::audio::AudioStreamConfig;
use crate::session::stream::control::ControlStreamConfig;
use crate::session::stream::video::VideoStreamConfig;

pub mod audio;
pub mod control;
pub mod video;

/// Bind a stream UDP socket, disabling `IPV6_V6ONLY` for IPv6 addresses so the
/// single socket also receives from IPv4-mapped peers — the UDP counterpart of
/// `rtsp::bind_rtsp_listener` and `webserver::bind_listener`.
///
/// A bare `UdpSocket::bind("::")` inherits the system default, which differs per
/// platform: Linux defaults `net.ipv6.bindv6only=0` (dual-stack, so this is a
/// no-op there), while FreeBSD defaults `net.inet6.ip6.v6only=1` (IPv6 only).
///
/// The video and audio streams only learn where to send from the client's `PING`
/// arriving on this socket, and stay silent until it does. On FreeBSD an
/// IPv6-only bind therefore dropped an IPv4 client's `PING`, leaving the stream
/// with no destination: pairing and `/launch` succeeded over the dual-stack TCP
/// listeners, then the session produced no video or audio at all, with nothing
/// logged to point at the cause.
pub(crate) async fn bind_stream_socket(address: &str, port: u16) -> std::io::Result<UdpSocket> {
	let ip = address
		.parse()
		.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{address}: {e}")))?;
	let address = SocketAddr::new(ip, port);

	let socket = Socket::new(Domain::for_address(address), Type::DGRAM, Some(Protocol::UDP))?;
	if address.is_ipv6() {
		socket.set_only_v6(false)?;
	}
	socket.bind(&address.into())?;
	socket.set_nonblocking(true)?;
	UdpSocket::from_std(socket.into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamConfig {
	/// Port to bind the RTSP server to.
	pub port: u16,

	/// Configuration for the video stream.
	pub video: VideoStreamConfig,

	/// Configuration for the audio stream.
	pub audio: AudioStreamConfig,

	/// Configuration for the control stream.
	pub control: ControlStreamConfig,

	/// Time in seconds since last ping after which the stream closes.
	pub timeout: u64,
}

impl Default for StreamConfig {
	fn default() -> Self {
		Self {
			port: 48010,
			video: Default::default(),
			audio: Default::default(),
			control: Default::default(),
			timeout: 60,
		}
	}
}

#[derive(Debug)]
#[repr(C)]
struct RtpHeader {
	header: u8,
	packet_type: u8,
	sequence_number: u16,
	timestamp: u32,
	ssrc: u32,
}

impl RtpHeader {}
