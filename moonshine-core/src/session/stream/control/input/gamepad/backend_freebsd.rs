//! Native FreeBSD gamepad backend: injects a virtual Xbox-style controller
//! through `/dev/uinput` + evdev, speaking the uinput ioctl protocol directly
//! (no `libevdev`/`inputtino`, which pull in Linux-only UAPI headers).
//!
//! FreeBSD ships a Linux-ABI-compatible evdev/uinput subsystem
//! (`/usr/include/dev/evdev/`), so the event codes and struct layouts match
//! Linux. The ioctl *request numbers* do NOT: FreeBSD's `sys/ioccom.h` uses a
//! 13-bit param length and different direction bits, and the bit-setting
//! ioctls are `_IOWINT` (IOC_VOID, int-by-value). We therefore compute request
//! numbers with a local `const fn` mirroring FreeBSD `_IOC`, and define every
//! ABI struct/constant here, sized to match the system headers (verified by
//! compiling against them on the target: input_event=24, input_id=8,
//! input_absinfo=24, uinput_setup=92, uinput_abs_setup=28).
//!
//! Scope: core pad only (buttons + dpad + 2 sticks + 2 analog triggers), always
//! Xbox layout. Rumble/motion/touchpad/battery are deferred no-ops. See
//! `~/myscripts/FreeBSD/moonshine/GAMEPAD-FREEBSD.md` for the full study.

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use super::{GamepadBattery, GamepadInfo, GamepadMotion, GamepadTouch, GamepadUpdate};
use crate::session::stream::control::FeedbackCommand;

// ---------------------------------------------------------------------------
// FreeBSD ioctl encoding (sys/ioccom.h) — 13-bit param length.
// ---------------------------------------------------------------------------

const IOCPARM_SHIFT: u64 = 13;
const IOCPARM_MASK: u64 = (1 << IOCPARM_SHIFT) - 1;
const IOC_VOID: u64 = 0x2000_0000;
const IOC_IN: u64 = 0x8000_0000;

const fn ioc(inout: u64, group: u8, num: u8, len: usize) -> u64 {
	inout | (((len as u64) & IOCPARM_MASK) << 16) | ((group as u64) << 8) | (num as u64)
}
const fn io(group: u8, num: u8) -> u64 {
	ioc(IOC_VOID, group, num, 0)
}
const fn iow<T>(group: u8, num: u8) -> u64 {
	ioc(IOC_IN, group, num, std::mem::size_of::<T>())
}
/// `_IOWINT` — IOC_VOID but sized as int; arg passed by value.
const fn iowint(group: u8, num: u8) -> u64 {
	ioc(IOC_VOID, group, num, std::mem::size_of::<libc::c_int>())
}

const UINPUT_IOCTL_BASE: u8 = b'U';

const UI_DEV_CREATE: u64 = io(UINPUT_IOCTL_BASE, 1);
const UI_DEV_DESTROY: u64 = io(UINPUT_IOCTL_BASE, 2);
const UI_DEV_SETUP: u64 = iow::<UinputSetup>(UINPUT_IOCTL_BASE, 3);
const UI_ABS_SETUP: u64 = iow::<UinputAbsSetup>(UINPUT_IOCTL_BASE, 4);
const UI_SET_EVBIT: u64 = iowint(UINPUT_IOCTL_BASE, 100);
const UI_SET_KEYBIT: u64 = iowint(UINPUT_IOCTL_BASE, 101);
const UI_SET_ABSBIT: u64 = iowint(UINPUT_IOCTL_BASE, 103);

// ---------------------------------------------------------------------------
// evdev event codes (input-event-codes.h) — identical to Linux values.
// ---------------------------------------------------------------------------

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;

const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

const BUS_USB: u16 = 0x03;

/// Moonlight gamepad button flags (see inputtino `include/inputtino/input.h`).
const DPAD_UP: u32 = 0x0001;
const DPAD_DOWN: u32 = 0x0002;
const DPAD_LEFT: u32 = 0x0004;
const DPAD_RIGHT: u32 = 0x0008;
const FLAG_START: u32 = 0x0010;
const FLAG_BACK: u32 = 0x0020;
const FLAG_LEFT_STICK: u32 = 0x0040;
const FLAG_RIGHT_STICK: u32 = 0x0080;
const FLAG_LEFT_BUTTON: u32 = 0x0100;
const FLAG_RIGHT_BUTTON: u32 = 0x0200;
const FLAG_HOME: u32 = 0x0400;
const FLAG_A: u32 = 0x1000;
const FLAG_B: u32 = 0x2000;
const FLAG_X: u32 = 0x4000;
const FLAG_Y: u32 = 0x8000;

/// (Moonlight flag, evdev BTN_* code) pairs, applied in `set_pressed`.
const BUTTON_MAP: &[(u32, u16)] = &[
	(FLAG_START, BTN_START),
	(FLAG_BACK, BTN_SELECT),
	(FLAG_LEFT_STICK, BTN_THUMBL),
	(FLAG_RIGHT_STICK, BTN_THUMBR),
	(FLAG_LEFT_BUTTON, BTN_TL),
	(FLAG_RIGHT_BUTTON, BTN_TR),
	(FLAG_HOME, BTN_MODE),
	(FLAG_A, BTN_SOUTH),
	(FLAG_B, BTN_EAST),
	(FLAG_X, BTN_NORTH),
	(FLAG_Y, BTN_WEST),
];

