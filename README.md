# MYY Player - Rust 跨平台视频播放器

> 基于 Rust + egui/eframe + FFmpeg 的桌面级播放器，专注本地与网络流媒体播放，提供硬件解码和高性能音画同步能力。

## 目录
- [项目简介](#项目简介)
- [核心亮点](#核心亮点)
- [架构概览](#架构概览)
- [跨平台支持](#跨平台支持)
- [依赖与准备](#依赖与准备)
- [快速开始](#快速开始)
- [打包与发布](#打包与发布)
- [日志与排障](#日志与排障)
- [路线图](#路线图)
- [贡献方式](#贡献方式)
- [许可证](#许可证)

## 项目简介
MYY Player 是一款采用 Rust 编写的跨平台视频播放器原型（MVP）。项目将 FFmpeg 的强大解封装/解码能力与 egui/eframe 的原生 GUI 体验结合，针对“本地文件 + 网络流（HTTP/RTSP/HLS）”场景做了精细的播放链路优化，并内建硬件加速、音画同步和多线程背压控制。

## 核心亮点
- ✅ **多格式播放**：支持主流视频容器（MP4/MKV/FLV/TS 等）与编码（H.264/H.265/VP9 等），同时支持 HLS/RTSP 等流媒体。
- ✅ **智能硬件解码**：自动检测 DXVA/D3D11VA（Windows）、VideoToolbox（macOS）、VAAPI（Linux），并在不可用时回退至软件解码。
- ✅ **网络流优化**：独立 `DemuxerThread` 负责解封装，搭配有界通道与解码线程“软/硬阈值”节流，降低抖动与卡顿。
- ✅ **音画同步**：自研播放时钟 (`core/clock`) + 音频主时钟策略，Seek/切流时自动清理状态并重新对齐。
- ✅ **实时日志追踪**：核心线程日志均带 `pid/tid` 前缀，便于排障；支持可调节的调试信息（解码帧/包统计等）。
- ✅ **跨平台 UI**：基于 egui/eframe + winit，可运行在 Windows / macOS / Linux，支持 OpenGL 与 wgpu 后端。

## 架构概览
```
┌──────────────────┐
│  egui 前端 (UI)  │  ── AppState / PlayerState / 快捷键
└─────────▲────────┘
          │
┌─────────┴────────┐
│ PlaybackManager  │  ── 播放调度中心：状态机、命令、线程管理
└───────┬─────────┬──┘
        │         │
  ┌─────▼───┐ ┌───▼────┐ ┌───────────┐
  │Demuxer  │ │Decoders │ │AudioOutput│
  │解封装   │ │视频/音频│ │cpal 输出  │
  └─────┬───┘ └───┬────┘ └───────────┘
        │         │
        ▼         ▼
  ┌──────────┐ ┌──────────┐
  │视频帧队列│ │音频帧队列│  ── 多线程背压 & 同步策略
  └──────────┘ └──────────┘
```
### 关键模块
- `app/`：egui UI 层，负责文件/URL 选择、播放控件、渲染纹理管理。
- `player/manager.rs`：播放管理器，维护状态机，调度 DemuxerThread、解码线程、音频输出线程。
- `player/demuxer_thread.rs`：独立解封装线程，处理网络流 Seek、背压和包分发。
- `player/decoder.rs` & `player/hw_decoder.rs`：软件/硬件解码器封装，处理 FFmpeg EOF/EAGAIN、解码帧管理。
- `core/clock.rs`：音画同步核心，实现主时钟、Seek 重置与漂移校正。

## 跨平台支持
| 平台 | 状态 | 备注 |
| ---- | ---- | ---- |
| Windows 10/11 | ✅ 已验证 | 推荐 GPU 驱动支持 D3D11VA；通过 `cargo wix` 可生成 MSI 安装包 |
| macOS 12+    | ✅ 已验证 | 依赖 VideoToolbox；建议使用 `brew` 安装 FFmpeg |
| Linux (X11/Wayland) | ✅ 已验证 | 首选 VAAPI；需安装 FFmpeg 开发库和编译工具链 |
| Android / HarmonyOS（核心库） | ✅ 已验证 | 可交叉编译核心播放引擎（无 UI），后续迭代将提供原生界面与输入控制 |
| iOS / iPadOS（核心库） | ⚠️ 实验性 | 支持生成静态库供集成，暂无 egui UI，未来版本将补全移动端渲染层 |

> ⚠️ 若使用硬件解码，请确保目标平台 GPU 与驱动满足要求（NVIDIA/AMD/Intel 均需正确安装）。

## 依赖与准备
1. **Rust 环境**：Rust 1.74+（建议使用稳定版 `rustup toolchain install stable`）。
2. **FFmpeg**：需要开发库头文件与动态库。以下为常规安装方式：
   - Windows：从 [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) 下载 `ffmpeg-master-latest-win64-gpl-shared.zip` → 解压到 `C:\ffmpeg` → 设置环境变量：
     ```powershell
     $env:FFMPEG_DIR = "C:\ffmpeg"
     $env:PATH += ";C:\ffmpeg\bin"
     ```
   - macOS：`brew install ffmpeg pkg-config`
   - Debian/Ubuntu：
     ```bash
     sudo apt update
     sudo apt install -y \
         libavcodec-dev libavformat-dev libavutil-dev \
         libavfilter-dev libavdevice-dev libswscale-dev \
         libswresample-dev pkg-config clang
     ```
3. **日志输出**：默认使用 `env_logger`，运行前可设置 `RUST_LOG=myy_player=info` 或 `=debug`。

## 从源码编译安装
1. **准备环境**
   - 按照上一节安装 Rust 工具链与 FFmpeg 依赖
   - Windows 需确保 `clang`/`lld` 已随 Visual Studio 或 LLVM 安装
   - macOS/Linux 如提示缺少 `pkg-config` 或 `clang`，请使用包管理器补齐
2. **获取源码**
   ```bash
   git clone https://github.com/your-org/myy_player.git
   cd myy_player
   ```
3. **（可选）同步外部资源**
   - 项目当前无 git submodule，如后续新增，请运行 `git submodule update --init --recursive`
4. **配置 FFmpeg 路径**
   - Windows：请确认 `FFMPEG_DIR` 与 `PATH` 已指向 `C:\ffmpeg`
   - Linux/macOS：确保 `pkg-config --libs libavcodec` 等命令可用
5. **编译调试版**
   ```bash
   cargo run
   ```
   若需要查看调试日志：`RUST_LOG=myy_player=debug cargo run`
6. **编译发布版**
   ```bash
   cargo build --release
   ```
   产物位于 `target/release/`
7. **安装/部署**
   - 创建 `dist/bin` 并复制 `myy_player(.exe)`
   - 将 FFmpeg 运行时库（Windows 下为 `.dll`，Linux/macOS 为 `.so/.dylib`）一并放入 `dist/bin`
   - 如需便携版，可直接压缩 `dist` 目录；若需安装包，请参考后文 `cargo wix` 指南
8. **功能开关**
   - 默认启用 `hwaccel` 特性，如需禁用硬件解码：`cargo run --no-default-features`
   - 针对特定平台启用硬件加速：`cargo run --features hwaccel-dx11` 等

## 快速开始
```bash
# 克隆项目
git clone https://github.com/your-org/myy_player.git
cd myy_player

# 开发模式运行
cargo run

# 或启用调试日志
RUST_LOG=myy_player=debug cargo run

# 发布构建
cargo build --release
```
编译完成后，可执行文件位于 `target/release/myy_player`（Windows 为 `.exe`）。确保运行目录中包含 FFmpeg 所需的 DLL/动态库。

## 打包与发布
### 便携版（绿色包）
1. 执行 `cargo build --release`。
2. 将 `target/release/myy_player(.exe)`、依赖 DLL、资源文件复制到 `dist/portable/`。
3. 压缩为 zip 分发。

### Windows 安装包（MSI）
1. 安装 WiX Toolset 3.11：
   ```powershell
   winget install --id=wixtoolset.wix311 --exact
   ```
2. 将 `C:\Program Files (x86)\WiX Toolset v3.11\bin` 加入 `PATH`。
3. 运行：
   ```powershell
   cargo wix
   ```
   生成的 MSI 位于 `target/wix/`。

### HarmonyOS（OpenHarmony）核心库
> 当前阶段仅支持编译核心播放引擎为静态库/可执行文件，尚无原生 UI。请参考下方步骤交叉编译并集成到鸿蒙应用工程中。

1. **安装 OpenHarmony NDK**（以 4.0.0.74 为例）
   - 下载地址：https://repo.huaweicloud.com/harmonyos/
   - 解压到本地，例如 `D:\openharmony\native-sdk`
2. **配置环境变量**（PowerShell 示例）：
   ```powershell
   $env:OHOS_NDK_HOME = "D:\openharmony\native-sdk"
   $env:PATH += ";$env:OHOS_NDK_HOME\llvm\bin"
   ```
3. **安装 Rust 交叉编译目标**：
   ```powershell
   rustup target add aarch64-unknown-linux-ohos
   rustup target add armv7-unknown-linux-ohos   # 如果需要 32 位
   ```
4. **生成 `.cargo/config.toml`（建议）**：
   ```toml
   [target.aarch64-unknown-linux-ohos]
   linker = "clang"
   ar = "llvm-ar"
   rustflags = [
       "-Clink-arg=--target=aarch64-unknown-linux-ohos",
       "-Clink-arg=--sysroot=${OHOS_NDK_HOME}/native/sysroot",
   ]
   ```
5. **构建核心库/二进制**：
   ```powershell
   cargo build --release --target aarch64-unknown-linux-ohos
   ```
   产物位于 `target/aarch64-unknown-linux-ohos/release/`，可作为鸿蒙 ArkUI/Stage 工程的原生库接入。
6. **集成建议**：
   - 将 `libmyy_player.a` 或自定义静态库复制到鸿蒙工程的 `libs` 目录
   - 通过 NAPI/FFI 暴露播放控制接口
   - UI 由鸿蒙前端（ArkUI、Stage 或 JS/TS）负责，实现播放按钮、进度条等

> 若需要生成可执行 Demo，可结合鸿蒙模板项目复用播放器核心逻辑；硬件解码支持取决于设备与 OpenHarmony NDK 能力。

### Android / Android TV 核心库
> 构建播放核心为 `.so` 静态库/动态库供 Android 原生应用调用，目前暂不包含 egui UI。

1. **安装 Android NDK**（推荐 r26+）
   - 使用 Android Studio 或直接从 https://developer.android.com/ndk 下载
   - 假设解压路径为 `D:\Android\ndk\26.1.10909125`
2. **配置环境变量**（PowerShell 示例）：
   ```powershell
   $env:ANDROID_NDK_HOME = "D:\Android\ndk\26.1.10909125"
   $env:PATH += ";$env:ANDROID_NDK_HOME\toolchains\llvm\prebuilt\windows-x86_64\bin"
   ```
3. **安装 Rust 目标**：
   ```powershell
   rustup target add aarch64-linux-android
   rustup target add armv7-linux-androideabi   # 若需要 32 位
   rustup target add x86_64-linux-android      # 若需要模拟器
   ```
4. **生成 `.cargo/config.toml`**（示例）：
   ```toml
   [target.aarch64-linux-android]
   linker = "aarch64-linux-android24-clang"
   ar = "llvm-ar"
   rustflags = [
       "-Clink-arg=-Wl,--build-id",
       "-Clink-arg=-lc",
   ]
   ```
   > 上述 `aarch64-linux-android24-clang` 来自 NDK，数值 24 代表 API Level；根据需求选择 21/24/29 等。
5. **交叉编译**：
   ```powershell
   cargo build --release --target aarch64-linux-android
   ```
   产物位于 `target/aarch64-linux-android/release/`，可打包成 `.so`。
6. **集成到 Android 工程**：
   - 将生成的 `libmyy_player.so` 置于 `app/src/main/jniLibs/arm64-v8a/`
   - 使用 JNI/NdkMedia/NAPI 暴露播放控制接口
   - UI 由 Jetpack Compose / View / Flutter 等负责
   - 若需硬件解码，需在 JNI 层调用 Android MediaCodec 或映射到设备支持的 FFmpeg 硬件模块
7. **测试与调试**：
   - 使用 `adb push` 将测试媒体复制到设备
   - 通过 `adb logcat | grep myy_player` 查看日志（默认包含 pid/tid）
   - 如需 Profile，可启用 `cargo build --profile=release` 并结合 Android Studio Profiler

### 其他平台
- macOS / Linux 可使用 `