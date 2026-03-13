#!/usr/bin/env python3
"""
Round-trip test: known FIB data → encode → interleave → soft bits → deinterleave → decode → check CRC.
Tests the full bit-processing pipeline independent of real IQ data.
"""
import cmath, math, sys

FFT_SIZE = 2048
NUM_CARRIERS = 1536
NUM_OUTPUTS = 4
K = 7
NUM_STATES = 64

# Viterbi polynomials (bit-reversed)
G = [109, 79, 83, 109]

def encode_bit(state, inp, poly):
    reg = (state << 1) | inp
    return bin(reg & poly).count('1') & 1

def conv_encode(bits, polys=None):
    if polys is None:
        polys = G
    state = 0
    out = []
    for b in bits:
        for p in polys:
            out.append(encode_bit(state, b, p))
        state = ((state << 1) | b) & (NUM_STATES - 1)
    return out

def viterbi_decode(soft_bits, polys=None):
    if polys is None:
        polys = G
    # Build transitions
    trans = []
    for state in range(NUM_STATES):
        row = []
        for inp in range(2):
            ns = ((state << 1) | inp) & (NUM_STATES - 1)
            outputs = [encode_bit(state, inp, p) for p in polys]
            row.append((ns, outputs))
        trans.append(row)

    n_sym = len(soft_bits) // NUM_OUTPUTS
    if n_sym == 0:
        return []
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
                metric = 0
                for i in range(NUM_OUTPUTS):
                    expected = 127 if outputs[i] == 0 else -127
                    metric += abs(sym[i] - expected)
                cand = pm[state] + metric
                if cand < new_pm[ns]:
                    new_pm[ns] = cand
                    survivors[t][ns] = state
        pm = new_pm
    best_state = min(range(NUM_STATES), key=lambda s: pm[s])
    bits = [0] * n_sym
    state = best_state
    for t in range(n_sym - 1, -1, -1):
        bits[t] = state & 1
        state = survivors[t][state]
    return bits

def build_deinterleave_table():
    T_U = FFT_SIZE
    V1 = 511
    CENTER = T_U // 2
    HALF_K = NUM_CARRIERS // 2
    LOW = CENTER - HALF_K
    HIGH = CENTER + HALF_K
    pi = [0] * T_U
    for j in range(1, T_U):
        pi[j] = (13 * pi[j-1] + V1) % T_U
    table = []
    for j in range(1, T_U):
        p = pi[j]
        if LOW <= p <= HIGH and p != CENTER:
            if p < CENTER:
                carrier_idx = p - LOW
            else:
                carrier_idx = p - CENTER - 1 + HALF_K
            table.append(carrier_idx)
    assert len(table) == NUM_CARRIERS
    return table

def interleave(logical_carriers, table):
    """Interleave: physical[table[log_idx]] = logical_carriers[log_idx]"""
    physical = [0.0] * NUM_CARRIERS
    for log_idx, phys_idx in enumerate(table):
        physical[phys_idx] = logical_carriers[log_idx]
    return physical

def deinterleave(physical_carriers, table):
    """Deinterleave: out[log_idx] = physical_carriers[table[log_idx]]"""
    out = [0.0] * NUM_CARRIERS
    for log_idx, phys_idx in enumerate(table):
        out[log_idx] = physical_carriers[phys_idx]
    return out

def prbs(length):
    reg = 0x1FF
    out = []
    for _ in range(length):
        bit = ((reg >> 8) ^ (reg >> 4)) & 1
        out.append(bit)
        reg = ((reg << 1) | bit) & 0x1FF
    return out

