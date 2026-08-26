//! Periodic clock used by the pulse-server thread to pace audio frames.
//!
//! Exposes a single `AudioClock` type with a raw fd that can be registered
//! into an `mio::Poll` via `SourceFd`. Each tick delivers a POLLIN edge; the
//! caller drains the fd with `AudioClock::drain()`.
//!
//! Linux: wraps `mio_timerfd::TimerFd` (kernel timer, one syscall per tick).
//! Other Unix (FreeBSD, macOS): a helper thread sleeps and writes one byte
//! per tick into a pipe. The read end is the fd exposed to mio.

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

#[cfg(target_os = "linux")]
mod imp {
	use super::*;

	pub struct AudioClock {
		inner: mio_timerfd::TimerFd,
	}

	impl AudioClock {
		pub fn new(interval: Duration) -> io::Result<Self> {
			let mut inner = mio_timerfd::TimerFd::new(mio_timerfd::ClockId::Monotonic)?;
			inner.set_timeout_interval(&interval)?;
			Ok(Self { inner })
		}

		pub fn drain(&mut self) -> io::Result<()> {
			// A wakeup can race its own drain and find no expirations left to
			// read (EAGAIN). That is not fatal: skip the tick rather than
			// propagating an error that would tear down the whole session.
			// Matches the non-Linux implementation below, which also treats an
			// empty drain as success.
			match self.inner.read() {
				Ok(_) => Ok(()),
				Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
				Err(e) => Err(e),
			}
		}
	}

	impl AsRawFd for AudioClock {
		fn as_raw_fd(&self) -> RawFd {
			self.inner.as_raw_fd()
		}
	}
}

#[cfg(not(target_os = "linux"))]
mod imp {
	use super::*;
	use std::os::unix::io::{FromRawFd, IntoRawFd, OwnedFd};
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::sync::Arc;
	use std::thread;

	pub struct AudioClock {
		read_fd: OwnedFd,
		stop: Arc<AtomicBool>,
	}

	impl AudioClock {
		pub fn new(interval: Duration) -> io::Result<Self> {
			// SAFETY: pipe2(O_CLOEXEC | O_NONBLOCK) initializes both fds.
			let (read_fd, write_fd) = unsafe {
				let mut fds: [libc::c_int; 2] = [0; 2];
				let flags = libc::O_CLOEXEC | libc::O_NONBLOCK;
				if libc::pipe2(fds.as_mut_ptr(), flags) < 0 {
					return Err(io::Error::last_os_error());
				}
				(OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))
			};

			let stop = Arc::new(AtomicBool::new(false));
			let stop_thread = stop.clone();
			let writer_fd = write_fd.into_raw_fd();

			thread::Builder::new()
				.name("audio-clock".to_string())
				.spawn(move || {
					// The tick byte value is irrelevant; the reader only cares
					// about readability edges.
					let tick: u8 = 0;
					while !stop_thread.load(Ordering::Relaxed) {
						thread::sleep(interval);
						// SAFETY: writer_fd is a valid pipe write end owned by
						// this thread. A nonblocking write of 1 byte either
						// succeeds or returns EAGAIN when the pipe buffer is
						// full — either is fine (a missed edge just means the
						// reader is already behind).
						let ret = unsafe {
							libc::write(writer_fd, &tick as *const u8 as *const _, 1)
						};
						if ret < 0 {
							let err = io::Error::last_os_error();
							match err.raw_os_error() {
								Some(libc::EAGAIN) => continue,
								_ => break,
							}
						}
					}
					// SAFETY: closing our owned write end on exit.
					unsafe {
						libc::close(writer_fd);
					}
				})
				.map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

			Ok(Self { read_fd, stop })
		}

		pub fn drain(&mut self) -> io::Result<()> {
			// Drain all pending tick bytes. The pipe is nonblocking so we
			// loop until EAGAIN.
			let mut buf = [0u8; 64];
			// SAFETY: we hold an OwnedFd for the read end; reading from it
			// through a &File built on a borrowed copy would double-close.
			// Use the raw fd directly instead.
			let fd = self.read_fd.as_raw_fd();
			loop {
				let ret = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
				if ret > 0 {
					continue;
				}
				if ret == 0 {
					return Ok(());
				}
				let err = io::Error::last_os_error();
				match err.raw_os_error() {
					Some(libc::EAGAIN) => return Ok(()),
					Some(libc::EINTR) => continue,
					_ => return Err(err),
				}
			}
		}
	}

	impl AsRawFd for AudioClock {
		fn as_raw_fd(&self) -> RawFd {
			self.read_fd.as_raw_fd()
		}
	}

	impl Drop for AudioClock {
		fn drop(&mut self) {
			// Signal the writer thread to exit at the next wakeup. The
			// pipe fds close naturally when the OwnedFds drop.
			self.stop.store(true, Ordering::Relaxed);
		}
	}
}

pub use imp::AudioClock;
