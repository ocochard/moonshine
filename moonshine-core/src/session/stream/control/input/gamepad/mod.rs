use serde::{Deserialize, Serialize};
use strum_macros::FromRepr;

#[cfg(target_os = "linux")]
mod backend_inputtino;
#[cfg(target_os = "linux")]
pub(crate) use backend_inputtino::Gamepad;

#[cfg(not(target_os = "linux"))]
mod backend_stub;
#[cfg(not(target_os = "linux"))]
pub(crate) use backend_stub::Gamepad;

/// Motion sensor kind reported by the client. Values match the wire protocol
/// (see `GamepadMotion::from_bytes`) and the LiUsbHidGamepadMotionType constants
/// used by Moonlight. Kept independent of the input backend so the wire parser
/// stays portable.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum MotionType {
	Acceleration = 1,
	Gyroscope = 2,
}

/// Configuration for the hold-to-Home gamepad button remap.
///
/// When enabled, holding the Back/Select button for `hold_ms` emits the
/// Home/Guide button instead. A short tap (released early) still sends Back.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeButtonConfig {
	/// How long (in milliseconds) the Back button must be held before the
	/// Home/Guide button is emitted instead. While held, the Back button is
	/// withheld; a short tap (released before this duration) still emits Back.
	/// Set to 0 to disable the remap entirely (the default).
	pub hold_ms: u64,

	/// Duration (in milliseconds) of the tactile rumble pulse fired when
	/// hold-to-Home activates. Set to 0 to disable the rumble pulse.
	pub rumble_duration_ms: u64,

	/// Rumble intensity for the hold-to-Home activation pulse (0.0-1.0).
	/// 0.0 means no rumble; 1.0 is maximum intensity.
	pub rumble_intensity: f64,

	/// Suppress the physical Home/Guide button from the client gamepad.
	/// When enabled, an actual Home press from the client is dropped so it
	/// doesn't trigger the host's overlay (Steam, desktop, etc.). The
	/// hold-to-Home remap-generated Home is unaffected.
	pub suppress_home: bool,
}

impl Default for HomeButtonConfig {
	fn default() -> Self {
		Self {
			hold_ms: 0,
			rumble_duration_ms: 50,
			rumble_intensity: 0.5,
			suppress_home: false,
		}
	}
}

/// Configuration for gamepad input handling.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GamepadConfig {
	/// Configuration for the hold-to-Home button remap.
	pub home_button: HomeButtonConfig,
}

#[derive(Debug, FromRepr)]
#[repr(u8)]
pub(crate) enum GamepadKind {
	Unknown = 0x00,
	Xbox = 0x01,
	PlayStation = 0x02,
	Nintendo = 0x03,
}

#[derive(Copy, Clone, Debug)]
#[repr(u16)]
enum GamepadCapability {
	/// Reports values between 0x00 and 0xFF for trigger axes.
	_AnalogTriggers = 0x01,

	/// Can rumble.
	_Rumble = 0x02,

	/// Can rumble triggers.
	_TriggerRumble = 0x04,

	/// Reports touchpad events.
	_Touchpad = 0x08,

	/// Can report accelerometer events.
	_Acceleration = 0x10,

	/// Can report gyroscope events.
	_Gyro = 0x20,

	/// Reports battery state.
	_BatteryState = 0x40,

	// Can set RGB LED state.
	_RgbLed = 0x80,
}

#[derive(Debug)]
pub(crate) struct GamepadInfo {
	pub index: u8,
	pub(super) kind: GamepadKind,
	capabilities: u16,
	_supported_buttons: u32,
}

impl GamepadInfo {
	pub fn from_bytes(buffer: &[u8]) -> Result<Self, ()> {
		const EXPECTED_SIZE: usize =
			std::mem::size_of::<u8>()    // index
			+ std::mem::size_of::<u8>()  // kind
			+ std::mem::size_of::<u16>() // capabilities
			+ std::mem::size_of::<u32>() // supported_buttons
		;

		if buffer.len() < EXPECTED_SIZE {
			tracing::warn!(
				"Expected at least {EXPECTED_SIZE} bytes for GamepadInfo, got {} bytes.",
				buffer.len()
			);
			return Err(());
		}

		Ok(Self {
			index: buffer[0],
			kind: GamepadKind::from_repr(buffer[1])
				.ok_or_else(|| tracing::warn!("Unknown gamepad kind: {}", buffer[1]))?,
			capabilities: u16::from_le_bytes(buffer[2..4].try_into().unwrap()),
			_supported_buttons: u32::from_le_bytes(buffer[4..8].try_into().unwrap()),
		})
	}

