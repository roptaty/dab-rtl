//! End-to-end test: IQ samples → OfdmProcessor → FIC decode.
//!
//! Uses the same raw IQ file as the diagnostic but processes through the
//! real OfdmProcessor pipeline (FrameSync + OfdmDemod + FreqDeinterleaver).
//!
//! Run with:
//!   cargo test -p ofdm --test iq_pipeline -- --nocapture

use num_complex::Complex32;
use ofdm::params::*;
use ofdm::OfdmProcessor;

const IQ_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/dab_13b.raw");

fn read_iq_file(path: &str) -> Vec<Complex32> {
    let data = std::fs::read(path).expect("Failed to read IQ file");
    data.chunks_exact(2)
        .map(|c| Complex32::new((c[0] as f32 - 127.5) / 127.5, (c[1] as f32 - 127.5) / 127.5))
        .collect()
}

fn fib_crc_ok(fib: &[u8]) -> bool {
    if fib.len() < 32 {
        return false;
    }
    let mut crc: u16 = 0xFFFF;
    for &byte in &fib[..30] {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    !crc == u16::from_be_bytes([fib[30], fib[31]])
}

fn pack_bits(bits: &[u8]) -> Vec<u8> {
    let n = bits.len().div_ceil(8);
    let mut out = vec![0u8; n];
    for (i, &b) in bits.iter().enumerate() {
        if b != 0 {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}

/// Energy dispersal PRBS: x^9 + x^5 + 1, all-ones init, reset per FIB.
fn energy_dispersal_prbs(len: usize) -> Vec<u8> {
    let mut reg: u16 = 0x1FF;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let bit = ((reg >> 8) ^ (reg >> 4)) & 1;
        out.push(bit as u8);
        reg = ((reg << 1) | bit) & 0x1FF;
    }
    out
}

fn xor_fib_with_prbs(fib: &mut [u8], prbs: &[u8]) {
    for (i, byte) in fib.iter_mut().enumerate() {
        let mut mask = 0u8;
        for bit in 0..8 {
            let idx = i * 8 + bit;
            if idx < prbs.len() && prbs[idx] != 0 {
                mask |= 0x80 >> bit;
            }
        }
        *byte ^= mask;
    }
}

/// Try decoding soft bits with given config and return number of CRC-OK FIBs.
fn try_decode(
    soft: &[f32],
    viterbi: &fec::ViterbiDecoder,
    prbs: &[u8],
    negate: bool,
    swap_pairs: bool,
    with_dispersal: bool,
) -> usize {
    let mut processed: Vec<f32> = if swap_pairs {
        // Swap im/re within each carrier pair
        soft.chunks_exact(2)
            .flat_map(|pair| [pair[1], pair[0]])
            .collect()
    } else {
        soft.to_vec()
    };
    if negate {
        for v in processed.iter_mut() {
            *v = -*v;
        }
    }

    // Normalize
    let max_abs = processed.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
    let normalized: Vec<f32> = processed.iter().map(|v| v * scale).collect();

    // Viterbi decode
    let mut padded = normalized;
    padded.extend(std::iter::repeat_n(0.0f32, 24));
    let decoded = viterbi.decode(&padded);
    let info = &decoded[..decoded.len().min(768)];
    let mut fic_bytes = pack_bits(info);

    // Energy de-dispersal per FIB
    if with_dispersal {
        for fib_idx in 0..3 {
            let start = fib_idx * 32;
            if start + 32 <= fic_bytes.len() {
                xor_fib_with_prbs(&mut fic_bytes[start..start + 32], prbs);
            }
        }
    }

    // Check CRC
    let mut ok = 0;
    for fib_idx in 0..3 {
        let start = fib_idx * 32;
        if start + 32 <= fic_bytes.len() && fib_crc_ok(&fic_bytes[start..start + 32]) {
            ok += 1;
        }
    }
    ok
}

#[test]
fn ofdm_pipeline_fic_decode() {
    if !std::path::Path::new(IQ_FILE).exists() {
        eprintln!("Skipping: IQ file not found at {IQ_FILE}");
        return;
    }

    let samples = read_iq_file(IQ_FILE);
    eprintln!(
        "Loaded {} IQ samples ({:.1} ms)",
        samples.len(),
        samples.len() as f64 / SAMPLE_RATE as f64 * 1000.0
    );

    // Process through OfdmProcessor in chunks.
    let mut ofdm = OfdmProcessor::new();
    let chunk_size = 65536;

    let prbs = energy_dispersal_prbs(256);

    // Create Viterbi decoders with different polynomial sets
    let _polys_reversed: [u8; 4] = [109, 79, 83, 109]; // Reversed (our current default)
    let polys_original: [u8; 4] = [91, 121, 101, 91]; // Original octal values

    let viterbi_rev = fec::ViterbiDecoder::new(35); // uses reversed polys
    let viterbi_orig = fec::ViterbiDecoder::with_polys(35, &polys_original);

    // Collect frames from good DQPSK frames only (metric < -0.7)
    let mut good_symbols: Vec<Vec<f32>> = Vec::new();
    let mut total_frames = 0;

    for chunk_start in (0..samples.len()).step_by(chunk_size) {
        let chunk_end = (chunk_start + chunk_size).min(samples.len());
        let chunk = &samples[chunk_start..chunk_end];

        for frame in ofdm.push_samples(chunk) {
            total_frames += 1;

            if let Some(sym0) = frame.soft_bits.first() {
                let mut dqpsk_sum = 0.0f64;
                let mut dqpsk_count = 0u32;
                for pair in sym0.chunks_exact(2) {
                    let angle = pair[0].atan2(pair[1]);
                    dqpsk_sum += (4.0 * angle as f64).cos();
                    dqpsk_count += 1;
                }
                let metric = dqpsk_sum / dqpsk_count as f64;

                if metric < -0.7 {
                    // Good frame — collect FIC symbols
                    for sym_idx in 0..3.min(frame.soft_bits.len()) {
                        good_symbols.push(frame.soft_bits[sym_idx].clone());
                    }
                }
            }
        }
    }

    eprintln!(
        "Frames: {total_frames}, good FIC symbols: {}",
        good_symbols.len()
    );

    if good_symbols.is_empty() {
        eprintln!("No good frames found, skipping systematic test");
        return;
    }

    // Systematic brute-force: try all combinations
    eprintln!("\n=== Systematic search ===");
    let configs = [
        ("rev-polys", &viterbi_rev as &fec::ViterbiDecoder),
        ("orig-polys", &viterbi_orig),
    ];
    let negate_opts = [false, true];
    let swap_opts = [false, true];
    let dispersal_opts = [true, false];

    let mut best_config = String::new();
    let mut best_ok = 0usize;

    for (poly_name, viterbi) in &configs {
        for &negate in &negate_opts {
            for &swap in &swap_opts {
                for &dispersal in &dispersal_opts {
                    let mut total_ok = 0;
                    let mut total_fibs = 0;
                    for sym in &good_symbols {
                        let ok = try_decode(sym, viterbi, &prbs, negate, swap, dispersal);
                        total_ok += ok;
                        total_fibs += 3;
                    }
                    let config = format!(
                        "{} negate={} swap={} dispersal={}",
                        poly_name, negate, swap, dispersal
                    );
                    if total_ok > 0 {
                        eprintln!("  *** {} → {}/{} CRC OK ***", config, total_ok, total_fibs);
                    }
                    if total_ok > best_ok {
                        best_ok = total_ok;
                        best_config = config;
                    }
                }
            }
        }
    }

    if best_ok == 0 {
        eprintln!("  All combinations: 0 CRC OK");

        // Debug: print first good symbol's decoded bytes for one config
        if let Some(sym) = good_symbols.first() {
            let max_abs = sym.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
            let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
            let normalized: Vec<f32> = sym.iter().map(|v| v * scale).collect();
            let mut padded = normalized;
            padded.extend(std::iter::repeat_n(0.0f32, 24));
            let decoded = viterbi_rev.decode(&padded);
            let info = &decoded[..decoded.len().min(768)];
            let mut fic_bytes = pack_bits(info);
            for fib_idx in 0..3 {
                let start = fib_idx * 32;
                if start + 32 <= fic_bytes.len() {
                    xor_fib_with_prbs(&mut fic_bytes[start..start + 32], &prbs);
                }
            }
            eprintln!("  FIB0 (rev polys, dispersal): {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
                fic_bytes[0], fic_bytes[1], fic_bytes[2], fic_bytes[3],
                fic_bytes[4], fic_bytes[5], fic_bytes[6], fic_bytes[7]);
            // Compute CRC and show what it should be
            let mut crc: u16 = 0xFFFF;
            for &byte in &fic_bytes[..30] {
                crc ^= (byte as u16) << 8;
                for _ in 0..8 {
                    if crc & 0x8000 != 0 {
                        crc = (crc << 1) ^ 0x1021;
                    } else {
                        crc <<= 1;
                    }
                }
            }
            eprintln!(
                "  CRC computed: {:04x}, stored: {:02x}{:02x}, complement: {:04x}",
                crc, fic_bytes[30], fic_bytes[31], !crc
            );
        }
    }

    eprintln!("\nBest config: {} ({} CRC OK)", best_config, best_ok);

    // For now, don't assert — we're still debugging
    if best_ok > 0 {
        eprintln!("SUCCESS: Found working configuration!");
    }
}
