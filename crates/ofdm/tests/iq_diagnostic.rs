//! Diagnostic test using real IQ samples captured from RTL-SDR.
//!
//! Run with:
//!   cargo test -p ofdm --test iq_diagnostic -- --nocapture

use num_complex::Complex32;
use ofdm::params::*;
use ofdm::FreqDeinterleaver;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use std::f32::consts::PI;

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

fn do_fft(
    fft: &dyn rustfft::Fft<f32>,
    symbol: &[Complex32],
    fine_offset: f32,
) -> Vec<Complex<f32>> {
    let start = if symbol.len() >= FFT_SIZE + GUARD_SIZE {
        GUARD_SIZE
    } else {
        0
    };
    let window = &symbol[start..];
    let window = &window[..FFT_SIZE.min(window.len())];

    let mut buf = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let phase_step = -2.0 * PI * fine_offset / FFT_SIZE as f32;
    for (i, (dst, &src)) in buf.iter_mut().zip(window.iter()).enumerate() {
        if fine_offset.abs() > 1e-6 {
            let phase = phase_step * i as f32;
            let correction = Complex32::new(phase.cos(), phase.sin());
            let corrected = src * correction;
            *dst = Complex::new(corrected.re, corrected.im);
        } else {
            *dst = Complex::new(src.re, src.im);
        }
    }

    fft.process(&mut buf);
    buf
}

fn extract_carriers(buf: &[Complex<f32>], offset: i32) -> Vec<Complex32> {
    let mut carriers = Vec::with_capacity(NUM_CARRIERS);
    for k in CARRIER_MIN..=CARRIER_MAX {
        if k == 0 {
            continue;
        }
        let base_bin = carrier_to_fft_bin(k) as i32;
        let bin = ((base_bin + offset + FFT_SIZE as i32) as usize) % FFT_SIZE;
        let c = buf[bin];
        carriers.push(Complex32::new(c.re, c.im));
    }
    carriers
}

fn dqpsk_metric(current: &[Complex32], previous: &[Complex32]) -> f64 {
    let mut sum = 0.0f64;
    let mut count = 0u32;
    for (&cur, &prev) in current.iter().zip(previous.iter()) {
        let z = cur * prev.conj();
        let angle = z.arg();
        sum += (4.0 * angle as f64).cos();
        count += 1;
    }
    sum / count as f64
}

fn dqpsk_metric_with_rotation(
    current: &[Complex32],
    previous: &[Complex32],
    correction: Complex32,
) -> f64 {
    let mut sum = 0.0f64;
    let mut count = 0u32;
    for (&cur, &prev) in current.iter().zip(previous.iter()) {
        let z = (cur * prev.conj()) * correction;
        let angle = z.arg();
        sum += (4.0 * angle as f64).cos();
        count += 1;
    }
    sum / count as f64
}

/// DAB energy dispersal PRBS: polynomial x^9 + x^5 + 1, all-ones init.
/// XOR with this sequence to undo dispersal on decoded FIB bytes.
fn energy_dispersal_prbs(len: usize) -> Vec<u8> {
    let mut reg: u16 = 0x1FF; // 9-bit register, all ones
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let bit = ((reg >> 8) ^ (reg >> 4)) & 1;
        out.push(bit as u8);
        reg = ((reg << 1) | bit) & 0x1FF;
    }
    out
}

/// XOR bytes with PRBS bit sequence for energy de-dispersal.
fn xor_with_prbs(data: &[u8], prbs: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for (i, &byte) in data.iter().enumerate() {
        let mut mask = 0u8;
        for bit in 0..8 {
            let prbs_idx = i * 8 + bit;
            if prbs_idx < prbs.len() && prbs[prbs_idx] != 0 {
                mask |= 0x80 >> bit;
            }
        }
        out.push(byte ^ mask);
    }
    out
}

