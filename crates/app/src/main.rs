mod countries;
mod pipeline;
mod tui;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dab-rtl", about = "Pure-Rust DAB/DAB+ radio receiver", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// RTL-SDR device index
    #[arg(short = 'd', long, default_value = "0", global = true)]
    device: u32,

    /// Crystal frequency correction in PPM
    #[arg(long, default_value = "0", global = true)]
    ppm: i32,

    /// Tuner gain in tenths of dB (−1 = hardware AGC)
    #[arg(short = 'g', long, default_value = "-1", global = true)]
    gain: i32,
}

#[derive(Subcommand)]
enum Command {
    /// List connected RTL-SDR devices
    ListDevices,

    /// List available audio output devices
    ListAudio,

    /// List supported countries and their DAB channels
    ListCountries,

    /// Scan for DAB stations on the given channel (no TUI)
    Scan {
        /// DAB Band III channel (e.g. 11C) or raw frequency in Hz
        #[arg(short = 'c', long, required_unless_present_any = ["country", "file"])]
        channel: Option<String>,

        /// ISO 3166-1 alpha-2 country code (e.g. NO, GB, DE); scans all channels
        #[arg(long, conflicts_with_all = ["channel", "file"])]
        country: Option<String>,

        /// Raw IQ file (u8 interleaved I/Q, e.g. from rtl_sdr) instead of live SDR
        #[arg(short = 'f', long, conflicts_with = "country")]
        file: Option<PathBuf>,
    },

    /// Tune to a channel and launch the interactive TUI
    Tune {
        /// DAB Band III channel (e.g. 11C) or raw frequency in Hz
        #[arg(
            short = 'c',
            long,
            required_unless_present_any = ["file", "country"]
        )]
        channel: Option<String>,

        /// ISO 3166-1 alpha-2 country code (e.g. NO, GB, DE).
        /// Scans all channels for that country and lets you pick a station.
        #[arg(long, conflicts_with_all = ["channel", "file"])]
        country: Option<String>,

        /// Audio output device name (default = system default)
        #[arg(short = 'a', long)]
        audio_device: Option<String>,

        /// Raw IQ file (u8 interleaved I/Q, e.g. from rtl_sdr) instead of live SDR
        #[arg(short = 'f', long, conflicts_with = "country")]
        file: Option<PathBuf>,
    },

    /// Play a specific station (non-interactive)
    Play {
        /// DAB Band III channel (e.g. 11C) or raw frequency in Hz
        #[arg(short = 'c', long, required_unless_present = "file")]
        channel: Option<String>,

        /// Station name (case-insensitive substring match)
        #[arg(short = 's', long)]
        station: String,

        /// Audio output device name (default = system default)
        #[arg(short = 'a', long)]
        audio_device: Option<String>,

        /// Raw IQ file (u8 interleaved I/Q, e.g. from rtl_sdr) instead of live SDR
        #[arg(short = 'f', long)]
        file: Option<PathBuf>,
    },
}

// ─────────────────────────────────────────────────────────────────────────── //
//  DAB Band III channel table                                                  //
// ─────────────────────────────────────────────────────────────────────────── //

pub fn channel_to_freq(ch: &str) -> Option<u32> {
    match ch.to_uppercase().as_str() {
        "5A" => Some(174_928_000),
        "5B" => Some(176_640_000),
        "5C" => Some(178_352_000),
        "5D" => Some(180_064_000),
        "6A" => Some(181_936_000),
        "6B" => Some(183_648_000),
        "6C" => Some(185_360_000),
        "6D" => Some(187_072_000),
        "7A" => Some(188_928_000),
        "7B" => Some(190_640_000),
        "7C" => Some(192_352_000),
        "7D" => Some(194_064_000),
        "8A" => Some(195_936_000),
        "8B" => Some(197_648_000),
        "8C" => Some(199_360_000),
        "8D" => Some(201_072_000),
        "9A" => Some(202_928_000),
        "9B" => Some(204_640_000),
        "9C" => Some(206_352_000),
        "9D" => Some(208_064_000),
        "10A" => Some(209_936_000),
        "10B" => Some(211_648_000),
        "10C" => Some(213_360_000),
        "10D" => Some(215_072_000),
        "11A" => Some(216_928_000),
        "11B" => Some(218_640_000),
        "11C" => Some(220_352_000),
        "11D" => Some(222_064_000),
        "12A" => Some(223_936_000),
        "12B" => Some(225_648_000),
        "12C" => Some(227_360_000),
        "12D" => Some(229_072_000),
        "13A" => Some(230_784_000),
        "13B" => Some(232_496_000),
        "13C" => Some(234_208_000),
        "13D" => Some(235_776_000),
        "13E" => Some(237_488_000),
        "13F" => Some(239_200_000),
        other => other.parse::<u32>().ok(),
    }
}

