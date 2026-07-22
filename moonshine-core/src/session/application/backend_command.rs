//! Portable Command-based application backend for non-Linux targets.
//!
//! The Linux backend launches the app as a transient systemd service via D-Bus.
//! On FreeBSD (and any other non-Linux Unix) systemd is unavailable, so we
//! spawn the app directly with `tokio::process::Command` and mirror the
//! ExecStartPre / ExecStopPost semantics with plain sequential invocations.

use std::fs::OpenOptions;
use std::process::Stdio;

use async_shutdown::ShutdownManager;
use tokio::process::Command;
use tokio::task::JoinHandle;

use super::{make_envs, ApplicationConfig, ApplicationContext};
use crate::session::manager::SessionShutdownReason;

pub(crate) struct Application {
	config: ApplicationConfig,
	/// Task that owns the tokio `Child`, awaits its exit, and triggers session
	/// shutdown. Aborting the task drops the Child, which triggers
	/// `kill_on_drop` and reaps the process via the tokio runtime.
	exit_monitor: Option<JoinHandle<()>>,
	/// Commands to run after the app exits, mirroring systemd's ExecStopPost.
	post_commands: Vec<Vec<String>>,
}

impl Application {
	pub async fn spawn(
		config: ApplicationConfig,
		context: ApplicationContext,
		stop: ShutdownManager<SessionShutdownReason>,
	) -> Result<Self, ()> {
		let Some(program) = config.command.first() else {
			tracing::error!("Application command is empty.");
			return Err(());
		};
		let args: Vec<String> = config.command[1..].to_vec();
		let envs = make_envs(&context)?;

		tracing::info!(program, ?args, "Launching application.");

		// Run pre_command entries sequentially (systemd ExecStartPre semantics).
		// A non-zero exit code aborts launch.
		for cmd in &config.pre_command {
			let Some(first) = cmd.first() else {
				continue;
			};
			let rest = &cmd[1..];
			tracing::info!(program = first.as_str(), args = ?rest, "Running pre-command.");
			let status = Command::new(first)
				.args(rest)
				.envs(env_pairs(&envs))
				.status()
				.await
				.map_err(|e| tracing::error!("Failed to run pre-command '{first}': {e}"))?;
			if !status.success() {
				tracing::error!(program = first.as_str(), ?status, "Pre-command failed.");
				return Err(());
			}
		}

		// Configure stdout/stderr redirection.
		let stdout = open_output(config.stdout.as_deref())?;
		let stderr = open_output(config.stderr.as_deref())?;

		let mut command = Command::new(program);
		command
			.args(&args)
			.envs(env_pairs(&envs))
			.stdout(stdout)
			.stderr(stderr)
			.kill_on_drop(true);

		let mut child = command
			.spawn()
			.map_err(|e| tracing::error!("Failed to spawn application '{program}': {e}"))?;

		let pid = child.id();
		tracing::info!("Launched application (pid={pid:?})");

		// The monitor task owns the Child. Awaiting `child.wait()` needs
		// `&mut self`, so the Child lives inside the task; on Drop we abort
		// the task which drops the Child, and `kill_on_drop` ensures the
		// runtime reaps the process.
		let stop_for_monitor = stop.clone();
		let title = config.title.clone();
		let exit_monitor = tokio::spawn(async move {
			match child.wait().await {
				Ok(status) => {
					tracing::info!(
						title = %title,
						?status,
						"Application exited; stopping session."
					);
				},
				Err(e) => {
					tracing::warn!(title = %title, "Failed to wait for application: {e}");
				},
			}
			let _ = stop_for_monitor.trigger_shutdown(SessionShutdownReason::ApplicationStopped);
		});

		let post_commands = config.post_command.clone();

		Ok(Self {
			config,
			exit_monitor: Some(exit_monitor),
			post_commands,
		})
	}
}

impl Drop for Application {
	fn drop(&mut self) {
		tracing::info!("Application '{}' is exiting.", self.config.title);

		// Aborting the monitor task drops the Child, which triggers
		// kill_on_drop and reaps the process via the tokio runtime.
		if let Some(handle) = self.exit_monitor.take() {
			handle.abort();
		}

		// Run post_command entries in a helper thread with its own runtime,
		// mirroring the systemd backend's `stop_unit_owned` pattern.
		let post_commands = std::mem::take(&mut self.post_commands);
		if post_commands.is_empty() {
			return;
		}
		std::thread::spawn(move || {
			let rt = match tokio::runtime::Runtime::new() {
				Ok(rt) => rt,
				Err(e) => {
					tracing::error!("Failed to create runtime for post_command execution: {e}");
					return;
				},
			};
			rt.block_on(async move {
				for cmd in &post_commands {
					let Some(first) = cmd.first() else {
						continue;
					};
					let rest = &cmd[1..];
					tracing::info!(program = first.as_str(), args = ?rest, "Running post-command.");
					match Command::new(first).args(rest).status().await {
						Ok(status) if status.success() => {},
						Ok(status) => {
							tracing::warn!(program = first.as_str(), ?status, "Post-command failed.");
						},
						Err(e) => {
							tracing::warn!(program = first.as_str(), "Post-command failed to run: {e}");
						},
					}
				}
			});
		})
		.join()
		.ok();
	}
}

/// Split "KEY=value" strings into (key, value) pairs suitable for `Command::envs`.
/// Entries missing an `=` are dropped with a warning.
fn env_pairs(envs: &[String]) -> Vec<(String, String)> {
	envs.iter()
		.filter_map(|entry| match entry.split_once('=') {
			Some((k, v)) => Some((k.to_string(), v.to_string())),
			None => {
				tracing::warn!("Ignoring malformed env entry (no '='): {entry}");
				None
			},
		})
		.collect()
}

/// Translate the systemd StandardOutput/StandardError string to a `Stdio`.
///
/// Only a small subset of systemd's syntax is honoured on the portable
/// backend: `None` inherits, `Some("null")` discards, `Some("inherit")`
/// inherits explicitly, and any other value is treated as a file path
/// opened with append+create (mirroring `append:PATH` semantics).
fn open_output(value: Option<&str>) -> Result<Stdio, ()> {
	match value {
		None | Some("inherit") => Ok(Stdio::inherit()),
		Some("null") => Ok(Stdio::null()),
		Some(spec) => {
			// Accept "append:PATH", "truncate:PATH", "file:PATH" or a bare path.
			let (mode, path) = match spec.split_once(':') {
				Some(("append", p)) => ("append", p),
				Some(("truncate", p)) => ("truncate", p),
				Some(("file", p)) => ("append", p),
				Some(_) => ("append", spec),
				None => ("append", spec),
			};
			let mut opts = OpenOptions::new();
			opts.create(true).write(true);
			if mode == "truncate" {
				opts.truncate(true);
			} else {
				opts.append(true);
			}
			let file = opts
				.open(path)
				.map_err(|e| tracing::error!("Failed to open output path '{path}': {e}"))?;
			Ok(Stdio::from(file))
		},
	}
}