/// Compute guard interval correlation magnitude at a given sample position.
fn guard_correlation(samples: &[Complex32], start: usize) -> f32 {
    if start + SYMBOL_SIZE > samples.len() {
        return 0.0;
    }
    let sym = &samples[start..start + SYMBOL_SIZE];
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

/// Verify encode→decode round trip for 768-bit FIC-like data.
#[test]
fn fic_round_trip() {
    const G: [u8; 4] = [109, 79, 83, 109];

    // Create a known FIB pattern (3 FIBs = 96 bytes = 768 bits)
    let mut fib_data = vec![0u8; 96];
    // Fill with some recognizable pattern
    for (i, b) in fib_data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(37).wrapping_add(17);
    }

    // Convert to bits (MSB first)
    let mut info_bits = Vec::with_capacity(768);
    for &byte in &fib_data {
        for bit in (0..8).rev() {
            info_bits.push((byte >> bit) & 1);
        }
    }
    assert_eq!(info_bits.len(), 768);

    // Append 6 tail bits (zeros to flush encoder)
    let mut input_bits = info_bits.clone();
    input_bits.extend(std::iter::repeat_n(0u8, 6));
    assert_eq!(input_bits.len(), 774);

    // Convolutional encode (rate 1/4)
    let mut state: u8 = 0;
    let mut coded = Vec::with_capacity(774 * 4);
    for &bit in &input_bits {
        let next_state = ((state << 1) | bit) & 63;
        for &poly in &G {
            let reg = ((state as u16) << 1) | (bit as u16);
            let xored = reg as u8 & poly;
            let out_bit = xored.count_ones() as u8 & 1;
            coded.push(if out_bit == 0 { 1.0f32 } else { -1.0f32 });
        }
        state = next_state;
    }
    assert_eq!(coded.len(), 3096);
    eprintln!(
        "Encoded: {} coded bits, encoder final state: {state}",
        coded.len()
    );

    // Truncate last 24 coded bits (tail) to get 3072 transmitted bits
    let transmitted = &coded[..3072];

    // Decode: append 24 tail erasures
    let mut padded = transmitted.to_vec();
    padded.extend(std::iter::repeat_n(0.0f32, 24));
    assert_eq!(padded.len(), 3096);

    let decoder = fec::ViterbiDecoder::new(35);
    let decoded = decoder.decode(&padded);
    assert_eq!(decoded.len(), 774);

    // Compare first 768 decoded bits to original
    let decoded_info = &decoded[..768];
    let mut mismatches = 0;
    for (i, (&dec, &orig)) in decoded_info.iter().zip(info_bits.iter()).enumerate() {
        if dec != orig {
            mismatches += 1;
            if mismatches <= 10 {
                eprintln!("  Bit {i}: decoded={dec}, expected={orig}");
            }
        }
    }
    eprintln!("Round trip: {mismatches}/768 bit errors");

    // Pack decoded bits to bytes and compare
    let decoded_bytes = pack_bits(decoded_info);
    assert_eq!(decoded_bytes.len(), 96);
    assert_eq!(decoded_bytes, fib_data, "FIC round trip failed!");
    eprintln!("FIC round trip: PASS (768 bits, 96 bytes, 0 errors)");
}

