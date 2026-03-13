# How DAB Works: From Tuning to Audio

## Overview

Digital Audio Broadcasting (DAB) uses OFDM (Orthogonal Frequency Division Multiplexing) in Mode I for terrestrial broadcast. A single frequency carries an ensemble — a multiplex of several radio stations (services) sharing the same RF channel.

The signal processing pipeline is:

```
RF → IQ samples → Frame Sync → OFDM Demod → FIC/MSC Demux → FEC Decode → Audio Decode
```

## 1. Tuning and IQ Acquisition

DAB Band III channels span 174–240 MHz (e.g., channel 5A = 174.928 MHz, 13B = 232.496 MHz). An RTL-SDR tunes to the center frequency and samples at 2.048 MHz, producing unsigned 8-bit IQ pairs. Each pair is converted to a Complex32 sample:

```
I = (byte[0] - 127.5) / 127.5    → [-1.0, +1.0]
Q = (byte[1] - 127.5) / 127.5    → [-1.0, +1.0]
sample = I + jQ
```

## 2. Frame Synchronization (Null Symbol Detection)

A DAB Mode I transmission frame is 96 ms long and contains:

| Component | Duration (samples) | Purpose |
|---|---|---|
| Null symbol | 2656 | Frame boundary marker (no transmission) |
| PRS (Phase Reference Symbol) | 2552 | Known reference for differential demod |
| 75 data symbols | 75 × 2552 | FIC + MSC data |

The null symbol is a period of near-zero power. The receiver detects it by measuring signal energy in a sliding window: when power drops well below average, a frame boundary is found. The Phase Reference Symbol (PRS) immediately follows the null symbol.

Guard interval correlation refines timing. Each OFDM symbol has a 504-sample cyclic prefix (guard interval) that is a copy of the last 504 samples of the 2048-sample useful part. Correlating `s[n]` with `s[n + 2048]` over the guard interval gives a sharp peak at the correct symbol boundary. A normalized correlation near 1.0 confirms correct timing.

## 3. OFDM Demodulation

Each symbol (2552 samples = 504 guard + 2048 useful) is processed:

### a) Guard Interval Removal

Strip the first 504 samples (cyclic prefix).

### b) Fine Frequency Correction

A frequency offset ε (in sub-carrier spacings) causes a phase rotation of 2πε between guard and useful-part copies. Estimate:

```
ε = arg(Σ s[n+2048] · conj(s[n])) / (2π)     for n = 0..503
```

Apply correction by multiplying each time-domain sample by `exp(-j·2π·ε·n/2048)`.

### c) 2048-Point FFT

Transforms the time-domain symbol into 2048 frequency bins.

### d) Coarse Frequency Correction

Fine estimation only resolves offsets within ±0.5 sub-carrier spacings. Coarse offset (integer bins) is found by searching offsets ±30 bins and measuring DQPSK constellation quality:

```
metric(offset) = mean(cos(4 · arg(z_k)))
```

where `z_k = data[k] · conj(prs[k])` is the differential product. For ideal DQPSK, the metric is −1.0; for random phases, ~0.0. The offset with the most negative metric wins.

### e) Active Carrier Extraction

Of 2048 FFT bins, only 1536 active carriers are used (sub-carriers k = −768..−1 and k = +1..+768; DC bin k=0 is skipped). The coarse offset shifts which bins are extracted.

### f) Differential Demodulation (π/4-DQPSK)

Each symbol is demodulated relative to the previous symbol:

```
z[k] = current[k] · conj(previous[k])
```

The PRS serves as the phase reference for the first data symbol. Per ETSI EN 300 401 Table 42 (Gray-coded DQPSK):

| Bits (b0, b1) | Phase change | Quadrant |
|---|---|---|
| (0, 0) | +π/4 | I > 0, Q > 0 |
| (0, 1) | +3π/4 | I < 0, Q > 0 |
| (1, 1) | −3π/4 | I < 0, Q < 0 |
| (1, 0) | −π/4 | I > 0, Q < 0 |

Soft bits are extracted as: `b0 = Im(z)` (Q axis), `b1 = Re(z)` (I axis). Positive values → bit 0, negative → bit 1. Each symbol yields 1536 × 2 = 3072 soft bits.