	pub fn default_for_index(index: u8) -> Self {
		Self {
			index,
			kind: GamepadKind::Unknown,
			capabilities: 0,
			_supported_buttons: 0,
		}
	}

	#[allow(dead_code)]
	fn has_capability(&self, capability: &GamepadCapability) -> bool {
		(self.capabilities & *capability as u16) != 0
	}
}

#[derive(Debug)]
pub(crate) struct GamepadTouch {
	pub index: u8,
	_event_type: u8,
	// zero: [u8; 2], // Alignment/reserved
	pub(super) pointer_id: u32,
	pub x: f32,
	pub y: f32,
	pub pressure: f32,
}

impl GamepadTouch {
	pub fn from_bytes(buffer: &[u8]) -> Result<Self, ()> {
		const EXPECTED_SIZE: usize =
			std::mem::size_of::<u8>()    // index
			+ std::mem::size_of::<u8>()  // event_type
			+ std::mem::size_of::<u16>() // zero
			+ std::mem::size_of::<u32>() // pointer_id
			+ std::mem::size_of::<f32>() // x
			+ std::mem::size_of::<f32>() // y
			+ std::mem::size_of::<f32>() // pressure
		;

		if buffer.len() < EXPECTED_SIZE {
			tracing::warn!(
				"Expected at least {EXPECTED_SIZE} bytes for GamepadTouch, got {} bytes.",
				buffer.len()
			);
			return Err(());
		}

		Ok(Self {
			index: buffer[0],
			_event_type: buffer[1],
			// zero: u16::from_le_bytes(buffer[2..4].try_into().unwrap()),
			pointer_id: u32::from_le_bytes(buffer[4..8].try_into().unwrap()),
			x: f32::from_le_bytes(buffer[8..12].try_into().unwrap()).clamp(0.0, 1.0),
			y: f32::from_le_bytes(buffer[12..16].try_into().unwrap()).clamp(0.0, 1.0),
			pressure: f32::from_le_bytes(buffer[16..20].try_into().unwrap()).clamp(0.0, 1.0),
		})
	}
}

#[derive(Debug)]
pub(crate) struct GamepadUpdate {
	pub index: u16,
	pub active_gamepad_mask: u16,
	button_flags: u32,
	pub(super) left_trigger: u8,
	pub(super) right_trigger: u8,
	pub(super) left_stick: (i16, i16),
	pub(super) right_stick: (i16, i16),
}

impl GamepadUpdate {
	pub fn from_bytes(buffer: &[u8]) -> Result<Self, ()> {
		const EXPECTED_SIZE: usize =
			std::mem::size_of::<u16>()   // header
			+ std::mem::size_of::<u16>() // index
			+ std::mem::size_of::<u16>() // active gamepad mask
			+ std::mem::size_of::<u16>() // mid B
			+ std::mem::size_of::<u16>() // button flags
			+ std::mem::size_of::<u8>()  // left trigger
			+ std::mem::size_of::<u8>()  // right trigger
			+ std::mem::size_of::<i16>() // left stick x
			+ std::mem::size_of::<i16>() // left stick y
			+ std::mem::size_of::<i16>() // right stick x
			+ std::mem::size_of::<i16>() // right stick y
			+ std::mem::size_of::<i16>() // tail a
			+ std::mem::size_of::<i16>() // button flags 2
			+ std::mem::size_of::<i16>() // tail b
		;

		if buffer.len() < EXPECTED_SIZE {
			tracing::warn!(
				"Expected at least {EXPECTED_SIZE} bytes for GamepadUpdate, got {} bytes.",
				buffer.len()
			);
			return Err(());
		}

		Ok(Self {
			index: u16::from_le_bytes(buffer[2..4].try_into().unwrap()),
			active_gamepad_mask: u16::from_le_bytes(buffer[4..6].try_into().unwrap()),
			button_flags: u16::from_le_bytes(buffer[8..10].try_into().unwrap()) as u32
				| (u16::from_le_bytes(buffer[22..24].try_into().unwrap()) as u32) << 16,
			left_trigger: buffer[10],
			right_trigger: buffer[11],
			left_stick: (
				i16::from_le_bytes(buffer[12..14].try_into().unwrap()),
				i16::from_le_bytes(buffer[14..16].try_into().unwrap()),
			),
			right_stick: (
				i16::from_le_bytes(buffer[16..18].try_into().unwrap()),
				i16::from_le_bytes(buffer[18..20].try_into().unwrap()),
			),
		})
	}

