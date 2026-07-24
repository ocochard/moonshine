//! FreeBSD-only PCM capture backend that reads samples from
//! `/dev/dsp.loop` — the `virtual_oss(8)` loopback device that games write
//! to via `SDL_AUDIODEV`. Produces the same `AudioFrame` values on the same
//! channel as the Linux PulseAudio server so the encoder is oblivious to
//! the source.
//!
//! Wire format is 48 kHz, S16_LE, `channels`-interleaved — matched to the
//! encoder's expectation of interleaved f32 samples via a per-sample
//! `i16 -> f32 / 32768.0` conversion.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::thread;
use std::time;

use async_shutdown::ShutdownManager;

use crate::session::manager::SessionShutdownReason;
use crate::session::stream::audio::frame::{AudioFrame, CAPTURE_SAMPLE_RATE};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// The virtual_oss(8) loopback device. `virtual_oss` is in the base
/// system on FreeBSD but is **not** enabled by default — moonshine's
/// pkg-message documents the two-step setup:
/// `service virtual_oss enable` + `service virtual_oss start`. The
/// default flags in `/etc/rc.d/virtual_oss` include `-l dsp.loop`,
/// which publishes this node.
const DSP_LOOP_PATH: &str = "/dev/dsp.loop";

/// Default clock tick rate when the negotiated `packet_duration_ms` is
/// not 5 or 10. Matches the pulse-server default (5 ms).
const DEFAULT_CLOCK_RATE_HZ: u32 = 200;

// FreeBSD OSS ioctls (sys/soundcard.h).
//
//   #define AFMT_S16_LE          0x00000010
//   #define SNDCTL_DSP_SPEED     _IOWR('P', 2, int)
//   #define SNDCTL_DSP_SETFMT    _IOWR('P', 5, int)
//   #define SNDCTL_DSP_CHANNELS  SOUND_PCM_WRITE_CHANNELS
//                                 = _IOWR('P', 6, int)
//
// `_IOWR(g, n, t)` on FreeBSD (sys/ioccom.h) expands to
//   IOC_INOUT | ((sizeof(t) & IOCPARM_MASK) << 16) | (g << 8) | n
// with IOC_INOUT = 0xC0000000 and IOCPARM_MASK = (1<<13)-1.
const AFMT_S16_LE: libc::c_int = 0x0000_0010;

const fn iowr_int(group: u8, num: u8) -> libc::c_ulong {
	const IOC_INOUT: libc::c_ulong = 0xC000_0000;
	const IOCPARM_MASK: libc::c_ulong = (1 << 13) - 1;
	let size = std::mem::size_of::<libc::c_int>() as libc::c_ulong;
	IOC_INOUT | ((size & IOCPARM_MASK) << 16) | ((group as libc::c_ulong) << 8) | (num as libc::c_ulong)
}

const SNDCTL_DSP_SPEED: libc::c_ulong = iowr_int(b'P', 2);
const SNDCTL_DSP_SETFMT: libc::c_ulong = iowr_int(b'P', 5);
const SNDCTL_DSP_CHANNELS: libc::c_ulong = iowr_int(b'P', 6);

pub(crate) struct OssCapture;

impl OssCapture {
	pub fn spawn(
		channels: u8,
		packet_duration_ms: u32,
		frame_tx: crossbeam_channel::Sender<AudioFrame>,
		frame_recycle_rx: crossbeam_channel::Receiver<AudioFrame>,
		stop: ShutdownManager<SessionShutdownReason>,
	) -> Result<(), Error> {
		let clock_rate_hz = match packet_duration_ms {
			5 | 10 => 1000 / packet_duration_ms,
			_ => {
				if packet_duration_ms != 0 {
					tracing::warn!(
						"Unsupported packet_duration_ms {}, falling back to default {}Hz",
						packet_duration_ms,
						DEFAULT_CLOCK_RATE_HZ,
					);
				}
				DEFAULT_CLOCK_RATE_HZ
			},
		};

		let fd = open_and_configure(channels)?;

		let num_frames = (CAPTURE_SAMPLE_RATE / clock_rate_hz) as usize;
		let samples_per_chunk = num_frames * channels as usize;
		let bytes_per_chunk = samples_per_chunk * std::mem::size_of::<i16>();

		tracing::debug!(
			"OssCapture: {} Hz, {} ch, chunk {} samples ({} bytes) @ {} Hz",
			CAPTURE_SAMPLE_RATE,
			channels,
			samples_per_chunk,
			bytes_per_chunk,
			clock_rate_hz,
		);

		thread::Builder::new()
			.name("oss-capture".to_string())
			.spawn(move || {
				let _stop_token = stop.trigger_shutdown_token(SessionShutdownReason::PulseServerStopped);
				let _delay_stop = stop.delay_shutdown_token();

				if let Err(e) = run_capture(fd, channels, samples_per_chunk, frame_tx, frame_recycle_rx, &stop) {
					tracing::error!("OssCapture error: {e}");
				}
				tracing::debug!("OssCapture stopped.");
			})
			.map_err(|e| -> Error { format!("failed to spawn oss-capture thread: {e}").into() })?;

		Ok(())
	}
}