## 4. Frequency De-interleaving

Carriers are transmitted in a scrambled order defined by an LCG (Linear Congruential Generator):

```
π(0) = 0
π(j) = (13 × π(j−1) + 511) mod 2048     for j = 1..2047
```

The de-interleaver restores logical (coded-bit) order by applying the forward permutation to the soft-bit pairs.

## 5. Channel Decoding (FEC)

### Convolutional Code

DAB uses a rate-1/4 convolutional code with constraint length K=7 (64 states). The Viterbi decoder takes soft-decision inputs (float values where +1.0 = confident bit 0, −1.0 = confident bit 1) and outputs hard bits.

### FIC (Fast Information Channel)

- Symbols 1–3 (first 3 data symbols after PRS)
- Each symbol: 3072 soft bits → Viterbi (rate 1/4) → 768 information bits = 96 bytes = 3 FIBs
- Total: 9 FIBs per frame

### MSC (Main Service Channel)

- Symbols 4–75 (72 data symbols)
- Organized as 4 CIFs (Common Interleaved Frames) × 18 symbols each
- Uses Equal Error Protection (EEP) or Unequal Error Protection (UEP) with puncturing patterns defined in ETSI EN 300 401 Tables 8/9
- Puncturing removes some coded bits to increase the effective code rate; the decoder must re-insert erasures (depuncturing) before Viterbi decoding

## 6. Energy Dispersal

Before transmission, all data is XORed with a PRBS (Pseudo-Random Binary Sequence) to avoid long runs of identical bits. The PRBS is generated by:

```
Polynomial: x^9 + x^5 + 1
Initial state: all ones (0x1FF)
Output bit = bit[8] XOR bit[4]
```

The receiver must XOR the decoded bytes with the same PRBS sequence to recover the original data. The PRBS is reset at the start of each FIB (96 bytes = 768 bits) and each CIF.

## 7. FIC Parsing — Discovering Services

Each FIB (Fast Information Block) is 32 bytes: 30 bytes of data + 2-byte CRC-16 (polynomial 0x1021, init 0xFFFF, complemented). Valid FIBs contain FIGs (Fast Information Groups) that describe the ensemble:

| FIG Type | Purpose |
|---|---|
| FIG 0/0 | Ensemble identifier (EId, country, change flags) |
| FIG 0/1 | Sub-channel organization (start address, size, protection level) |
| FIG 0/2 | Service organization (service → service component → sub-channel mapping) |
| FIG 0/3 | Service component in packet mode |
| FIG 1/0 | Ensemble label (name of the multiplex) |
| FIG 1/1 | Service label (station name, e.g., "BBC Radio 1") |

### FIB Byte Structure

```
 Byte  0                            29  30  31
┌─────────────────────────────────────┬───────┐
│          FIG data (30 bytes)        │ CRC16 │
│  FIG₀ │ FIG₁ │ … │ FIGₙ │ padding  │       │
└─────────────────────────────────────┴───────┘
```

FIGs are packed back-to-back within the 30-byte payload. Each FIG starts with a 1-byte header; a byte of `0xFF` signals end-of-FIBs (padding). CRC-16 covers bytes 0–29 (polynomial 0x1021, init 0xFFFF, output complemented).

### FIG Byte Structure

Every FIG begins with a 1-byte header (§8.1):

```
Bit  7   6   5   4   3   2   1   0
    ├───────────┼───────────────────┤
    │  type[2:0]│    length[4:0]    │
    └───────────┴───────────────────┘
```

- **type** (bits 7–5): FIG type (0 = MCI/SI, 1 = labels, 5 = FIDC, 6 = CA, 7 = end marker)
- **length** (bits 4–0): number of data bytes that follow this header byte (0–30)

**FIG type 0** — Multiplex Configuration Information (MCI) and Service Information (SI)

The first data byte is a FIG 0 header:

```
Bit  7   6   5   4   3   2   1   0
    ├───┬───┬───┼───────────────────┤
    │C/N│OE │P/D│   extension[4:0]  │
    └───┴───┴───┴───────────────────┘
```

