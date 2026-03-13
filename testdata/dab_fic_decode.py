#!/usr/bin/env python3
"""
Minimal independent DAB FIC decoder in pure Python.
Processes raw IQ file → frame sync → FFT → DQPSK → deinterleave → Viterbi → FIB CRC check.
No numpy dependency.
"""
import struct, cmath, math, sys

# DAB Mode I constants
FFT_SIZE = 2048
GUARD_SIZE = 504
SYMBOL_SIZE = FFT_SIZE + GUARD_SIZE  # 2552
NULL_SIZE = 2656
FRAME_SYMBOLS = 76
NUM_CARRIERS = 1536
CARRIER_MIN = -768
CARRIER_MAX = 768
SAMPLE_RATE = 2048000

# ─── IQ file reading ───
def read_iq(path):
    with open(path, 'rb') as f:
        data = f.read()
    samples = []
    for i in range(0, len(data) - 1, 2):
        r = (data[i] - 127.5) / 127.5
        im = (data[i+1] - 127.5) / 127.5
        samples.append(complex(r, im))
    return samples

# ─── FFT (Cooley-Tukey radix-2) ───
def fft(x):
    N = len(x)
    if N <= 1:
        return list(x)
    if N & (N - 1) != 0:
        raise ValueError("FFT size must be power of 2")
    even = fft(x[0::2])
    odd = fft(x[1::2])
    T = [cmath.exp(-2j * cmath.pi * k / N) * odd[k] for k in range(N // 2)]
    return [even[k] + T[k] for k in range(N // 2)] + \
           [even[k] - T[k] for k in range(N // 2)]

# ─── Null symbol detection ───
def find_null(samples, start=0):
    """Find null symbol by energy drop."""
    win = 256
    # Compute running energy
    if len(samples) - start < win * 4:
        return None
    energies = []
    for i in range(start, min(start + 500000, len(samples) - win), win):
        e = sum(abs(s)**2 for s in samples[i:i+win])
        energies.append((i, e))

    if not energies:
        return None

    # Find mean energy
    mean_e = sum(e for _, e in energies) / len(energies)
    threshold = mean_e * 0.1

    # Find first energy drop below threshold followed by recovery
    for idx in range(1, len(energies) - 2):
        if energies[idx][1] < threshold and energies[idx+2][1] > threshold:
            # Found null → return position after null (PRS start)
            return energies[idx+2][0]

    return None

# ─── Guard interval correlation for PRS refinement ───
def guard_corr(samples, start):
    if start + SYMBOL_SIZE > len(samples):
        return 0.0
    corr = 0.0
    power = 0.0
    for n in range(GUARD_SIZE):
        a = samples[start + n]
        b = samples[start + n + FFT_SIZE]
        corr_c = b * a.conjugate()
        corr += abs(corr_c)  # Use magnitude
        power += abs(a)**2 + abs(b)**2
    if power > 0:
        return corr / (power / 2)
    return 0.0

def refine_prs(samples, raw_pos):
    best_pos = raw_pos
    best_c = 0.0
    for p in range(max(0, raw_pos - 512), min(raw_pos + 64, len(samples) - SYMBOL_SIZE), 4):
        c = guard_corr(samples, p)
        if c > best_c:
            best_c = c
            best_pos = p
    # Fine
    for p in range(max(0, best_pos - 4), min(best_pos + 5, len(samples) - SYMBOL_SIZE)):
        c = guard_corr(samples, p)
        if c > best_c:
            best_c = c
            best_pos = p
    return best_pos, best_c

# ─── Carrier extraction ───
def carrier_to_bin(k):
    return (k + FFT_SIZE) % FFT_SIZE

def extract_carriers(fft_out, offset=0):
    carriers = []
    for k in range(CARRIER_MIN, CARRIER_MAX + 1):
        if k == 0:
            continue
        bin_idx = (carrier_to_bin(k) + offset + FFT_SIZE) % FFT_SIZE
        carriers.append(fft_out[bin_idx])
    return carriers

# ─── Fine frequency offset ───
def estimate_fine_freq(symbol_samples):
    if len(symbol_samples) < FFT_SIZE + GUARD_SIZE:
        return 0.0
    corr = 0.0 + 0j
    for n in range(GUARD_SIZE):
        a = symbol_samples[n]
        b = symbol_samples[n + FFT_SIZE]
        corr += b * a.conjugate()
    return cmath.phase(corr) / (2 * math.pi)

# ─── Coarse frequency search ───
def dqpsk_metric(prs_fft, data_fft, offset, correction):
    s = 0.0
    count = 0
    for k in range(CARRIER_MIN, CARRIER_MAX + 1):
        if k == 0:
            continue
        b = (carrier_to_bin(k) + offset + FFT_SIZE) % FFT_SIZE
        cur = data_fft[b]
        prev = prs_fft[b]
        z = cur * prev.conjugate() * correction
        angle = cmath.phase(z)
        s += math.cos(4 * angle)
        count += 1
    return s / count

def search_coarse(prs_fft, data_fft, correction):
    best_off = 0
    best_m = 999
    for off in range(-30, 31):
        m = dqpsk_metric(prs_fft, data_fft, off, correction)
        if m < best_m:
            best_m = m
            best_off = off
    return best_off, best_m

# ─── FFT with fine freq correction ───
def do_fft(symbol_samples, fine_offset):
    start = GUARD_SIZE if len(symbol_samples) >= FFT_SIZE + GUARD_SIZE else 0
    window = symbol_samples[start:start + FFT_SIZE]
    if abs(fine_offset) > 1e-6:
        phase_step = -2 * math.pi * fine_offset / FFT_SIZE
        corrected = [s * cmath.exp(1j * phase_step * i) for i, s in enumerate(window)]
    else:
        corrected = list(window)
    while len(corrected) < FFT_SIZE:
        corrected.append(0)
    return fft(corrected)

# ─── Frequency deinterleaver ───
def build_deinterleave_table():
    T_U = FFT_SIZE
    V1 = 511
    CENTER = T_U // 2  # 1024
    HALF_K = NUM_CARRIERS // 2  # 768
    LOW = CENTER - HALF_K  # 256
    HIGH = CENTER + HALF_K  # 1792

    pi = [0] * T_U
    for j in range(1, T_U):
        pi[j] = (13 * pi[j-1] + V1) % T_U

    table = []
    for j in range(1, T_U):  # skip pi[0] = 0
        p = pi[j]
        if LOW <= p <= HIGH and p != CENTER:
            if p < CENTER:
                carrier_idx = p - LOW
            else:
                carrier_idx = p - CENTER - 1 + HALF_K
            table.append(carrier_idx)

    assert len(table) == NUM_CARRIERS, f"Expected {NUM_CARRIERS}, got {len(table)}"
    return table

def deinterleave(carriers_f32, table):
    out = [0.0] * NUM_CARRIERS
    for logical, src in enumerate(table):
        out[logical] = carriers_f32[src]
    return out

# ─── Viterbi decoder ───
# ETSI polynomials, bit-reversed for LSB-first register
G = [109, 79, 83, 109]  # reversed from [91, 121, 101, 91]
K = 7
NUM_STATES = 64
NUM_OUTPUTS = 4

def encode_bit(state, inp, poly):
    reg = (state << 1) | inp
    return bin(reg & poly).count('1') & 1

def build_transitions(polys=None):
    if polys is None:
        polys = G
    table = []
    for state in range(NUM_STATES):
        row = []
        for inp in range(2):
            ns = ((state << 1) | inp) & (NUM_STATES - 1)
            outputs = [encode_bit(state, inp, p) for p in polys]
            row.append((ns, outputs))
        table.append(row)
    return table

def viterbi_decode(soft_bits, polys=None):
    trans = build_transitions(polys)
    n_sym = len(soft_bits) // NUM_OUTPUTS
    if n_sym == 0:
        return []

    # Scale to int
    scaled = [max(-127, min(127, int(v * 127))) for v in soft_bits]

    LARGE = 2**30
    pm = [LARGE] * NUM_STATES
    pm[0] = 0

    survivors = [[0] * NUM_STATES for _ in range(n_sym)]

    for t in range(n_sym):
        new_pm = [LARGE] * NUM_STATES
        sym = scaled[t*4:(t+1)*4]

        for state in range(NUM_STATES):
            if pm[state] == LARGE:
                continue
            for inp in range(2):
                ns, outputs = trans[state][inp]
                # Branch metric
                metric = 0
                for i in range(NUM_OUTPUTS):
                    expected = 127 if outputs[i] == 0 else -127
                    metric += abs(sym[i] - expected)
                cand = pm[state] + metric
                if cand < new_pm[ns]:
                    new_pm[ns] = cand
                    survivors[t][ns] = state

        pm = new_pm

    # Find best end state
    best_state = min(range(NUM_STATES), key=lambda s: pm[s])

    # Traceback
    bits = [0] * n_sym
    state = best_state
    for t in range(n_sym - 1, -1, -1):
        bits[t] = state & 1
        state = survivors[t][state]

    return bits

# ─── FIB CRC ───
def fib_crc_ok(fib_bytes):
    if len(fib_bytes) < 32:
        return False
    crc = 0xFFFF
    for byte in fib_bytes[:30]:
        crc ^= byte << 8
        for _ in range(8):
            if crc & 0x8000:
                crc = (crc << 1) ^ 0x1021
            else:
                crc <<= 1
            crc &= 0xFFFF
    stored = (fib_bytes[30] << 8) | fib_bytes[31]
    return (~crc & 0xFFFF) == stored

def pack_bits(bits):
    n = (len(bits) + 7) // 8
    out = [0] * n
    for i, b in enumerate(bits):
        if b:
            out[i // 8] |= 0x80 >> (i % 8)
    return out

# ─── Energy dispersal PRBS ───
def prbs(length):
    reg = 0x1FF
    out = []
    for _ in range(length):
        bit = ((reg >> 8) ^ (reg >> 4)) & 1
        out.append(bit)
        reg = ((reg << 1) | bit) & 0x1FF
    return out

def xor_dispersal(fib_bytes, prbs_bits):
    result = list(fib_bytes)
    for i in range(len(result)):
        mask = 0
        for bit in range(8):
            idx = i * 8 + bit
            if idx < len(prbs_bits) and prbs_bits[idx]:
                mask |= 0x80 >> bit
        result[i] ^= mask
    return result

# ─── Main ───
def main():
    iq_file = sys.argv[1] if len(sys.argv) > 1 else "dab_13b.raw"
    print(f"Reading {iq_file}...")
    samples = read_iq(iq_file)
    print(f"Loaded {len(samples)} IQ samples ({len(samples)/SAMPLE_RATE*1000:.1f} ms)")

    # Find null symbol
    prs_pos = find_null(samples)
    if prs_pos is None:
        print("ERROR: No null symbol found")
        return

    print(f"Null detected, raw PRS pos: {prs_pos}")

    # Refine PRS position
    prs_pos, gc = refine_prs(samples, prs_pos)
    print(f"Refined PRS pos: {prs_pos}, guard_corr: {gc:.4f}")

    # Check we have enough data
    needed = prs_pos + FRAME_SYMBOLS * SYMBOL_SIZE
    if len(samples) < needed:
        print(f"ERROR: Not enough samples (have {len(samples)}, need {needed})")
        return

    # Process PRS
    prs_samples = samples[prs_pos:prs_pos + SYMBOL_SIZE]
    fine_offset = estimate_fine_freq(prs_samples)
    print(f"Fine freq offset: {fine_offset:.4f} sub-carrier spacings")

    # Residual correction
    residual_phase = 2 * math.pi * fine_offset * SYMBOL_SIZE / FFT_SIZE
    correction = cmath.exp(-1j * residual_phase)
    print(f"Residual phase: {math.degrees(residual_phase):.1f}°")

    # FFT PRS
    prs_fft = do_fft(prs_samples, fine_offset)

    # FFT first data symbol (for coarse search)
    data_start = prs_pos + SYMBOL_SIZE
    sym0_samples = samples[data_start:data_start + SYMBOL_SIZE]
    sym0_fft = do_fft(sym0_samples, fine_offset)

    # Coarse frequency search
    coarse_off, coarse_metric = search_coarse(prs_fft, sym0_fft, correction)
    print(f"Coarse offset: {coarse_off} bins, metric: {coarse_metric:.4f}")

    # Build deinterleave table
    di_table = build_deinterleave_table()

    # PRBS for energy dispersal
    prbs_bits = prbs(256)

    # Process FIC symbols (first 3 data symbols)
    total_crc_ok = 0
    total_fibs = 0

    # Try both with and without various options
    configs = [
        ("normal", False, False, True),
        ("negated", True, False, True),
        ("swapped", False, True, True),
        ("neg+swap", True, True, True),
        ("normal-nodisp", False, False, False),
        ("negated-nodisp", True, False, False),
    ]

    prev_carriers = extract_carriers(prs_fft, coarse_off)

    for sym_idx in range(3):
        sym_start = data_start + sym_idx * SYMBOL_SIZE
        sym_samples = samples[sym_start:sym_start + SYMBOL_SIZE]
        sym_fft = do_fft(sym_samples, fine_offset)
        cur_carriers = extract_carriers(sym_fft, coarse_off)

        # DQPSK differential product
        raw_bits = []
        for ci in range(NUM_CARRIERS):
            z = cur_carriers[ci] * prev_carriers[ci].conjugate() * correction
            raw_bits.append(z.imag)  # b0 = d_{2k}
            raw_bits.append(z.real)  # b1 = d_{2k+1}

        prev_carriers = cur_carriers

        # Deinterleave (split re/im channels)
        b0_channel = [raw_bits[i] for i in range(0, len(raw_bits), 2)]
        b1_channel = [raw_bits[i] for i in range(1, len(raw_bits), 2)]
        b0_di = deinterleave(b0_channel, di_table)
        b1_di = deinterleave(b1_channel, di_table)

        # Re-interleave
        soft = []
        for r, i in zip(b0_di, b1_di):
            soft.append(r)
            soft.append(i)

        # Normalize
        max_abs = max(abs(v) for v in soft) if soft else 1.0
        scale = 1.0 / max_abs if max_abs > 0 else 1.0
        soft = [v * scale for v in soft]

        print(f"\nSymbol {sym_idx}: {len(soft)} soft bits, first 8: {[f'{v:.3f}' for v in soft[:8]]}")

        for config_name, negate, swap, dispersal in configs:
            processed = list(soft)
            if swap:
                processed = [processed[i^1] for i in range(len(processed))]
            if negate:
                processed = [-v for v in processed]

            # Add tail zeros
            processed.extend([0.0] * 24)

            # Viterbi decode
            decoded = viterbi_decode(processed)
            info = decoded[:768]
            fic_bytes = pack_bits(info)

            # Energy dispersal per FIB
            ok = 0
            for fib_idx in range(3):
                start = fib_idx * 32
                if start + 32 <= len(fic_bytes):
                    fib = fic_bytes[start:start+32]
                    if dispersal:
                        fib = xor_dispersal(fib, prbs_bits)
                    if fib_crc_ok(fib):
                        ok += 1
                        print(f"  *** CRC OK: sym{sym_idx} fib{fib_idx} ({config_name}) ***")
                        print(f"      {' '.join(f'{b:02x}' for b in fib[:16])}")

            if ok > 0:
                total_crc_ok += ok
            total_fibs += 3

        # Also try with original (non-reversed) polynomials
        for config_name, negate, swap, dispersal in [("orig-polys", False, False, True), ("orig-neg", True, False, True)]:
            processed = list(soft)
            if swap:
                processed = [processed[i^1] for i in range(len(processed))]
            if negate:
                processed = [-v for v in processed]
            processed.extend([0.0] * 24)
            decoded = viterbi_decode(processed, polys=[91, 121, 101, 91])
            info = decoded[:768]
            fic_bytes = pack_bits(info)
            ok = 0
            for fib_idx in range(3):
                start = fib_idx * 32
                if start + 32 <= len(fic_bytes):
                    fib = fic_bytes[start:start+32]
                    if dispersal:
                        fib = xor_dispersal(fib, prbs_bits)
                    if fib_crc_ok(fib):
                        ok += 1
                        print(f"  *** CRC OK: sym{sym_idx} fib{fib_idx} ({config_name}) ***")

    print(f"\nTotal CRC OK: {total_crc_ok}/{total_fibs}")

    # Debug: dump first FIB bytes for inspection
    print("\n=== Debug: First symbol, normal config, decoded FIB bytes ===")
    sym_start = data_start
    sym_samples = samples[sym_start:sym_start + SYMBOL_SIZE]
    sym_fft = do_fft(sym_samples, fine_offset)
    prs_carriers = extract_carriers(prs_fft, coarse_off)
    data_carriers = extract_carriers(sym_fft, coarse_off)

    raw_bits = []
    for ci in range(NUM_CARRIERS):
        z = data_carriers[ci] * prs_carriers[ci].conjugate() * correction
        raw_bits.append(z.imag)
        raw_bits.append(z.real)

    b0 = [raw_bits[i] for i in range(0, len(raw_bits), 2)]
    b1 = [raw_bits[i] for i in range(1, len(raw_bits), 2)]
    b0_di = deinterleave(b0, di_table)
    b1_di = deinterleave(b1, di_table)
    soft = []
    for r, i in zip(b0_di, b1_di):
        soft.append(r)
        soft.append(i)
    max_abs = max(abs(v) for v in soft) if soft else 1.0
    soft = [v / max_abs for v in soft]
    soft.extend([0.0] * 24)
    decoded = viterbi_decode(soft)
    info = decoded[:768]
    fic_bytes = pack_bits(info)

    # With dispersal
    for fib_idx in range(3):
        start = fib_idx * 32
        fib = xor_dispersal(fic_bytes[start:start+32], prbs_bits)
        print(f"FIB{fib_idx} (w/ dispersal): {' '.join(f'{b:02x}' for b in fib)}")
        # Show CRC
        crc = 0xFFFF
        for byte in fib[:30]:
            crc ^= byte << 8
            for _ in range(8):
                if crc & 0x8000:
                    crc = (crc << 1) ^ 0x1021
                else:
                    crc <<= 1
                crc &= 0xFFFF
        print(f"  CRC computed: {(~crc)&0xFFFF:04x}, stored: {fib[30]:02x}{fib[31]:02x}")

if __name__ == "__main__":
    main()