// ---------------------------------------------------------------------------
// ABI structs (dev/evdev/input.h, dev/evdev/uinput.h). #[repr(C)], amd64 LE.
// ---------------------------------------------------------------------------

#[repr(C)]
struct InputId {
	bustype: u16,
	vendor: u16,
	product: u16,
	version: u16,
}

#[repr(C)]
struct InputAbsinfo {
	value: i32,
	minimum: i32,
	maximum: i32,
	fuzz: i32,
	flat: i32,
	resolution: i32,
}

#[repr(C)]
struct UinputSetup {
	id: InputId,
	name: [libc::c_char; 80], // UINPUT_MAX_NAME_SIZE
	ff_effects_max: u32,
}

#[repr(C)]
struct UinputAbsSetup {
	code: u16,
	// 2 bytes tail padding before the i32-aligned absinfo — #[repr(C)] inserts it.
	absinfo: InputAbsinfo,
}

/// Matches `struct input_event`: `struct timeval` (tv_sec/tv_usec both i64 on
/// amd64) + type + code + value. We always emit a zero timeval; the kernel
/// timestamps events itself.
#[repr(C)]
struct InputEvent {
	tv_sec: i64,
	tv_usec: i64,
	type_: u16,
	code: u16,
	value: i32,
}

const _: () = {
	assert!(std::mem::size_of::<InputEvent>() == 24);
	assert!(std::mem::size_of::<InputId>() == 8);
	assert!(std::mem::size_of::<InputAbsinfo>() == 24);
	assert!(std::mem::size_of::<UinputSetup>() == 92);
	assert!(std::mem::size_of::<UinputAbsSetup>() == 28);
};

// ---------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------

pub(crate) struct Gamepad {
	fd: RawFd,
	index: u8,
	/// Bitmask of buttons currently reported pressed, to emit only changed keys.
	pressed: u32,
	/// Warn at most once per unsupported-feature class (motion/touch/battery).
	warned: AtomicBool,
}

