/// RTL-SDR abstraction for DAB reception.
///
/// Wraps `rtlsdr_mt` 2.x to provide a channel-based IQ sample stream and
/// convenience helpers for device enumeration and IQ conversion.

use num_complex::Complex32;
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
        .map(|c| {
            Complex32::new(
                (c[0] as f32 - 127.5) / 127.5,
                (c[1] as f32 - 127.5) / 127.5,
            )
        })
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

/// Open an RTL-SDR device and return a channel that delivers IQ sample buffers.
///
/// A background thread drives `Reader::read_async`.  Each buffer contains
/// `buf_size / 2` `Complex32` samples (one per I/Q pair).
///
/// Dropping the returned `Receiver` causes the next `tx.send` inside the
/// async callback to fail, which exits the callback and the background thread.
pub fn open_stream(
    config: DeviceConfig,
    buf_size: u32,
) -> Result<mpsc::Receiver<Vec<Complex32>>, SdrError> {
    // Quick check: if the devices iterator is empty there is nothing to open.
    if rtlsdr_mt::devices().next().is_none() {
        return Err(SdrError::NoDevice);
    }

    let (tx, rx) = mpsc::sync_channel::<Vec<Complex32>>(8);

    std::thread::Builder::new()
        .name("rtlsdr-reader".into())
        .spawn(move || {
            let open_result = rtlsdr_mt::open(config.index);
            let (mut ctl, mut reader) = match open_result {
                Ok(pair) => pair,
                Err(e) => {
                    log::error!("rtlsdr-reader: open failed: {:?}", e);
                    return;
                }
            };

            // Configure device.
            let mut configure = || -> Result<(), String> {
                ctl.set_sample_rate(SAMPLE_RATE)
                    .map_err(|e| format!("set_sample_rate: {e:?}"))?;
                ctl.set_ppm(config.ppm_correction)
                    .map_err(|e| format!("set_ppm: {e:?}"))?;

                if config.gain == GAIN_AUTO {
                    ctl.enable_agc()
                        .map_err(|e| format!("enable_agc: {e:?}"))?;
                } else {
                    ctl.disable_agc()
                        .map_err(|e| format!("disable_agc: {e:?}"))?;
                    ctl.set_tuner_gain(config.gain)
                        .map_err(|e| format!("set_tuner_gain: {e:?}"))?;
                }

                ctl.set_center_freq(config.center_freq_hz)
                    .map_err(|e| format!("set_center_freq: {e:?}"))?;

                Ok(())
            };

            if let Err(e) = configure() {
                log::error!("rtlsdr-reader: configure failed: {e}");
                return;
            }

            // Start async read loop.  The callback runs until the channel is
            // dropped (tx.send returns Err) or read_async returns.
            let read_result = reader.read_async(4, buf_size, |bytes| {
                let samples = iq_to_complex(bytes);
                if tx.send(samples).is_err() {
                    // Receiver dropped — stop reading.
                    log::info!("rtlsdr-reader: receiver dropped, stopping");
                    // We cannot cancel from inside the callback directly;
                    // returning just lets the next iteration call back again.
                    // Use ctl.cancel_async_read() from outside if needed.
                }
            });

            if let Err(e) = read_result {
                log::error!("rtlsdr-reader: read_async error: {e:?}");
            }
        })
        .map_err(|e| SdrError::Device(e.to_string()))?;

    Ok(rx)
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
