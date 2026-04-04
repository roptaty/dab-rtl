pub mod decode;
mod fdk;
pub use decode::{firecode_check, DabPlusDecoder};
#[cfg(feature = "mp2")]
pub use decode::{decode_mp2, Mp2Decoder};

/// Audio output via cpal (ALSA or PulseAudio on Linux).
///
/// A ring-buffer of f32 PCM samples is shared between the caller (writer)
/// and the cpal stream callback (reader).  Samples are interleaved if stereo.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Initial PCM ring capacity in samples.
const PCM_RING_INITIAL_CAPACITY: usize = 48_000;

struct PcmRingBuffer {
    data: Vec<f32>,
    read_pos: usize,
    len: usize,
}

impl PcmRingBuffer {
    fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            data: vec![0.0; cap],
            read_pos: 0,
            len: 0,
        }
    }

    fn capacity(&self) -> usize {
        self.data.len()
    }

    fn write_slice(&mut self, input: &[f32]) {
        self.ensure_capacity(self.len + input.len());
        let write_pos = (self.read_pos + self.len) % self.capacity();
        let first = input.len().min(self.capacity() - write_pos);
        self.data[write_pos..write_pos + first].copy_from_slice(&input[..first]);
        let remaining = input.len() - first;
        if remaining > 0 {
            self.data[..remaining].copy_from_slice(&input[first..]);
        }
        self.len += input.len();
    }

    fn read_into_f32(&mut self, out: &mut [f32]) -> usize {
        let count = self.len.min(out.len());
        if count == 0 {
            return 0;
        }
        let first = count.min(self.capacity() - self.read_pos);
        out[..first].copy_from_slice(&self.data[self.read_pos..self.read_pos + first]);
        let remaining = count - first;
        if remaining > 0 {
            out[first..count].copy_from_slice(&self.data[..remaining]);
        }
        self.consume(count);
        count
    }

    fn read_into_i16(&mut self, out: &mut [i16]) -> usize {
        let count = self.len.min(out.len());
        if count == 0 {
            return 0;
        }
        let first = count.min(self.capacity() - self.read_pos);
        for (dst, src) in out[..first]
            .iter_mut()
            .zip(self.data[self.read_pos..self.read_pos + first].iter())
        {
            *dst = (src.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        }
        let remaining = count - first;
        if remaining > 0 {
            for (dst, src) in out[first..count].iter_mut().zip(self.data[..remaining].iter()) {
                *dst = (src.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            }
        }
        self.consume(count);
        count
    }

    fn ensure_capacity(&mut self, required: usize) {
        if required <= self.capacity() {
            return;
        }
        let new_capacity = required.next_power_of_two();
        let mut new_data = vec![0.0; new_capacity];
        let first = self.len.min(self.capacity() - self.read_pos);
        new_data[..first].copy_from_slice(&self.data[self.read_pos..self.read_pos + first]);
        let remaining = self.len - first;
        if remaining > 0 {
            new_data[first..self.len].copy_from_slice(&self.data[..remaining]);
        }
        self.data = new_data;
        self.read_pos = 0;
    }

    fn consume(&mut self, count: usize) {
        debug_assert!(count <= self.len);
        self.read_pos = (self.read_pos + count) % self.capacity();
        self.len -= count;
        if self.len == 0 {
            self.read_pos = 0;
        }
    }
}

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
    buf: Arc<Mutex<PcmRingBuffer>>,
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
        let buf: Arc<Mutex<PcmRingBuffer>> =
            Arc::new(Mutex::new(PcmRingBuffer::new(PCM_RING_INITIAL_CAPACITY)));
        let buf_reader = Arc::clone(&buf);

        let stream = match sample_format {
            cpal::SampleFormat::I16 => {
                let buf_r = buf_reader;
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            let mut guard = buf_r.lock().unwrap();
                            let available = guard.read_into_i16(data);
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
                            let available = guard.read_into_f32(data);
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
                            let available = guard.read_into_f32(data);
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
        guard.write_slice(samples);
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

    #[test]
    fn pcm_ring_buffer_round_trips_without_reordering() {
        let mut buf = PcmRingBuffer::new(4);
        buf.write_slice(&[1.0, 2.0, 3.0]);
        let mut out = [0.0; 2];
        assert_eq!(buf.read_into_f32(&mut out), 2);
        assert_eq!(out, [1.0, 2.0]);

        buf.write_slice(&[4.0, 5.0, 6.0]);
        let mut out = [0.0; 4];
        assert_eq!(buf.read_into_f32(&mut out), 4);
        assert_eq!(out, [3.0, 4.0, 5.0, 6.0]);
        assert_eq!(buf.len, 0);
    }

    #[test]
    fn pcm_ring_buffer_grows_and_preserves_contents() {
        let mut buf = PcmRingBuffer::new(2);
        buf.write_slice(&[0.1, 0.2, 0.3, 0.4, 0.5]);
        assert!(buf.capacity() >= 5);

        let mut out = [0.0; 5];
        assert_eq!(buf.read_into_f32(&mut out), 5);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4, 0.5]);
    }
}