/// Open `/dev/dsp.loop` and negotiate S16_LE / channels / 48000 Hz.
fn open_and_configure(channels: u8) -> Result<OwnedFd, Error> {
	let path = std::ffi::CString::new(DSP_LOOP_PATH).unwrap();

	// SAFETY: passing a valid nul-terminated path; O_RDONLY only.
	let raw = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
	if raw < 0 {
		let err = std::io::Error::last_os_error();
		match err.raw_os_error() {
			Some(libc::ENOENT) | Some(libc::ENXIO) | Some(libc::ENODEV) => {
				tracing::error!(
					"{} is missing. virtual_oss(8) is in the base system but \
					 not enabled by default — enable it with \
					 `service virtual_oss enable` then \
					 `service virtual_oss start`. Its default flags include \
					 `-l dsp.loop` which publishes this node.",
					DSP_LOOP_PATH,
				);
			},
			Some(libc::EBUSY) => {
				tracing::error!(
					"{} is busy — another process holds the loopback. \
					 Check virtual_oss(8) status.",
					DSP_LOOP_PATH,
				);
			},
			_ => {
				tracing::error!("Failed to open {}: {}", DSP_LOOP_PATH, err);
			},
		}
		return Err(format!("open({}): {}", DSP_LOOP_PATH, err).into());
	}

	// SAFETY: `raw` is a freshly-opened fd owned by us.
	let fd = unsafe { OwnedFd::from_raw_fd(raw) };

	set_int_ioctl(fd.as_raw_fd(), SNDCTL_DSP_SETFMT, AFMT_S16_LE, "SNDCTL_DSP_SETFMT")?;
	set_int_ioctl(
		fd.as_raw_fd(),
		SNDCTL_DSP_CHANNELS,
		channels as libc::c_int,
		"SNDCTL_DSP_CHANNELS",
	)?;
	set_int_ioctl(
		fd.as_raw_fd(),
		SNDCTL_DSP_SPEED,
		CAPTURE_SAMPLE_RATE as libc::c_int,
		"SNDCTL_DSP_SPEED",
	)?;

	Ok(fd)
}

/// Perform one in-out `int` ioctl and log a warning if the driver
/// coerced the value.
fn set_int_ioctl(fd: RawFd, req: libc::c_ulong, want: libc::c_int, name: &str) -> Result<(), Error> {
	let mut val: libc::c_int = want;
	// SAFETY: `req` is one of the OSS `_IOWR('P', N, int)` codes, which
	// requires a pointer to a single `int` for both directions.
	let rc = unsafe { libc::ioctl(fd, req, &mut val as *mut libc::c_int) };
	if rc < 0 {
		let err = std::io::Error::last_os_error();
		return Err(format!("{name} ioctl failed: {err}").into());
	}
	if val != want {
		tracing::warn!("OSS {name}: requested {want}, driver returned {val}");
	}
	Ok(())
}

