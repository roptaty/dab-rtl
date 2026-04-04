# DAB/DAB+ Receiver Specification

## Purpose

This document describes the receiver behavior the application should implement.

The application shall:

1. Scan DAB Band III frequencies
2. Detect whether a frequency carries a valid DAB ensemble
3. Identify the services available on that frequency
4. Let the user choose a service
5. After selection, focus on that service: play audio and display its metadata
6. When available, display structured now-playing data and slideshow / cover art

This is an implementation-oriented specification, not a tutorial on every part of the standards.

## User-Facing Behavior

### Scan Mode

In scan mode, the application should:

1. Step through known DAB Band III channels
2. Attempt OFDM lock on each frequency
3. Decode FIC data long enough to discover the ensemble and its services
4. Build a service list with:
   - Frequency
   - Ensemble label
   - Service label
   - Audio type: DAB or DAB+
   - Known metadata capabilities when signalled

The purpose of scan mode is discovery only. It should stop being the primary UI once a user starts playing a service.

### Playback Mode

Once the user selects a service, the application should switch to a playback-focused state.

Playback mode should:

1. Continuously decode the selected service's audio
2. Output PCM audio to the audio device
3. Show the selected service label prominently
4. Show dynamic label text for the selected service
5. Show structured artist/title when DL+ is available
6. Show slideshow / cover art when available
7. Continue monitoring FIC in the background for reconfiguration or label updates

Playback mode should not keep scan details as the primary focus. The selected station is the main object in the UI.

## RF and OFDM Layer

### Frequencies

The receiver should scan standard DAB Band III channel centers, for example:

- `5A = 174.928 MHz`
- `5B = 176.640 MHz`
- `5C = 178.352 MHz`
- `5D = 180.064 MHz`
- `...`
- `13F = 239.200 MHz`

The tuner should use:

- Center frequency = channel center
- Sample rate = `2.048 MHz`
- RTL-SDR unsigned 8-bit IQ samples

Each IQ byte pair is converted to complex baseband:

```text
I = (i_byte - 127.5) / 127.5
Q = (q_byte - 127.5) / 127.5
sample = I + jQ
```

### Mode I Constants

| Parameter | Value |
|---|---|
| Sample rate | `2.048 MHz` |
| FFT size | `2048` |
| Guard interval | `504 samples` |
| OFDM symbol length | `2552 samples` |
| Null symbol | `2656 samples` |
| Active carriers | `1536` |
| Symbols after null | `1 PRS + 75 data symbols` |
| Frame duration | `96 ms` |
| Frame length | `196608 samples` |

### Synchronization

The receiver should lock a frame using:

1. Null symbol detection
2. PRS timing acquisition
3. Guard interval correlation to refine symbol timing
4. Fine frequency correction from guard correlation
5. Coarse carrier offset correction before carrier extraction

### OFDM Demodulation

For each data symbol:

1. Remove guard interval
2. Apply fine frequency correction
3. Run a `2048`-point FFT
4. Extract the `1536` active carriers
5. Apply differential demodulation against the previous symbol
6. Produce `3072` soft bits per symbol
7. Frequency de-interleave the carrier order

The PRS is used as the phase reference for the first data symbol.

## Logical Channel Structure

After OFDM demodulation, the receiver sees two logical channels:

- `FIC` = Fast Information Channel
- `MSC` = Main Service Channel

### FIC

In Mode I:

- The first `3` data symbols belong to the FIC
- There are `4` CIF periods per transmission frame
- Each CIF contributes `3` FIBs
- Therefore, a full Mode I transmission frame contains `12` FIBs

Each FIB is:

- `32 bytes`
- `30 bytes` FIB data field
- `2 bytes` CRC

Energy dispersal for the FIC is applied to the `3-FIB` group associated with a CIF, not separately per individual FIB.

### MSC

In Mode I:

- Symbols `4` through `75` belong to the MSC
- The MSC is organized as `4 CIFs`
- Each CIF contains `18` OFDM symbols
- Each CIF contains `864` Capacity Units
- `1 CU = 64 bits`

The MSC carries:

- Audio stream-mode components
- Packet-mode components
- PAD-associated data carried alongside audio

## FIC Parsing and Service Discovery

The FIC is used to discover the ensemble and map services to components.

The receiver shall parse at least:

- `FIG 0/0` ensemble information
- `FIG 0/1` sub-channel organization
- `FIG 0/2` service organization
- `FIG 0/3` packet-mode service components
- `FIG 1/0` ensemble label
- `FIG 1/1` service label

To support metadata and slideshow properly, the receiver should also parse:

- `FIG 0/13` user application signalling

### Required Discovery Output

For each discovered service, the receiver should build a structure containing:

- Service ID
- Service label
- Whether the primary audio component is DAB or DAB+
- Component list
- For each component:
  - Sub-channel ID
  - Start address in CU
  - Size in CU
  - Protection profile
  - Packet address if packet mode
  - User applications if signalled

### When a Frequency Counts as Valid

A frequency should be treated as carrying a DAB ensemble only after the receiver gets repeated valid FIB CRCs and enough FIG data to identify at least:

