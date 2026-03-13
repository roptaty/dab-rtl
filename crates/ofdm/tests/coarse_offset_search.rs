//! Test: manually try all coarse frequency offsets on real IQ data.
//!
//! Uses OfdmProcessor's FrameSync for reliable frame detection, then
//! manually processes FIC symbols with each coarse offset to find
//! the one that produces valid CRCs.

use num_complex::Complex32;
use ofdm::params::*;
use ofdm::{FrameSync, FreqDeinterleaver};
use rustfft::{num_complex::Complex, FftPlanner};
use std::f32::consts::PI;
use std::sync::Arc;

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

fn prbs(len: usize) -> Vec<u8> {
    let mut reg: u16 = 0x1FF;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let bit = ((reg >> 8) ^ (reg >> 4)) & 1;
        out.push(bit as u8);
        reg = ((reg << 1) | bit) & 0x1FF;
    }
    out
}

fn guard_corr(buf: &[Complex32], start: usize) -> f32 {
    if start + SYMBOL_SIZE > buf.len() {
        return 0.0;
    }
    let sym = &buf[start..start + SYMBOL_SIZE];
    let mut corr = Complex32::new(0.0, 0.0);
    let mut power = 0.0f32;
    for n in 0..GUARD_SIZE {
        corr += sym[n + FFT_SIZE] * sym[n].conj();
        power += sym[n].norm_sqr() + sym[n + FFT_SIZE].norm_sqr();
    }
    if power > 0.0 {
        corr.norm() / (power / 2.0)
    } else {
        0.0
    }
}

fn refine_prs(buf: &[Complex32], raw_offset: usize) -> usize {
    let mut best_pos = raw_offset;
    let mut best_corr = 0.0f32;
    let start = raw_offset.saturating_sub(512);
    let end = (raw_offset + 64).min(buf.len().saturating_sub(SYMBOL_SIZE));
    for p in (start..=end).step_by(4) {
        let c = guard_corr(buf, p);
        if c > best_corr {
            best_corr = c;
            best_pos = p;
        }
    }
    for p in best_pos.saturating_sub(4)..=(best_pos + 4).min(end) {
        let c = guard_corr(buf, p);
        if c > best_corr {
            best_corr = c;
            best_pos = p;
        }
    }
    best_pos
}

fn do_fft(
    fft: &Arc<dyn rustfft::Fft<f32>>,
    symbol_samples: &[Complex32],
    fine_offset: f32,
) -> Vec<Complex<f32>> {
    let start = GUARD_SIZE;
    let window = &symbol_samples[start..start + FFT_SIZE];
    let phase_step = -2.0 * PI * fine_offset / FFT_SIZE as f32;
    let mut buf: Vec<Complex<f32>> = window
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            if fine_offset.abs() > 1e-6 {
                let phase = phase_step * i as f32;
                let c = Complex32::new(phase.cos(), phase.sin());
                let r = s * c;
                Complex::new(r.re, r.im)
            } else {
                Complex::new(s.re, s.im)
            }
        })
        .collect();
    fft.process(&mut buf);
    buf
}

fn estimate_fine_freq(symbol_samples: &[Complex32]) -> f32 {
    let mut corr = Complex32::new(0.0, 0.0);
    for n in 0..GUARD_SIZE {
        corr += symbol_samples[n + FFT_SIZE] * symbol_samples[n].conj();
    }
    corr.arg() / (2.0 * PI)
}

fn extract_carriers(fft_buf: &[Complex<f32>], offset: i32) -> Vec<Complex32> {
    let mut carriers = Vec::with_capacity(NUM_CARRIERS);
    for k in CARRIER_MIN..=CARRIER_MAX {
        if k == 0 {
            continue;
        }
        let base_bin = carrier_to_fft_bin(k) as i32;
        let bin = ((base_bin + offset + FFT_SIZE as i32) as usize) % FFT_SIZE;
        let c = fft_buf[bin];
        carriers.push(Complex32::new(c.re, c.im));
    }
    carriers
}