impl Gamepad {
	pub async fn new(info: &GamepadInfo, _feedback_tx: mpsc::Sender<FeedbackCommand>) -> Result<Self, ()> {
		// SAFETY: standard libc open of a device node.
		let fd = unsafe { libc::open(c"/dev/uinput".as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
		if fd < 0 {
			let err = std::io::Error::last_os_error();
			tracing::warn!(
				"Failed to open /dev/uinput for gamepad {}: {err}. \
				 Is the device accessible to this user? (crw------- root:wheel by default)",
				info.index
			);
			return Err(());
		}

		let mut gamepad = Self {
			fd,
			index: info.index,
			pressed: 0,
			warned: AtomicBool::new(false),
		};

		if gamepad.setup().is_err() {
			let err = std::io::Error::last_os_error();
			tracing::warn!("Failed to configure virtual gamepad {}: {err}.", info.index);
			// Drop closes the fd; the device was never created so no destroy needed,
			// but UI_DEV_DESTROY on a non-created device is harmless.
			return Err(());
		}

		tracing::info!("Created virtual Xbox gamepad {} via /dev/uinput.", info.index);
		Ok(gamepad)
	}

	/// Register capabilities, absinfo, identity and create the device.
	fn setup(&mut self) -> Result<(), ()> {
		self.set_bit(UI_SET_EVBIT, EV_KEY)?;
		self.set_bit(UI_SET_EVBIT, EV_ABS)?;
		self.set_bit(UI_SET_EVBIT, EV_SYN)?;

		for (_, code) in BUTTON_MAP {
			self.set_bit(UI_SET_KEYBIT, *code)?;
		}

		// Dpad hats: -1..1.
		for code in [ABS_HAT0X, ABS_HAT0Y] {
			self.set_bit(UI_SET_ABSBIT, code)?;
			self.abs_setup(code, -1, 1, 0, 0)?;
		}
		// Sticks: full i16 range, fuzz 16, flat 128.
		for code in [ABS_X, ABS_Y, ABS_RX, ABS_RY] {
			self.set_bit(UI_SET_ABSBIT, code)?;
			self.abs_setup(code, -32768, 32767, 16, 128)?;
		}
		// Triggers: 0..255.
		for code in [ABS_Z, ABS_RZ] {
			self.set_bit(UI_SET_ABSBIT, code)?;
			self.abs_setup(code, 0, 255, 0, 0)?;
		}

		let mut name = [0 as libc::c_char; 80];
		for (dst, src) in name.iter_mut().zip(b"Moonshine XOne controller".iter()) {
			*dst = *src as libc::c_char;
		}
		let setup = UinputSetup {
			id: InputId {
				bustype: BUS_USB,
				vendor: 0x045e,
				product: 0x02dd,
				version: 0x0100,
			},
			name,
			ff_effects_max: 0,
		};
		// SAFETY: UI_DEV_SETUP takes a *const uinput_setup.
		if unsafe { libc::ioctl(self.fd, UI_DEV_SETUP as _, &setup) } < 0 {
			return Err(());
		}
		// SAFETY: UI_DEV_CREATE takes no argument.
		if unsafe { libc::ioctl(self.fd, UI_DEV_CREATE as _) } < 0 {
			return Err(());
		}
		Ok(())
	}

	/// `_IOWINT` bit-setter: the code is passed as an int by value.
	fn set_bit(&self, request: u64, code: u16) -> Result<(), ()> {
		// SAFETY: request is an IOC_VOID/int-sized ioctl; arg is a c_int by value.
		let r = unsafe { libc::ioctl(self.fd, request as _, code as libc::c_int) };
		if r < 0 { Err(()) } else { Ok(()) }
	}

	fn abs_setup(&self, code: u16, min: i32, max: i32, fuzz: i32, flat: i32) -> Result<(), ()> {
		let setup = UinputAbsSetup {
			code,
			absinfo: InputAbsinfo {
				value: 0,
				minimum: min,
				maximum: max,
				fuzz,
				flat,
				resolution: 0,
			},
		};
		// SAFETY: UI_ABS_SETUP takes a *const uinput_abs_setup.
		let r = unsafe { libc::ioctl(self.fd, UI_ABS_SETUP as _, &setup) };
		if r < 0 { Err(()) } else { Ok(()) }
	}

	/// Write one input_event (kernel fills the timestamp).
	fn emit(&self, type_: u16, code: u16, value: i32) {
		let ev = InputEvent { tv_sec: 0, tv_usec: 0, type_, code, value };
		// SAFETY: write of a POD struct to the uinput fd; short writes shouldn't
		// happen for a single 24-byte record, but we ignore the count either way.
		let n = unsafe {
			libc::write(
				self.fd,
				&ev as *const InputEvent as *const libc::c_void,
				std::mem::size_of::<InputEvent>(),
			)
		};
		if n < 0 {
			tracing::debug!("uinput write failed for gamepad {}: {}", self.index, std::io::Error::last_os_error());
		}
	}

	fn syn(&self) {
		self.emit(EV_SYN, SYN_REPORT, 0);
	}

	fn warn_once(&self, what: &str) {
		if !self.warned.swap(true, Ordering::Relaxed) {
			tracing::warn!("Gamepad {} {} not supported by the FreeBSD backend (core pad only).", self.index, what);
		}
	}

	/// Apply button flags: emit key events for buttons whose state changed, and
	/// the dpad hats, then a SYN.
	pub fn set_pressed(&mut self, button_flags: u32) {
		let changed = button_flags ^ self.pressed;

		for (flag, code) in BUTTON_MAP {
			if changed & flag != 0 {
				self.emit(EV_KEY, *code, if button_flags & flag != 0 { 1 } else { 0 });
			}
		}

		// Dpad → hats. Only emit when a dpad bit changed.
		if changed & (DPAD_UP | DPAD_DOWN) != 0 {
			let y = if button_flags & DPAD_UP != 0 {
				-1
			} else if button_flags & DPAD_DOWN != 0 {
				1
			} else {
				0
			};
			self.emit(EV_ABS, ABS_HAT0Y, y);
		}
		if changed & (DPAD_LEFT | DPAD_RIGHT) != 0 {
			let x = if button_flags & DPAD_LEFT != 0 {
				-1
			} else if button_flags & DPAD_RIGHT != 0 {
				1
			} else {
				0
			};
			self.emit(EV_ABS, ABS_HAT0X, x);
		}

		self.pressed = button_flags;
		self.syn();
	}

	/// Apply stick and trigger positions, then a SYN. Note the Y inversion:
	/// evdev "up" is negative, Moonlight sends up as positive.
	pub fn apply_update(&self, update: &GamepadUpdate) {
		self.emit(EV_ABS, ABS_X, update.left_stick.0 as i32);
		self.emit(EV_ABS, ABS_Y, -(update.left_stick.1 as i32));
		self.emit(EV_ABS, ABS_RX, update.right_stick.0 as i32);
		self.emit(EV_ABS, ABS_RY, -(update.right_stick.1 as i32));
		self.emit(EV_ABS, ABS_Z, update.left_trigger as i32);
		self.emit(EV_ABS, ABS_RZ, update.right_trigger as i32);
		self.syn();
	}

	pub fn touch(&mut self, _touch: &GamepadTouch) {
		self.warn_once("touchpad events");
	}

	pub fn set_motion(&self, _motion: &GamepadMotion) {
		self.warn_once("motion events");
	}

	pub fn set_battery(&self, _gamepad_battery: &GamepadBattery) {
		self.warn_once("battery reports");
	}
}

impl Drop for Gamepad {
	fn drop(&mut self) {
		if self.fd >= 0 {
			// SAFETY: destroy the virtual device then close the fd.
			unsafe {
				libc::ioctl(self.fd, UI_DEV_DESTROY as _);
				libc::close(self.fd);
			}
		}
	}
}
