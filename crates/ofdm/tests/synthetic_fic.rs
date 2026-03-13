//! Synthetic OFDM FIC test: create a known signal, modulate to OFDM, demod, decode.
//!
//! This tests the full OFDM demod pipeline independent of real IQ data.

use num_complex::Complex32;
use ofdm::params::*;
use ofdm::FreqDeinterleaver;
use ofdm::OfdmDemod;
use rustfft::{num_complex::Complex, FftPlanner};
use std::f32::consts::PI;

/// Rate-1/4 convolutional encoder (matching fec crate).
fn conv_encode(bits: &[u8]) -> Vec<u8> {
    const G: [u8; 4] = [109, 79, 83, 109];
    const NUM_STATES: usize = 64;
    let mut state: u8 = 0;
    let mut out = Vec::with_capacity(bits.len() * 4);
    for &b in bits {
        for &poly in &G {
            let reg = ((state as u16) << 1) | (b as u16);
            let xored = reg as u8 & poly;
            out.push(xored.count_ones() as u8 & 1);
        }
        state = ((state << 1) | b) & (NUM_STATES as u8 - 1);
    }
    out
}

/// Build frequency interleaving table (logical → physical carrier index).
fn build_interleave_table() -> Vec<usize> {
    const T_U: usize = FFT_SIZE;
    const V1: usize = 511;
    const CENTER: usize = T_U / 2;
    const HALF_K: usize = NUM_CARRIERS / 2;
    const LOW: usize = CENTER - HALF_K;
    const HIGH: usize = CENTER + HALF_K;

    let mut pi = vec![0usize; T_U];
    for j in 1..T_U {
        pi[j] = (13 * pi[j - 1] + V1) % T_U;
    }

    let mut table = Vec::with_capacity(NUM_CARRIERS);
    for &p in pi.iter().skip(1) {
        if (LOW..=HIGH).contains(&p) && p != CENTER {
            let carrier_idx = if p < CENTER {
                p - LOW
            } else {
                p - CENTER - 1 + HALF_K
            };
            table.push(carrier_idx);
        }
    }
    assert_eq!(table.len(), NUM_CARRIERS);
    table
}

/// Create OFDM symbol from carrier soft bits.
/// Returns SYMBOL_SIZE time-domain samples (guard + useful).
fn create_ofdm_symbol(carrier_values: &[Complex32]) -> Vec<Complex32> {
    assert_eq!(carrier_values.len(), NUM_CARRIERS);

    // Place carriers into FFT bins
    let mut freq = vec![Complex::<f32>::new(0.0, 0.0); FFT_SIZE];
    let mut idx = 0;
    for k in CARRIER_MIN..=CARRIER_MAX {
        if k == 0 {
            continue;
        }
        let bin = carrier_to_fft_bin(k);
        freq[bin] = Complex::new(carrier_values[idx].re, carrier_values[idx].im);
        idx += 1;
    }

    // IFFT to get time-domain samples
    let mut planner = FftPlanner::<f32>::new();
    let ifft = planner.plan_fft_inverse(FFT_SIZE);
    ifft.process(&mut freq);

    // Scale by 1/N (rustfft doesn't normalize)
    let scale = 1.0 / FFT_SIZE as f32;
    let useful: Vec<Complex32> = freq
        .iter()
        .map(|c| Complex32::new(c.re * scale, c.im * scale))
        .collect();

    // Add cyclic prefix (guard interval)
    let mut symbol = Vec::with_capacity(SYMBOL_SIZE);
    symbol.extend_from_slice(&useful[FFT_SIZE - GUARD_SIZE..]);
    symbol.extend_from_slice(&useful);
    assert_eq!(symbol.len(), SYMBOL_SIZE);
    symbol
}

/// CRC-16/CCITT-FALSE
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn make_fib(data: &[u8; 30]) -> [u8; 32] {
    let crc = crc16(data);
    let crc_inv = !crc;
    let mut fib = [0u8; 32];
    fib[..30].copy_from_slice(data);
    fib[30] = (crc_inv >> 8) as u8;
    fib[31] = (crc_inv & 0xFF) as u8;
    fib
}

fn fib_crc_ok(fib: &[u8]) -> bool {
    if fib.len() < 32 {
        return false;
    }
    let crc = crc16(&fib[..30]);
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

fn unpack_bits(bytes: &[u8]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(bytes.len() * 8);
    for &byte in bytes {
        for bit in 0..8 {
            bits.push((byte >> (7 - bit)) & 1);
        }
    }
    bits
}

/// PRBS generator (x^9 + x^5 + 1, all-ones init).
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

fn xor_bits(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(&x, &y)| x ^ y).collect()
}