- **C/N** (bit 7): 0 = current configuration, 1 = next configuration
- **OE** (bit 6): 0 = this ensemble, 1 = other ensemble
- **P/D** (bit 5): 0 = 16-bit (programme) service IDs, 1 = 32-bit (data) service IDs
- **extension** (bits 4–0): selects the FIG 0 variant (0 = ensemble info, 1 = sub-channel org, 2 = service org, …)

Remaining bytes are extension-specific fields (identifiers, bit fields, etc.).

**FIG type 1** — Labels

The first data byte is a FIG 1 header:

```
Bit  7   6   5   4   3   2   1   0
    ├───────────────┬───┬───────────┤
    │  charset[3:0] │OE │ ext[2:0]  │
    └───────────────┴───┴───────────┘
```

- **charset** (bits 7–4): character set encoding (0x00 = EBU Latin, 0x06 = UTF-8)
- **OE** (bit 3): other ensemble flag
- **extension** (bits 2–0): selects the label target (0 = ensemble, 1 = programme service, 4 = service component, …)

Following bytes: identifier for the labelled entity, then 16 bytes of label text (space-padded), then a 2-byte character flag field indicating which of the 16 characters are significant.

Scanning a frequency means:
1. Tune to the channel
2. Find frame sync (null symbol)
3. Demodulate and decode FIC symbols
4. Parse FIBs until all FIG 0/1, 0/2, and 1/1 have been received
5. Build a map: Service → Sub-channel (with start CU address, size, protection level)

If no valid FIB CRCs are received after several frames, there is no DAB ensemble on that frequency.

## 8. Selecting and Decoding a Service

Once the ensemble is mapped, selecting a service (station) means:

1. Look up the service's sub-channel parameters:
    - Start address: CU (Capacity Unit) offset within the CIF
    - Sub-channel size: number of CUs (1 CU = 64 bits)
    - Protection level and form: determines which puncturing/depuncturing pattern to use
2. For each CIF (18 MSC symbols), extract the sub-channel's CUs from the interleaved bit stream
3. Depuncture the extracted bits according to the protection profile (re-insert erasures where punctured bits were removed)
4. Viterbi decode to recover the audio frame bytes
5. Energy de-dispersal (XOR with PRBS)
6. The resulting bytes are MPEG-1 Audio Layer II (MP2) frames for DAB, or HE-AAC frames for DAB+
7. Feed to an audio decoder (e.g., symphonia for MP2, fdk-aac for HE-AAC) and output to speakers

## Signal Flow Summary

```
RTL-SDR (2.048 MHz, 8-bit IQ)
    │
    ▼
Null detection → Frame sync
    │
    ▼
Per symbol: strip guard → fine freq correct → 2048-pt FFT → coarse freq correct
    │
    ▼
Extract 1536 carriers → π/4-DQPSK differential demod → 3072 soft bits
    │
    ▼
Frequency de-interleave
    │
    ├─ Symbols 1-3 (FIC) → Viterbi (rate 1/4) → energy de-dispersal → FIB parse → ensemble metadata
    │
    └─ Symbols 4-75 (MSC) → depuncture → Viterbi → energy de-dispersal → MP2/AAC frames → audio
```

## Key Mode I Constants

| Parameter | Value |
|---|---|
| Sample rate | 2.048 MHz |
| FFT size (Tᵤ) | 2048 |
| Guard interval | 504 samples |
| Symbol size | 2552 samples (504 + 2048) |
| Null symbol | 2656 samples |
| Frame duration | ~96 ms |
| Active carriers | 1536 (k = ±1..±768) |
| Sub-carrier spacing | 1 kHz (2.048 MHz / 2048) |
| Symbols per frame | 76 (1 PRS + 75 data) |
| FIC symbols | 3 (symbols 1–3) |
| MSC symbols | 72 (symbols 4–75, 4 CIFs × 18) |

## Services and the Ensemble

### What an Ensemble Is

A DAB **ensemble** (also called a multiplex or "mux") is a collection of services (radio stations, data services) sharing a single RF channel. All services in an ensemble are broadcast together at the same frequency, multiplexed in the time domain across the 72 MSC symbols per frame. A typical ensemble carries 6–12 audio services.

The ensemble has a global identity:

| Field | Description |
|---|---|
| EId (Ensemble Identifier) | 16-bit ID: upper 4 bits = country code (ECC), lower 12 bits = ensemble reference |
| Ensemble label | Human-readable name, e.g. "BBC National DAB" |
| LTO | Local time offset from UTC |

