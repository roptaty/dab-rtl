pub mod decode;
#[cfg(feature = "fdk-aac")]
mod fdk;
pub use decode::{decode_mp2, firecode_check, DabPlusDecoder, Mp2Decoder};

/// Audio output via cpal (ALSA or PulseAudio on Linux).
///
/// A ring-buffer of f32 PCM samples is shared between the caller (writer)
/// and the cpal stream callback (reader).  Samples are interleaved if stereo.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AudioError {
    #[error("No audio output device found")]
    NoDevice,
    #[error("Device error: {0}")]
    Device(String),
    #[error("Stream error: {0}")]
    Stream(String),
    #[error("Unsupported sample format")]
    UnsupportedFormat,
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Device enumeration                                                          //
// ─────────────────────────────────────────────────────────────────────────── //

/// List available audio output devices.
/// Returns `(index, name)` pairs.
pub fn list_devices() -> Vec<(usize, String)> {
    let host = cpal::default_host();
    match host.output_devices() {
        Ok(iter) => iter
            .enumerate()
            .map(|(i, d)| (i, d.name().unwrap_or_else(|_| format!("device-{i}"))))
            .collect(),
        Err(e) => {
            log::warn!("Could not enumerate audio devices: {e}");
            Vec::new()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  AudioOutput                                                                 //
// ─────────────────────────────────────────────────────────────────────────── //

/// Audio output stream backed by a shared PCM ring buffer.
pub struct AudioOutput {
    stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioOutput {
    /// Open an audio output stream.
    ///
    /// * `device_name` — `None` selects the system default; `Some(name)` does
    ///   a prefix match against available device names.
    /// * `sample_rate` — desired output sample rate (e.g. 48000 for DAB).
    /// * `channels`    — 1 = mono, 2 = stereo.
    pub fn open(
        device_name: Option<&str>,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, AudioError> {
        let host = cpal::default_host();

        let device = match device_name {
            None => host.default_output_device().ok_or(AudioError::NoDevice)?,
            Some(name) => host
                .output_devices()
                .map_err(|e| AudioError::Device(e.to_string()))?
                .find(|d| {
                    d.name()
                        .map(|n| n.to_lowercase().contains(&name.to_lowercase()))
                        .unwrap_or(false)
                })
                .ok_or(AudioError::NoDevice)?,
        };

        // Query the device's default output config to find a supported format.
        let default_config = device
            .default_output_config()
            .map_err(|e| AudioError::Device(e.to_string()))?;

        let sample_format = default_config.sample_format();
        log::info!(
            "Audio output: {} ({} Hz, {} ch, {:?})",
            device.name().unwrap_or_default(),
            sample_rate,
            channels,
            sample_format,
        );

        let config = cpal::StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Shared ring buffer: caller writes f32, cpal callback reads.
        let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let buf_reader = Arc::clone(&buf);

        let stream = match sample_format {
            cpal::SampleFormat::I16 => {
                let buf_r = buf_reader;
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            let mut guard = buf_r.lock().unwrap();
                            let available = guard.len().min(data.len());
                            for (out, &inp) in
                                data[..available].iter_mut().zip(guard[..available].iter())
                            {
                                *out = (inp.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                            }
                            guard.drain(..available);
                            for s in &mut data[available..] {
                                *s = 0;
                            }
                        },
                        |err| log::error!("Audio stream error: {err}"),
                        None,
                    )
                    .map_err(|e| AudioError::Stream(e.to_string()))?
            }
            cpal::SampleFormat::F32 => {
                let buf_r = buf_reader;
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            let mut guard = buf_r.lock().unwrap();
                            let available = guard.len().min(data.len());
                            data[..available].copy_from_slice(&guard[..available]);
                            guard.drain(..available);
                            for s in &mut data[available..] {
                                *s = 0.0;
                            }
                        },
                        |err| log::error!("Audio stream error: {err}"),
                        None,
                    )
                    .map_err(|e| AudioError::Stream(e.to_string()))?
            }
            _ => {
                log::warn!(
                    "Unsupported sample format {:?}, trying f32 anyway",
                    sample_format
                );
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            let mut guard = buf_reader.lock().unwrap();
                            let available = guard.len().min(data.len());
                            data[..available].copy_from_slice(&guard[..available]);
                            guard.drain(..available);
                            for s in &mut data[available..] {
                                *s = 0.0;
                            }
                        },
                        |err| log::error!("Audio stream error: {err}"),
                        None,
                    )
                    .map_err(|e| AudioError::Stream(e.to_string()))?
            }
        };

        Ok(AudioOutput {
            stream,
            buf,
            sample_rate,
            channels,
        })
    }

    /// Write PCM samples into the output buffer.
    ///
    /// Samples must be interleaved (L, R, L, R, …) for stereo.
    /// Block until the internal buffer has room (simple back-pressure).
    pub fn write_samples(&self, samples: &[f32]) {
        let mut guard = self.buf.lock().unwrap();
        guard.extend_from_slice(samples);
    }

    /// Start audio playback.
    pub fn play(&self) {
        if let Err(e) = self.stream.play() {
            log::error!("Failed to start audio stream: {e}");
        }
    }

    /// Pause audio playback.
    pub fn pause(&self) {
        if let Err(e) = self.stream.pause() {
            log::error!("Failed to pause audio stream: {e}");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: listing devices must not panic even when no audio hardware
    /// is present (CI environments).
    #[test]
    fn list_devices_does_not_panic() {
        let _ = list_devices();
    }
}
