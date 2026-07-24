//! Backend-agnostic audio frame type shared by all capture backends and the
//! Opus encoder.
//!
//! An `AudioFrame` is one tick worth of interleaved f32 PCM samples for the
//! negotiated channel count, ready to be handed to `opus::MSEncoder`. Backends
//! (`pulse_server` on Linux, `oss_capture` on FreeBSD) populate `buf` and
//! `capture_ts_ms`, then send it on the shared `frame_tx` channel; the
//! encoder recycles empty buffers back through `frame_recycle_tx`.

/// Sample rate at which frames are produced. Matches Opus' native 48 kHz.
pub(crate) const CAPTURE_SAMPLE_RATE: u32 = 48000;

/// A buffer of interleaved f32 samples ready for Opus encoding.
pub(crate) struct AudioFrame {
	/// Interleaved f32 samples for the negotiated channel count.
	pub buf: Vec<f32>,

	/// Capture timestamp in milliseconds since process start.
	pub capture_ts_ms: u64,
}