### Roles

| Role | Responsibility |
|---|---|
| **Spectrum regulator** | Assigns DAB frequencies and geographic coverage areas; issues licences to multiplex operators (e.g. Ofcom in the UK, Bundesnetzagentur in Germany) |
| **Multiplex operator** | Holds the spectrum licence; operates the transmitter network; signs contracts with broadcasters; configures the ensemble |
| **Broadcaster / service provider** | Produces the audio programme; delivers an encoded audio stream to the multiplex operator for inclusion |

### Administrative Steps to Join an Ensemble

1. **Spectrum allocation.** The regulator assigns a Band III channel (e.g. 11D = 220.352 MHz) to a multiplex operator for a defined geographic area. The licence specifies power, antenna height, and coverage obligations.

2. **Carriage agreement.** A broadcaster negotiates a carriage contract with the multiplex operator. The contract specifies:
   - Number of Capacity Units (CUs) reserved — directly sets the bitrate: `bitrate = CU_count × 8 kbps / protection_overhead`
   - Protection level (EEP 1-A … 4-B or UEP) — trades bitrate against error resilience
   - Audio codec: DAB (MP2) or DAB+ (HE-AAC)
   - Service IDs and labels to be broadcast

3. **Service ID assignment.** The multiplex operator allocates:
   - **SId** (Service Identifier): 16-bit programme ID, upper nibble = country ECC
   - **SCIdS** (Service Component Identifier within Service): usually 0 for the primary audio component
   - **Sub-channel ID** (SubChId): 6-bit index (0–63), unique within the ensemble

4. **Multiplex reconfiguration.** The operator updates the multiplexer configuration. A **reconfiguration counter** in FIG 0/0 is incremented; receivers detect the change flag and re-read the FIC to update their service maps. New services become visible to receivers within a few seconds (one or more FIC cycles).

### Technical Encoding (Broadcaster → Transmitter)

```
Programme audio
    │
    ▼  Audio codec
MP2 encoder (DAB)  or  HE-AAC encoder (DAB+)
    │  e.g. 128 kbps MP2 = 384 bytes per 96 ms frame
    │
    ▼  Channel coding
Convolutional encoder (K=7, rate 1/4) + puncturing to agreed protection level
    │
    ▼  Multiplexer
Bits slotted into assigned CU range within each CIF
    │  (18 MSC symbols × 4 CIFs, start_CU .. start_CU + size_CU)
    │
    ▼  Ensemble multiplexer
FIC updated with FIG 0/1, 0/2, 1/1 describing the service
    │
    ▼
OFDM modulator → transmitter
```

### How the Receiver Discovers a Service

The FIC (transmitted in symbols 1–3 of every frame) carries the ensemble configuration. On first tune, the receiver collects FIBs until it has seen:

| FIG | What it provides |
|---|---|
| FIG 0/0 | EId, change flags — tells the receiver whether the config is still current |
| FIG 0/1 | Sub-channel table: SubChId → start CU, size, protection level |
| FIG 0/2 | Service → component → SubChId mapping (links SId to a sub-channel) |
| FIG 1/1 | Service label (human-readable station name) |

Once FIG 0/1 and 0/2 are both received for a service, the receiver knows where in the MSC bit stream to find it and how to depuncture and decode it. FIG 1/1 provides the label shown in the UI.

A service can be **removed** by the multiplex operator by omitting its FIG 0/2 entry and incrementing the reconfiguration counter, or by reducing its CU allocation to zero. Receivers that fail to see a service in the FIC for several consecutive frames typically remove it from the service list.

## Terms

### Viterbi Decoder

The Viterbi algorithm finds the most likely sequence of input bits that could have produced a given sequence of (possibly noisy) coded bits, by exhaustively tracking all possible encoder state paths through a trellis.

**Convolutional encoder background.** DAB's rate-1/4 encoder has constraint length K=7, meaning its output at any moment depends on the current input bit and the 6 preceding bits — giving 2⁶ = 64 possible internal states. For every input bit it emits 4 coded bits (one per generator polynomial). The full history of transitions from state to state for all possible input sequences forms the **trellis**.

