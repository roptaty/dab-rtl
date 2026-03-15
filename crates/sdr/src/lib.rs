/// RTL-SDR abstraction for DAB reception.
///
/// Wraps `rtlsdr_mt` 2.x to provide a channel-based IQ sample stream and
/// convenience helpers for device enumeration and IQ conversion.
use num_complex::Complex32;
use std::path::Path;
use std::sync::mpsc;
use thiserror::Error;

/// DAB/DAB+ sample rate (2.048 Msps, required by Mode I).
pub const SAMPLE_RATE: u32 = 2_048_000;

/// Gain sentinel meaning "use hardware AGC".
pub const GAIN_AUTO: i32 = -1;

#[derive(Error, Debug)]
pub enum SdrError {
    #[error("No RTL-SDR device found")]
    NoDevice,
    #[error("RTL-SDR device error: {0}")]
    Device(String),
}

// ─────────────────────────────────────────────────────────────────────────── //
//  IQ conversion                                                               //
// ─────────────────────────────────────────────────────────────────────────── //

/// Convert raw RTL-SDR bytes (interleaved u8 I/Q pairs) to `Complex32`.
///
/// The RTL-SDR outputs unsigned 8-bit samples offset by 127.5.
/// This maps [0, 255] → [−1.0, +1.0].
#[inline]
pub fn iq_to_complex(raw: &[u8]) -> Vec<Complex32> {
    raw.chunks_exact(2)
        .map(|c| Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Device enumeration                                                          //
// ─────────────────────────────────────────────────────────────────────────── //

/// List connected RTL-SDR devices.
/// Returns a vector of `(device_index, name)` pairs.
pub fn list_devices() -> Vec<(u32, String)> {
    rtlsdr_mt::devices()
        .enumerate()
        .map(|(i, name)| (i as u32, name.to_string_lossy().into_owned()))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Device configuration                                                        //
// ─────────────────────────────────────────────────────────────────────────── //

/// Configuration for opening an RTL-SDR device.
pub struct DeviceConfig {
    /// Device index (0 = first device).
    pub index: u32,
    /// Tuner centre frequency in Hz.
    pub center_freq_hz: u32,
    /// Gain in tenths of dB, or `GAIN_AUTO` (−1) to enable hardware AGC.
    pub gain: i32,
    /// Crystal frequency correction in PPM.
    pub ppm_correction: i32,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        DeviceConfig {
            index: 0,
            // DAB channel 11C (220.352 MHz) — common in Germany/Netherlands.
            center_freq_hz: 220_352_000,
            gain: GAIN_AUTO,
            ppm_correction: 0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Streaming                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

/// Handle to a running RTL-SDR stream.
///
/// Dropping this handle cancels the async read and waits for the background
/// thread to finish, ensuring the USB device is fully released before the
/// struct goes out of scope.
pub struct SdrStream {
    pub rx: mpsc::Receiver<Vec<Complex32>>,
    ctl: Option<rtlsdr_mt::Controller>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SdrStream {
    fn drop(&mut self) {
        // Cancel the async read so read_async returns promptly.
        if let Some(ref mut ctl) = self.ctl {
            ctl.cancel_async_read();
        }
        // Drop the controller so its Arc<Device> ref is released.
        self.ctl.take();
        // Wait for the background thread (and its Reader) to finish.
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Open an RTL-SDR device and return a stream handle delivering IQ sample buffers.
///
/// A background thread drives `Reader::read_async`.  Each buffer contains
/// `buf_size / 2` `Complex32` samples (one per I/Q pair).
///
/// Dropping the returned `SdrStream` cancels the async read, waits for the
/// background thread to exit, and releases the USB device.
pub fn open_stream(config: DeviceConfig, buf_size: u32) -> Result<SdrStream, SdrError> {
    // Quick check: if the devices iterator is empty there is nothing to open.
    if rtlsdr_mt::devices().next().is_none() {
        return Err(SdrError::NoDevice);
    }

    // Open and configure the device on the calling thread so errors are
    // reported immediately (not silently swallowed in a background thread).
    let (mut ctl, mut reader) =
        rtlsdr_mt::open(config.index).map_err(|e| SdrError::Device(format!("{e:?}")))?;

    ctl.set_sample_rate(SAMPLE_RATE)
        .map_err(|e| SdrError::Device(format!("set_sample_rate: {e:?}")))?;
    ctl.set_ppm(config.ppm_correction)
        .map_err(|e| SdrError::Device(format!("set_ppm: {e:?}")))?;

    if config.gain == GAIN_AUTO {
        ctl.enable_agc()
            .map_err(|e| SdrError::Device(format!("enable_agc: {e:?}")))?;
    } else {
        ctl.disable_agc()
            .map_err(|e| SdrError::Device(format!("disable_agc: {e:?}")))?;
        ctl.set_tuner_gain(config.gain)
            .map_err(|e| SdrError::Device(format!("set_tuner_gain: {e:?}")))?;
    }

    ctl.set_center_freq(config.center_freq_hz)
        .map_err(|e| SdrError::Device(format!("set_center_freq: {e:?}")))?;

    let (tx, rx) = mpsc::sync_channel::<Vec<Complex32>>(8);

    let thread = std::thread::Builder::new()
        .name("rtlsdr-reader".into())
        .spawn(move || {
            let read_result = reader.read_async(4, buf_size, |bytes| {
                let samples = iq_to_complex(bytes);
                if tx.send(samples).is_err() {
                    log::info!("rtlsdr-reader: receiver dropped, stopping");
                }
            });

            if let Err(e) = read_result {
                log::error!("rtlsdr-reader: read_async error: {e:?}");
            }
        })
        .map_err(|e| SdrError::Device(e.to_string()))?;

    Ok(SdrStream {
        rx,
        ctl: Some(ctl),
        thread: Some(thread),
    })
}

/// Open a raw IQ file and return a stream handle delivering sample buffers.
///
/// The file must contain interleaved unsigned 8-bit I/Q pairs (the same format
/// produced by `rtl_sdr`).  Samples are read in chunks and converted to
/// `Complex32`, then delivered through the same `mpsc::Receiver` interface as
/// a live RTL-SDR stream.
///
/// The stream ends (receiver returns `RecvError`) when the file has been fully
/// read.
pub fn open_file_stream(path: &Path, buf_size: usize) -> Result<SdrStream, SdrError> {
    use std::fs::File;
    use std::io::Read;

    let mut file =
        File::open(path).map_err(|e| SdrError::Device(format!("open {}: {e}", path.display())))?;

    let (tx, rx) = mpsc::sync_channel::<Vec<Complex32>>(8);

    let thread = std::thread::Builder::new()
        .name("file-reader".into())
        .spawn(move || {
            let mut raw = vec![0u8; buf_size];
            loop {
                match file.read(&mut raw) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        // Ensure we only convert complete I/Q pairs.
                        let usable = n & !1;
                        if usable == 0 {
                            continue;
                        }
                        let samples = iq_to_complex(&raw[..usable]);
                        if tx.send(samples).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        log::error!("file-reader: {e}");
                        break;
                    }
                }
            }
            log::info!("file-reader: finished");
        })
        .map_err(|e| SdrError::Device(e.to_string()))?;

    Ok(SdrStream {
        rx,
        ctl: None,
        thread: Some(thread),
    })
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iq_to_complex_zero_maps_to_minus_one() {
        let raw = [0u8, 0u8];
        let c = iq_to_complex(&raw);
        assert!((c[0].re - (-1.0f32)).abs() < 1e-4);
        assert!((c[0].im - (-1.0f32)).abs() < 1e-4);
    }

    #[test]
    fn iq_to_complex_255_maps_to_plus_one() {
        let raw = [255u8, 255u8];
        let c = iq_to_complex(&raw);
        assert!((c[0].re - 1.0f32).abs() < 0.01);
        assert!((c[0].im - 1.0f32).abs() < 0.01);
    }

    #[test]
    fn iq_to_complex_ignores_trailing_odd_byte() {
        let raw = [127u8, 128u8, 200u8];
        let c = iq_to_complex(&raw);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn iq_to_complex_empty_input() {
        assert!(iq_to_complex(&[]).is_empty());
    }

    #[test]
    fn list_devices_does_not_panic() {
        // No hardware in CI — just verify it doesn't panic.
        let _ = list_devices();
    }
}
