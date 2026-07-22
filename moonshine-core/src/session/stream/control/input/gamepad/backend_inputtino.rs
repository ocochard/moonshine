//! Linux-only gamepad backend: injects events into `/dev/uinput` and
//! `/dev/uhid` via the `inputtino` crate. Requires `linux/input.h` UAPI
//! headers at build time and kernel modules at runtime.

use inputtino::{
	BatteryState as InputtinoBatterState, DeviceDefinition, Joypad, JoypadMotionType, JoypadStickPosition, PS5Joypad,
	SwitchJoypad, XboxOneJoypad,
};
use tokio::sync::mpsc;

use super::{BatteryState, GamepadBattery, GamepadInfo, GamepadKind, GamepadMotion, GamepadTouch, GamepadUpdate, MotionType};
use crate::session::stream::control::{
	feedback::{EnableMotionEventCommand, RumbleCommand, SetLedCommand, TriggerEffectCommand},
	FeedbackCommand,
};

impl From<MotionType> for JoypadMotionType {
	fn from(m: MotionType) -> Self {
		match m {
			MotionType::Acceleration => JoypadMotionType::ACCELERATION,
			MotionType::Gyroscope => JoypadMotionType::GYROSCOPE,
		}
	}
}

pub(crate) struct Gamepad {
	/// The underlying inputtino joypad, used to inject button presses, stick
	/// positions, triggers, touchpad events, and motion data.
	gamepad: inputtino::Joypad,
}

impl Gamepad {
	pub async fn new(info: &GamepadInfo, feedback_tx: mpsc::Sender<FeedbackCommand>) -> Result<Self, ()> {
		let id = format!("00:11:22:33:{:02x}", info.index);
		let definition = match info.kind {
			GamepadKind::Unknown | GamepadKind::Xbox => DeviceDefinition::new(
				"Moonshine XOne controller",
				0x045e,
				0x02dd,
				0x0100,
				id.as_str(),
				id.as_str(),
			),
			GamepadKind::PlayStation => DeviceDefinition::new(
				"Moonshine PS5 controller",
				0x054C,
				0x0CE6,
				0x8111,
				id.as_str(),
				id.as_str(),
			),
			GamepadKind::Nintendo => DeviceDefinition::new(
				"Moonshine Switch controller",
				0x057e,
				0x2009,
				0x8111,
				id.as_str(),
				id.as_str(),
			),
		};

		let mut gamepad = match info.kind {
			GamepadKind::Unknown | GamepadKind::Xbox => Joypad::XboxOne(
				XboxOneJoypad::new(&definition).map_err(|e| tracing::warn!("Failed to create gamepad: {e}"))?,
			),
			GamepadKind::PlayStation => {
				let mut gamepad =
					PS5Joypad::new(&definition).map_err(|e| tracing::warn!("Failed to create gamepad: {e}"))?;

				gamepad.set_on_led({
					let feedback_tx = feedback_tx.clone();
					let index = info.index;
					move |r, g, b| {
						let _ = feedback_tx.blocking_send(FeedbackCommand::SetLed(SetLedCommand {
							id: index as u16,
							rgb: (r as u8, g as u8, b as u8),
						}));
					}
				});

				gamepad.set_on_trigger_effect({
					let feedback_tx = feedback_tx.clone();
					let index = info.index;
					move |trigger_event_flags, type_left, type_right, left, right| {
						let left: &[u8; 10] = if let Ok(left) = left.try_into() {
							left
						} else {
							tracing::warn!("Couldn't convert left trigger effect.");
							return;
						};

						let right: &[u8; 10] = if let Ok(right) = right.try_into() {
							right
						} else {
							tracing::warn!("Couldn't convert right trigger effect.");
							return;
						};

						let _ = feedback_tx.blocking_send(FeedbackCommand::TriggerEffect(TriggerEffectCommand {
							id: index as u16,
							trigger_event_flags,
							type_left,
							type_right,
							left: left.to_owned(),
							right: right.to_owned(),
						}));
					}
				});

				// Enable gyro and accelerometer events.
				let _ = feedback_tx
					.send(FeedbackCommand::EnableMotionEvent(EnableMotionEventCommand {
						id: info.index as u16,
						report_rate: 100,
						motion_type: JoypadMotionType::ACCELERATION as u8,
					}))
					.await;
				let _ = feedback_tx
					.send(FeedbackCommand::EnableMotionEvent(EnableMotionEventCommand {
						id: info.index as u16,
						report_rate: 100,
						motion_type: JoypadMotionType::GYROSCOPE as u8,
					}))
					.await;

				Joypad::PS5(gamepad)
			},
			GamepadKind::Nintendo => Joypad::Switch(
				SwitchJoypad::new(&definition).map_err(|e| tracing::warn!("Failed to create gamepad: {e}"))?,
			),
		};

		let feedback_tx_for_rumble = feedback_tx.clone();
		gamepad.set_on_rumble({
			let index = info.index;
			move |low_frequency, high_frequency| {
				let _ = feedback_tx_for_rumble.blocking_send(FeedbackCommand::Rumble(RumbleCommand {
					id: index as u16,
					low_frequency: low_frequency as u16,
					high_frequency: high_frequency as u16,
				}));
			}
		});

		Ok(Self { gamepad })
	}

	/// Apply button flags to the gamepad.
	pub fn set_pressed(&self, button_flags: u32) {
		self.gamepad.set_pressed(button_flags as i32);
	}

	/// Apply a gamepad update (sticks, triggers) to the device.
	pub fn apply_update(&self, update: &GamepadUpdate) {
		self.gamepad
			.set_stick(JoypadStickPosition::LS, update.left_stick.0, update.left_stick.1);
		self.gamepad
			.set_stick(JoypadStickPosition::RS, update.right_stick.0, update.right_stick.1);
		self.gamepad
			.set_triggers(update.left_trigger as i16, update.right_trigger as i16);
	}

	pub fn touch(&mut self, touch: &GamepadTouch) {
		if let Joypad::PS5(gamepad) = &self.gamepad {
			if touch.pressure > 0.5 {
				gamepad.place_finger(
					touch.pointer_id,
					(touch.x * PS5Joypad::TOUCHPAD_WIDTH as f32) as u16,
					(touch.y * PS5Joypad::TOUCHPAD_HEIGHT as f32) as u16,
				);
			} else {
				gamepad.release_finger(touch.pointer_id);
			}
		}
	}

	pub fn set_motion(&self, motion: &GamepadMotion) {
		if let Joypad::PS5(gamepad) = &self.gamepad {
			gamepad.set_motion(
				motion.motion_type.into(),
				motion.x.to_radians(),
				motion.y.to_radians(),
				motion.z.to_radians(),
			);
		}
	}

	pub fn set_battery(&self, gamepad_battery: &GamepadBattery) {
		if let Joypad::PS5(gamepad) = &self.gamepad {
			let state = match gamepad_battery.battery_state {
				BatteryState::Discharging => InputtinoBatterState::BATTERY_DISCHARGING,
				BatteryState::Charging => InputtinoBatterState::BATTERY_CHARGHING,
				BatteryState::Full => InputtinoBatterState::BATTERY_FULL,
				BatteryState::NotPresent => return,
				BatteryState::NotCharging => return,
				BatteryState::Unknown => return,
				_ => {
					tracing::warn!("Unknown battery state: {:?}", gamepad_battery.battery_state);
					return;
				},
			};

			gamepad.set_battery(state, gamepad_battery.battery_percentage);
		}
	}
}