fn resolve_channel(ch: &str) -> u32 {
    channel_to_freq(ch).unwrap_or_else(|| {
        eprintln!("error: unknown channel '{ch}'");
        std::process::exit(1);
    })
}

/// Open an IQ sample stream from either a raw file or a live RTL-SDR device.
fn open_iq_source(
    file: Option<&PathBuf>,
    channel: Option<&str>,
    device_idx: u32,
    ppm: i32,
    gain: i32,
) -> sdr::SdrStream {
    if let Some(path) = file {
        match sdr::open_file_stream(path, 32_768) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let ch = channel.expect("channel or file required");
        let freq_hz = resolve_channel(ch);
        let config = sdr::DeviceConfig {
            index: device_idx,
            center_freq_hz: freq_hz,
            gain,
            ppm_correction: ppm,
        };
        match sdr::open_stream(config, 32_768) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Describe the IQ source for user-facing messages.
fn source_label(file: Option<&PathBuf>, channel: Option<&str>) -> String {
    if let Some(path) = file {
        format!("file '{}'", path.display())
    } else {
        let ch = channel.expect("channel or file required");
        let freq_hz = resolve_channel(ch);
        format!("channel {ch} at {:.3} MHz", freq_hz as f64 / 1e6)
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Entry point                                                                 //
// ─────────────────────────────────────────────────────────────────────────── //

fn main() {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Command::ListDevices => cmd_list_devices(),
        Command::ListAudio => cmd_list_audio(),
        Command::ListCountries => countries::print_countries(),
        Command::Scan {
            channel,
            country,
            file,
        } => cmd_scan(cli.device, cli.ppm, cli.gain, channel, country, file),
        Command::Tune {
            channel,
            country,
            audio_device,
            file,
        } => cmd_tune(
            cli.device,
            cli.ppm,
            cli.gain,
            channel,
            country,
            audio_device,
            file,
        ),
        Command::Play {
            channel,
            station,
            audio_device,
            file,
        } => cmd_play(
            cli.device,
            cli.ppm,
            cli.gain,
            channel,
            station,
            audio_device,
            file,
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Subcommand implementations                                                  //
// ─────────────────────────────────────────────────────────────────────────── //

fn cmd_list_devices() {
    let devices = sdr::list_devices();
    if devices.is_empty() {
        println!("No RTL-SDR devices found.");
    } else {
        for (idx, name) in devices {
            println!("[{idx}] {name}");
        }
    }
}

fn cmd_list_audio() {
    let devices = audio::list_devices();
    if devices.is_empty() {
        println!("No audio output devices found.");
    } else {
        for (idx, name) in devices {
            println!("[{idx}] {name}");
        }
    }
}

/// Headless scan: print services as they are decoded from the FIC.
///
/// Stops when either:
/// - No ensemble is detected within `NO_LOCK_SECS` seconds, or
/// - `SETTLE_SECS` seconds pass with no new service appearing.
fn cmd_scan(
    device_idx: u32,
    ppm: i32,
    gain: i32,
    channel: Option<String>,
    country: Option<String>,
    file: Option<PathBuf>,
) {
    // Country mode: scan all channels for that country (file not supported).
    if let Some(code) = country {
        let channels = match countries::channels_for_country(&code) {
            Some(chs) => chs.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            None => {
                eprintln!("error: unknown country code '{code}'. Try `list-countries`.");
                std::process::exit(1);
            }
        };
        for ch in &channels {
            scan_single(device_idx, ppm, gain, Some(ch.as_str()), None);
        }
        return;
    }

    scan_single(device_idx, ppm, gain, channel.as_deref(), file.as_ref());
}

/// Scan a single IQ source for DAB services.
fn scan_single(
    device_idx: u32,
    ppm: i32,
    gain: i32,
    channel: Option<&str>,
    file: Option<&PathBuf>,
) {
    use ofdm::OfdmProcessor;
    use pipeline::FicDecoder;
    use std::time::{Duration, Instant};

    /// Give up if no DAB ensemble is detected within this time.
    const NO_LOCK_SECS: u64 = 30;
    /// After the first service appears, wait this long for more to arrive.
    const SETTLE_SECS: u64 = 5;

    let label = source_label(file, channel);
    println!("Scanning {label}…");

    let stream = open_iq_source(file, channel, device_idx, ppm, gain);

    let mut ofdm = OfdmProcessor::new();
    let mut fic = FicDecoder::new();
    let mut known_sids: std::collections::HashSet<u32> = std::collections::HashSet::new();

    let start = Instant::now();
    let mut last_new_service = Option::<Instant>::None;

    'outer: for iq_buf in stream.rx.iter() {
        for frame in ofdm.push_samples(&iq_buf) {
            // Decode the 3 FIC symbols.
            fic.begin_frame();
            for sym in frame.soft_bits.get(0..3).unwrap_or_default() {
                fic.process_symbol(sym);
            }

            let ens = fic.handler.ensemble();
            if !ens.label.is_empty() {
                for svc in &ens.services {
                    if known_sids.insert(svc.id) {
                        last_new_service = Some(Instant::now());
                    }
                }
            }
        }

        // Timeout checks run on every IQ buffer, not just when frames are
        // produced.  Without signal the OFDM processor never yields frames,
        // so placing these checks inside the `for frame` loop caused the
        // scan to hang indefinitely on empty channels.

        // Timeout: no ensemble lock.
        if last_new_service.is_none() && start.elapsed() > Duration::from_secs(NO_LOCK_SECS) {
            println!("  (no DAB signal — skipping)");
            break 'outer;
        }

        // Timeout: no new services for SETTLE_SECS after first discovery.
        if let Some(t) = last_new_service {
            if t.elapsed() > Duration::from_secs(SETTLE_SECS) {
                break 'outer;
            }
        }
    }

    // Print final results with labels that arrived during the settle period.
    let ens = fic.handler.ensemble();
    if !ens.label.is_empty() {
        println!("Ensemble: {} (EId {:04X})", ens.label, ens.id);
        for svc in &ens.services {
            let tag = if svc.is_dab_plus { " [DAB+]" } else { "" };
            println!(
                "  [{:08X}]  {}{}",
                svc.id,
                if svc.label.is_empty() {
                    "<no label>"
                } else {
                    &svc.label
                },
                tag,
            );
        }
    }
}

/// Interactive TUI: tune to a specific channel, or scan all channels for a country.
fn cmd_tune(
    device_idx: u32,
    ppm: i32,
    gain: i32,
    channel: Option<String>,
    country: Option<String>,
    audio_device: Option<String>,
    file: Option<PathBuf>,
) {
    // Country mode: scan all channels for that country in the TUI.
    if let Some(ref code) = country {
        let channels = match countries::channels_for_country(code) {
            Some(chs) => chs,
            None => {
                eprintln!("error: unknown country code '{code}'. Try `list-countries`.");
                std::process::exit(1);
            }
        };

        // Start the pipeline on the first channel; the TUI will retune as it scans.
        let first_ch = channels[0];
        let freq_hz = resolve_channel(first_ch);
        let config = sdr::DeviceConfig {
            index: device_idx,
            center_freq_hz: freq_hz,
            gain,
            ppm_correction: ppm,
        };
        println!(
            "Starting country scan for {code} ({} channels)…",
            channels.len()
        );
        let handle = match pipeline::start_for_device(config, audio_device) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        };
        let ch_list: Vec<(String, u32)> = channels
            .iter()
            .filter_map(|&ch| channel_to_freq(ch).map(|f| (ch.to_string(), f)))
            .collect();
        if let Err(e) = tui::run(handle, ch_list) {
            eprintln!("TUI error: {e}");
        }
        return;
    }

    // Single-channel mode (or file mode).
    let label = source_label(file.as_ref(), channel.as_deref());
    println!("Tuning to {label}…");

    if file.is_none() {
        // Live SDR: use start_for_device so the TUI can retune via [c] → country select.
        let ch = channel.as_deref().expect("channel or file required");
        let freq_hz = resolve_channel(ch);
        let config = sdr::DeviceConfig {
            index: device_idx,
            center_freq_hz: freq_hz,
            gain,
            ppm_correction: ppm,
        };
        let handle = match pipeline::start_for_device(config, audio_device) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = tui::run(handle, vec![]) {
            eprintln!("TUI error: {e}");
        }
    } else {
        // File mode: retune not supported.
        let stream = open_iq_source(file.as_ref(), channel.as_deref(), device_idx, ppm, gain);
        let handle = match pipeline::start_with_stream(stream, audio_device) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = tui::run(handle, vec![]) {
            eprintln!("TUI error: {e}");
        }
    }
}

/// Non-interactive play: find the named station and start audio immediately.
fn cmd_play(
    device_idx: u32,
    ppm: i32,
    gain: i32,
    channel: Option<String>,
    station: String,
    audio_device: Option<String>,
    file: Option<PathBuf>,
) {
    use pipeline::{PipelineCmd, PipelineUpdate};

    let label = source_label(file.as_ref(), channel.as_deref());
    println!("Searching for '{station}' on {label}…  Press Ctrl-C to stop.");

    let stream = open_iq_source(file.as_ref(), channel.as_deref(), device_idx, ppm, gain);

    let handle = match pipeline::start_with_stream(stream, audio_device) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    let mut started = false;

    for update in handle.update_rx.iter() {
        match update {
            PipelineUpdate::Ensemble(ens) => {
                if !started {
                    for svc in &ens.services {
                        if svc.label.to_lowercase().contains(&station.to_lowercase()) {
                            println!("Found: {} — starting playback", svc.label);
                            let _ = handle.cmd_tx.try_send(PipelineCmd::Play(svc.id));
                            started = true;
                            break;
                        }
                    }
                }
            }
            PipelineUpdate::Playing { label } => {
                println!("Playing: {label}");
            }
            PipelineUpdate::Status(s) => {
                log::info!("Pipeline: {s}");
            }
            PipelineUpdate::Dls { sid, text } => {
                log::info!("DLS SId={:04X}: {}", sid, text);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_channels_resolve() {
        assert_eq!(channel_to_freq("11C"), Some(220_352_000));
        assert_eq!(channel_to_freq("5A"), Some(174_928_000));
        assert_eq!(channel_to_freq("13F"), Some(239_200_000));
    }

    #[test]
    fn channel_case_insensitive() {
        assert_eq!(channel_to_freq("11c"), channel_to_freq("11C"));
    }

    #[test]
    fn raw_frequency_passthrough() {
        assert_eq!(channel_to_freq("220352000"), Some(220_352_000));
    }

    #[test]
    fn unknown_channel_returns_none() {
        assert_eq!(channel_to_freq("99Z"), None);
    }

    #[test]
    fn all_band3_channels_covered() {
        let channels = [
            "5A", "5B", "5C", "5D", "6A", "6B", "6C", "6D", "7A", "7B", "7C", "7D", "8A", "8B",
            "8C", "8D", "9A", "9B", "9C", "9D", "10A", "10B", "10C", "10D", "11A", "11B", "11C",
            "11D", "12A", "12B", "12C", "12D", "13A", "13B", "13C", "13D", "13E", "13F",
        ];
        for ch in &channels {
            assert!(channel_to_freq(ch).is_some(), "missing channel {ch}");
        }
    }
}