- Ensemble ID
- At least one service
- A consistent sub-channel map

Failure to decode FIB CRCs does not prove there is no ensemble. It can also indicate weak signal or poor synchronization.

## Selecting a Service

When the user selects a service, the receiver should:

1. Resolve the service to its audio component
2. Resolve the component to:
   - Sub-channel ID
   - Start CU
   - Size in CU
   - Protection profile
   - Audio type
3. Start decoding that sub-channel continuously
4. Keep background FIC monitoring active
5. Reset service-local metadata assemblers so stale data is not shown after a station switch

If the service has no decodable audio component, the receiver should report that clearly and stay in browse mode.

## MSC Processing

For the selected component, MSC decoding shall follow this order:

1. Extract the sub-channel CU range from each CIF
2. Apply MSC time de-interleaving
3. Depuncture according to the signalled protection profile
4. Run Viterbi decoding
5. Apply energy de-dispersal
6. Interpret the decoded bytes according to component type

This order matters. For a working receiver, it is not enough to extract bytes from the CIF and send them directly to an audio decoder.

## DAB Audio Path

For legacy DAB audio services:

1. The audio component yields MPEG Layer II frame data after MSC decoding
2. MPEG Layer II frames are decoded to PCM
3. PAD data is extracted from each MPEG audio frame

For MP2 services:

- F-PAD is the last `2` bytes of the MPEG audio frame
- X-PAD, when present, occupies bytes immediately before F-PAD in the ancillary area

## DAB+ Audio Path

For DAB+ services:

1. The MSC output is not yet directly playable AAC
2. DAB+ audio is assembled across `5 CIFs` into a superframe
3. Firecode is used to confirm superframe alignment
4. Access Units are extracted from the superframe
5. AU CRCs are checked
6. Raw AAC AUs are fed to the HE-AAC decoder
7. PAD data is extracted from the AU payload

Important points:

- DAB+ uses additional framing beyond plain MSC decoding
- Access Units are `960-sample` AAC frames, not 1024-sample ADTS-style frames
- PAD in DAB+ is carried in a `data_stream_element()` at the start of the AU payload

## Metadata Model

The receiver should support four increasingly rich layers of metadata:

1. Static service label
2. Dynamic label text
3. Structured now-playing fields
4. Slideshow / cover art

### 1. Static Labels

Static labels come from the FIC:

- Ensemble label: `FIG 1/0`
- Service label: `FIG 1/1`

These are used for the service list and as a fallback display when no dynamic metadata is present.

### 2. Dynamic Label Segment (DLS)

DLS carries dynamic text such as:

- Artist and title in a single string
- Programme title
- Presenter or show information
- Other now-playing text

### Primary DLS Transport

The primary DLS transport is `PAD/X-PAD` inside the selected audio service itself.

That means:

- DLS is usually embedded in the audio service path
- It is not usually a separate packet-mode component
- A receiver should treat X-PAD parsing as the main metadata path

### Secondary DLS Transport

Some services may also carry DLS or DLS-like data in packet mode.

That path depends on:

- `FIG 0/3` to identify the packet component and packet address
- User application signalling to identify the application running on that component

Packet-mode metadata should be treated as secondary or fallback behavior, not the baseline design.

## PAD, F-PAD, and X-PAD

To display real metadata, the receiver must parse PAD.

### F-PAD

F-PAD is the fixed part of PAD and is used to indicate whether X-PAD is present and how it is structured.

For implementation purposes, F-PAD parsing should expose at least:

- X-PAD presence / type
- Whether a Content Indicator list is present

### X-PAD

X-PAD is the variable part of PAD.

It carries application payloads such as:

- DLS
- DL+
- MOT data groups used for slideshow

When a Content Indicator list is present, the receiver uses it to determine which application payloads are present in the current frame or AU.

The receiver should maintain X-PAD assembly state across audio frames and reset it on service changes.

## DL+ Structured Metadata

Plain DLS is just text. It does not reliably separate artist and title.

DL+ provides structure by tagging character ranges inside the current DLS string.

Typical fields include:

- Title
- Artist
- Programme now
- Programme next

Receiver behavior:

1. Reassemble the current DLS text
2. Decode any DL+ command information associated with that text
3. Use DL+ tags to extract structured fields
4. If DL+ is absent, display the raw DLS string without trying to force a split

Heuristic splitting of DLS text may be offered as a UI fallback, but it should not be considered standards-based structured metadata.

## Slideshow and Cover Art

Cover art and slideshow images should be treated as a separate metadata layer.

### Transport

Slideshow is carried as `MOT` objects.

MOT data may be transported through:

- X-PAD associated with the audio service
- Packet-mode components

### Signalling

The receiver should use `FIG 0/13` user application signalling to determine which components carry applications such as slideshow.

The implementation should preserve unknown application identifiers as raw values so support can be extended safely later.

### Receiver Behavior

For slideshow support, the receiver should:

1. Detect that a slideshow-capable user application is signalled
2. Collect the relevant PAD or packet-mode data groups
3. Reassemble MOT headers and bodies
4. Decode supported image formats such as JPEG and PNG
5. Update the now-playing view when a new valid image arrives