**Decoding via the trellis.** The decoder maintains one accumulated path metric per state (64 values). For each new group of 4 received soft bits it:

1. Computes the **branch metric** for every possible transition: the sum of squared distances between the received soft values and the ideal ±1 values that transition would have emitted.
2. For each destination state, keeps only the **survivor** — the incoming path with the lower accumulated metric — discarding the other (the ACS: Add-Compare-Select step).
3. Stores the surviving predecessor for each state in a **traceback buffer**.

After processing all coded bits, the decoder traces back through the buffer from the state with the lowest total metric to reconstruct the decoded bit sequence.

**Soft-decision inputs.** DAB uses soft decisions: each received value is a float (e.g. +0.85 or −0.32) rather than a hard 0/1. The branch metric is computed as a Euclidean distance in soft space, which preserves confidence information and gives ~2 dB coding gain over hard decisions.

**DAB parameters.**

| Parameter | Value |
|---|---|
| Constraint length K | 7 |
| States | 64 (2^(K−1)) |
| Native code rate | 1/4 |
| Generator polynomials | G1=133₈, G2=171₈, G3=145₈, G4=133₈ |
| Input per step | 1 bit |
| Output per step | 4 coded bits (before puncturing) |
| Traceback depth | typically 5×K = 35 bits |

**Relationship to puncturing.** When punctured bits are removed before transmission, the receiver re-inserts them as **erasures** (soft value = 0.0, meaning maximum uncertainty) before feeding the sequence to the Viterbi decoder. The decoder treats these as carrying no information and the branch metrics are dominated by the non-erased positions.

### Soft Bits and Hard Bits

A **hard bit** is a definite 0 or 1 decision — the receiver picks whichever value seems more likely and discards the confidence information. Hard bits are simple and cheap to work with, but lose information at the decision boundary.

A **soft bit** is a real-valued confidence score that represents both the decision and how certain the receiver is. Conventions vary; DAB uses a sign-magnitude scheme: positive values indicate bit 0 and negative values indicate bit 1, with larger magnitude meaning higher confidence. For example:
- `+1.5` → strongly 0
- `+0.1` → weakly 0 (near decision boundary)
- `−0.9` → fairly confident 1
- `−2.3` → strongly 1

In DAB's DQPSK demodulator, the I and Q components of the differential product `z = current · conj(previous)` are used directly as soft bits without thresholding. This preserves the analog channel quality all the way through to the Viterbi decoder.

The Viterbi decoder computes branch metrics as Euclidean distances in soft space (e.g., distance between the received `+0.1` and the ideal `+1.0` for a 0 bit). Using soft inputs gives approximately **2 dB of coding gain** over hard decisions — the decoder can make better choices at every branch because it knows which received bits to trust less.

**Erasures** are a special case of soft bits: a value of exactly 0.0 means total uncertainty (the bit was punctured and never transmitted). The Viterbi decoder treats erasures as carrying no branch metric information.

### Puncturing

In DAB, puncturing is a technique to increase the code rate of the convolutional encoder beyond its native rate of 1/4.

The mother convolutional code (K=7, rate 1/4) produces 4 coded bits for every 1 input bit using the 4 generator polynomials (G1=133₈, G2=171₈, G3=145₈, G4=133₈). Puncturing selectively deletes some of those coded bits before transmission, which:

- Increases the code rate (e.g., from 1/4 toward 1/3, 3/8, 1/2, etc.)
- Reduces redundancy → allows higher data throughput at the cost of weaker error correction
- Is defined by a puncturing vector — a pattern of 1s (keep) and 0s (discard) applied cyclically to the encoder output

In practice for DAB:

- **FIC (Fast Information Channel):** Uses puncturing index PI=16, which means no puncturing — all 4 coded bits are kept (full rate 1/4). This gives maximum error protection for the critical ensemble metadata.
- **MSC (Main Service Channel):** Uses various puncturing patterns defined in ETSI EN 300 401 Tables 8a/8b (EEP — Equal Error Protection). Different audio services can use different protection levels (EEP 1-A through 4-A, 1-B through 4-B), each with a specific puncturing vector that trades off bitrate vs. robustness.
- **UEP (Unequal Error Protection):** Some services use different puncturing rates for different parts of the audio frame — the header gets stronger protection (lower code rate) while the body uses weaker protection (higher code rate).