#[test]
fn synthetic_fic_roundtrip() {
    // Create 3 FIBs with known data
    let fib0 = make_fib(&[
        0x01, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22,
        0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x42,
    ]);
    let fib1 = make_fib(&[0xAA; 30]);
    let fib2 = make_fib(&[0x55; 30]);

    assert!(fib_crc_ok(&fib0));
    assert!(fib_crc_ok(&fib1));
    assert!(fib_crc_ok(&fib2));
    eprintln!("FIB CRCs valid before encoding ✓");

    // Concatenate FIBs → 96 bytes → 768 bits
    let mut fic_bytes = Vec::new();
    fic_bytes.extend_from_slice(&fib0);
    fic_bytes.extend_from_slice(&fib1);
    fic_bytes.extend_from_slice(&fib2);
    let data_bits = unpack_bits(&fic_bytes);
    assert_eq!(data_bits.len(), 768);

    // Energy dispersal
    let prbs_bits = prbs(768);
    let dispersed = xor_bits(&data_bits, &prbs_bits);

    // Convolutional encode
    let coded = conv_encode(&dispersed);
    assert_eq!(coded.len(), 3072);
    eprintln!("Encoded: 768 → 3072 coded bits ✓");

    // Map coded bits to QPSK soft values
    // Table 42: d_{2k},d_{2k+1} → phase change
    // (0,0)→+π/4, (0,1)→+3π/4, (1,0)→-π/4, (1,1)→-3π/4
    let mut carrier_symbols = vec![Complex32::new(0.0, 0.0); NUM_CARRIERS];
    for k in 0..NUM_CARRIERS {
        let b0 = coded[2 * k]; // d_{2k}
        let b1 = coded[2 * k + 1]; // d_{2k+1}
        let phase = match (b0, b1) {
            (0, 0) => PI / 4.0,
            (0, 1) => 3.0 * PI / 4.0,
            (1, 0) => -PI / 4.0,
            (1, 1) => -3.0 * PI / 4.0,
            _ => unreachable!(),
        };
        // This is the phase CHANGE from PRS to data symbol
        carrier_symbols[k] = Complex32::new(phase.cos(), phase.sin());
    }

    // Frequency interleave the carriers
    let interleave_table = build_interleave_table();
    let mut interleaved_carriers = vec![Complex32::new(0.0, 0.0); NUM_CARRIERS];
    for (log_idx, &phys_idx) in interleave_table.iter().enumerate() {
        interleaved_carriers[phys_idx] = carrier_symbols[log_idx];
    }

    // Create PRS symbol (random but fixed phase reference)
    let mut prs_carriers = vec![Complex32::new(0.0, 0.0); NUM_CARRIERS];
    for (k, prs_c) in prs_carriers.iter_mut().enumerate() {
        // Use a deterministic "random" phase for the PRS
        let phase = (k as f32 * 2.731) % (2.0 * PI);
        *prs_c = Complex32::new(phase.cos(), phase.sin());
    }

    // The data symbol's carriers = PRS_carrier * exp(j * phase_change)
    // Because the receiver computes: data * conj(PRS) = exp(j * phase_change)
    let mut data_carriers = vec![Complex32::new(0.0, 0.0); NUM_CARRIERS];
    for k in 0..NUM_CARRIERS {
        data_carriers[k] = prs_carriers[k] * interleaved_carriers[k];
    }

    // Create OFDM symbols
    let prs_symbol = create_ofdm_symbol(&prs_carriers);
    let data_symbol = create_ofdm_symbol(&data_carriers);

    eprintln!("Created synthetic OFDM symbols ✓");

    // ---- Now demodulate using our OfdmDemod ----
    let mut demod = OfdmDemod::new();
    demod.process_phase_ref(&prs_symbol);
    let raw_bits = demod.demod_symbol(&data_symbol);
    assert_eq!(raw_bits.len(), NUM_CARRIERS * 2);

    // Check DQPSK metric (split layout: first half is Re, second half is Im)
    let mut dqpsk_sum = 0.0f64;
    for k in 0..NUM_CARRIERS {
        let re = raw_bits[k];
        let im = raw_bits[NUM_CARRIERS + k];
        let angle = im.atan2(re);
        dqpsk_sum += (4.0 * angle as f64).cos();
    }
    let dqpsk_metric = dqpsk_sum / (NUM_CARRIERS as f64);
    eprintln!("DQPSK metric: {dqpsk_metric:.4} (ideal: -1.0)");

    // Frequency deinterleave
    // Demod now outputs split layout [Re(0)..Re(K-1), Im(0)..Im(K-1)].
    let deinterleaver = FreqDeinterleaver::new();
    let (re_channel, im_channel) = raw_bits.split_at(NUM_CARRIERS);
    let re_di = deinterleaver.deinterleave(re_channel);
    let im_di = deinterleaver.deinterleave(im_channel);

    // The demod outputs split layout [Re..., Im...], but this synthetic
    // test uses unpunctured rate-1/4 (no FIC block accumulation).
    // Convert back to interleaved [Im(0), Re(0), Im(1), Re(1), ...]
    // which is the natural coded-bit ordering for the Viterbi decoder.
    let mut soft_bits = Vec::with_capacity(NUM_CARRIERS * 2);
    for k in 0..NUM_CARRIERS {
        soft_bits.push(im_di[k]); // d_{2k}   = Q axis
        soft_bits.push(re_di[k]); // d_{2k+1} = I axis
    }

    // Normalize
    let max_abs = soft_bits.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
    let normalized: Vec<f32> = soft_bits.iter().map(|v| v * scale).collect();

    // Show first few soft bits
    eprintln!("First 8 soft bits: {:?}", &normalized[..8]);

    // Viterbi decode (unpunctured — append 24 tail erasures)
    let mut padded = normalized;
    padded.extend(std::iter::repeat_n(0.0f32, 24));
    let viterbi = fec::ViterbiDecoder::new(35);
    let decoded = viterbi.decode(&padded);
    let info = &decoded[..768];

    // Check bit errors before de-dispersal
    let bit_errors: usize = info
        .iter()
        .zip(dispersed.iter())
        .map(|(&a, &b)| if a != b { 1 } else { 0 })
        .sum();
    eprintln!("Viterbi bit errors (vs dispersed): {bit_errors}/768");

    // De-dispersal
    let decoded_bits: Vec<u8> = info
        .iter()
        .zip(prbs_bits.iter())
        .map(|(&d, &p)| d ^ p)
        .collect();
    let decoded_bytes = pack_bits(&decoded_bits);

    // Check CRCs
    for fib_idx in 0..3 {
        let start = fib_idx * 32;
        let fib = &decoded_bytes[start..start + 32];
        let ok = fib_crc_ok(fib);
        eprintln!("FIB {fib_idx} CRC: {}", if ok { "OK ✓" } else { "FAIL ✗" });
        if !ok {
            eprintln!("  Expected: {:02x?}", &fic_bytes[start..start + 32]);
            eprintln!("  Got:      {:02x?}", fib);
        }
    }

    // Assert at least FIB 0 passes
    assert!(
        fib_crc_ok(&decoded_bytes[0..32]),
        "FIB 0 CRC must pass in synthetic test"
    );
}