Slideshow support is optional for basic audio playback, but it is part of the target feature set for a full receiver.

## Minimum Receiver Behavior for a Selected Service

When a service is playing, the application should aim to display the best available information in this order:

1. Service label
2. DLS raw text
3. DL+ title and artist
4. Slideshow image

If richer metadata is unavailable, the receiver should fall back gracefully without clearing the whole display unnecessarily.

## Application State Requirements

The application should maintain separate state for:

- Ensemble discovery
- Selected service
- Audio decoding
- Now-playing metadata
- Slideshow image state

Changing service should:

1. Keep the current ensemble map if the frequency is unchanged
2. Reset the audio decoder state for the new service
3. Reset X-PAD and packet metadata assemblers
4. Clear stale slideshow images unless they are explicitly tied to the new service

Retuning to a new frequency should:

1. Reset OFDM state
2. Reset FIC and MSC state
3. Reset selected-service playback state
4. Clear service-local metadata

## What This Repository Already Implements

The current repository already contains significant parts of the receive chain:

- OFDM synchronization and demodulation
- Frequency de-interleaving
- FIC parsing for core service discovery
- MSC extraction and decoding
- DAB+ audio decoding path
- X-PAD DLS and DL+ parsing
- Packet-mode DLS fallback handling
- TUI-based service list and now-playing updates

The spec in this document should stay aligned with that architecture.

## Known Gaps Relative to This Specification

The current codebase does not yet fully implement everything required by this spec.

Main gaps:

1. `FIG 0/13` user application parsing
2. Explicit capability mapping per service component
3. MOT reassembly for slideshow / cover art
4. Playback-first UI state after service selection
5. Robust handling of all metadata transports and reconfiguration cases

## Implementation Plan

### Phase 1: Align the Receiver Model

Goal: make the application behavior match this specification without changing the overall architecture.

Tasks:

1. Treat X-PAD as the primary metadata path for selected services
2. Keep packet-mode DLS as fallback only
3. Ensure service selection resets metadata state cleanly
4. Ensure playback mode centers the selected station in the UI

Expected code areas:

- `crates/app/src/pipeline.rs`
- `crates/app/src/tui.rs`
- `crates/protocol/src/pad.rs`

### Phase 2: Parse User Application Signalling

Goal: discover which metadata applications are present on which service components.

Tasks:

1. Add `FIG 0/13` parsing to the FIC parser
2. Extend ensemble/service/component data structures to store user applications
3. Expose application capabilities in pipeline updates and UI state

Expected code areas:

- `crates/protocol/src/fib.rs`
- `crates/protocol/src/ensemble.rs`
- `crates/app/src/pipeline.rs`
- `crates/app/src/tui.rs`

### Phase 3: Tighten Playback Mode

Goal: after a service is selected, make the app behave like a radio player, not a scanner.

Tasks:

1. Add an explicit playback-focused UI mode
2. Show:
   - Service label
   - Audio status
   - DLS text
   - DL+ title and artist
   - Slideshow status or image
3. Minimize scan and ensemble details while listening
4. Preserve background monitoring without making it the primary view

Expected code areas:

- `crates/app/src/tui.rs`
- `crates/app/src/main.rs`

### Phase 4: Implement Slideshow / Cover Art

Goal: support MOT-based image display for the selected service.

Tasks:

1. Add MOT data group reassembly
2. Add MOT header/body parsing
3. Decode JPEG/PNG slideshow images
4. Associate slideshow state with the selected service
5. Display images in terminals that support graphics, with text fallback elsewhere

Expected code areas:

- `crates/protocol/` new MOT module
- `crates/app/src/pipeline.rs`
- `crates/app/src/tui.rs`

### Phase 5: Improve Robustness

Goal: make the receiver more resilient and closer to a production-quality design.

Tasks:

1. Improve protection-profile handling where current decoding is approximate
2. Monitor ensemble reconfiguration and refresh service mappings safely
3. Expand tests around:
   - FIC parsing
   - X-PAD DLS / DL+
   - Service switching
   - DAB+ superframe handling
   - MOT slideshow assembly

Expected code areas:

- `crates/fec/`
- `crates/protocol/`
- `crates/app/`
- existing tests plus new fixture-based tests

## Acceptance Criteria

The implementation adheres to this spec when all of the following are true:

1. The app can scan frequencies and list valid services
2. Selecting a service starts or attempts audio playback immediately
3. The selected service becomes the focus of the UI
4. Static service labels are shown correctly
5. DLS text is shown when present
6. DL+ title and artist are shown separately when present
7. Stale metadata is cleared or replaced correctly on service change
8. Slideshow images are shown when supported and available
9. FIC reconfiguration does not leave the app in an inconsistent state

## Non-Goals

This document does not attempt to specify:

- Every bitfield of every ETSI table
- Every regional scanning policy
- Hybrid RadioDNS behavior
- Advanced data applications beyond what is required for now-playing and slideshow support

Those may be added later if the project grows beyond the current receiver goals.
