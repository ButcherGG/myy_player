use anyhow::Result;
use log::info;

mod core;
mod player;
mod renderer;
mod app;

use app::VideoPlayerApp;

fn main() -> Result<()> {
    // 初始化日志
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        // 过滤掉 wgpu_hal 和 wgpu_core 的警告日志，减少日志噪音
        .filter_module("wgpu_hal", log::LevelFilter::Error)
        .filter_module("wgpu_core", log::LevelFilter::Error)
        .init();

    info!("🎬 MYY Player - egui 版本启动");

    // 初始化 FFmpeg
    ffmpeg_next::init().map_err(|e| anyhow::anyhow!("FFmpeg 初始化失败: {}", e))?;
    info!("✅ FFmpeg 初始化成功");

    // 启动 egui 应用
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([800.0, 600.0])
            .with_title("喜洋洋播放器")
            .with_decorations(true), // 使用系统原生标题栏（避免拖动抖动）
        renderer: eframe::Renderer::Wgpu, // 使用 wgpu 后端获得最佳性能
        ..Default::default()
    };

    eframe::run_native(
        "喜洋洋播放器",
        options,
        Box::new(|cc| Box::new(VideoPlayerApp::new(cc))),
    )
    .map_err(|e| anyhow::anyhow!("应用启动失败: {}", e))?;

            Ok(())
}
