# dab-rtl
**NOT WORKING**




A pure-Rust DAB/DAB+ software-defined radio receiver for RTL-SDR and HackRF.

## Features

- Scan for DAB stations by Band III channel
- Play any station by name (CLI or TUI)
- Select audio output device (ALSA / PulseAudio)
- Low power footprint — no external DAB decoding daemon required

## Requirements

### Native build

| Dependency | Debian/Ubuntu package |
|---|---|
| librtlsdr | `librtlsdr-dev` |
| ALSA | `libasound2-dev` |
| PulseAudio (optional) | `libpulse-dev` |
| pkg-config | `pkg-config` |

```bash
sudo apt-get install librtlsdr-dev libasound2-dev pkg-config
cargo build --release
```

## Usage

```bash
# List connected RTL-SDR devices
dab-rtl list-devices

# List audio output devices
dab-rtl list-audio

# Scan channel 11C for stations
dab-rtl scan --channel 11C

# Play a station by name on channel 11C
dab-rtl play --channel 11C --station "BBC Radio 4"

# Use a specific audio output device
dab-rtl play --channel 11C --station "Radio 3" --audio-device "hw:1,0"

# Use a different RTL-SDR device (index 1) with PPM correction
dab-rtl --device 1 --ppm 42 scan --channel 11C
```

Enable debug logging with `RUST_LOG=debug`.

## Running in Docker

The container needs access to the USB RTL-SDR device and the host audio system.

### ALSA audio

```bash
docker build -t dab-rtl .

docker run --rm \
  --device /dev/bus/usb \
  --group-add audio \
  -v /run/user/$(id -u)/pulse:/run/user/1000/pulse \
  -e PULSE_SERVER=unix:/run/user/1000/pulse/native \
  dab-rtl scan --channel 11C
```

### PulseAudio audio

```bash
docker run --rm \
  --device /dev/bus/usb \
  -e PULSE_SERVER=unix:${XDG_RUNTIME_DIR}/pulse/native \
  -v ${XDG_RUNTIME_DIR}/pulse/native:${XDG_RUNTIME_DIR}/pulse/native \
  --group-add $(getent group audio | cut -d: -f3) \
  dab-rtl play --channel 11C --station "BBC Radio 4"
```

### RTL-SDR USB device permissions

If the RTL-SDR is not accessible, add a udev rule on the **host**:

```bash
# /etc/udev/rules.d/20-rtlsdr.rules
SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2838", MODE="0664", GROUP="plugdev"
```

Then reload udev and replug the device:

```bash
sudo udevadm control --reload-rules && sudo udevadm trigger
sudo usermod -aG plugdev $USER   # log out and back in
```

Alternatively pass the device explicitly:

```bash
# Find the bus and device number
lsusb | grep Realtek
# e.g.: Bus 001 Device 004

docker run --rm \
  --device /dev/bus/usb/001/004 \
  dab-rtl list-devices
```

## Architecture

```
RTL-SDR IQ samples
       │  (sdr crate)
       ▼
OFDM demodulator         rustfft + num-complex
  frame sync + FFT + π/4-DQPSK
       │  (ofdm crate)
    ┌──┴──┐
    ▼     ▼
  FIC    MSC              (protocol crate)
  meta   audio frames
    │     │
    ▼     ▼
FIB parser   Viterbi FEC  (fec crate)
  ensemble   depuncturing
  services
              │
         ┌────┴────┐
         ▼         ▼
      MP2 (DAB)  HE-AAC (DAB+)
      symphonia  fdk-aac
              │
       cpal audio output  (audio crate)
```

## DAB Band III Channel Table

| Channel | Frequency |
|---------|-----------|
| 5A | 174.928 MHz |
| 5B | 176.640 MHz |
| … | … |
| 11C | 220.352 MHz |
| 11D | 222.064 MHz |
| … | … |
| 13F | 239.200 MHz |

Full table: run `dab-rtl scan --channel <name>` for any channel 5A–13F.

## Thanks

This project stands on the shoulders of the broader open-source SDR community.

- **[welle.io](https://github.com/AlbrechtL/welle.io)** — An open-source DAB/DAB+ receiver that served as the primary inspiration for the signal pipeline and protocol implementation in this project.
- **[librtlsdr](https://github.com/osmocom/rtl-sdr)** — The foundational C library that enables software-defined radio with low-cost RTL-SDR hardware.
- **[rtlsdr_mt](https://crates.io/crates/rtlsdr_mt)** — The Rust bindings to librtlsdr used for hardware access.
- **[Symphonia](https://github.com/pdeljanov/Symphonia)** — A pure-Rust audio decoding library used for MP2 playback.
- **[rustfft](https://github.com/ejmahler/RustFFT)** — High-performance FFT used in OFDM demodulation.
- **[ratatui](https://github.com/ratatui/ratatui)** — The terminal UI framework powering the interactive station browser.
- The **ETSI EN 300 401** standard authors for publicly documenting the DAB specification.

## License

This project is licensed under the [MIT License](LICENSE).

> **Note:** At runtime, this application links against `librtlsdr`, which is licensed under GPL-2.0+.
> If you distribute a compiled binary, you may need to comply with the GPL for the combined work.