The receiver needs to know the puncturing pattern to depuncture — inserting erasures (zero-confidence soft bits) at the positions where coded bits were deleted — before feeding the data to the Viterbi decoder. The `crates/fec` crate in this project has 24 EEP/UEP depuncturing vectors for this purpose.

## Example

This example traces a complete DAB Mode I frame — one 96 ms transmission cycle — from raw sample offsets through to decoded audio bytes, using realistic but simplified values.

### Frame Layout

A single frame at 2.048 MHz occupies 196,608 samples (96 ms × 2,048,000 samples/s):

```
Sample offset    Length    Content
─────────────────────────────────────────────────────────────────
0                2,656     Null symbol (near-zero power, ~30 dB drop)
2,656            2,552     PRS — Phase Reference Symbol
5,208            2,552     Symbol  1 ─┐
7,760            2,552     Symbol  2  ├─ FIC (3 symbols)
10,312           2,552     Symbol  3 ─┘
12,864           2,552     Symbol  4 ─┐
...                        ...        ├─ MSC CIF 0 (symbols 4–21, 18 symbols)
57,800           2,552     Symbol 21 ─┘
60,352           2,552     Symbol 22 ─┐
...                        ...        ├─ MSC CIF 1 (symbols 22–39)
...                        ...        ├─ MSC CIF 2 (symbols 40–57)
...                        ...        ├─ MSC CIF 3 (symbols 58–75)
193,552          2,552     Symbol 75 ─┘
─────────────────────────────────────────────────────────────────
Total            196,104 samples used  (+ ~504 trailing guard)
```

### Step 1 — Null Symbol Detection

A sliding 2,656-sample energy window sees power collapse:

```
samples 0–2655:   mean power ≈ 0.002   ← null symbol
samples 2656–:    mean power ≈ 0.85    ← PRS begins
```

The frame boundary is flagged at sample offset 0. Guard-interval correlation over the PRS (samples 2,656–3,159) produces a normalised peak of ~0.97, confirming ±0 sample timing error.

### Step 2 — Fine Frequency Correction (PRS)

```
Σ s[n+2048] · conj(s[n])  for n = 2656..3159
  → complex sum ≈ 0.89 · exp(j · 0.031)
  → ε = 0.031 / (2π) ≈ +0.005 sub-carrier spacings   (≈ +5 Hz)
```

Each PRS sample is multiplied by `exp(-j·2π·0.005·n/2048)` before the FFT.

### Step 3 — OFDM Demodulation of One Symbol (Symbol 1)

Taking Symbol 1 (FIC, sample offset 5,208):

```
1. Strip guard:   discard samples 5,208–5,711 (504 samples)
2. FFT input:     samples 5,712–7,759 (2,048 samples)
3. 2048-pt FFT:   → complex spectrum X[0..2047]
4. Coarse search: offsets −30..+30; best metric at offset 0 → no coarse shift
5. Extract carriers k = −768..−1, +1..+768  (skip DC k=0)
```

Differential product against PRS reference:

```
z[k] = X₁[k] · conj(X_PRS[k])

Example carriers (after de-interleaving, first 4 logical positions):
  k=0 (logical):  z = (+0.72 + j·0.71)  → phase ≈ +π/4  → bits (0,0)
  k=1 (logical):  z = (−0.68 + j·0.73)  → phase ≈ +3π/4 → bits (0,1)
  k=2 (logical):  z = (−0.70 − j·0.69)  → phase ≈ −3π/4 → bits (1,1)
  k=3 (logical):  z = (+0.71 − j·0.70)  → phase ≈ −π/4  → bits (1,0)
```

Soft bits for these 4 carriers: `[+0.71, +0.72, +0.73, −0.68, −0.69, −0.70, −0.70, +0.71, …]`

Symbol 1 produces 1,536 carriers × 2 soft bits = **3,072 soft bits**.

### Step 4 — FIC Decoding (Symbols 1–3)

Three symbols × 3,072 soft bits = 9,216 soft bits fed to the Viterbi decoder (rate 1/4, K=7, no puncturing):