/// Helper to run the full synthetic FIC test with a given frequency offset.
fn synthetic_with_freq_offset(epsilon: f32) -> usize {
    // Create 3 FIBs with known data
    let fib0 = make_fib(&[
        0x01, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22,
        0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x42,
    ]);
    let fib1 = make_fib(&[0xAA; 30]);
    let fib2 = make_fib(&[0x55; 30]);

    let mut fic_bytes = Vec::new();
    fic_bytes.extend_from_slice(&fib0);
    fic_bytes.extend_from_slice(&fib1);
    fic_bytes.extend_from_slice(&fib2);
    let data_bits = unpack_bits(&fic_bytes);

    let prbs_bits = prbs(768);
    let dispersed = xor_bits(&data_bits, &prbs_bits);
    let coded = conv_encode(&dispersed);

    // Map to DQPSK phase changes
    let mut carrier_phases = vec![Complex32::new(0.0, 0.0); NUM_CARRIERS];
    for k in 0..NUM_CARRIERS {
        let b0 = coded[2 * k];
        let b1 = coded[2 * k + 1];
        let phase = match (b0, b1) {
            (0, 0) => PI / 4.0,
            (0, 1) => 3.0 * PI / 4.0,
            (1, 0) => -PI / 4.0,
            (1, 1) => -3.0 * PI / 4.0,
            _ => unreachable!(),
        };
        carrier_phases[k] = Complex32::new(phase.cos(), phase.sin());
    }

    // Frequency interleave
    let interleave_table = build_interleave_table();
    let mut interleaved = vec![Complex32::new(0.0, 0.0); NUM_CARRIERS];
    for (log_idx, &phys_idx) in interleave_table.iter().enumerate() {
        interleaved[phys_idx] = carrier_phases[log_idx];
    }

    // PRS carriers
    let mut prs_carriers = vec![Complex32::new(0.0, 0.0); NUM_CARRIERS];
    for (k, prs_c) in prs_carriers.iter_mut().enumerate() {
        let phase = (k as f32 * 2.731) % (2.0 * PI);
        *prs_c = Complex32::new(phase.cos(), phase.sin());
    }

    // Data carriers = PRS * phase_change
    let mut data_carriers = vec![Complex32::new(0.0, 0.0); NUM_CARRIERS];
    for k in 0..NUM_CARRIERS {
        data_carriers[k] = prs_carriers[k] * interleaved[k];
    }

    // Create OFDM symbols
    let prs_ofdm = create_ofdm_symbol(&prs_carriers);
    let data_ofdm = create_ofdm_symbol(&data_carriers);

    // Apply frequency offset to time-domain samples
    // Frequency offset of ε sub-carrier spacings = ε × Δf Hz
    // Phase rotation per sample: 2π × ε / FFT_SIZE
    let apply_freq_offset = |samples: &[Complex32], start_phase: f32| -> (Vec<Complex32>, f32) {
        let phase_step = 2.0 * PI * epsilon / FFT_SIZE as f32;
        let mut phase = start_phase;
        let out: Vec<Complex32> = samples
            .iter()
            .map(|&s| {
                let rot = Complex32::new(phase.cos(), phase.sin());
                phase += phase_step;
                s * rot
            })
            .collect();
        (out, phase)
    };

    let (prs_shifted, phase_after_prs) = apply_freq_offset(&prs_ofdm, 0.0);
    let (data_shifted, _) = apply_freq_offset(&data_ofdm, phase_after_prs);

    // Demodulate
    let mut demod = OfdmDemod::new();
    demod.process_phase_ref(&prs_shifted);
    let raw_bits = demod.demod_symbol(&data_shifted);

    // DQPSK metric (split layout)
    let mut dqpsk_sum = 0.0f64;
    for k in 0..NUM_CARRIERS {
        let re = raw_bits[k];
        let im = raw_bits[NUM_CARRIERS + k];
        let angle = im.atan2(re);
        dqpsk_sum += (4.0 * angle as f64).cos();
    }
    let dqpsk_metric = dqpsk_sum / (NUM_CARRIERS as f64);
    eprintln!("  ε={epsilon:.3}: DQPSK metric={dqpsk_metric:.4}");

    // Deinterleave (split layout)
    let deinterleaver = FreqDeinterleaver::new();
    let (re_ch, im_ch) = raw_bits.split_at(NUM_CARRIERS);
    let re_di = deinterleaver.deinterleave(re_ch);
    let im_di = deinterleaver.deinterleave(im_ch);

    // Convert to interleaved for Viterbi (no puncturing in synthetic test)
    let mut soft_bits = Vec::with_capacity(NUM_CARRIERS * 2);
    for k in 0..NUM_CARRIERS {
        soft_bits.push(im_di[k]); // d_{2k}
        soft_bits.push(re_di[k]); // d_{2k+1}
    }

    let max_abs = soft_bits.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    let scale = if max_abs > 0.0 { 1.0 / max_abs } else { 1.0 };
    let normalized: Vec<f32> = soft_bits.iter().map(|v| v * scale).collect();

    let mut padded = normalized;
    padded.extend(std::iter::repeat_n(0.0f32, 24));
    let viterbi = fec::ViterbiDecoder::new(35);
    let decoded = viterbi.decode(&padded);
    let info = &decoded[..768];

    let bit_errors: usize = info
        .iter()
        .zip(dispersed.iter())
        .map(|(&a, &b)| if a != b { 1 } else { 0 })
        .sum();
    eprintln!("  ε={epsilon:.3}: bit errors={bit_errors}/768");

    let decoded_bits: Vec<u8> = info
        .iter()
        .zip(prbs_bits.iter())
        .map(|(&d, &p)| d ^ p)
        .collect();
    let decoded_bytes = pack_bits(&decoded_bits);

    let mut crc_ok = 0;
    for fib_idx in 0..3 {
        let start = fib_idx * 32;
        if fib_crc_ok(&decoded_bytes[start..start + 32]) {
            crc_ok += 1;
        }
    }
    eprintln!("  ε={epsilon:.3}: CRC OK={crc_ok}/3");
    crc_ok
}

#[test]
fn synthetic_with_freq_offsets() {
    eprintln!("=== Testing with various frequency offsets ===");
    // Test fractional offsets (within one sub-carrier)
    for &eps in &[0.0, 0.05, 0.1, 0.15, 0.2, 0.3, 0.4] {
        let ok = synthetic_with_freq_offset(eps);
        assert!(ok >= 2, "Should decode with ε={eps}");
    }
    // Test integer + fractional offsets (coarse + fine)
    eprintln!("\n=== Testing with coarse+fine offsets (like real data) ===");
    for &eps in &[1.0, 1.155, 2.0, 2.155, -1.0, -2.0, 3.0, 5.0, 10.0] {
        synthetic_with_freq_offset(eps);
    }
}
