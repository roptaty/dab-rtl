//! End-to-end test: IQ samples → OfdmProcessor → FIC decode.
//!
//! Uses the 2-minute IQ capture to test the full pipeline including
//! proper FIC depuncturing (PI_16/PI_15/PI_X, 2304→3096 bits).
//!
//! Run with:
//!   cargo test -p ofdm --test iq_pipeline -- --nocapture

use num_complex::Complex32;
use ofdm::params::*;
use ofdm::OfdmProcessor;

const IQ_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../testdata/dab_13b_2min.raw"
);
const IQ_FILE_SHORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/dab_13b.raw");

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

/// Energy dispersal PRBS: x^9 + x^5 + 1, all-ones init.
fn generate_prbs(len: usize) -> Vec<u8> {
    let mut reg: u16 = 0x1FF;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let bit = ((reg >> 8) ^ (reg >> 4)) & 1;
        out.push(bit as u8);
        reg = ((reg << 1) | bit) & 0x1FF;
    }
    out
}

fn xor_with_prbs(data: &mut [u8], prbs: &[u8]) {
    for (i, byte) in data.iter_mut().enumerate() {
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

/// Process FIC with proper 2304-bit block accumulation and depuncturing.
///
/// Takes 3 FIC symbol soft-bit vectors (3072 each = 9216 total),
/// accumulates into 2304-bit blocks, depunctures, Viterbi-decodes,
/// and checks CRC on each of the 4 resulting FIC blocks (12 FIBs total).
fn decode_fic_frame(
    fic_symbols: &[Vec<f32>],
    viterbi: &fec::ViterbiDecoder,
    prbs_768: &[u8],
    negate: bool,
) -> usize {
    // Flatten all 3 FIC symbols into one stream.
    let mut all_soft: Vec<f32> = Vec::with_capacity(9216);
    for sym in fic_symbols {
        if negate {
            all_soft.extend(sym.iter().map(|&v| -v));
        } else {
            all_soft.extend_from_slice(sym);
        }
    }

    let mut crc_ok_count = 0;

    // Process 4 FIC blocks of 2304 bits each.
    for block_idx in 0..4 {
        let start = block_idx * fec::FIC_PUNCTURED_BITS;
        let end = start + fec::FIC_PUNCTURED_BITS;
        if end > all_soft.len() {
            break;
        }
        let block = &all_soft[start..end];

        // Normalize to [-1, +1].
        let max_abs = block.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
        let normalized: Vec<f32> = block.iter().map(|v| v * scale).collect();

        // Depuncture 2304 → 3096.
        let depunctured = fec::fic_depuncture(&normalized);

        // Viterbi decode.
        let decoded = viterbi.decode(&depunctured);
        let info = &decoded[..decoded.len().min(768)];
        let mut fic_bytes = pack_bits(info);

        // Energy de-dispersal with continuous PRBS across all 3 FIBs.
        xor_with_prbs(&mut fic_bytes, prbs_768);

        // Check each FIB's CRC.
        for fib_idx in 0..3 {
            let fib_start = fib_idx * 32;
            if fib_start + 32 <= fic_bytes.len()
                && fib_crc_ok(&fic_bytes[fib_start..fib_start + 32])
            {
                crc_ok_count += 1;
                if block_idx == 0 {
                    eprintln!(
                        "    CRC OK! block={} fib={} header={:02x}",
                        block_idx, fib_idx, fic_bytes[fib_start]
                    );
                }
            }
        }
    }

    crc_ok_count
}

#[test]
fn ofdm_pipeline_fic_decode() {
    // Prefer the 2-minute file, fall back to the 5-second one.
    let iq_path = if std::path::Path::new(IQ_FILE).exists() {
        IQ_FILE
    } else if std::path::Path::new(IQ_FILE_SHORT).exists() {
        IQ_FILE_SHORT
    } else {
        eprintln!("Skipping: no IQ file found");
        return;
    };

    let samples = read_iq_file(iq_path);
    eprintln!(
        "Loaded {} IQ samples ({:.1} s) from {}",
        samples.len(),
        samples.len() as f64 / SAMPLE_RATE as f64,
        iq_path
    );

    let mut ofdm = OfdmProcessor::new();
    let chunk_size = 65536;
    let viterbi = fec::ViterbiDecoder::new(35);
    let prbs_768 = generate_prbs(768);

    let mut total_frames = 0;
    let mut total_crc_ok = 0;
    let mut total_fibs_checked = 0;

    // Limit to first 20 seconds to keep test runtime reasonable.
    let max_samples = 20 * SAMPLE_RATE as usize;
    let sample_limit = samples.len().min(max_samples);

    for chunk_start in (0..sample_limit).step_by(chunk_size) {
        let chunk_end = (chunk_start + chunk_size).min(sample_limit);
        let chunk = &samples[chunk_start..chunk_end];

        for frame in ofdm.push_samples(chunk) {
            total_frames += 1;

            // Collect FIC symbols (first 3 data symbols).
            let fic_syms: Vec<Vec<f32>> = frame.soft_bits.iter().take(3).cloned().collect();

            if fic_syms.len() < 3 {
                continue;
            }

            // Try normal polarity.
            let ok = decode_fic_frame(&fic_syms, &viterbi, &prbs_768, false);
            if ok > 0 {
                total_crc_ok += ok;
                total_fibs_checked += 12;
                continue;
            }

            // Try negated polarity.
            let ok_neg = decode_fic_frame(&fic_syms, &viterbi, &prbs_768, true);
            total_crc_ok += ok_neg;
            total_fibs_checked += 12;
        }
    }

    eprintln!(
        "\nResults: {} frames, {}/{} FIB CRCs OK",
        total_frames, total_crc_ok, total_fibs_checked
    );

    if total_crc_ok > 0 {
        eprintln!("SUCCESS: FIC decoding works!");
    } else {
        eprintln!("FAIL: No CRC matches found");

        // Additional debug: try with split layout [Re..., Im...] like welle.io
        eprintln!("\n=== Trying split layout (welle.io style) ===");
        let mut ofdm2 = OfdmProcessor::new();
        let mut split_crc_ok = 0;

        for chunk_start in (0..sample_limit).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(sample_limit);
            let chunk = &samples[chunk_start..chunk_end];

            for frame in ofdm2.push_samples(chunk) {
                let fic_syms: Vec<Vec<f32>> = frame.soft_bits.iter().take(3).cloned().collect();
                if fic_syms.len() < 3 {
                    continue;
                }

                // Convert interleaved [Im, Re, Im, Re, ...] to split [Re..., Im...]
                let split_syms: Vec<Vec<f32>> = fic_syms
                    .iter()
                    .map(|sym| {
                        let re: Vec<f32> = sym.iter().skip(1).step_by(2).copied().collect();
                        let im: Vec<f32> = sym.iter().step_by(2).copied().collect();
                        let mut split = Vec::with_capacity(sym.len());
                        split.extend_from_slice(&re);
                        split.extend_from_slice(&im);
                        split
                    })
                    .collect();

                for &negate in &[false, true] {
                    let ok = decode_fic_frame(&split_syms, &viterbi, &prbs_768, negate);
                    if ok > 0 {
                        split_crc_ok += ok;
                        eprintln!("  Split layout negate={}: {} CRCs OK", negate, ok);
                        break;
                    }
                }
            }
        }

        if split_crc_ok > 0 {
            eprintln!("Split layout works! {} total CRCs OK", split_crc_ok);
        } else {
            eprintln!("Split layout also failed");

            // Try swapped: [Re, Im, Re, Im, ...] instead of [Im, Re, ...]
            eprintln!("\n=== Trying swapped pairs ===");
            let mut ofdm3 = OfdmProcessor::new();
            let mut swap_crc_ok = 0;

            for chunk_start in (0..sample_limit).step_by(chunk_size) {
                let chunk_end = (chunk_start + chunk_size).min(sample_limit);
                let chunk = &samples[chunk_start..chunk_end];

                for frame in ofdm3.push_samples(chunk) {
                    let fic_syms: Vec<Vec<f32>> = frame.soft_bits.iter().take(3).cloned().collect();
                    if fic_syms.len() < 3 {
                        continue;
                    }

                    // Swap pairs: [b1, b0, b3, b2, ...]
                    let swapped_syms: Vec<Vec<f32>> = fic_syms
                        .iter()
                        .map(|sym| {
                            sym.chunks_exact(2)
                                .flat_map(|pair| [pair[1], pair[0]])
                                .collect()
                        })
                        .collect();

                    for &negate in &[false, true] {
                        let ok = decode_fic_frame(&swapped_syms, &viterbi, &prbs_768, negate);
                        if ok > 0 {
                            swap_crc_ok += ok;
                            eprintln!("  Swapped pairs negate={}: {} CRCs OK", negate, ok);
                            break;
                        }
                    }
                }
            }

            if swap_crc_ok > 0 {
                eprintln!("Swapped pairs works! {} total CRCs OK", swap_crc_ok);
            }
        }
    }
}