```
9,216 coded soft bits ÷ 4 = 2,304 decoded bits = 288 bytes
288 bytes ÷ 3 FIBs per symbol = 3 symbols × 3 FIBs = 9 FIBs total
Each FIB = 32 bytes (30 data + 2 CRC)
```

After energy de-dispersal (XOR with PRBS, reset each FIB), FIB 0 passes CRC and contains:

```
FIG 0/0  (3 bytes payload):
  EId = 0x1023,  country = DE,  change flags = 0

FIG 0/1  (10 bytes payload):
  Sub-channel 4:  start CU = 188,  size = 84 CUs,  EEP 3-A
  Sub-channel 7:  start CU = 272,  size = 54 CUs,  EEP 2-A

FIG 1/1  (21 bytes payload):
  Service 0xD220:  label = "Deutschlandfunk   "  (16 chars, flag = 0xFF00)
```

### Step 5 — MSC Sub-channel Extraction (CIF 0, Sub-channel 4)

Sub-channel 4: start CU = 188, size = 84 CUs. One CIF = 18 symbols × 1,536 carriers × 2 bits = 55,296 bits = 864 CUs.

```
Bit offset into CIF:  188 × 64 = 12,032
Bit count:            84  × 64 = 5,376 bits
```

These 5,376 bits are extracted from the de-interleaved bit stream of symbols 4–21.

### Step 6 — Depuncturing and Viterbi (EEP 3-A)

EEP 3-A at 84 CUs uses puncturing pattern PI=7 for the first sub-region, PI=3 for the tail. After depuncturing, erasures are inserted where bits were punctured:

```
5,376 coded bits  →  depuncture  →  ~21,504 soft inputs (with erasures = 0.0)
Viterbi decoder:  ~21,504 inputs → 1,344 decoded bits = 168 bytes per CIF
4 CIFs × 168 bytes = 672 bytes per frame
```

After energy de-dispersal (PRBS reset each CIF), the 672 bytes form a partial **MPEG-1 Layer II audio frame** (MP2). At 128 kbit/s the full MP2 frame is 768 bytes (48 kHz, 1152 PCM samples), spanning slightly more than one DAB frame — the audio decoder handles the frame boundary.

### Frame Summary

```
One 96 ms DAB frame:
  ├─ 9 FIBs decoded  → ensemble + service metadata refreshed
  └─ 4 CIFs decoded  → 672 bytes MP2 audio data
                        → ~1,152 PCM stereo samples @ 48 kHz after MP2 decode
                        → ~24 ms of audio output
```

Four frames (384 ms) fill one complete MP2 audio frame, maintaining continuous playback.

## Example — DAB+

DAB+ reuses the same OFDM physical layer and FIC parsing as DAB. The differences start after Viterbi decoding: a Reed-Solomon outer code and HE-AAC audio codec replace the direct MP2 stream. This example uses a 48 kbps HE-AAC v2 stereo service.

FIB parsing reveals the sub-channel is flagged as DAB+ (ASCTy = 0x3F in FIG 0/2):

```
FIG 0/1:
  Sub-channel 3:  start CU = 84,  size = 48 CUs,  EEP 2-A

FIG 0/2 (service component):
  Service 0xD310, SCId = 0, ASCTy = 0x3F  ← DAB+ indicator
  Sub-channel ID = 3
```

### Step 1 — Sub-channel Extraction (same as DAB)

Sub-channel 3: start CU = 84, size = 48 CUs.

```
Bit offset into CIF:  84 × 64 = 5,376
Bit count:            48 × 64 = 3,072 bits per CIF
4 CIFs per frame  →  12,288 bits per frame
```

### Step 2 — Depuncturing and Viterbi (EEP 2-A)

EEP 2-A at 48 CUs: puncturing pattern PI=13 for the main region, PI=12 for the tail.

```
3,072 coded bits  →  depuncture  →  ~11,264 soft inputs
Viterbi:  11,264 inputs → 768 decoded bits = 96 bytes per CIF
4 CIFs per frame → 384 bytes per logical frame
Energy de-dispersal (PRBS, reset each CIF) → 384 bytes net
```

Gross bitrate: 384 bytes × 8 / 0.096 s = **32 kbps net** (after Viterbi). With DAB+ overhead the HE-AAC payload is ~48 kbps effective audio.

