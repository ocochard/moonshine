use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub fn default_launch_timeout() -> u64 {
	2
}

/// Configuration for a single application that can be launched in a session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationConfig {
	/// Title of the application.
	pub title: String,

	/// Path to a boxart image.
	pub boxart: Option<PathBuf>,

	/// The command to run.
	pub command: Vec<String>,

	/// Commands to run before launching the application.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pre_command: Vec<Vec<String>>,

	/// Commands to run after the streaming session ends.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub post_command: Vec<Vec<String>>,

	/// systemd StandardOutput value. If not set, defaults to "null".
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stdout: Option<String>,

	/// systemd StandardError value. If not set, defaults to "null".
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stderr: Option<String>,

	/// Seconds to wait for the application to reach an active state after launch.
	#[serde(default = "default_launch_timeout")]
	pub launch_timeout_secs: u64,
}

impl Default for ApplicationConfig {
	fn default() -> Self {
		Self {
			title: String::new(),
			boxart: None,
			command: Vec::new(),
			pre_command: Vec::new(),
			post_command: Vec::new(),
			stdout: None,
			stderr: None,
			launch_timeout_secs: default_launch_timeout(),
		}
	}
}

impl ApplicationConfig {
	pub fn id(&self) -> i32 {
		let mut hasher = DefaultHasher::new();
		self.title.hash(&mut hasher);
		// Clients only accept non-negative application IDs.
		(hasher.finish() as i32) & i32::MAX
	}
}

/// Runtime context required to launch an application.
pub(crate) struct ApplicationContext {
	/// systemd transient unit name (e.g. `"moonshine-session.service"`).
	pub unit_name: String,
	/// Path to the PulseAudio socket created by the audio stream.
	pub pulse_socket_path: PathBuf,
	/// X11 display number reported by XWayland (e.g. `0` → `":0"`).
	pub xdisplay: u32,
	/// Wayland socket name reported by the compositor.
	pub wayland_display: String,
	/// Effective HDR mode — `true` only when the compositor confirmed an HDR-capable DMA-BUF format is in use.
	pub hdr: bool,
	/// Environment variables to pass on.
	pub extra_env: HashMap<String, String>,
}

/// Build environment variables for the application based on the context (e.g. display, PulseAudio socket).
fn make_envs(context: &ApplicationContext) -> Result<Vec<String>, ()> {
	// Build environment variables as "KEY=value" strings for systemd.
	let mut envs: Vec<String> = vec![
		format!("PULSE_SERVER=unix:{}", context.pulse_socket_path.display()),
		format!(
			"PULSE_RUNTIME_PATH={}",
			context
				.pulse_socket_path
				.parent()
				.ok_or_else(|| tracing::error!("Failed to get parent directory of PulseAudio socket."))?
				.to_string_lossy()
		),
		format!("DISPLAY=:{}", context.xdisplay),
		format!("WAYLAND_DISPLAY={}", context.wayland_display),
		format!("MOONSHINE_WAYLAND_DISPLAY={}", context.wayland_display),
		// Activate the moonshine WSI Vulkan layer.
		"ENABLE_MOONSHINE_WSI=1".to_string(),
		// Force Proton to use winepulse.drv instead of winepipewire.drv,
		// so it respects PULSE_SERVER and routes audio through Moonshine.
		"PROTON_USE_PIPEWIRE=0".to_string(),
	];

	// Impersonate gamescope so Steam uses its external-overlay mode.
	envs.push("XDG_CURRENT_DESKTOP=gamescope".to_string());
	envs.push(format!("GAMESCOPE_WAYLAND_DISPLAY={}", context.wayland_display));
	envs.push(format!("STEAM_GAME_DISPLAY_0=:{}", context.xdisplay));
	// Steam keys gamescope features (HDR, VRR, scaling, FPS limit) off these.
	envs.push("STEAM_GAMESCOPE_DYNAMIC_FPSLIMITER=1".to_string());
	envs.push("STEAM_GAMESCOPE_FANCY_SCALING_SUPPORT=1".to_string());
	envs.push("STEAM_GAMESCOPE_NIS_SUPPORTED=1".to_string());
	envs.push("STEAM_GAMESCOPE_VRR_SUPPORTED=1".to_string());
	if context.hdr {
		envs.push("STEAM_GAMESCOPE_HDR_SUPPORTED=1".to_string());
	}

	// Audio backend selection depends on the host:
	// - Linux: apps go through PulseAudio (or Pipewire in Pulse-compat
	//   mode). moonshine hosts its own PulseAudio server on
	//   $XDG_RUNTIME_DIR/moonshine/pulse/native.
	// - FreeBSD: default sdl3 port ships with PULSEAUDIO=off, so
	//   PULSE_SERVER is useless. Instead we route the game to
	//   /dev/dsp.loop, virtual_oss's loopback device (base system
	//   from FreeBSD 13+, already running as the boot-time
	//   virtual_oss service on typical hosts). moonshine's own
	//   audio capture reads PCM back from the same device.
	#[cfg(target_os = "linux")]
	{
		envs.push("SDL_AUDIODRIVER=pulseaudio".to_string());
		envs.push("AUDIODRIVER=pulse".to_string());
	}
	#[cfg(target_os = "freebsd")]
	{
		envs.push("SDL_AUDIODRIVER=dsp".to_string());
		envs.push("SDL_AUDIODEV=/dev/dsp.loop".to_string());
		envs.push("AUDIODEV=/dev/dsp.loop".to_string());
	}

	if context.hdr {
		// DXVK's dxgi.dll gates HDR color space exposure on this env var.
		// Without it, both DX11 (DXVK) and DX12 (vkd3d-proton via DXVK dxgi)
		// games will not see HDR as available.
		envs.push("DXVK_HDR=1".to_string());
		// Signal HDR mode to the moonshine-wsi layer so it can advertise HDR
		// surface formats correctly (the factory global is always present for
		// SDR sessions too, so we need an explicit capability signal).
		envs.push("MOONSHINE_HDR=1".to_string());
	}

	for (key, value) in &context.extra_env {
		envs.push(format!("{key}={value}"));
	}

	Ok(envs)
}

#[cfg(target_os = "linux")]
mod backend_systemd;
#[cfg(target_os = "linux")]
pub(crate) use backend_systemd::Application;

#[cfg(not(target_os = "linux"))]
mod backend_command;
#[cfg(not(target_os = "linux"))]
pub(crate) use backend_command::Application;