/// Try decoding FIC from one symbol with given parameters.
#[allow(clippy::too_many_arguments)]
fn try_decode_symbol(
    prs_fft: &[Complex<f32>],
    data_fft: &[Complex<f32>],
    offset: i32,
    fine_offset: f32,
    deinterleaver: &FreqDeinterleaver,
    viterbi: &fec::ViterbiDecoder,
    prbs_bits: &[u8],
    negate: bool,
) -> (usize, f64) {
    let residual_phase = 2.0 * PI * fine_offset * SYMBOL_SIZE as f32 / FFT_SIZE as f32;
    let correction = Complex32::new((-residual_phase).cos(), (-residual_phase).sin());

    let prs_carriers = extract_carriers(prs_fft, offset);
    let data_carriers = extract_carriers(data_fft, offset);

    let mut raw_bits = Vec::with_capacity(NUM_CARRIERS * 2);
    let mut dqpsk_sum = 0.0f64;
    for (&cur, &prev) in data_carriers.iter().zip(prs_carriers.iter()) {
        let z = (cur * prev.conj()) * correction;
        let (b0, b1) = if negate { (-z.im, -z.re) } else { (z.im, z.re) };
        raw_bits.push(b0);
        raw_bits.push(b1);
        let angle = z.im.atan2(z.re);
        dqpsk_sum += (4.0 * angle as f64).cos();
    }
    let metric = dqpsk_sum / NUM_CARRIERS as f64;

    let re_ch: Vec<f32> = raw_bits.iter().step_by(2).copied().collect();
    let im_ch: Vec<f32> = raw_bits.iter().skip(1).step_by(2).copied().collect();
    let re_di = deinterleaver.deinterleave(&re_ch);
    let im_di = deinterleaver.deinterleave(&im_ch);

    let mut soft: Vec<f32> = Vec::with_capacity(NUM_CARRIERS * 2);
    for (r, i) in re_di.into_iter().zip(im_di.into_iter()) {
        soft.push(r);
        soft.push(i);
    }

    let max_abs = soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    if max_abs > 0.0 {
        let scale = 1.0 / max_abs;
        for v in soft.iter_mut() {
            *v *= scale;
        }
    }

    soft.extend(std::iter::repeat_n(0.0f32, 24));
    let decoded = viterbi.decode(&soft);
    let info = &decoded[..768];
    let fic_bytes = pack_bits(info);

    let mut ok = 0;
    for fib_idx in 0..3 {
        let start = fib_idx * 32;
        if start + 32 <= fic_bytes.len() {
            let mut fib = fic_bytes[start..start + 32].to_vec();
            for (i, byte) in fib.iter_mut().enumerate() {
                let mut mask = 0u8;
                for bit in 0..8 {
                    let idx = fib_idx * 256 + i * 8 + bit;
                    if idx < prbs_bits.len() && prbs_bits[idx] != 0 {
                        mask |= 0x80 >> bit;
                    }
                }
                *byte ^= mask;
            }
            if fib_crc_ok(&fib) {
                ok += 1;
            }
        }
    }
    (ok, metric)
}