def pack_bits(bits):
    n = (len(bits) + 7) // 8
    out = [0] * n
    for i, b in enumerate(bits):
        if b:
            out[i // 8] |= 0x80 >> (i % 8)
    return out

def unpack_bits(bytes_data, n_bits=None):
    bits = []
    for byte in bytes_data:
        for bit in range(8):
            bits.append((byte >> (7 - bit)) & 1)
    if n_bits is not None:
        bits = bits[:n_bits]
    return bits

def crc16(data_bytes):
    crc = 0xFFFF
    for byte in data_bytes:
        crc ^= byte << 8
        for _ in range(8):
            if crc & 0x8000:
                crc = (crc << 1) ^ 0x1021
            else:
                crc <<= 1
            crc &= 0xFFFF
    return crc

def make_fib(data_bytes_30):
    """Create a 32-byte FIB with valid CRC."""
    assert len(data_bytes_30) == 30
    crc = crc16(data_bytes_30)
    crc_inv = (~crc) & 0xFFFF
    return list(data_bytes_30) + [crc_inv >> 8, crc_inv & 0xFF]

def xor_dispersal(data_bits, prbs_bits):
    return [d ^ p for d, p in zip(data_bits, prbs_bits)]

def fib_crc_ok(fib_bytes):
    if len(fib_bytes) < 32:
        return False
    crc = crc16(fib_bytes[:30])
    stored = (fib_bytes[30] << 8) | fib_bytes[31]
    return (~crc & 0xFFFF) == stored

def test_roundtrip():
    print("=== Round-trip test ===")
    di_table = build_deinterleave_table()
    prbs_bits = prbs(768)  # Full CIF worth

    # Create 3 FIBs with known data
    fib0 = make_fib([0x01, 0x00, 0x00, 0x00, 0x00, 0x12, 0x34] + [0x00]*23)
    fib1 = make_fib([0x02, 0x00, 0x01, 0x00, 0x00, 0x56, 0x78] + [0x00]*23)
    fib2 = make_fib([0x00]*30)
    fibs = fib0 + fib1 + fib2  # 96 bytes
    assert len(fibs) == 96

    # Verify CRCs before processing
    for i in range(3):
        assert fib_crc_ok(fibs[i*32:(i+1)*32]), f"FIB {i} CRC failed before encoding!"
    print("  FIB CRCs valid before encoding ✓")

    # Unpack to bits
    data_bits = unpack_bits(fibs, 768)

    # Energy dispersal
    dispersed_bits = xor_dispersal(data_bits, prbs_bits)

    # Convolutional encode (768 bits → 3072 coded bits)
    coded_bits = conv_encode(dispersed_bits)
    assert len(coded_bits) == 3072, f"Expected 3072, got {len(coded_bits)}"
    print(f"  Encoded: {len(dispersed_bits)} → {len(coded_bits)} coded bits ✓")

    # Map coded bits to soft values: 0 → +1.0, 1 → -1.0
    soft_coded = [1.0 if b == 0 else -1.0 for b in coded_bits]

    # Split into carrier pairs (b0, b1) for 1536 carriers
    b0_logical = [soft_coded[i] for i in range(0, 3072, 2)]  # 1536 values
    b1_logical = [soft_coded[i] for i in range(1, 3072, 2)]  # 1536 values

    # Frequency interleave
    b0_physical = interleave(b0_logical, di_table)
    b1_physical = interleave(b1_logical, di_table)

    # Frequency deinterleave
    b0_recovered = deinterleave(b0_physical, di_table)
    b1_recovered = deinterleave(b1_physical, di_table)

    # Re-interleave to coded bit stream
    soft_recovered = []
    for r, i in zip(b0_recovered, b1_recovered):
        soft_recovered.append(r)
        soft_recovered.append(i)

    # Verify soft bits match
    errors = sum(1 for a, b in zip(soft_coded, soft_recovered) if abs(a - b) > 0.001)
    print(f"  Interleave round-trip errors: {errors}/3072")

    # Viterbi decode
    soft_recovered.extend([0.0] * 24)  # tail padding
    decoded = viterbi_decode(soft_recovered)
    info = decoded[:768]

    # Check decoded bits vs dispersed bits
    bit_errors = sum(1 for a, b in zip(dispersed_bits, info) if a != b)
    print(f"  Viterbi bit errors: {bit_errors}/768")

    # Pack and de-disperse
    fic_bytes = pack_bits(info)

    # De-dispersal per FIB (using continuous PRBS)
    for fib_idx in range(3):
        start_byte = fib_idx * 32
        start_bit = fib_idx * 256
        fib_bits = unpack_bits(fic_bytes[start_byte:start_byte+32], 256)
        dedispersed = xor_dispersal(fib_bits, prbs_bits[start_bit:start_bit+256])
        fib_bytes_out = pack_bits(dedispersed)
        ok = fib_crc_ok(fib_bytes_out)
        print(f"  FIB {fib_idx} CRC: {'OK ✓' if ok else 'FAIL ✗'}")
        if not ok:
            print(f"    Expected: {' '.join(f'{b:02x}' for b in fibs[fib_idx*32:(fib_idx+1)*32])}")
            print(f"    Got:      {' '.join(f'{b:02x}' for b in fib_bytes_out)}")

    # Also test with the WRONG dispersal (per-FIB reset like the bug)
    print("\n  Testing with per-FIB PRBS reset (buggy):")
    prbs_256 = prbs(256)
    for fib_idx in range(3):
        start_byte = fib_idx * 32
        fib_bits = unpack_bits(fic_bytes[start_byte:start_byte+32], 256)
        dedispersed = xor_dispersal(fib_bits, prbs_256)
        fib_bytes_out = pack_bits(dedispersed)
        ok = fib_crc_ok(fib_bytes_out)
        print(f"  FIB {fib_idx} CRC (buggy PRBS): {'OK ✓' if ok else 'FAIL ✗'}")

def test_viterbi_roundtrip():
    print("\n=== Viterbi round-trip test ===")
    import random
    random.seed(42)
    data = [random.randint(0, 1) for _ in range(768)]
    coded = conv_encode(data)
    soft = [1.0 if b == 0 else -1.0 for b in coded]
    soft.extend([0.0] * 24)
    decoded = viterbi_decode(soft)
    errors = sum(1 for a, b in zip(data, decoded[:768]) if a != b)
    print(f"  Bit errors: {errors}/768")
    # First K-1 bits may be wrong
    errors_skip = sum(1 for a, b in zip(data[K-1:], decoded[K-1:768]) if a != b)
    print(f"  Bit errors (skip first {K-1}): {errors_skip}/{768-K+1}")

def test_deinterleaver_consistency():
    """Verify that interleave → deinterleave is identity."""
    print("\n=== Deinterleaver consistency ===")
    table = build_deinterleave_table()

    # Check it's a valid permutation
    sorted_table = sorted(table)
    expected = list(range(NUM_CARRIERS))
    assert sorted_table == expected, "Table is not a valid permutation!"
    print("  Table is a valid permutation ✓")

    # Check round-trip
    data = list(range(NUM_CARRIERS))
    data_f = [float(x) for x in data]
    interleaved = interleave(data_f, table)
    recovered = deinterleave(interleaved, table)
    errors = sum(1 for a, b in zip(data_f, recovered) if abs(a - b) > 0.001)
    print(f"  Interleave → deinterleave errors: {errors}/{NUM_CARRIERS}")

if __name__ == "__main__":
    test_viterbi_roundtrip()
    test_deinterleaver_consistency()
    test_roundtrip()
