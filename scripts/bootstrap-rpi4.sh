#!/usr/bin/env bash
#
# Bootstrap script for building dab-rtl on a Raspberry Pi 4 (Raspberry Pi OS / Debian).
# Run as a regular user (uses sudo where needed).
#
set -euo pipefail

info()  { printf '\033[1;32m[INFO]\033[0m  %s\n' "$*"; }
warn()  { printf '\033[1;33m[WARN]\033[0m  %s\n' "$*"; }
error() { printf '\033[1;31m[ERROR]\033[0m %s\n' "$*" >&2; exit 1; }

# ---------- System packages ----------

info "Updating package lists"
sudo apt-get update

info "Installing native build dependencies"
sudo apt-get install -y \
    build-essential \
    pkg-config \
    cmake \
    git \
    curl \
    librtlsdr-dev \
    libasound2-dev \
    libfdk-aac-dev

# ---------- Rust toolchain ----------

if command -v rustup &>/dev/null; then
    info "Rust toolchain already installed ($(rustc --version))"
    rustup update stable
else
    # Remove distro-packaged Rust if present (conflicts with rustup)
    if dpkg -l rustc &>/dev/null 2>&1; then
        warn "Removing distro-packaged Rust to avoid conflicts"
        sudo apt-get remove -y rustc cargo rust-all 2>/dev/null || true
    fi

    info "Installing Rust via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi

# Ensure cargo is on PATH for the rest of this script
export PATH="$HOME/.cargo/bin:$PATH"

# ---------- RTL-SDR udev rules (allow non-root access) ----------

UDEV_RULE="/etc/udev/rules.d/20-rtlsdr.rules"
if [ ! -f "$UDEV_RULE" ]; then
    info "Installing RTL-SDR udev rules for non-root USB access"
    sudo tee "$UDEV_RULE" >/dev/null <<'RULES'
# RTL2832U DVB-T dongles — allow non-root access
SUBSYSTEM=="usb", ATTR{idVendor}=="0bda", ATTR{idProduct}=="2832", MODE="0666"
SUBSYSTEM=="usb", ATTR{idVendor}=="0bda", ATTR{idProduct}=="2838", MODE="0666"
RULES
    sudo udevadm control --reload-rules
    sudo udevadm trigger
    info "Udev rules installed — re-plug the RTL-SDR dongle if it is already connected"
else
    info "RTL-SDR udev rules already present"
fi

# ---------- Blacklist DVB-T kernel driver ----------

BLACKLIST="/etc/modprobe.d/blacklist-dvb.conf"
if [ ! -f "$BLACKLIST" ]; then
    info "Blacklisting dvb_usb_rtl28xxu kernel module (interferes with direct SDR access)"
    sudo tee "$BLACKLIST" >/dev/null <<'CONF'
blacklist dvb_usb_rtl28xxu
blacklist rtl2832
blacklist rtl2830
CONF
    # Unload if currently loaded
    sudo modprobe -r dvb_usb_rtl28xxu 2>/dev/null || true
    info "Kernel module blacklisted — a reboot may be needed if the dongle is in use"
else
    info "DVB-T kernel modules already blacklisted"
fi

# ---------- Build ----------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

info "Building dab-rtl (release mode)"
cd "$PROJECT_DIR"
cargo build --release

info "Running tests"
cargo test --all

# ---------- Done ----------

BINARY="$PROJECT_DIR/target/release/dab-rtl"
info "Build complete: $BINARY"
info ""
info "Quick start:"
info "  $BINARY --help"
info "  RUST_LOG=info $BINARY"
