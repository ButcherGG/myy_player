# MYY Player - Rust Cross-Platform Video Player

> A desktop-grade video player built with Rust, egui/eframe, and FFmpeg. It targets both local files and network streams while providing hardware acceleration and precise A/V synchronization.

## Table of Contents
- [Overview](#overview)
- [Key Highlights](#key-highlights)
- [Architecture](#architecture)
- [Platform Support](#platform-support)
- [Dependencies](#dependencies)
- [Quick Start](#quick-start)
- [Packaging & Distribution](#packaging--distribution)
- [Logging & Troubleshooting](#logging--troubleshooting)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [License](#license)

## Overview
MYY Player is an MVP-stage, cross-platform media player written in Rust. It combines FFmpeg for demuxing/decoding with egui/eframe for a native desktop UI. The pipeline is optimized for both local media and network streams (HTTP/RTSP/HLS) and features multi-threaded backpressure control, hardware acceleration, and robust audio/video clocking.

## Key Highlights
- ✅ **Broad format support** – Playback for popular containers (MP4, MKV, FLV, TS, …) and codecs (H.264/H.265/VP9, …), plus HLS/RTSP streaming.
- ✅ **Adaptive hardware acceleration** – Automatically detects D3D11VA/DXVA (Windows), VideoToolbox (macOS), VAAPI (Linux) and gracefully falls back to software decoding if unavailable.
- ✅ **Network-stream tuning** – Dedicated `DemuxerThread` with bounded channels and soft/hard throttling inside decoder threads to minimize jitter and buffering spikes.
- ✅ **Accurate A/V sync** – Custom playback clock (`core/clock`) driven by the audio timeline, with seek/reset handling and drift compensation.
- ✅ **Rich diagnostics** – Every core thread logs with `[pid:xxx tid:yyy]` prefixes; optional verbose counters for decoded frames/packets simplify tracing.
- ✅ **Cross-platform UI** – Built on egui/eframe + winit; runs on Windows, macOS, and Linux with either OpenGL or wgpu backends.

## Architecture
```
┌──────────────────┐
│  egui Frontend   │  ── AppState / PlayerState / hotkeys
└─────────▲────────┘
          │
┌─────────┴────────┐
│ PlaybackManager  │  ── state machine, command routing, thread control
└───────┬─────────┬──┘
        │         │
  ┌─────▼───┐ ┌───▼────┐ ┌───────────┐
  │ Demuxer │ │Decoders │ │AudioOutput│
  │ thread  │ │video/audio│ │cpal sink │
  └─────┬───┘ └───┬────┘ └───────────┘
        │         │
        ▼         ▼
  ┌──────────┐ ┌──────────┐
  │Video queue│ │Audio queue│  ── backpressure & sync strategy
  └──────────┘ └──────────┘
```
### Core Modules
- `app/` – egui UI layer handling file/URL selection, playback controls, texture rendering.
- `player/manager.rs` – playback orchestrator managing threads, state machine, clock alignment.
- `player/demuxer_thread.rs` – dedicated demux loop for network streams, seek commands, packet distribution.
- `player/decoder.rs` & `player/hw_decoder.rs` – software/hardware decoder wrappers dealing with FFmpeg EOF/EAGAIN scenarios.
- `core/clock.rs` – custom timing source to anchor audio/video synchronization and seek recovery.

## Platform Support
| Platform | Status | Notes |
| -------- | ------ | ----- |
| Windows 10/11 | ✅ Verified | D3D11VA recommended; MSI installer available via `cargo wix`. |
| macOS 12+     | ✅ Verified | Uses VideoToolbox; install FFmpeg through Homebrew. |
| Linux (X11/Wayland) | ✅ Verified | Prefer VAAPI; requires FFmpeg dev packages and build toolchain. |
| Android / HarmonyOS (core library) | ⚠️ Experimental | Core playback engine cross-compiles as a static library (no egui UI yet); native UI & input planned. |
| iOS / iPadOS (core library) | ⚠️ Experimental | Supports static library builds without UI; future milestones will add mobile rendering layers. |

> ⚠️ When enabling hardware acceleration, ensure GPU drivers (NVIDIA/AMD/Intel) are correctly installed on target machines.

## Dependencies
1. **Rust toolchain** – Rust 1.74+ (`rustup toolchain install stable`).
2. **FFmpeg development libraries**:
   - **Windows**: Download the shared build from [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) → extract to `C:\ffmpeg` → configure environment variables:
     ```powershell
     # Temporary (current session)
     $env:FFMPEG_DIR = "C:\ffmpeg"
     $env:PATH += ";C:\ffmpeg\bin"

     # Permanent (recommended)
     [System.Environment]::SetEnvironmentVariable("FFMPEG_DIR", "C:\ffmpeg", "User")
     $currentPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
     [System.Environment]::SetEnvironmentVariable("PATH", "$currentPath;C:\ffmpeg\bin", "User")
     ```
   - **macOS**: `brew install ffmpeg pkg-config`
   - **Debian/Ubuntu**:
     ```bash
     sudo apt update
     sudo apt install -y \
         libavcodec-dev libavformat-dev libavutil-dev \
         libavfilter-dev libavdevice-dev libswscale-dev \
         libswresample-dev pkg-config clang
     ```
3. **Logging** – Uses `env_logger`. Enable verbose logs with `RUST_LOG=myy_player=debug`.

## Quick Start
```bash
# Clone the repository
git clone https://github.com/your-org/myy_player.git
cd myy_player

# Run in development mode
cargo run

# Enable verbose logging
RUST_LOG=myy_player=debug cargo run

# Release build
cargo build --release
```
The binary is located at `target/release/myy_player` (`.exe` on Windows). Ship FFmpeg DLLs/so files alongside the executable when distributing.

## Packaging & Distribution
### Portable build
1. `cargo build --release`
2. Copy `target/release/myy_player(.exe)` plus required DLLs/resources into `dist/portable/`
3. Zip the folder for distribution.

### Windows MSI installer
1. Install WiX Toolset 3.11:
   ```powershell
   winget install --id=wixtoolset.wix311 --exact
   ```
2. Add `C:\Program Files (x86)\WiX Toolset v3.11\bin` to `PATH`.
3. Run:
   ```powershell
   cargo wix
   ```
   The resulting MSI is placed under `target/wix/`.

### macOS & Linux
- Consider `cargo-bundle`, AppImage, or platform-native packaging tools. For now, distributing the release binary plus a launcher script is recommended.

### HarmonyOS (OpenHarmony) core library
> Currently we only support building the playback engine as a static library/binary without a native UI. Follow these steps to cross-compile and embed it into your HarmonyOS project.

1. **Install the OpenHarmony NDK** (e.g. 4.0.0.74)
   - Download from https://repo.huaweicloud.com/harmonyos/
   - Extract to a directory, e.g. `D:\openharmony\native-sdk`
2. **Set environment variables** (PowerShell example):
   ```powershell
   $env:OHOS_NDK_HOME = "D:\openharmony\native-sdk"
   $env:PATH += ";$env:OHOS_NDK_HOME\llvm\bin"
   ```
3. **Add Rust targets**:
   ```powershell
   rustup target add aarch64-unknown-linux-ohos
   rustup target add armv7-unknown-linux-ohos   # optional 32-bit support
   ```
4. **Configure `.cargo/config.toml`** (recommended):
   ```toml
   [target.aarch64-unknown-linux-ohos]
   linker = "clang"
   ar = "llvm-ar"
   rustflags = [
       "-Clink-arg=--target=aarch64-unknown-linux-ohos",
       "-Clink-arg=--sysroot=${OHOS_NDK_HOME}/native/sysroot",
   ]
   ```
5. **Build the core library/binary**:
   ```powershell
   cargo build --release --target aarch64-unknown-linux-ohos
   ```
   Artifacts reside in `target/aarch64-unknown-linux-ohos/release/` and can be consumed by ArkUI/Stage projects as native libs.
6. **Integration tips**:
   - Copy `libmyy_player.a` (or your custom static lib) into the Harmony project `libs` folder
   - Expose playback controls via NAPI/FFI
   - Implement UI (play/pause, seek bar, etc.) using HarmonyOS front-end technologies (ArkUI/Stage/JS/TS)

> To build a runnable demo, reuse the playback core within an official Harmony template project; hardware acceleration availability depends on the target device and NDK capabilities.

## Logging & Troubleshooting
- All major logs include `