fn run_capture(
	fd: OwnedFd,
	channels: u8,
	samples_per_chunk: usize,
	frame_tx: crossbeam_channel::Sender<AudioFrame>,
	frame_recycle_rx: crossbeam_channel::Receiver<AudioFrame>,
	stop: &ShutdownManager<SessionShutdownReason>,
) -> Result<(), Error> {
	let raw = fd.as_raw_fd();
	let epoch = time::Instant::now();

	// Interleaved S16_LE scratch buffer, one chunk = one AudioFrame.
	let mut scratch = vec![0u8; samples_per_chunk * std::mem::size_of::<i16>()];

	// Poll every 100 ms so shutdown latency is bounded even if the
	// device stops producing data (e.g. game exited, virtual_oss idle).
	let poll_timeout_ms: libc::c_int = 100;

	// Frame-drop streak tracking: virtual_oss produces samples continuously
	// even when no client is playing, and the AudioEncoder is start_notify-
	// gated on RTSP PLAY. Between capture start and PLAY (and briefly again
	// on PLAY tear-down) we'll drop every frame we produce. Log a single
	// debug! at the start of each streak and a summary when it ends, rather
	// than a trace! per frame (~200 per second of noise otherwise).
	let mut drop_streak: u32 = 0;

	while !stop.is_shutdown_triggered() {
		// Wait for the fd to become readable, or timeout.
		let mut pfd = libc::pollfd {
			fd: raw,
			events: libc::POLLIN,
			revents: 0,
		};
		// SAFETY: pollfd struct is valid for the duration of the call.
		let rc = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, poll_timeout_ms) };
		if rc < 0 {
			let err = std::io::Error::last_os_error();
			if err.raw_os_error() == Some(libc::EINTR) {
				continue;
			}
			return Err(format!("poll(/dev/dsp.loop): {err}").into());
		}
		if rc == 0 {
			continue; // timeout, check stop and loop
		}
		if pfd.revents & libc::POLLIN == 0 {
			// POLLHUP / POLLERR — device gone.
			return Err(format!("poll revents={:#x} on /dev/dsp.loop", pfd.revents).into());
		}

		// Read one full chunk. OSS may return short reads; loop until full.
		let mut filled = 0usize;
		while filled < scratch.len() {
			// SAFETY: scratch is a valid mutable slice.
			let n = unsafe {
				libc::read(
					raw,
					scratch.as_mut_ptr().add(filled) as *mut libc::c_void,
					scratch.len() - filled,
				)
			};
			if n < 0 {
				let err = std::io::Error::last_os_error();
				match err.raw_os_error() {
					Some(libc::EINTR) => continue,
					Some(libc::EAGAIN) => break, // shouldn't happen — device is blocking — but be robust
					_ => return Err(format!("read(/dev/dsp.loop): {err}").into()),
				}
			}
			if n == 0 {
				// EOF: loopback closed on the other side.
				return Ok(());
			}
			filled += n as usize;
			if stop.is_shutdown_triggered() {
				return Ok(());
			}
		}
		if filled < scratch.len() {
			continue; // short chunk (EAGAIN mid-read); resync on next tick
		}

		// Acquire an AudioFrame — recycle if possible, allocate otherwise.
		let mut frame = match frame_recycle_rx.try_recv() {
			Ok(mut frame) => {
				frame.buf.resize(samples_per_chunk, 0.0);
				frame
			},
			Err(crossbeam_channel::TryRecvError::Empty) => AudioFrame {
				buf: vec![0.0; samples_per_chunk],
				capture_ts_ms: 0,
			},
			Err(crossbeam_channel::TryRecvError::Disconnected) => return Ok(()),
		};

		// Convert interleaved S16_LE bytes -> f32 samples.
		debug_assert_eq!(frame.buf.len(), samples_per_chunk);
		let _ = channels; // capture kept for future channel-map validation
		for (i, out) in frame.buf.iter_mut().enumerate() {
			let bytes = [scratch[i * 2], scratch[i * 2 + 1]];
			*out = i16::from_le_bytes(bytes) as f32 / 32768.0;
		}

		frame.capture_ts_ms = epoch.elapsed().as_millis() as u64;

		// Non-blocking send: if the encoder is behind we simply drop the
		// frame. The recycle pool guarantees the encoder always has a
		// pre-allocated buffer to send back, so we don't deadlock.
		match frame_tx.try_send(frame) {
			Ok(()) => {
				if drop_streak > 0 {
					tracing::debug!("Encoder caught up after dropping {drop_streak} audio frame(s).");
					drop_streak = 0;
				}
			},
			Err(crossbeam_channel::TrySendError::Full(_dropped)) => {
				if drop_streak == 0 {
					tracing::debug!(
						"AudioEncoder not draining; dropping audio frames (typical pre-RTSP-PLAY silence)."
					);
				}
				drop_streak = drop_streak.saturating_add(1);
			},
			Err(crossbeam_channel::TrySendError::Disconnected(_)) => return Ok(()),
		}
	}

	Ok(())
}