### Step 3 — Super-frame Assembly

DAB+ groups **5 consecutive logical frames** into one audio super-frame for Reed-Solomon protection. With 384 bytes per logical frame:

```
5 frames × 384 bytes = 1,920 bytes of RS input
```

These bytes are arranged into RS codewords. The RS code is RS(120, 110, t=5) over GF(2⁸):

```
n_rs  = ceil(1,920 / 110) = 18 codewords  (18 × 110 = 1,980; pad 60 bytes with 0x00)
Input to RS encoder: 18 × 110 = 1,980 bytes (includes 60 bytes padding)
RS adds 18 × 10 = 180 parity bytes
Total RS block:  18 × 120 = 2,160 bytes transmitted
```

After RS correction at the receiver, the 60 padding bytes are discarded:

```
RS decode: 2,160 bytes → 1,980 bytes → strip 60 padding → 1,920 bytes super-frame payload
RS can correct up to 5 byte errors (or 10 erasures) per 120-byte codeword
```

### Step 4 — Super-frame Sync (Fire Code)

The first 3 bytes of the super-frame carry a **Fire code** (BCH(16,5)) used to confirm super-frame alignment and detect bit errors in the header:

```
Byte 0:  0x00        ← DAB+ sync word (STC = 0, DACf = 0, GS = 00)
Byte 1:  RFA | num_AUs << 4 | ...
Byte 2:  Fire code CRC

If Fire code passes → super-frame boundary confirmed
```

The header also encodes the number of audio access units (AUs) in this super-frame and whether HE-AAC v1 or v2 is in use.

### Step 5 — Access Unit Extraction

Remaining super-frame bytes are packed HE-AAC **access units**. Each AU is preceded by a 12-bit length field in a header table at the start of the super-frame:

```
Super-frame (1,920 bytes):
  ├─ 3 bytes:  Fire code header
  ├─ n × 2 bytes:  AU length table  (one 16-bit entry per AU, minus the last)
  └─ AUs packed back-to-back:
       AU 0:  248 bytes  (HE-AAC frame, 1,024 PCM samples)
       AU 1:  251 bytes
       AU 2:  249 bytes
       AU 3:  250 bytes
       AU 4:  248 bytes  ← last AU length inferred from super-frame size
       (each AU ends with a 2-byte AU CRC)
```

5 AUs × 1,024 PCM samples = **5,120 samples** per super-frame. At 48 kHz: 5,120 / 48,000 ≈ **106.7 ms** of audio per super-frame (5 DAB frames × 96 ms / 4.5 ≈ matches closely when SBR doubles the 24 kHz core rate).

### Step 6 — HE-AAC Decoding

Each AU is passed to the HE-AAC v2 decoder (fdk-aac):

```
AU bytes → HE-AAC v2 decoder (core AAC-LC + SBR + PS):
  Core AAC-LC:  decodes 512 samples @ 24 kHz core rate
  SBR:          spectral band replication → 1,024 samples @ 48 kHz
  PS:           parametric stereo → 2 channels
Output per AU:  1,024 × 2 ch = 2,048 PCM samples @ 48 kHz
```

### Super-frame Summary

```
Five 96 ms DAB frames (480 ms total) → one DAB+ audio super-frame:
  ├─ FIC decoded each frame → ensemble metadata (same as DAB)
  └─ 5 logical frames assembled
       → RS(120,110) outer FEC corrects burst errors
       → Fire code sync confirmed
       → 5 HE-AAC AUs decoded
       → 5,120 stereo samples @ 48 kHz  ≈ 106.7 ms audio output
```

Compared to DAB, the RS outer code provides an additional error-correction layer on top of Viterbi, making DAB+ significantly more robust at the same transmit power. The tradeoff is a 480 ms minimum latency before the first audio (one full super-frame must be received before HE-AAC decoding can begin).



## References

- ETSI EN 300 401 — Radio Broadcasting Systems; Digital Audio Broadcasting (DAB) to mobile, portable and fixed receivers
  - §5.1: Transmission frame structure
  - §14.4: DQPSK symbol mapping (Table 42)
  - §14.6: Frequency interleaving
  - §11: FIC structure and FIG encoding
  - §12: Energy dispersal