#[test]
fn diagnose_fic_decoding() {
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

    // ── Step 1: Find symbol boundaries via guard interval correlation ──
    // Scan with a coarse step, then refine
    eprintln!("\n=== Finding symbol boundaries via guard interval correlation ===");

    // First, compute running power to find the approximate signal region
    let block_size = 2048;
    let mut powers: Vec<f64> = Vec::new();
    for i in (0..samples.len() - block_size).step_by(block_size) {
        let p: f64 = samples[i..i + block_size]
            .iter()
            .map(|s| s.norm_sqr() as f64)
            .sum::<f64>()
            / block_size as f64;
        powers.push(p);
    }
    let avg_power: f64 = powers.iter().sum::<f64>() / powers.len() as f64;
    eprintln!("Average power: {avg_power:.6}");

    // Find null symbols: blocks with power < 5% of average, at least NULL_SIZE/block_size consecutive
    let null_threshold = avg_power * 0.05;
    let min_null_blocks = NULL_SIZE / block_size; // ~1 block
    let mut null_starts = Vec::new();
    let mut consecutive_low = 0;
    let mut low_start = 0;

    for (i, &p) in powers.iter().enumerate() {
        if p < null_threshold {
            if consecutive_low == 0 {
                low_start = i;
            }
            consecutive_low += 1;
        } else {
            if consecutive_low >= min_null_blocks {
                null_starts.push(low_start * block_size);
            }
            consecutive_low = 0;
        }
    }

    eprintln!("Found {} power dips (null candidates)", null_starts.len());
    for (i, &pos) in null_starts.iter().take(10).enumerate() {
        eprintln!(
            "  Null {i}: sample={pos} ({:.1} ms)",
            pos as f64 / SAMPLE_RATE as f64 * 1000.0
        );
    }

    // For each null candidate, find the exact PRS start using guard correlation
    let mut best_prs_start = 0;
    let mut best_prs_corr = 0.0f32;

    for &null_pos in &null_starts {
        // PRS should start approximately NULL_SIZE after the null start
        let approx_prs = null_pos + NULL_SIZE;
        // Search ±500 samples around the estimated position
        for delta in (-500i32..=500).step_by(4) {
            let start = (approx_prs as i32 + delta) as usize;
            if start + SYMBOL_SIZE > samples.len() {
                continue;
            }
            let corr = guard_correlation(&samples, start);
            if corr > best_prs_corr {
                best_prs_corr = corr;
                best_prs_start = start;
            }
        }
    }

    // Fine-tune with single-sample precision
    let coarse_best = best_prs_start;
    for delta in -4i32..=4 {
        let start = (coarse_best as i32 + delta) as usize;
        if start + SYMBOL_SIZE > samples.len() {
            continue;
        }
        let corr = guard_correlation(&samples, start);
        if corr > best_prs_corr {
            best_prs_corr = corr;
            best_prs_start = start;
        }
    }

    eprintln!(
        "\nBest PRS start: sample={best_prs_start} ({:.1} ms), guard_corr={best_prs_corr:.4}",
        best_prs_start as f64 / SAMPLE_RATE as f64 * 1000.0
    );

    // Verify: check guard correlation for the next few symbols too
    for sym in 0..5 {
        let pos = best_prs_start + sym * SYMBOL_SIZE;
        if pos + SYMBOL_SIZE <= samples.len() {
            let corr = guard_correlation(&samples, pos);
            eprintln!("  Symbol {sym}: corr={corr:.4}");
        }
    }

    if best_prs_corr < 0.3 {
        eprintln!(
            "\nWARNING: Guard correlation too low ({best_prs_corr:.4}). No OFDM symbols found."
        );
        eprintln!("This might mean the signal is too weak or the sample rate is wrong.");

        // As a last resort, scan the ENTIRE recording with coarser step
        eprintln!("\n=== Full recording scan (step=128) ===");
        let mut top_corrs: Vec<(usize, f32)> = Vec::new();
        for start in (0..samples.len().saturating_sub(SYMBOL_SIZE)).step_by(128) {
            let corr = guard_correlation(&samples, start);
            if corr > 0.3 {
                top_corrs.push((start, corr));
            }
        }
        top_corrs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        eprintln!("Positions with guard_corr > 0.3: {}", top_corrs.len());
        for &(pos, corr) in top_corrs.iter().take(20) {
            eprintln!(
                "  sample={pos} ({:.1} ms): corr={corr:.4}",
                pos as f64 / SAMPLE_RATE as f64 * 1000.0
            );
        }

        if top_corrs.is_empty() {
            eprintln!("\nNo OFDM symbol boundaries found in entire recording!");
            eprintln!("Possible causes:");
            eprintln!("  1. Wrong center frequency");
            eprintln!("  2. No DAB signal present");
            eprintln!("  3. Sample rate mismatch");
            eprintln!("  4. RTL-SDR issue (AGC, gain)");
            return;
        }

        best_prs_start = top_corrs[0].0;
        best_prs_corr = top_corrs[0].1;
        eprintln!("\nUsing best position: {best_prs_start}, corr={best_prs_corr:.4}");
    }

    let prs_start = best_prs_start;
    let needed = prs_start + FRAME_SYMBOLS * SYMBOL_SIZE;
    if needed > samples.len() {
        eprintln!("Not enough samples after PRS for full frame");
        return;
    }

    // ── Step 2: Fine frequency estimation ──
    let prs_symbol = &samples[prs_start..prs_start + SYMBOL_SIZE];
    let data_sym0 = &samples[prs_start + SYMBOL_SIZE..prs_start + 2 * SYMBOL_SIZE];

    let mut fine_corr = Complex32::new(0.0, 0.0);
    for n in 0..GUARD_SIZE {
        fine_corr += prs_symbol[n + FFT_SIZE] * prs_symbol[n].conj();
    }
    let fine_offset = fine_corr.arg() / (2.0 * PI);
    eprintln!(
        "\nFine frequency offset: {fine_offset:.4} bins ({:.1} Hz)",
        fine_offset * SAMPLE_RATE as f32 / FFT_SIZE as f32
    );

    // ── Step 3: FFT and DQPSK metric ──
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    // The residual inter-symbol phase from fine frequency correction:
    // After per-sample correction exp(-j*2π*ε*n/N), the FFT output still has
    // a symbol-dependent constant phase exp(j*2π*ε*(S_m+G)/N).
    // The differential product gets a residual: exp(j*2π*ε*SYMBOL_SIZE/FFT_SIZE).
    let residual_phase = 2.0 * PI * fine_offset * SYMBOL_SIZE as f32 / FFT_SIZE as f32;
    let residual_correction = Complex32::new((-residual_phase).cos(), (-residual_phase).sin());
    eprintln!(
        "\nResidual inter-symbol phase: {residual_phase:.4} rad ({:.1}°)",
        residual_phase.to_degrees()
    );

    let prs_fft = do_fft(fft.as_ref(), prs_symbol, fine_offset);
    let data_fft = do_fft(fft.as_ref(), data_sym0, fine_offset);

    // Also try with conjugated samples (E4000 spectrum inversion)
    let prs_conj: Vec<Complex32> = prs_symbol.iter().map(|s| s.conj()).collect();
    let data_conj: Vec<Complex32> = data_sym0.iter().map(|s| s.conj()).collect();
    let fine_offset_conj = -fine_offset; // conjugation flips phase
    let prs_fft_conj = do_fft(fft.as_ref(), &prs_conj, fine_offset_conj);
    let data_fft_conj = do_fft(fft.as_ref(), &data_conj, fine_offset_conj);
    let residual_phase_conj = 2.0 * PI * fine_offset_conj * SYMBOL_SIZE as f32 / FFT_SIZE as f32;
    let residual_correction_conj =
        Complex32::new((-residual_phase_conj).cos(), (-residual_phase_conj).sin());

    eprintln!("\n=== DQPSK metric for offsets -30..+30 ===");
    eprintln!("(a) With fine correction, no residual de-rotation:");
    let mut metrics: Vec<(i32, f64)> = Vec::new();
    for offset in -30i32..=30 {
        let prs_c = extract_carriers(&prs_fft, offset);
        let data_c = extract_carriers(&data_fft, offset);
        let metric = dqpsk_metric(&data_c, &prs_c);
        metrics.push((offset, metric));
    }
    metrics.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    for &(off, met) in metrics.iter().take(5) {
        eprintln!("  offset={off:+3}: metric={met:.4}");
    }

    eprintln!("(b) With fine correction + residual de-rotation:");
    let mut metrics_corr: Vec<(i32, f64)> = Vec::new();
    for offset in -30i32..=30 {
        let prs_c = extract_carriers(&prs_fft, offset);
        let data_c = extract_carriers(&data_fft, offset);
        let metric = dqpsk_metric_with_rotation(&data_c, &prs_c, residual_correction);
        metrics_corr.push((offset, metric));
    }
    metrics_corr.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    for &(off, met) in metrics_corr.iter().take(5) {
        eprintln!("  offset={off:+3}: metric={met:.4}");
    }

    eprintln!("(c) Conjugated input (E4000 spectrum inversion) + residual de-rotation:");
    let mut metrics_conj: Vec<(i32, f64)> = Vec::new();
    for offset in -30i32..=30 {
        let prs_c = extract_carriers(&prs_fft_conj, offset);
        let data_c = extract_carriers(&data_fft_conj, offset);
        let metric = dqpsk_metric_with_rotation(&data_c, &prs_c, residual_correction_conj);
        metrics_conj.push((offset, metric));
    }
    metrics_conj.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    for &(off, met) in metrics_conj.iter().take(5) {
        eprintln!("  offset={off:+3}: metric={met:.4}");
    }

    eprintln!("(d) No fine correction:");
    let mut metrics_nofine: Vec<(i32, f64)> = Vec::new();
    for offset in -30i32..=30 {
        let prs_c = extract_carriers(&do_fft(fft.as_ref(), prs_symbol, 0.0), offset);
        let data_c = extract_carriers(&do_fft(fft.as_ref(), data_sym0, 0.0), offset);
        let metric = dqpsk_metric(&data_c, &prs_c);
        metrics_nofine.push((offset, metric));
    }
    metrics_nofine.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    for &(off, met) in metrics_nofine.iter().take(5) {
        eprintln!("  offset={off:+3}: metric={met:.4}");
    }

    // Phase histogram for the best offset with residual correction
    let best_offset_corr = metrics_corr[0].0;
    let best_metric = metrics_corr[0].1;
    let prs_c = extract_carriers(&prs_fft, best_offset_corr);
    let data_c = extract_carriers(&data_fft, best_offset_corr);
    eprintln!("\n=== Phase histogram (offset={best_offset_corr}, with residual correction) ===");
    let mut hist = [0u32; 36]; // 10-degree bins
    for (&cur, &prev) in data_c.iter().zip(prs_c.iter()) {
        let z = (cur * prev.conj()) * residual_correction;
        let angle = z.arg().to_degrees();
        let bin = ((angle + 180.0) / 10.0) as usize;
        if bin < 36 {
            hist[bin] += 1;
        }
    }
    for (i, &count) in hist.iter().enumerate() {
        let angle = -180 + i as i32 * 10;
        let bar = "#".repeat(count as usize / 4);
        eprintln!("  {angle:+4}°: {count:4} {bar}");
    }

    if best_metric > -0.3 {
        eprintln!(
            "\nWARNING: Best DQPSK metric ({best_metric:.4}) is still poor after correction."
        );
    }

    // ── Step 4: Focused FIC decode diagnostic ──
    eprintln!("\n=== FIC decode diagnostic (best DQPSK offset={best_offset_corr}) ===");
    {
        let deinterleaver = FreqDeinterleaver::new();
        // Verify deinterleaver table: first few entries
        {
            let id: Vec<f32> = (0..NUM_CARRIERS as u32).map(|i| i as f32).collect();
            let perm = deinterleaver.deinterleave(&id);
            eprintln!(
                "  Deinterleaver table[0..5] (logical→physical): {:.0}, {:.0}, {:.0}, {:.0}, {:.0}",
                perm[0], perm[1], perm[2], perm[3], perm[4]
            );
            // Verify against manual LCG: table should start with [255, 754, 1096, 1459, ...]
            // because π(1)=511→idx 255, π(2)=1010→idx 754, π(3)=1353→idx 1096, π(4)=1716→idx 1459
        }
        let prbs = energy_dispersal_prbs(768);

        let prs_f = do_fft(fft.as_ref(), prs_symbol, fine_offset);
        let mut prev_c = extract_carriers(&prs_f, best_offset_corr);

        for sym_idx in 0..3 {
            let s = prs_start + (1 + sym_idx) * SYMBOL_SIZE;
            let e = s + SYMBOL_SIZE;
            if e > samples.len() {
                break;
            }

            let sym_f = do_fft(fft.as_ref(), &samples[s..e], fine_offset);
            let cur_c = extract_carriers(&sym_f, best_offset_corr);

            // DQPSK with residual correction
            let prev_c_saved = prev_c.clone();
            let mut bits = Vec::with_capacity(NUM_CARRIERS * 2);
            for (&cur, &prev) in cur_c.iter().zip(prev_c_saved.iter()) {
                let z = (cur * prev.conj()) * residual_correction;
                bits.push(z.im); // b0
                bits.push(z.re); // b1
            }

            // Deinterleave
            let ch0: Vec<f32> = bits.iter().step_by(2).copied().collect();
            let ch1: Vec<f32> = bits.iter().skip(1).step_by(2).copied().collect();
            let d0 = deinterleaver.deinterleave(&ch0);
            let d1 = deinterleaver.deinterleave(&ch1);

            let mut soft = Vec::with_capacity(NUM_CARRIERS * 2);
            for (a, b) in d0.into_iter().zip(d1.into_iter()) {
                soft.push(a);
                soft.push(b);
            }

            // Print soft bit statistics
            let max_abs = soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
            let mean_abs: f32 = soft.iter().map(|v| v.abs()).sum::<f32>() / soft.len() as f32;
            eprintln!("\n  Symbol {sym_idx}: max_abs={max_abs:.2}, mean_abs={mean_abs:.2}");
            // Print first few 4-bit groups (before normalization)
            eprintln!("  First 5 Viterbi groups (raw soft bits):");
            for g in 0..5 {
                let i = g * 4;
                if i + 3 < soft.len() {
                    let signs: String = (0..4)
                        .map(|j| if soft[i + j] >= 0.0 { '+' } else { '-' })
                        .collect();
                    eprintln!(
                        "    group {g}: [{:+8.2}, {:+8.2}, {:+8.2}, {:+8.2}]  signs={signs}",
                        soft[i],
                        soft[i + 1],
                        soft[i + 2],
                        soft[i + 3]
                    );
                }
            }
            // Also print first 5 groups WITHOUT deinterleaving
            let mut raw_soft = Vec::with_capacity(NUM_CARRIERS * 2);
            for (&cur, &prev) in cur_c.iter().zip(prev_c_saved.iter()) {
                let z = (cur * prev.conj()) * residual_correction;
                raw_soft.push(z.im);
                raw_soft.push(z.re);
            }
            eprintln!("  First 5 Viterbi groups (NO deinterleaving):");
            for g in 0..5 {
                let i = g * 4;
                if i + 3 < raw_soft.len() {
                    let signs: String = (0..4)
                        .map(|j| if raw_soft[i + j] >= 0.0 { '+' } else { '-' })
                        .collect();
                    eprintln!(
                        "    group {g}: [{:+8.2}, {:+8.2}, {:+8.2}, {:+8.2}]  signs={signs}",
                        raw_soft[i],
                        raw_soft[i + 1],
                        raw_soft[i + 2],
                        raw_soft[i + 3]
                    );
                }
            }

            // Normalize
            if max_abs > 0.0 {
                for v in &mut soft {
                    *v /= max_abs;
                }
            }

            // Viterbi decode
            let mut padded = soft;
            padded.extend(std::iter::repeat_n(0.0f32, 24));
            let decoded = fec::ViterbiDecoder::new(35).decode(&padded);
            let info = &decoded[..decoded.len().min(768)];
            let fic_bytes = pack_bits(info);
            let fic_dispersed = xor_with_prbs(&fic_bytes, &prbs);

            eprintln!("  Raw bytes[0..16]:  {:02x?}", &fic_bytes[..16]);
            eprintln!("  Dispersed[0..16]:  {:02x?}", &fic_dispersed[..16]);

            for fib_idx in 0..3 {
                let start = fib_idx * 32;
                let fib_raw = &fic_bytes[start..start + 32];
                let fib_disp = &fic_dispersed[start..start + 32];
                eprintln!(
                    "  FIB {fib_idx} CRC (raw): {}, (dispersed): {}",
                    fib_crc_ok(fib_raw),
                    fib_crc_ok(fib_disp)
                );

                // Check if first byte looks like a FIG header
                // FIG type 0: 0x0X, FIG type 1: 0x1X-0x3X
                let hdr_raw = fib_raw[0];
                let hdr_disp = fib_disp[0];
                eprintln!(
                    "    FIG header raw=0x{hdr_raw:02x} (type {}), dispersed=0x{hdr_disp:02x} (type {})",
                    hdr_raw >> 5,
                    hdr_disp >> 5
                );
            }

            prev_c = cur_c;
        }
    }

    // ── Step 5: Also try without deinterleaving ──
    eprintln!("\n=== FIC decode WITHOUT deinterleaving ===");
    {
        let prbs = energy_dispersal_prbs(768);
        let prs_f = do_fft(fft.as_ref(), prs_symbol, fine_offset);
        let mut prev_c = extract_carriers(&prs_f, best_offset_corr);

        for sym_idx in 0..1 {
            // Just first symbol
            let s = prs_start + (1 + sym_idx) * SYMBOL_SIZE;
            let e = s + SYMBOL_SIZE;
            if e > samples.len() {
                break;
            }

            let sym_f = do_fft(fft.as_ref(), &samples[s..e], fine_offset);
            let cur_c = extract_carriers(&sym_f, best_offset_corr);

            let mut soft = Vec::with_capacity(NUM_CARRIERS * 2);
            for (&cur, &prev) in cur_c.iter().zip(prev_c.iter()) {
                let z = (cur * prev.conj()) * residual_correction;
                soft.push(z.im);
                soft.push(z.re);
            }

            let max_abs = soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
            if max_abs > 0.0 {
                for v in &mut soft {
                    *v /= max_abs;
                }
            }

            let mut padded = soft;
            padded.extend(std::iter::repeat_n(0.0f32, 24));
            let decoded = fec::ViterbiDecoder::new(35).decode(&padded);
            let info = &decoded[..decoded.len().min(768)];
            let fic_bytes = pack_bits(info);
            let fic_dispersed = xor_with_prbs(&fic_bytes, &prbs);

            for fib_idx in 0..3 {
                let start = fib_idx * 32;
                eprintln!(
                    "  FIB {fib_idx} CRC (raw): {}, (dispersed): {}",
                    fib_crc_ok(&fic_bytes[start..start + 32]),
                    fib_crc_ok(&fic_dispersed[start..start + 32])
                );
            }

            prev_c = cur_c;
        }
    }

    // ── Step 6: FIC decode brute-force ──
    eprintln!("\n=== FIC decode brute-force ===");
    let deinterleaver = FreqDeinterleaver::new();
    let prbs = energy_dispersal_prbs(768); // 768 bits = 96 bytes

    // Build inverse deinterleave table
    let identity: Vec<f32> = (0..NUM_CARRIERS as u32).map(|i| i as f32).collect();
    let fwd_table = deinterleaver.deinterleave(&identity);

    // Precompute conjugated samples for E4000 spectrum inversion test
    let samples_conj: Vec<Complex32> = samples.iter().map(|s| s.conj()).collect();

    // Try all combinations: conjugation × fine × offset × negate × swap × deint_inv × dispersal
    for &conjugate in &[false, true] {
        let samps = if conjugate { &samples_conj } else { &samples };
        let fine_est = if conjugate { -fine_offset } else { fine_offset };

        for &use_fine in &[true, false] {
            let fine = if use_fine { fine_est } else { 0.0 };
            let res_phase = 2.0 * PI * fine * SYMBOL_SIZE as f32 / FFT_SIZE as f32;
            let res_corr = Complex32::new((-res_phase).cos(), (-res_phase).sin());

            for offset in -30i32..=30 {
                for &negate in &[false, true] {
                    for &swap_re_im in &[false, true] {
                        for &deint_inverse in &[false, true] {
                            for &with_dispersal in &[false, true] {
                                let mut all_crc = Vec::new();
                                let prs_f = do_fft(
                                    fft.as_ref(),
                                    &samps[prs_start..prs_start + SYMBOL_SIZE],
                                    fine,
                                );
                                let mut prev_c = extract_carriers(&prs_f, offset);

                                for sym_idx in 0..3 {
                                    let s = prs_start + (1 + sym_idx) * SYMBOL_SIZE;
                                    let e = s + SYMBOL_SIZE;
                                    if e > samps.len() {
                                        break;
                                    }

                                    let sym_f = do_fft(fft.as_ref(), &samps[s..e], fine);
                                    let cur_c = extract_carriers(&sym_f, offset);

                                    let mut bits = Vec::with_capacity(NUM_CARRIERS * 2);
                                    for (&cur, &prev) in cur_c.iter().zip(prev_c.iter()) {
                                        // Apply residual inter-symbol phase correction
                                        let z = (cur * prev.conj()) * res_corr;
                                        if swap_re_im {
                                            bits.push(z.re);
                                            bits.push(z.im);
                                        } else {
                                            bits.push(z.im);
                                            bits.push(z.re);
                                        }
                                    }
                                    if negate {
                                        for v in &mut bits {
                                            *v = -*v;
                                        }
                                    }

                                    let ch0: Vec<f32> = bits.iter().step_by(2).copied().collect();
                                    let ch1: Vec<f32> =
                                        bits.iter().skip(1).step_by(2).copied().collect();

                                    let (d0, d1) = if deint_inverse {
                                        let mut o0 = vec![0.0f32; NUM_CARRIERS];
                                        let mut o1 = vec![0.0f32; NUM_CARRIERS];
                                        for m in 0..NUM_CARRIERS {
                                            let dest = fwd_table[m] as usize;
                                            if dest < NUM_CARRIERS {
                                                o0[dest] = ch0[m];
                                                o1[dest] = ch1[m];
                                            }
                                        }
                                        (o0, o1)
                                    } else {
                                        (
                                            deinterleaver.deinterleave(&ch0),
                                            deinterleaver.deinterleave(&ch1),
                                        )
                                    };

                                    let mut soft = Vec::with_capacity(NUM_CARRIERS * 2);
                                    for (a, b) in d0.into_iter().zip(d1.into_iter()) {
                                        soft.push(a);
                                        soft.push(b);
                                    }
                                    let max_abs =
                                        soft.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
                                    if max_abs > 0.0 {
                                        for v in &mut soft {
                                            *v /= max_abs;
                                        }
                                    }

                                    let mut padded = soft;
                                    padded.extend(std::iter::repeat_n(0.0f32, 24));
                                    let decoded = fec::ViterbiDecoder::new(35).decode(&padded);
                                    let info = &decoded[..decoded.len().min(768)];
                                    let mut fic_bytes = pack_bits(info);

                                    if with_dispersal {
                                        fic_bytes = xor_with_prbs(&fic_bytes, &prbs);
                                    }

                                    for fib_idx in 0..3 {
                                        let start = fib_idx * 32;
                                        if start + 32 <= fic_bytes.len() {
                                            all_crc.push(fib_crc_ok(&fic_bytes[start..start + 32]));
                                        }
                                    }

                                    prev_c = cur_c;
                                }

                                let pass = all_crc.iter().filter(|&&ok| ok).count();
                                if pass > 0 {
                                    eprintln!(
                                        "** CRC {pass}/{} PASS ** conj={conjugate} fine={use_fine} off={offset} neg={negate} swap={swap_re_im} inv={deint_inverse} dispersal={with_dispersal}",
                                        all_crc.len()
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    eprintln!("\nDiagnostic complete.");
}