	pub fn button_flags(&self) -> u32 {
		self.button_flags
	}
}

#[derive(Debug)]
pub(crate) struct GamepadMotion {
	pub index: u8,
	pub(crate) motion_type: MotionType,
	// zero: [u8; 2], // Alignment/reserved
	pub(crate) x: f32,
	pub(crate) y: f32,
	pub(crate) z: f32,
}

impl GamepadMotion {
	pub fn from_bytes(buffer: &[u8]) -> Result<Self, ()> {
		const EXPECTED_SIZE: usize =
			std::mem::size_of::<u8>() // index
			+ std::mem::size_of::<u8>() // motion type
			+ std::mem::size_of::<u16>() // alignment/reserved
			+ std::mem::size_of::<f32>() // x
			+ std::mem::size_of::<f32>() // y
			+ std::mem::size_of::<f32>() // z
		;

		if buffer.len() < EXPECTED_SIZE {
			tracing::warn!(
				"Expected at least {EXPECTED_SIZE} bytes for GamepadMotion, got {} bytes.",
				buffer.len()
			);
			return Err(());
		}

		Ok(Self {
			index: buffer[0],
			motion_type: match buffer[1] {
				1 => MotionType::Acceleration,
				2 => MotionType::Gyroscope,
				_ => {
					tracing::warn!("Unknown gamepad motion type: {}", buffer[1]);
					return Err(());
				},
			},
			// zero: u16::from_le_bytes(buffer[2..4].try_into().unwrap()),
			x: f32::from_le_bytes(buffer[4..8].try_into().unwrap()),
			y: f32::from_le_bytes(buffer[8..12].try_into().unwrap()),
			z: f32::from_le_bytes(buffer[12..16].try_into().unwrap()),
		})
	}
}

#[derive(Debug, FromRepr)]
#[repr(u8)]
pub(super) enum BatteryState {
	Unknown = 0x00,
	NotPresent = 0x01,
	Discharging = 0x02,
	Charging = 0x03,
	NotCharging = 0x04,
	Full = 0x05,
	PercentageUnknown = 0xFF,
}

#[derive(Debug)]
pub(crate) struct GamepadBattery {
	pub index: u8,
	pub(super) battery_state: BatteryState,
	pub(super) battery_percentage: u8,
}

impl GamepadBattery {
	pub fn from_bytes(buffer: &[u8]) -> Result<Self, ()> {
		const EXPECTED_SIZE: usize =
			std::mem::size_of::<u8>() // index
			+ std::mem::size_of::<u8>() // battery state
			+ std::mem::size_of::<u8>() // battery percentage
			+ std::mem::size_of::<u8>() // padding
		;

		if buffer.len() < EXPECTED_SIZE {
			tracing::warn!(
				"Expected at least {EXPECTED_SIZE} bytes for GamepadBattery, got {} bytes.",
				buffer.len()
			);
			return Err(());
		}

		Ok(Self {
			index: buffer[0],
			battery_state: BatteryState::from_repr(buffer[1])
				.ok_or_else(|| tracing::warn!("Unknown battery state: {}", buffer[1]))?,
			battery_percentage: buffer[2],
		})
	}
}

// `Gamepad` lives in the backend submodule (`backend_inputtino` on Linux,
// `backend_stub` elsewhere). See the `use` at the top of this file.