#[test]
fn brute_force_coarse_offset() {
    if !std::path::Path::new(IQ_FILE).exists() {
        eprintln!("Skipping: IQ file not found");
        return;
    }

    let samples = read_iq_file(IQ_FILE);
    eprintln!(
        "Loaded {} samples ({:.1} ms)",
        samples.len(),
        samples.len() as f64 / SAMPLE_RATE as f64 * 1000.0
    );

    // Use FrameSync to find frames
    let mut sync = FrameSync::new();
    let mut frame_starts = Vec::new();
    if let Some(fs) = sync.push_samples(&samples) {
        let refined = refine_prs(&samples, fs.sample_offset);
        frame_starts.push(refined);
        eprintln!(
            "Frame 1: PRS at {} (raw {}), guard_corr={:.4}",
            refined,
            fs.sample_offset,
            guard_corr(&samples, refined)
        );
    }

    if frame_starts.is_empty() {
        eprintln!("No frames found!");
        return;
    }

    let prs_start = frame_starts[0];
    let needed = prs_start + 4 * SYMBOL_SIZE;
    if samples.len() < needed {
        eprintln!("Not enough samples for frame");
        return;
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    let prs_samples = &samples[prs_start..prs_start + SYMBOL_SIZE];
    let fine_offset = estimate_fine_freq(prs_samples);
    eprintln!(
        "Fine freq offset: {fine_offset:.4} ({:.1} Hz)",
        fine_offset * SAMPLE_RATE as f32 / FFT_SIZE as f32
    );

    let prs_fft = do_fft(&fft, prs_samples, fine_offset);

    // Show PRS FFT power spectrum at edges to understand the signal
    eprintln!("\nPRS power at key bins:");
    for k in [-770, -769, -768, -767, -1, 0, 1, 767, 768, 769, 770] {
        let bin = ((k + FFT_SIZE as i32) as usize) % FFT_SIZE;
        let c = &prs_fft[bin];
        let power = c.re * c.re + c.im * c.im;
        eprintln!("  k={k:+5} (bin {bin:4}): power={power:.2}");
    }

    let deinterleaver = FreqDeinterleaver::new();
    let viterbi = fec::ViterbiDecoder::new(35);
    let prbs_bits = prbs(768);

    // Process first data symbol with all offsets
    let data_start = prs_start + SYMBOL_SIZE;
    let data_samples = &samples[data_start..data_start + SYMBOL_SIZE];
    let data_fft = do_fft(&fft, data_samples, fine_offset);

    eprintln!("\n=== Trying all coarse offsets (normal polarity) ===");
    let mut best_offset = 0i32;
    let mut best_ok = 0usize;

    for offset in -30i32..=30 {
        let (ok, metric) = try_decode_symbol(
            &prs_fft,
            &data_fft,
            offset,
            fine_offset,
            &deinterleaver,
            &viterbi,
            &prbs_bits,
            false,
        );
        if ok > 0 || offset % 5 == 0 {
            eprintln!("  offset={offset:+3}: metric={metric:.4}, CRC={ok}/3");
        }
        if ok > best_ok {
            best_ok = ok;
            best_offset = offset;
        }
    }
    eprintln!("Best: offset={best_offset}, {best_ok} CRC OK");

    eprintln!("\n=== Trying all coarse offsets (negated polarity) ===");
    for offset in -30i32..=30 {
        let (ok, metric) = try_decode_symbol(
            &prs_fft,
            &data_fft,
            offset,
            fine_offset,
            &deinterleaver,
            &viterbi,
            &prbs_bits,
            true,
        );
        if ok > 0 || offset % 5 == 0 {
            eprintln!("  offset={offset:+3}: metric={metric:.4}, CRC={ok}/3");
        }
        if ok > best_ok {
            best_ok = ok;
            best_offset = offset;
        }
    }
    eprintln!("Best overall: offset={best_offset}, {best_ok} CRC OK");

    // Diagnostic: re-encode the decoded bits and compare with soft bits
    eprintln!("\n=== Re-encode diagnostic ===");
    {
        let residual_phase = 2.0 * PI * fine_offset * SYMBOL_SIZE as f32 / FFT_SIZE as f32;
        let correction = Complex32::new((-residual_phase).cos(), (-residual_phase).sin());
        let prs_carriers = extract_carriers(&prs_fft, 0);
        let data_carriers = extract_carriers(&data_fft, 0);

        let mut raw_bits = Vec::with_capacity(NUM_CARRIERS * 2);
        for (&cur, &prev) in data_carriers.iter().zip(prs_carriers.iter()) {
            let z = (cur * prev.conj()) * correction;
            raw_bits.push(z.im);
            raw_bits.push(z.re);
        }

        let re_ch: Vec<f32> = raw_bits.iter().step_by(2).copied().collect();
        let im_ch: Vec<f32> = raw_bits.iter().skip(1).step_by(2).copied().collect();
        let re_di = deinterleaver.deinterleave(&re_ch);
        let im_di = deinterleaver.deinterleave(&im_ch);

        let mut soft: Vec<f32> = Vec::with_capacity(NUM_CARRIERS * 2);
        for (r, i) in re_di.into_iter().zip(im_di.into_iter()) {
            soft.push(r);
            soft.push(i);
        }

        // Hard-decide soft bits
        let hard: Vec<u8> = soft.iter().map(|&v| if v < 0.0 { 1 } else { 0 }).collect();

        let max_abs = soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
        let normalized: Vec<f32> = soft.iter().map(|v| v * scale).collect();

        // Viterbi decode
        let mut padded = normalized.clone();
        padded.extend(std::iter::repeat_n(0.0f32, 24));
        let decoded = viterbi.decode(&padded);
        let info = &decoded[..768];

        // Re-encode the decoded bits
        let g: [u8; 4] = [109, 79, 83, 109];
        let mut state: u8 = 0;
        let mut reencoded = Vec::with_capacity(768 * 4);
        for &b in info {
            for &poly in &g {
                let reg = ((state as u16) << 1) | (b as u16);
                let xored = reg as u8 & poly;
                reencoded.push(xored.count_ones() as u8 & 1);
            }
            state = ((state << 1) | b) & 63;
        }

        // Compare re-encoded with hard-decided soft bits
        let hard_match: usize = reencoded
            .iter()
            .zip(hard.iter())
            .map(|(&a, &b)| if a == b { 1 } else { 0 })
            .sum();
        eprintln!(
            "Re-encoded vs hard-decided: {hard_match}/3072 match ({:.1}%)",
            100.0 * hard_match as f64 / 3072.0
        );

        // Compare re-encoded with soft bit signs
        let soft_match: usize = reencoded
            .iter()
            .zip(soft.iter())
            .map(|(a, v)| {
                let expected_positive = *a == 0;
                let actual_positive = *v >= 0.0;
                if expected_positive == actual_positive {
                    1
                } else {
                    0
                }
            })
            .sum();
        eprintln!(
            "Re-encoded vs soft sign: {soft_match}/3072 match ({:.1}%)",
            100.0 * soft_match as f64 / 3072.0
        );

        // Show soft bit distribution
        let positive_count = soft.iter().filter(|&&v| v > 0.0).count();
        let near_zero = soft.iter().filter(|&&v| v.abs() < max_abs * 0.1).count();
        eprintln!("Soft bits: {positive_count} positive, {} negative, {near_zero} near zero (±10% of max)",
            soft.len() - positive_count);
        eprintln!(
            "Soft bit range: {:.4} to {:.4} (max_abs={max_abs:.4})",
            soft.iter().cloned().fold(f32::INFINITY, f32::min),
            soft.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
        );

        // Also check WITHOUT deinterleaving
        eprintln!("\n--- Without deinterleaving ---");
        let mut soft_nodi: Vec<f32> = raw_bits.clone();
        let max_abs_nd = soft_nodi.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        let scale_nd = if max_abs_nd > 0.0 {
            1.0 / max_abs_nd
        } else {
            1.0
        };
        for v in soft_nodi.iter_mut() {
            *v *= scale_nd;
        }
        let mut padded_nd = soft_nodi.clone();
        padded_nd.extend(std::iter::repeat_n(0.0f32, 24));
        let decoded_nd = viterbi.decode(&padded_nd);
        let info_nd = &decoded_nd[..768];

        // Re-encode
        state = 0;
        let mut reenc_nd = Vec::with_capacity(768 * 4);
        for &b in info_nd {
            for &poly in &g {
                let reg = ((state as u16) << 1) | (b as u16);
                let xored = reg as u8 & poly;
                reenc_nd.push(xored.count_ones() as u8 & 1);
            }
            state = ((state << 1) | b) & 63;
        }
        let hard_nd: Vec<u8> = raw_bits
            .iter()
            .map(|&v| if v < 0.0 { 1 } else { 0 })
            .collect();
        let match_nd: usize = reenc_nd
            .iter()
            .zip(hard_nd.iter())
            .map(|(&a, &b)| if a == b { 1 } else { 0 })
            .sum();
        eprintln!(
            "Re-encoded vs hard (no deinterleave): {match_nd}/3072 ({:.1}%)",
            100.0 * match_nd as f64 / 3072.0
        );

        // Check FIBs without deinterleaving
        let fic_bytes_nd = pack_bits(info_nd);
        for fib_idx in 0..3 {
            let start = fib_idx * 32;
            if start + 32 <= fic_bytes_nd.len() {
                let mut fib = fic_bytes_nd[start..start + 32].to_vec();
                for (i, byte) in fib.iter_mut().enumerate() {
                    let mut mask = 0u8;
                    for bit in 0..8 {
                        let idx = fib_idx * 256 + i * 8 + bit;
                        if idx < prbs_bits.len() && prbs_bits[idx] != 0 {
                            mask |= 0x80 >> bit;
                        }
                    }
                    *byte ^= mask;
                }
                if fib_crc_ok(&fib) {
                    eprintln!("  *** FIB{fib_idx} CRC OK (no deinterleave)! ***");
                }
            }
        }
    }

    // Try IQ variants: swap I/Q, conjugate, etc. at the raw sample level
    eprintln!("\n=== Trying IQ variants ===");
    #[allow(clippy::type_complexity)]
    let iq_variants: Vec<(&str, Box<dyn Fn(Complex32) -> Complex32>)> = vec![
        ("normal", Box::new(|s| s)),
        ("conjugate", Box::new(|s: Complex32| s.conj())),
        (
            "iq_swap",
            Box::new(|s: Complex32| Complex32::new(s.im, s.re)),
        ),
        (
            "neg_q",
            Box::new(|s: Complex32| Complex32::new(s.re, -s.im)),
        ),
        (
            "neg_i",
            Box::new(|s: Complex32| Complex32::new(-s.re, s.im)),
        ),
        (
            "neg_both",
            Box::new(|s: Complex32| Complex32::new(-s.re, -s.im)),
        ),
    ];

    for (name, transform) in &iq_variants {
        // Transform the raw samples
        let t_prs: Vec<Complex32> = samples[prs_start..prs_start + SYMBOL_SIZE]
            .iter()
            .map(|&s| transform(s))
            .collect();
        let t_data: Vec<Complex32> = samples[data_start..data_start + SYMBOL_SIZE]
            .iter()
            .map(|&s| transform(s))
            .collect();

        let t_fine = estimate_fine_freq(&t_prs);
        let t_prs_fft = do_fft(&fft, &t_prs, t_fine);
        let t_data_fft = do_fft(&fft, &t_data, t_fine);

        let mut found = false;
        for offset in -10i32..=10 {
            for &neg in &[false, true] {
                let (ok, metric) = try_decode_symbol(
                    &t_prs_fft,
                    &t_data_fft,
                    offset,
                    t_fine,
                    &deinterleaver,
                    &viterbi,
                    &prbs_bits,
                    neg,
                );
                if ok > 0 {
                    eprintln!("  *** {name} offset={offset} neg={neg}: CRC={ok}/3, metric={metric:.4} ***");
                    found = true;
                }
            }
        }
        if !found {
            // Just show the best metric
            let mut best_m = 0.0f64;
            for offset in -5i32..=5 {
                let (_, metric) = try_decode_symbol(
                    &t_prs_fft,
                    &t_data_fft,
                    offset,
                    t_fine,
                    &deinterleaver,
                    &viterbi,
                    &prbs_bits,
                    false,
                );
                if metric.abs() > best_m.abs() {
                    best_m = metric;
                }
            }
            eprintln!("  {name}: 0 CRC OK (best metric={best_m:.4}, fine={t_fine:.4})");
        }
    }

    // Also try second and third data symbols
    for sym_idx in 1..3 {
        let sym_start = prs_start + (sym_idx + 1) * SYMBOL_SIZE;
        if sym_start + SYMBOL_SIZE > samples.len() {
            break;
        }
        let prev_start = prs_start + sym_idx * SYMBOL_SIZE;
        let prev_samples = &samples[prev_start..prev_start + SYMBOL_SIZE];
        let sym_samples = &samples[sym_start..sym_start + SYMBOL_SIZE];
        let prev_fft = do_fft(&fft, prev_samples, fine_offset);
        let sym_fft = do_fft(&fft, sym_samples, fine_offset);

        eprintln!(
            "\n=== Symbol {sym_idx} (diff ref: symbol {}) ===",
            sym_idx - 1
        );
        for offset in -30i32..=30 {
            for &neg in &[false, true] {
                let (ok, metric) = try_decode_symbol(
                    &prev_fft,
                    &sym_fft,
                    offset,
                    fine_offset,
                    &deinterleaver,
                    &viterbi,
                    &prbs_bits,
                    neg,
                );
                if ok > 0 {
                    eprintln!(
                        "  *** offset={offset:+3}, neg={neg}: metric={metric:.4}, CRC={ok}/3 ***"
                    );
                    best_ok = best_ok.max(ok);
                }
            }
        }
    }

    eprintln!("\nFinal best: {best_ok} CRC OK");
}
