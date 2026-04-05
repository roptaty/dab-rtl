/// RTL-SDR abstraction for DAB reception.
///
/// Wraps `rtlsdr_mt` 2.x to provide a channel-based IQ sample stream and
/// convenience helpers for device enumeration and IQ conversion.
/// Also supports `rtl_tcp` network sources.
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
    let mut out = Vec::with_capacity(raw.len() / 2);
    iq_to_complex_into(raw, &mut out);
    out
}

/// Convert raw RTL-SDR bytes into an existing output buffer.
#[inline]
pub fn iq_to_complex_into(raw: &[u8], out: &mut Vec<Complex32>) {
    out.clear();
    out.reserve(raw.len() / 2);
    for chunk in raw.chunks_exact(2) {
        out.push(Complex32::new(
            (chunk[0] as f32 - 127.5) / 127.5,
            (chunk[1] as f32 - 127.5) / 127.5,
        ));
    }
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
#[derive(Clone)]
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
            let mut scratch = Vec::with_capacity(buf_size as usize / 2);
            let read_result = reader.read_async(4, buf_size, |bytes| {
                iq_to_complex_into(bytes, &mut scratch);
                let samples = std::mem::take(&mut scratch);
                if tx.send(samples).is_err() {
                    log::info!("rtlsdr-reader: receiver dropped, stopping");
                } else {
                    scratch = Vec::with_capacity(buf_size as usize / 2);
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
            let mut scratch = Vec::with_capacity(buf_size / 2);
            loop {
                match file.read(&mut raw) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        // Ensure we only convert complete I/Q pairs.
                        let usable = n & !1;
                        if usable == 0 {
                            continue;
                        }
                        iq_to_complex_into(&raw[..usable], &mut scratch);
                        let samples = std::mem::take(&mut scratch);
                        if tx.send(samples).is_err() {
                            break;
                        }
                        scratch = Vec::with_capacity(buf_size / 2);
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
//  rtl_tcp streaming                                                           //
// ─────────────────────────────────────────────────────────────────────────── //

/// Configuration for connecting to an `rtl_tcp` server.
#[derive(Clone, Debug)]
pub struct TcpConfig {
    /// Server address in `host:port` form (default port 1234).
    pub address: String,
    /// Tuner centre frequency in Hz.
    pub center_freq_hz: u32,
    /// Gain in tenths of dB, or `GAIN_AUTO` (−1) to enable hardware AGC.
    pub gain: i32,
    /// Crystal frequency correction in PPM.
    pub ppm_correction: i32,
}

/// Unified source configuration for opening an IQ stream.
#[derive(Clone)]
pub enum SourceConfig {
    /// Local RTL-SDR USB device.
    Device(DeviceConfig),
    /// Remote `rtl_tcp` server.
    Tcp(TcpConfig),
}

impl SourceConfig {
    /// Return a new config with the centre frequency changed.
    pub fn with_freq(&self, freq_hz: u32) -> Self {
        match self {
            SourceConfig::Device(c) => SourceConfig::Device(DeviceConfig {
                center_freq_hz: freq_hz,
                ..c.clone()
            }),
            SourceConfig::Tcp(c) => SourceConfig::Tcp(TcpConfig {
                center_freq_hz: freq_hz,
                ..c.clone()
            }),
        }
    }

    /// Return the current centre frequency.
    pub fn center_freq_hz(&self) -> u32 {
        match self {
            SourceConfig::Device(c) => c.center_freq_hz,
            SourceConfig::Tcp(c) => c.center_freq_hz,
        }
    }
}

/// Open an IQ stream from any supported source.
pub fn open_source(config: &SourceConfig, buf_size: u32) -> Result<SdrStream, SdrError> {
    match config {
        SourceConfig::Device(c) => open_stream(c.clone(), buf_size),
        SourceConfig::Tcp(c) => open_tcp_stream(c),
    }
}

/// Build a 5-byte rtl_tcp command: `[cmd_id, param (big-endian u32)]`.
fn rtl_tcp_cmd(cmd: u8, param: u32) -> [u8; 5] {
    let p = param.to_be_bytes();
    [cmd, p[0], p[1], p[2], p[3]]
}

/// Connect to an `rtl_tcp` server and return a stream handle delivering IQ
/// sample buffers.
///
/// The rtl_tcp protocol:
/// 1. Server sends a 12-byte header: `"RTL0"` + tuner type (u32 BE) + gain
///    count (u32 BE).
/// 2. Client sends 5-byte commands to configure the dongle.
/// 3. Server streams raw interleaved u8 I/Q pairs continuously.
///
/// Samples are delivered through the same `mpsc::Receiver<Vec<Complex32>>`
/// interface as local RTL-SDR and file sources.
pub fn open_tcp_stream(config: &TcpConfig) -> Result<SdrStream, SdrError> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let mut tcp = TcpStream::connect(&config.address)
        .map_err(|e| SdrError::Device(format!("rtl_tcp connect {}: {e}", config.address)))?;

    // Read and validate the 12-byte server header.
    let mut header = [0u8; 12];
    tcp.read_exact(&mut header)
        .map_err(|e| SdrError::Device(format!("rtl_tcp header read: {e}")))?;
    if &header[0..4] != b"RTL0" {
        return Err(SdrError::Device(format!(
            "rtl_tcp: invalid magic {:?}",
            &header[0..4]
        )));
    }
    log::info!(
        "rtl_tcp: connected to {} (tuner type {}, {} gains)",
        config.address,
        u32::from_be_bytes(header[4..8].try_into().unwrap()),
        u32::from_be_bytes(header[8..12].try_into().unwrap()),
    );

    // Send configuration commands.
    // 0x02 = set sample rate
    tcp.write_all(&rtl_tcp_cmd(0x02, SAMPLE_RATE))
        .map_err(|e| SdrError::Device(format!("rtl_tcp set_sample_rate: {e}")))?;
    // 0x05 = set freq correction (PPM)
    tcp.write_all(&rtl_tcp_cmd(0x05, config.ppm_correction as u32))
        .map_err(|e| SdrError::Device(format!("rtl_tcp set_ppm: {e}")))?;

    if config.gain == GAIN_AUTO {
        // 0x03 = set gain mode (0 = auto)
        tcp.write_all(&rtl_tcp_cmd(0x03, 0))
            .map_err(|e| SdrError::Device(format!("rtl_tcp set_gain_mode: {e}")))?;
        // 0x08 = set AGC mode (1 = on)
        tcp.write_all(&rtl_tcp_cmd(0x08, 1))
            .map_err(|e| SdrError::Device(format!("rtl_tcp set_agc: {e}")))?;
    } else {
        // 0x03 = set gain mode (1 = manual)
        tcp.write_all(&rtl_tcp_cmd(0x03, 1))
            .map_err(|e| SdrError::Device(format!("rtl_tcp set_gain_mode: {e}")))?;
        // 0x04 = set tuner gain
        tcp.write_all(&rtl_tcp_cmd(0x04, config.gain as u32))
            .map_err(|e| SdrError::Device(format!("rtl_tcp set_gain: {e}")))?;
        // 0x08 = set AGC mode (0 = off)
        tcp.write_all(&rtl_tcp_cmd(0x08, 0))
            .map_err(|e| SdrError::Device(format!("rtl_tcp set_agc: {e}")))?;
    }

    // 0x01 = set centre frequency
    tcp.write_all(&rtl_tcp_cmd(0x01, config.center_freq_hz))
        .map_err(|e| SdrError::Device(format!("rtl_tcp set_freq: {e}")))?;

    tcp.flush()
        .map_err(|e| SdrError::Device(format!("rtl_tcp flush: {e}")))?;

    // Set a read timeout so the background thread can detect shutdown.
    tcp.set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(|e| SdrError::Device(format!("rtl_tcp set_read_timeout: {e}")))?;

    let (tx, rx) = mpsc::sync_channel::<Vec<Complex32>>(8);
    let buf_size: usize = 32_768;

    let thread = std::thread::Builder::new()
        .name("rtl-tcp-reader".into())
        .spawn(move || {
            let mut raw = vec![0u8; buf_size];
            let mut scratch = Vec::with_capacity(buf_size / 2);
            loop {
                match tcp.read(&mut raw) {
                    Ok(0) => {
                        log::info!("rtl-tcp-reader: server closed connection");
                        break;
                    }
                    Ok(n) => {
                        let usable = n & !1;
                        if usable == 0 {
                            continue;
                        }
                        iq_to_complex_into(&raw[..usable], &mut scratch);
                        let samples = std::mem::take(&mut scratch);
                        if tx.send(samples).is_err() {
                            log::info!("rtl-tcp-reader: receiver dropped, stopping");
                            break;
                        }
                        scratch = Vec::with_capacity(buf_size / 2);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Read timeout — check if receiver is still alive.
                        if tx.send(Vec::new()).is_err() {
                            log::info!("rtl-tcp-reader: receiver dropped, stopping");
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                        if tx.send(Vec::new()).is_err() {
                            log::info!("rtl-tcp-reader: receiver dropped, stopping");
                            break;
                        }
                    }
                    Err(e) => {
                        log::error!("rtl-tcp-reader: {e}");
                        break;
                    }
                }
            }
            log::info!("rtl-tcp-reader: finished");
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

    #[test]
    fn rtl_tcp_cmd_encoding() {
        // Command 0x01 (set freq), param = 220_352_000 (0x0D22_4000)
        let cmd = super::rtl_tcp_cmd(0x01, 220_352_000);
        assert_eq!(cmd[0], 0x01);
        assert_eq!(&cmd[1..], &220_352_000u32.to_be_bytes());
    }

    #[test]
    fn rtl_tcp_cmd_zero_param() {
        let cmd = super::rtl_tcp_cmd(0x03, 0);
        assert_eq!(cmd, [0x03, 0, 0, 0, 0]);
    }

    #[test]
    fn source_config_with_freq() {
        let dev = SourceConfig::Device(DeviceConfig {
            center_freq_hz: 100_000,
            ..DeviceConfig::default()
        });
        let retuned = dev.with_freq(200_000);
        assert_eq!(retuned.center_freq_hz(), 200_000);

        let tcp = SourceConfig::Tcp(TcpConfig {
            address: "localhost:1234".into(),
            center_freq_hz: 100_000,
            gain: GAIN_AUTO,
            ppm_correction: 0,
        });
        let retuned = tcp.with_freq(300_000);
        assert_eq!(retuned.center_freq_hz(), 300_000);
    }
}
