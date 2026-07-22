//! No-op gamepad backend for non-Linux targets (FreeBSD, macOS, …).
//!
//! `inputtino` wraps Linux uinput/uhid and #includes the `linux/input.h`
//! UAPI headers, which don't exist on BSDs. This stub keeps the wire
//! parser and dispatch logic building; virtual-gamepad injection needs a
//! platform-specific replacement (evdev+libwacom analogue, or a per-BSD
//! injector). Until then, gamepad events are dropped with a warning.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use super::{GamepadBattery, GamepadInfo, GamepadMotion, GamepadTouch, GamepadUpdate};
use crate::session::stream::control::FeedbackCommand;

pub(crate) struct Gamepad {
	index: u8,
	/// Log the "not supported" warning at most once per instance, so an
	/// active session doesn't flood the log at every update tick.
	warned: AtomicBool,
}

impl Gamepad {
	pub async fn new(info: &GamepadInfo, _feedback_tx: mpsc::Sender<FeedbackCommand>) -> Result<Self, ()> {
		tracing::warn!(
			"Gamepad {} requested, but virtual-gamepad injection is not implemented on this platform; \
			 events will be dropped.",
			info.index
		);
		Ok(Self { index: info.index, warned: AtomicBool::new(true) })
	}

	fn warn_once(&self, what: &str) {
		if !self.warned.swap(true, Ordering::Relaxed) {
			tracing::warn!("Dropping gamepad {} {}: no virtual-gamepad backend on this platform.", self.index, what);
		}
	}

	pub fn set_pressed(&self, _button_flags: u32) {
		self.warn_once("button state");
	}

	pub fn apply_update(&self, _update: &GamepadUpdate) {
		self.warn_once("stick/trigger update");
	}

	pub fn touch(&mut self, _touch: &GamepadTouch) {
		self.warn_once("touchpad event");
	}

	pub fn set_motion(&self, _motion: &GamepadMotion) {
		self.warn_once("motion event");
	}

	pub fn set_battery(&self, _gamepad_battery: &GamepadBattery) {
		self.warn_once("battery report");
	}
}
