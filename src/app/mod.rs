use anyhow::Result;
use egui::{Context, Ui, FontDefinitions, FontData, FontFamily, ColorImage, TextureHandle, TextureOptions};
use log::{debug, error, info, warn};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::path::Path;

use crate::player::manager::PlaybackManager;
use crate::renderer::egui_video_renderer::EguiVideoRenderer;
use crate::core::{MediaSource, StreamState};

pub struct VideoPlayerApp {
    /// 播放管理器
    playback_manager: Arc<RwLock<PlaybackManager>>,
    
    /// egui 视频渲染器
    video_renderer: Option<EguiVideoRenderer>,
    
    /// UI 状态
    ui_state: UiState,
    
    /// 性能统计
    perf_stats: PerformanceStats,
    
    /// 当前显示的帧 PTS（用于避免重复更新）
    current_frame_pts: Option<i64>,
    
    /// 图标缓存
    icons: Option<ControlIcons>,
    
    /// Windows 标题栏颜色是否已设置（避免重复设置）
    #[cfg(target_os = "windows")]
    title_bar_color_set: bool,
    
    /// Demuxer 创建结果接收通道（新架构）
    demuxer_result_rx: crossbeam_channel::Receiver<crate::player::DemuxerCreationResult>,
    demuxer_result_tx: crossbeam_channel::Sender<crate::player::DemuxerCreationResult>,
    
    /// 正在加载的 URL（用于显示加载提示）
    loading_url: Option<String>,
}

#[derive(Default)]
struct UiState {
    /// 当前文件路径
    current_file: Option<String>,
    
    /// 控制面板可见性
    controls_visible: bool,
    controls_hide_timer: Option<Instant>,
    
    /// 音量 (0.0 - 1.0)
    volume: f32,
    
    /// 播放速度
    playback_speed: f32,
    
    /// 是否全屏
    is_fullscreen: bool,
    
    /// 拖拽进度条状态
    seeking: bool,
    seek_position: f64,
    seek_complete_time: Option<Instant>,  // seek完成的时间，用于延迟重置seeking状态
    seek_executed: bool,  // 标记seek是否已执行，避免重复执行
    
    /// 信息面板可见性
    info_panel_visible: bool,
    
    /// 网络流相关
    show_url_dialog: bool,        // 是否显示打开 URL 对话框
    url_input: String,            // URL 输入框内容
}

struct PerformanceStats {
    fps: f32,
    frame_time: Duration,
    last_frame_time: Instant,
    frame_count: u32,
    last_fps_update: Instant,
}

/// 控制按钮图标
struct ControlIcons {
    play: TextureHandle,
    pause: TextureHandle,
    stop: TextureHandle,
    open_file: TextureHandle,
}

impl Default for PerformanceStats {
    fn default() -> Self {
        Self {
            fps: 0.0,
            frame_time: Duration::from_secs(0),
            last_frame_time: Instant::now(),
            frame_count: 0,
            last_fps_update: Instant::now(),
        }
    }
}

impl VideoPlayerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        info!("🎮 初始化 VideoPlayerApp");

        // 配置中文字体
        Self::setup_chinese_fonts(&cc.egui_ctx);

        // 创建播放管理器
        let playback_manager = Arc::new(RwLock::new(PlaybackManager::new()));

        // 初始化视频渲染器
        let video_renderer = if let Some(wgpu_render_state) = cc.wgpu_render_state.as_ref() {
            match EguiVideoRenderer::new(wgpu_render_state) {
                Ok(renderer) => {
                    info!("✅ egui 视频渲染器初始化成功");
                    Some(renderer)
                }
                Err(e) => {
                    error!("❌ egui 视频渲染器初始化失败: {}", e);
                    None
                }
            }
        } else {
            error!("❌ 无法获取 wgpu 渲染状态");
            None
        };

        // 创建图标
        let icons = Self::create_control_icons(&cc.egui_ctx);

        // 配置窗口标题栏样式（背景色和文字颜色）
        Self::setup_window_theme(&cc.egui_ctx);

        // 创建 Demuxer 结果通道（新架构）
        let (demuxer_result_tx, demuxer_result_rx) = crossbeam_channel::unbounded();

        Self {
            playback_manager,
            video_renderer,
            ui_state: UiState {
                volume: 1.0,
                playback_speed: 1.0,
                controls_visible: true,
                ..Default::default()
            },
            perf_stats: PerformanceStats {
                last_frame_time: Instant::now(),
                last_fps_update: Instant::now(),
                ..Default::default()
            },
            current_frame_pts: None,
            icons: Some(icons),
            #[cfg(target_os = "windows")]
            title_bar_color_set: false,
            demuxer_result_rx,
            demuxer_result_tx,
            loading_url: None,
        }
    }

    /// 配置窗口主题（标题栏颜色）
    fn setup_window_theme(ctx: &Context) {
        // 设置窗口视觉样式
        let mut style = (*ctx.style()).clone();
        
        // 设置背景颜色为深色
        style.visuals.dark_mode = true;
        style.visuals.window_fill = egui::Color32::from_rgb(29, 29, 29);
        style.visuals.panel_fill = egui::Color32::from_rgb(29, 29, 29);
        
        ctx.set_style(style);
        // 注意：系统标题栏颜色的设置将在 setup_window_style 中进行（需要 frame 参数）
    }
    
    /// 设置窗口样式（包括系统标题栏背景色）
    fn setup_window_style(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        // 设置窗口视觉样式
        let mut style = (*ctx.style()).clone();
        
        // 设置背景颜色为深色
        style.visuals.dark_mode = true;
        style.visuals.window_fill = egui::Color32::from_rgb(29, 29, 29);
        style.visuals.panel_fill = egui::Color32::from_rgb(29, 29, 29);
        
        ctx.set_style(style);
        
        // 在 Windows 上尝试设置标题栏背景色（只设置一次）
        #[cfg(target_os = "windows")]
        {
            if !self.title_bar_color_set {
                if Self::setup_windows_title_bar_color(frame) {
                    self.title_bar_color_set = true;
                }
            }
        }
    }
    
    /// Windows 平台特定的标题栏颜色设置
    /// 使用 DwmSetWindowAttribute 设置标题栏背景色为 rgb(29, 29, 29)
    /// 返回 true 表示成功设置
    #[cfg(target_os = "windows")]
    fn setup_windows_title_bar_color(frame: &mut eframe::Frame) -> bool {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        
        // 获取窗口句柄
        if let Ok(window_handle) = frame.window_handle() {
            let raw_handle = window_handle.as_raw();
            
            // raw_window_handle 0.6 使用 RawWindowHandle 枚举
            if let RawWindowHandle::Win32(handle) = raw_handle {
                unsafe {
                    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
                    use windows::Win32::Foundation::HWND;
                    use log::{info, warn};
                    
                    // HWND 期望 isize 类型，handle.hwnd.get() 返回指针，需要转换为 isize
                    let hwnd = HWND(handle.hwnd.get() as isize);
                    
                    // 首先启用深色模式标题栏（Windows 11，必需）
                    // DWMWA_USE_IMMERSIVE_DARK_MODE = 20
                    let mut use_dark_mode = 1u32; // TRUE
                    let result1 = DwmSetWindowAttribute(
                        hwnd,
                        DWMWINDOWATTRIBUTE(20), // DWMWA_USE_IMMERSIVE_DARK_MODE
                        &mut use_dark_mode as *mut _ as *mut _,
                        std::mem::size_of::<u32>() as u32,
                    );
                    if result1.is_err() {
                        warn!("⚠️  启用深色模式标题栏失败: {:?}", result1);
                        return false;
                    }
                    info!("✓ 已启用深色模式标题栏");
                    
                    // 设置标题栏背景色为 rgb(29, 29, 29)
                    // RGB 格式转换为 COLORREF: BGR (Blue-Green-Red)
                    let color_value = (29u32) | (29u32 << 8) | (29u32 << 16);
                    
                    // 设置标题栏颜色 (DWMWA_CAPTION_COLOR = 35, Windows 11 Build 22621+)
                    let mut caption_color = color_value;
                    let result2 = DwmSetWindowAttribute(
                        hwnd,
                        DWMWINDOWATTRIBUTE(35), // DWMWA_CAPTION_COLOR
                        &mut caption_color as *mut _ as *mut _,
                        std::mem::size_of::<u32>() as u32,
                    );
                    if result2.is_ok() {
                        info!("✓ 已设置标题栏颜色为 rgb(29, 29, 29)");
                        return true;
                    } else {
                        warn!("⚠️  设置标题栏颜色失败 (错误: {:?})，尝试设置边框颜色", result2);
                    }
                    
                    // 设置窗口边框颜色（作为备选方案，Windows 10 1809+ 支持）
                    let mut border_color = color_value;
                    let result3 = DwmSetWindowAttribute(
                        hwnd,
                        DWMWINDOWATTRIBUTE(34), // DWMWA_BORDER_COLOR
                        &mut border_color as *mut _ as *mut _,
                        std::mem::size_of::<u32>() as u32,
                    );
                    if result3.is_ok() {
                        info!("✓ 已设置窗口边框颜色为 rgb(29, 29, 29)");
                        return true;
                    } else {
                        warn!("⚠️  设置窗口边框颜色也失败 (错误: {:?})", result3);
                    }
                }
            } else {
                use log::warn;
                warn!("⚠️  无法获取 Win32 窗口句柄");
            }
        } else {
            use log::warn;
            warn!("⚠️  无法获取窗口句柄，可能窗口尚未创建");
        }
        false
    }
    
    #[cfg(not(target_os = "windows"))]
    fn setup_window_style(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // 非 Windows 平台：只设置 egui 样式
        let mut style = (*ctx.style()).clone();
        style.visuals.dark_mode = true;
        style.visuals.window_fill = egui::Color32::from_rgb(29, 29, 29);
        style.visuals.panel_fill = egui::Color32::from_rgb(29, 29, 29);
        ctx.set_style(style);
    }

    /// 配置中文字体支持
    fn setup_chinese_fonts(ctx: &Context) {
        let mut fonts = FontDefinitions::default();
        
        // Windows 系统中文字体路径
        #[cfg(target_os = "windows")]
        let chinese_font_paths = vec![
            "C:/Windows/Fonts/msyh.ttc",      // 微软雅黑
            "C:/Windows/Fonts/simsun.ttc",     // 宋体
            "C:/Windows/Fonts/simhei.ttf",    // 黑体
            "C:/Windows/Fonts/simkai.ttf",    // 楷体
        ];
        
        #[cfg(target_os = "macos")]
        let chinese_font_paths = vec![
            "/System/Library/Fonts/PingFang.ttc",      // 苹方
            "/System/Library/Fonts/STHeiti Light.ttc", // 黑体
        ];
        
        #[cfg(target_os = "linux")]
        let chinese_font_paths = vec![
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        ];

        // 尝试加载第一个可用的中文字体
        let mut font_loaded = false;
        for font_path in chinese_font_paths {
            if Path::new(font_path).exists() {
                match std::fs::read(font_path) {
                    Ok(font_data) => {
                        fonts.font_data.insert(
                            "chinese_font".to_owned(),
                            FontData::from_owned(font_data),
                        );
                        
                        // 将中文字体添加到默认字体族
                        if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
                            family.insert(0, "chinese_font".to_owned());
                        }
                        if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
                            family.insert(0, "chinese_font".to_owned());
                        }
                        
                        info!("✅ 成功加载中文字体: {}", font_path);
                        font_loaded = true;
                        break;
                    }
                    Err(e) => {
                        warn!("⚠️ 无法读取字体文件 {}: {}", font_path, e);
                    }
                }
            }
        }

        if !font_loaded {
            warn!("⚠️ 未找到可用的中文字体文件，中文可能显示为方块");
        }

        // 应用字体配置
        ctx.set_fonts(fonts);
    }

    /// 创建控制按钮图标（使用 VS Code Codicons SVG）
    /// 直接使用 codicons 的 SVG 字符串，通过 resvg 渲染
    fn create_control_icons(ctx: &Context) -> ControlIcons {
        // 使用高分辨率渲染以获得更好的显示效果
        let icon_size = 96;
        
        info!("🎨 创建控制按钮图标（使用 VS Code Codicons，分辨率: {}x{}）", icon_size, icon_size);
        
        // VS Code Codicons SVG 图标（来自 https://github.com/microsoft/vscode-codicons）
        // 使用真实的 codicons SVG 路径数据
        
        // 播放图标 - play (codicons: play-triangle)
        let play_svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M3 3v10l10-5z" fill="white"/></svg>"#;
        let play_image = Self::svg_to_image(play_svg, icon_size);
        let play = ctx.load_texture("play_icon", play_image, TextureOptions::LINEAR);

        // 暂停图标 - debug-pause (codicons)
        let pause_svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M4.5 3C4.22386 3 4 3.22386 4 3.5V12.5C4 12.7761 4.22386 13 4.5 13H7.5C7.77614 13 8 12.7761 8 12.5V3.5C8 3.22386 7.77614 3 7.5 3H4.5ZM9.5 3C9.22386 3 9 3.22386 9 3.5V12.5C9 12.7761 9.22386 13 9.5 13H12.5C12.7761 13 13 12.7761 13 12.5V3.5C13 3.22386 12.7761 3 12.5 3H9.5Z" fill="white"/></svg>"#;
        let pause_image = Self::svg_to_image(pause_svg, icon_size);
        let pause = ctx.load_texture("pause_icon", pause_image, TextureOptions::LINEAR);

        // 停止图标 - debug-stop (codicons)
        let stop_svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect x="3" y="3" width="10" height="10" rx="1" fill="white"/></svg>"#;
        let stop_image = Self::svg_to_image(stop_svg, icon_size);
        let stop = ctx.load_texture("stop_icon", stop_image, TextureOptions::LINEAR);

        // 打开文件夹图标 - folder-opened (codicons)
        let folder_svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M1.75 2A1.75 1.75 0 0 0 0 3.75v8.5C0 13.216.784 14 1.75 14h12.5A1.75 1.75 0 0 0 16 12.25v-8.5A1.75 1.75 0 0 0 14.25 2H7.5a.25.25 0 0 1-.2-.1l-.9-1.2C6.07.22 5.26 0 4.75 0h-3A1.75 1.75 0 0 0 0 1.75V3h1.5a.25.25 0 0 1 .2.1l.9 1.2c.23.31.934.7 1.44.7H1.75zM1.5 6.5v5.75c0 .138.112.25.25.25H14.25a.25.25 0 0 0 .25-.25V6.5H1.5z" fill="white"/></svg>"#;
        let folder_image = Self::svg_to_image(folder_svg, icon_size);
        let open_file = ctx.load_texture("open_file_icon", folder_image, TextureOptions::LINEAR);

        info!("✅ 控制按钮图标创建完成");
        
        ControlIcons {
            play,
            pause,
            stop,
            open_file,
        }
    }
    
    /// 将 SVG 字符串转换为 egui ColorImage
    fn svg_to_image(svg_str: &str, size: usize) -> ColorImage {
        use resvg::tiny_skia;
        use usvg::{Options, Tree, TreeParsing};
        
        // SVG 已经包含 fill="white"，不需要替换 currentColor
        let svg_with_color = svg_str.to_string();
        
        // 解析 SVG
        let opt = Options::default();
        let tree = match Tree::from_str(&svg_with_color, &opt) {
            Ok(tree) => tree,
            Err(e) => {
                error!("解析 SVG 失败: {}", e);
                return Self::create_placeholder_image(size);
            }
        };
        
        // 创建渲染目标
        let mut pixmap = match tiny_skia::Pixmap::new(size as u32, size as u32) {
            Some(pixmap) => pixmap,
            None => {
                error!("创建 Pixmap 失败");
                return Self::create_placeholder_image(size);
            }
        };
        
        // 计算缩放和居中
        let svg_size = tree.view_box.rect.size();
        let scale = (size as f32 / svg_size.width()).min(size as f32 / svg_size.height());
        let scaled_width = svg_size.width() * scale;
        let scaled_height = svg_size.height() * scale;
        let x = (size as f32 - scaled_width) / 2.0;
        let y = (size as f32 - scaled_height) / 2.0;
        
        let transform = tiny_skia::Transform::from_translate(x, y).post_scale(scale, scale);
        
        // 渲染 SVG（确保完全透明背景）
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let rtree = resvg::Tree::from_usvg(&tree);
        // 使用 BlendMode::SourceOver 确保正确渲染透明部分
        rtree.render(transform, &mut pixmap.as_mut());
        
        // 转换为 RGBA
        // tiny_skia::Pixmap 使用 premultiplied BGRA 格式（Blue, Green, Red, Alpha）
        // 需要转换为 unmultiplied RGBA 格式（Red, Green, Blue, Alpha）
        // 关键：premultiplied 意味着颜色值已经乘以了 alpha，需要除以 alpha 得到原始值
        let pixels: Vec<u8> = pixmap.pixels()
            .iter()
            .flat_map(|p| {
                let alpha = p.alpha();
                if alpha == 0 {
                    // 完全透明的像素，直接返回透明
                    [0, 0, 0, 0]
                } else {
                    // tiny_skia::ColorU8 提供了 red(), green(), blue(), alpha() 方法
                    // 这些值已经是 premultiplied 的，需要转换
                    let alpha_f = alpha as f32 / 255.0;
                    
                    // 从 premultiplied 转换为 unmultiplied
                    // 公式：unmultiplied = premultiplied / alpha
                    let r = (p.red() as f32 / alpha_f).min(255.0).max(0.0) as u8;
                    let g = (p.green() as f32 / alpha_f).min(255.0).max(0.0) as u8;
                    let b = (p.blue() as f32 / alpha_f).min(255.0).max(0.0) as u8;
                    
                    // 输出为 RGBA 格式（egui 需要的格式）
                    [r, g, b, alpha]
                }
            })
            .collect();
        
        ColorImage::from_rgba_unmultiplied([size, size], &pixels)
    }
    
    /// 创建占位符图标（当 SVG 渲染失败时使用）
    fn create_placeholder_image(size: usize) -> ColorImage {
        use image::{Rgba, RgbaImage, DynamicImage};
        let mut image = RgbaImage::new(size as u32, size as u32);
        for pixel in image.pixels_mut() {
            *pixel = Rgba([200, 200, 200, 255]);
        }
        let dynamic = DynamicImage::ImageRgba8(image);
        let rgb_image = dynamic.to_rgb8();
        let pixels: Vec<u8> = rgb_image.pixels()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        ColorImage::from_rgba_unmultiplied([size, size], &pixels)
    }

    // 旧的图标生成函数已完全移除，现在使用 VS Code Codicons SVG
    // 所有 generate_*_icon 函数已删除，改用 Codicons SVG 渲染

    /// 打开文件
    pub fn open_file(&mut self, file_path: String) -> Result<()> {
        info!("📂 打开文件: {}", file_path);
        
        // 先清理 UI 状态，避免旧视频的数据影响新视频
        self.current_frame_pts = None;
        self.ui_state.seeking = false;
        self.ui_state.seek_position = 0.0;
        self.ui_state.seek_complete_time = None;
        self.ui_state.seek_executed = false;
        
        // 清理视频渲染器的纹理缓存（在打开新文件之前清理，避免显示旧视频帧）
        if let Some(renderer) = &mut self.video_renderer {
            renderer.cleanup();
            info!("🧹 已清理视频渲染器缓存");
        }
        
        // 打开新文件（manager.open_file() 内部会调用 stop() 清理播放器状态）
        // stop() 会：停止所有线程、清空所有帧队列、重置播放时钟、清理音频输出
        let mut manager = self.playback_manager.write();
        manager.open_file(&file_path)?;
        
        // 自动开始播放
        if let Err(e) = manager.play() {
            error!("自动播放失败: {}", e);
            // 即使自动播放失败，也继续完成文件打开流程
        } else {
            info!("✅ 已自动开始播放");
        }
        
        // 打开新文件后，再次确保 UI 状态正确（双重保险）
        self.current_frame_pts = None;
        
        // 更新 UI 状态
        self.ui_state.current_file = Some(file_path);
        self.ui_state.controls_visible = true;
        self.ui_state.controls_hide_timer = Some(Instant::now() + Duration::from_secs(3));
        
        info!("✅ 文件打开完成，状态已重置");
        
        Ok(())
    }

    /// 更新性能统计
    fn update_performance_stats(&mut self) {
        let now = Instant::now();
        self.perf_stats.frame_time = now - self.perf_stats.last_frame_time;
        self.perf_stats.last_frame_time = now;
        self.perf_stats.frame_count += 1;

        // 每秒更新一次 FPS
        if now.duration_since(self.perf_stats.last_fps_update) >= Duration::from_secs(1) {
            self.perf_stats.fps = self.perf_stats.frame_count as f32;
            self.perf_stats.frame_count = 0;
            self.perf_stats.last_fps_update = now;
        }
    }

    /// 更新控制面板可见性
    fn update_controls_visibility(&mut self, ctx: &Context) {
        let is_fullscreen = self.is_fullscreen(ctx);
        
        if is_fullscreen {
            // 全屏模式：鼠标移动时显示控制面板，3秒后自动隐藏
            let is_moving = ctx.input(|i| i.pointer.is_moving());
            
            // 鼠标移动时显示控制面板并重置计时器
            if is_moving {
                self.ui_state.controls_visible = true;
                self.ui_state.controls_hide_timer = Some(Instant::now() + Duration::from_secs(3));
            }
            
            // 3秒后自动隐藏控制面板（全屏模式）
            if let Some(hide_time) = self.ui_state.controls_hide_timer {
                if Instant::now() > hide_time {
                    self.ui_state.controls_visible = false;
                    self.ui_state.controls_hide_timer = None;
                }
            }
        } else {
            // 非全屏模式：鼠标移动时显示控制面板，或始终显示（根据需要）
            if ctx.input(|i| i.pointer.is_moving()) {
                self.ui_state.controls_visible = true;
                self.ui_state.controls_hide_timer = Some(Instant::now() + Duration::from_secs(3));
            }

            // 非全屏模式下，可以选择始终显示或3秒后隐藏
            // 这里保持3秒后自动隐藏的行为
            if let Some(hide_time) = self.ui_state.controls_hide_timer {
                if Instant::now() > hide_time {
                    self.ui_state.controls_visible = false;
                    self.ui_state.controls_hide_timer = None;
                }
            }
        }
    }

    /// 动态更新窗口标题（在系统标题栏显示文件名）
    fn update_window_title(&mut self, ctx: &Context) {
        let new_title = if let Some(file_path) = &self.ui_state.current_file {
            let file_name = Path::new(file_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(file_path);
            format!("喜洋洋播放器 - {}", file_name)
        } else {
            "喜洋洋播放器".to_string()
        };
        
        // 检查标题是否需要更新（避免频繁更新）
        let current_title = ctx.input(|i| i.viewport().title.clone());
        if current_title.as_ref() != Some(&new_title) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(new_title));
        }
    }

    /// 渲染信息栏（在系统标题栏下方显示文件名等信息，使用自定义标题栏背景）
    fn render_info_bar(&mut self, ctx: &Context) {
        // 使用与之前自定义标题栏相同的背景色和样式
        let title_bar_color = egui::Color32::from_rgb(29, 29, 29);
        
        // 在系统标题栏下方显示信息栏（始终显示）
        egui::TopBottomPanel::top("info_bar")
            .frame(egui::Frame::none()
                .fill(title_bar_color)
                .stroke(egui::Stroke::new(0.0, egui::Color32::TRANSPARENT))
            )
            .resizable(false)
            .show_separator_line(false)
            .height_range(32.0..=32.0)
            .show(ctx, |ui| {
                ui.set_height(32.0);
                ui.horizontal(|ui| {
                    ui.set_height(32.0);
                    
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing = egui::Vec2::new(8.0, 0.0);
                        ui.add_space(12.0);
                        
                        // 显示应用标题（深色 RGB(29, 29, 29)）
                        ui.label(
                            egui::RichText::new("喜洋洋播放器")
                                .color(egui::Color32::from_rgb(29, 29, 29))
                                .size(13.0)
                        );
                        
                        // 显示文件名（白色，如果有）
                        if let Some(file_path) = &self.ui_state.current_file {
                            let file_name = Path::new(file_path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(file_path);
                            
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new(file_name)
                                    .color(egui::Color32::WHITE)
                                    .size(13.0)
                            );
                        }
                    });
                });
            });
    }

    /// 渲染自定义标题栏
    fn render_custom_title_bar(&mut self, ctx: &Context) {
        const TITLE_BAR_HEIGHT: f32 = 32.0;
        const BUTTON_SIZE: f32 = 32.0;
        const BUTTON_ICON_SIZE: f32 = 14.0;
        
        let title_bar_color = egui::Color32::from_rgb(29, 29, 29);
        let _title_text_color = egui::Color32::from_rgb(112, 112, 112);
        let _filename_color = egui::Color32::WHITE;
        
        // 顶部标题栏面板
        egui::TopBottomPanel::top("custom_title_bar")
            .frame(egui::Frame::none()
                .fill(title_bar_color)
                .stroke(egui::Stroke::new(0.0, egui::Color32::TRANSPARENT)))
            .resizable(false)
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.set_height(TITLE_BAR_HEIGHT);
                ui.horizontal(|ui| {
                    ui.set_height(TITLE_BAR_HEIGHT);
                    
                    // 左侧：标题和文件名
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing = egui::Vec2::new(8.0, 0.0);
                        ui.add_space(12.0);
                        
                        // 播放器标题（深色 RGB(29, 29, 29)）
                        ui.label(
                            egui::RichText::new("喜洋洋播放器")
                                .color(egui::Color32::from_rgb(29, 29, 29))
                                .size(13.0)
                        );
                        
                        // 文件名（白色，如果有）
                        if let Some(file_path) = &self.ui_state.current_file {
                            let file_name = Path::new(file_path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(file_path);
                            
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new(file_name)
                                    .color(egui::Color32::WHITE)
                                    .size(13.0)
                            );
                        }
                    });
                    
                    // 中间：可拖拽区域（占用剩余空间）
                    ui.allocate_ui_with_layout(
                        egui::Vec2::new(ui.available_width() - BUTTON_SIZE * 3.0, TITLE_BAR_HEIGHT),
                        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                        |ui| {
                            let drag_response = ui.allocate_response(
                                ui.available_size(),
                                egui::Sense::drag()
                            );
                            
                            if drag_response.dragged() {
                                let delta = drag_response.drag_delta();
                                if delta != egui::Vec2::ZERO {
                                    if let Some(outer_rect) = ctx.input(|i| i.viewport().outer_rect) {
                                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                            outer_rect.min + delta
                                        ));
                                    }
                                }
                            }
                        }
                    );
                    
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing = egui::Vec2::new(0.0, 0.0);
                        
                        // 右侧：窗口控制按钮（统一大小和样式）
                        
                        // 辅助函数：绘制窗口控制按钮
                        let draw_window_button = |ui: &mut egui::Ui, size: f32, hover_color: egui::Color32| -> egui::Response {
                            let response = ui.add_sized(
                                egui::Vec2::new(size, size),
                                egui::Button::new("")
                                    .frame(false)
                            );
                            
                            // 绘制按钮背景
                            if response.hovered() {
                                ui.painter().rect_filled(
                                    response.rect,
                                    0.0,
                                    hover_color
                                );
                            }
                            
                            response
                        };
                        
                        // 关闭按钮（×）
                        let close_response = draw_window_button(ui, BUTTON_SIZE, egui::Color32::from_rgb(232, 17, 35));
                        
                        // 绘制关闭图标（×）
                        let close_icon_color = if close_response.hovered() {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::from_rgb(200, 200, 200)
                        };
                        let icon_size = 10.0;
                        let center = close_response.rect.center();
                        let half_size = icon_size / 2.0;
                        ui.painter().line_segment(
                            [center + egui::Vec2::new(-half_size, -half_size), center + egui::Vec2::new(half_size, half_size)],
                            egui::Stroke::new(1.5, close_icon_color)
                        );
                        ui.painter().line_segment(
                            [center + egui::Vec2::new(-half_size, half_size), center + egui::Vec2::new(half_size, -half_size)],
                            egui::Stroke::new(1.5, close_icon_color)
                        );
                        
                        if close_response.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        
                        // 最大化/还原按钮
                        let is_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                        let max_response = draw_window_button(ui, BUTTON_SIZE, egui::Color32::from_rgb(60, 60, 60));
                        
                        // 绘制最大化/还原图标
                        let max_icon_color = if max_response.hovered() {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::from_rgb(200, 200, 200)
                        };
                        let icon_rect = egui::Rect::from_center_size(
                            max_response.rect.center(),
                            egui::Vec2::new(10.0, 10.0)
                        );
                        if is_maximized {
                            // 还原图标：两个重叠的方框
                            ui.painter().rect_stroke(
                                icon_rect.translate(egui::Vec2::new(-2.0, -2.0)),
                                0.0,
                                egui::Stroke::new(1.0, max_icon_color)
                            );
                            ui.painter().rect_stroke(
                                icon_rect.translate(egui::Vec2::new(2.0, 2.0)),
                                0.0,
                                egui::Stroke::new(1.0, max_icon_color)
                            );
                        } else {
                            // 最大化图标：单个方框
                            ui.painter().rect_stroke(
                                icon_rect,
                                0.0,
                                egui::Stroke::new(1.0, max_icon_color)
                            );
                        }
                        
                        if max_response.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
                        }
                        
                        // 最小化按钮（—）
                        let min_response = draw_window_button(ui, BUTTON_SIZE, egui::Color32::from_rgb(60, 60, 60));
                        
                        // 绘制最小化图标（横线）
                        let min_icon_color = if min_response.hovered() {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::from_rgb(200, 200, 200)
                        };
                        let center = min_response.rect.center();
                        ui.painter().line_segment(
                            [
                                center + egui::Vec2::new(-5.0, 0.0),
                                center + egui::Vec2::new(5.0, 0.0)
                            ],
                            egui::Stroke::new(1.5, min_icon_color)
                        );
                        
                        if min_response.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    });
                });
            });
    }
}

impl eframe::App for VideoPlayerApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // 处理 Demuxer 创建结果（新架构 - 异步打开）
        if let Ok(result) = self.demuxer_result_rx.try_recv() {
            use crate::player::DemuxerCreationResult;
            
            match result {
                DemuxerCreationResult::Success { demuxer, url } => {
                    info!("✅ Demuxer 创建成功: {}", url);
                    
                    // 判断是否为网络流
                    let is_network = url.starts_with("http://") 
                        || url.starts_with("https://")
                        || url.starts_with("rtsp://")
                        || url.starts_with("rtmp://")
                        || url.contains(".m3u8");  // HLS
                    
                    // 切换媒体源前先清理 UI 状态，避免残留帧
                    self.current_frame_pts = None;
                    self.ui_state.seeking = false;
                    self.ui_state.seek_position = 0.0;
                    self.ui_state.seek_complete_time = None;
                    self.ui_state.seek_executed = false;
                    if let Some(renderer) = &mut self.video_renderer {
                        renderer.cleanup();
                    }
                    
                    // 在主线程中附加 Demuxer
                    if let Some(mut manager) = self.playback_manager.try_write() {
                        let result = if is_network {
                            // 网络流：使用新架构（DemuxerThread）
                            info!("🌐 使用新架构（DemuxerThread）处理网络流");
                            manager.attach_demuxer_async(demuxer)
                        } else {
                            // 本地文件：使用现有方式
                            info!("📁 使用现有方式处理本地文件");
                            manager.attach_demuxer(demuxer)
                        };
                        
                        match result {
                            Ok(media_info) => {
                                info!("✅ 播放器已就绪: {:?}", media_info);
                                self.ui_state.current_file = Some(url.clone());
                                
                                // 自动播放
                                if let Err(e) = manager.play() {
                                    error!("❌ 自动播放失败: {}", e);
                                }
                            }
                            Err(e) => {
                                error!("❌ 附加 Demuxer 失败: {}", e);
                            }
                        }
                    }
                    
                    // 清除加载状态
                    self.loading_url = None;
                }
                DemuxerCreationResult::Failed { url, error } => {
                    error!("❌ 创建 Demuxer 失败: {} - {}", url, error);
                    self.loading_url = None;
                }
            }
        }
        
        // 动态更新窗口标题（显示文件名）
        self.update_window_title(ctx);
        
        // 设置系统标题栏样式（背景色等）
        self.setup_window_style(ctx, _frame);
        
        // 隐藏自定义信息栏（不再显示）
        // self.render_info_bar(ctx);
        
        // 更新音频输出（重要！必须定期调用以保持音频播放）
        if let Some(mut manager) = self.playback_manager.try_write() {
            manager.update_audio();
        }
        
        // 更新性能统计
        self.update_performance_stats();
        
        // 更新控制面板可见性
        self.update_controls_visibility(ctx);
        
        // 检测全屏状态
        let is_fullscreen = self.is_fullscreen(ctx);
        
        // 只在可见时或非全屏模式下渲染控制面板
        // 全屏模式下根据可见性决定是否渲染
        if !is_fullscreen || self.ui_state.controls_visible {
            self.render_controls_panel(ctx);
        }
        
        // 主视频区域 - 占满整个窗口
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                self.render_video_area(ui);
            });

        // 控制面板 - 悬浮在底部
        //if self.ui_state.controls_visible {
        //    self.render_controls_panel(ctx);
        //}

        // 信息面板 - 悬浮在左上角
        self.render_info_panel(ctx);
        
        // URL 对话框 - 最后渲染，确保在最上层
        self.render_url_dialog(ctx);

        // 处理键盘快捷键
        self.handle_keyboard_input(ctx);

        // 持续请求重绘以达到 60fps
        // 使用更短的间隔确保高帧率
        ctx.request_repaint_after(Duration::from_millis(16));
        
        // // 如果正在播放视频，确保持续重绘
        // if self.current_frame_pts.is_some() {
        //     // 视频播放时也需要持续重绘以保持流畅
        //     ctx.request_repaint();
        // }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        info!("🔚 VideoPlayerApp 退出");
        
        // 停止播放
        if let Some(mut manager) = self.playback_manager.try_write() {
            let _ = manager.stop();
        }
    }
}

impl VideoPlayerApp {
    /// 渲染视频区域
    fn render_video_area(&mut self, ui: &mut Ui) {
        let available_rect = ui.available_rect_before_wrap();
        
        // ==================== UI 层：视频帧渲染与同步 ====================
        if let Some(renderer) = &mut self.video_renderer {
            if let Some(manager) = self.playback_manager.try_read() {
                // ========== 获取当前播放时间（音频时钟） ==========
                // 这是音画同步的关键：UI 根据音频时钟来选择显示哪一帧
                let current_time_ms = manager.get_position().map(|pos| (pos * 1000.0) as i64).unwrap_or(0);
                
                // ========== 帧更新策略：按需获取（防止快进优化版）==========
                // 目的：避免过度频繁地从队列获取帧，减少锁竞争，防止视频"快进"
                // 
                // 核心策略：**限制追赶速度**
                // - 即使视频落后音频，也要保持最小帧间隔
                // - 避免"一次性追上"导致的快进感
                // 
                // 三级策略：
                // 1. 同步状态（-10ms ~ +50ms）：正常显示，1帧/更新
                // 2. 轻微落后（50-150ms）：慢速追赶，1帧/更新，但阈值降低到30ms
                // 3. 严重落后（>150ms）：快速跳跃，直接丢弃过期帧
                let frame = if let Some(current_pts) = self.current_frame_pts {
                    // --- 已有当前帧：检查是否需要更新 ---
                    let time_diff = current_time_ms - current_pts;
                    
                    // 根据落后程度选择不同的更新阈值
                    let update_threshold = if time_diff > 150 {
                        // 严重落后（>150ms）：直接跳跃到最新帧
                        0  // 立即更新
                    } else if time_diff > 50 {
                        // 轻微落后（50-150ms）：慢速追赶
                        // 阈值降低到30ms，追赶速度约为 1.33x 播放速度
                        // 例如：24fps → 32fps 的追赶速度，用户几乎感觉不到
                        30
                    } else {
                        // 同步良好（-10~50ms）：正常播放
                        // 保持40ms阈值，即 24fps
                        40
                    };
                    
                    if time_diff >= update_threshold {
                        // 需要更新帧
                        
                        if time_diff > 150 {
                            // --- 严重落后（>150ms）：快速跳跃 ---
                            // 场景：卡顿、解码慢、seek 后等
                            // 策略：跳过所有过期帧，直接显示最接近当前时间的帧
                            debug!("🎬 视频严重落后 {}ms，快速跳跃到最新帧", time_diff);
                            let mut latest_frame = None;
                            let mut skipped_count = 0;
                            
                            // 最多检查10帧，避免阻塞UI
                            for _ in 0..10 {
                                if let Some(f) = manager.get_current_frame() {
                                    // 如果这一帧还是太旧（比当前时间早80ms以上），继续取下一帧
                                    if f.pts < current_time_ms - 80 {
                                        skipped_count += 1;
                                        latest_frame = Some(f);  // 暂存，继续找更新的
                                    } else {
                                        // 找到合适的帧（在目标前后80ms内），停止
                                        latest_frame = Some(f);
                                        break;
                                    }
                                } else {
                                    break;  // 队列空了
                                }
                            }
                            
                            if skipped_count > 0 {
                                debug!("🎬 跳过 {} 个过期帧，恢复同步", skipped_count);
                            }
                            
                            latest_frame
                        } else {
                            // --- 同步良好 或 轻微落后：逐帧播放/慢速追赶 ---
                            // 每次UI更新最多取1帧
                            // 轻微落后时通过降低阈值（30ms）来慢速追赶
                            // 追赶速度：24fps → 约32fps，非常平滑
                            manager.get_current_frame()
                        }
                    } else {
                        // 时间未到，继续显示当前帧
                        // 包括：
                        // 1. 视频超前音频（罕见）
                        // 2. 时间差小于阈值
                        None
                    }
                } else {
                    // --- 首次获取：立即获取帧 ---
                    // 或 seek 后 current_frame_pts 被重置为 None
                    manager.get_current_frame()
                };
                
                // ========== 帧渲染逻辑 ==========
                if let Some(frame) = frame {
                    // --- 获取到新帧 ---
                    if self.current_frame_pts != Some(frame.pts) {
                        // 新的帧（PTS 不同），更新纹理并渲染
                        // GPU 纹理更新较耗时，只在帧变化时执行
                        
                        // 调试日志：追踪音视频同步情况
                        let sync_diff = current_time_ms - frame.pts;
                        if sync_diff.abs() > 50 {
                            debug!("🎬 音视频同步差异: {}ms (音频={}, 视频={})", sync_diff, current_time_ms, frame.pts);
                        }
                        
                        if let Err(e) = renderer.update_and_render(ui, &frame, available_rect) {
                            error!("视频渲染失败: {}", e);
                        }
                        self.current_frame_pts = Some(frame.pts);
                    } else {
                        // 相同 PTS 的帧（理论上不应该出现，但做容错处理）
                        // 只渲染不更新纹理，避免不必要的 GPU 操作
                        if let Err(e) = renderer.render_video_frame_only(ui, available_rect) {
                            error!("视频渲染失败: {}", e);
                        }
                    }
                } else {
                    // --- 没有新帧：继续显示上一帧 ---
                    // 原因可能是：
                    // 1. 时间未到（current_time_ms < current_pts + 40）
                    // 2. 解码线程还没来得及推送新帧到队列
                    // 3. Seek 后，新帧还在路上
                    let has_frame = renderer.has_texture();
                    if !has_frame {
                        // 没有任何帧可显示，渲染占位符
                        self.render_placeholder(ui, available_rect);
                        self.current_frame_pts = None;
                    } else {
                        // 有上一帧的纹理，继续显示（避免闪烁）
                        if let Err(e) = renderer.render_video_frame_only(ui, available_rect) {
                            error!("视频渲染失败: {}", e);
                        }
                    }
                }
                
                // ========== 渲染字幕 ==========
                // 叠加在视频上方，根据当前播放时间选择合适的字幕
                self.render_subtitle(ui, available_rect, current_time_ms);
            } else {
                self.render_placeholder(ui, available_rect);
            }
        } else {
            // 渲染器未初始化时显示错误信息
            self.render_error_message(ui, available_rect, "视频渲染器未初始化");
        }
    }
    
    /// 渲染字幕
    /// 
    /// 功能特点：
    /// - 字幕显示在视频底部中央
    /// - 支持多行字幕
    /// - 黑色描边提高可读性
    /// - 半透明背景
    /// - 自适应字体大小
    fn render_subtitle(&self, ui: &mut Ui, video_rect: egui::Rect, current_time_ms: i64) {
        // 获取当前时间的字幕
        if let Some(manager) = self.playback_manager.try_read() {
            if let Some(subtitle) = manager.get_current_subtitle(current_time_ms) {
                // 字幕显示参数
                let subtitle_margin_bottom = 80.0; // 距离底部的间距
                let subtitle_max_width = video_rect.width() * 0.85; // 字幕最大宽度为视频宽度的85%
                
                // 根据视频尺寸自适应字体大小
                let base_font_size = (video_rect.height() * 0.03).max(18.0).min(32.0);
                let font_size = base_font_size;
                let line_height = font_size * 1.3;
                
                // 分行显示字幕文本
                let lines: Vec<&str> = subtitle.text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .collect();
                
                if lines.is_empty() {
                    return;
                }
                
                // 计算所需的总高度
                let total_height = lines.len() as f32 * line_height + 16.0; // 16.0 是上下padding
                
                // 计算字幕显示区域
                let subtitle_rect = egui::Rect::from_min_max(
                    egui::pos2(
                        video_rect.center().x - subtitle_max_width / 2.0,
                        video_rect.bottom() - subtitle_margin_bottom - total_height
                    ),
                    egui::pos2(
                        video_rect.center().x + subtitle_max_width / 2.0,
                        video_rect.bottom() - subtitle_margin_bottom
                    )
                );
                
                // 绘制半透明背景（提高可读性）
                ui.painter().rect_filled(
                    subtitle_rect.expand(8.0), // 扩大区域以创建padding
                    6.0, // 圆角
                    egui::Color32::from_rgba_premultiplied(0, 0, 0, 150) // 半透明黑色背景
                );
                
                // 绘制字幕文本（带描边效果以提高可读性）
                let painter = ui.painter();
                let text_color = egui::Color32::WHITE;
                let stroke_color = egui::Color32::from_rgb(0, 0, 0);
                let stroke_width = 2.0; // 描边宽度
                
                // 计算文本起始位置（垂直居中）
                let start_y = subtitle_rect.center().y - (lines.len() as f32 - 1.0) * line_height / 2.0;
                
                for (i, line) in lines.iter().enumerate() {
                    let trimmed_line = line.trim();
                    if trimmed_line.is_empty() {
                        continue;
                    }
                    
                    let y_pos = start_y + i as f32 * line_height;
                    let text_pos = egui::pos2(subtitle_rect.center().x, y_pos);
                    
                    // 绘制描边（多个方向的偏移以创建描边效果）
                    // 使用更精细的偏移模式，创建更好的描边效果
                    for dx in [-stroke_width, 0.0, stroke_width] {
                        for dy in [-stroke_width, 0.0, stroke_width] {
                            if dx != 0.0 || dy != 0.0 {
                                painter.text(
                                    text_pos + egui::vec2(dx, dy),
                                    egui::Align2::CENTER_CENTER,
                                    trimmed_line,
                                    egui::FontId::proportional(font_size),
                                    stroke_color,
                                );
                            }
                        }
                    }
                    
                    // 绘制文本本身
                    painter.text(
                        text_pos,
                        egui::Align2::CENTER_CENTER,
                        trimmed_line,
                        egui::FontId::proportional(font_size),
                        text_color,
                    );
                }
            }
        }
    }

    /// 渲染占位符
    fn render_placeholder(&self, ui: &mut Ui, rect: egui::Rect) {
        ui.allocate_ui_at_rect(rect, |ui| {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    
                    // 如果正在加载，显示加载动画
                    if let Some(ref url) = self.loading_url {
                        ui.label(
                            egui::RichText::new("⏳")
                                .size(64.0)
                                .color(egui::Color32::from_rgb(100, 149, 237))
                        );
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new("正在连接网络流...")
                                .size(24.0)
                                .color(egui::Color32::LIGHT_GRAY)
                        );
                        ui.add_space(5.0);
                        ui.label(
                            egui::RichText::new(url)
                                .size(14.0)
                                .color(egui::Color32::GRAY)
                        );
                        
                        // 添加旋转动画
                        ui.ctx().request_repaint();
                    } else {
                        // 默认占位符
                        ui.label(
                            egui::RichText::new("🎬")
                                .size(64.0)
                                .color(egui::Color32::GRAY)
                        );
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new("喜洋洋播放器")
                                .size(24.0)
                                .color(egui::Color32::LIGHT_GRAY)
                        );
                        ui.add_space(5.0);
                        ui.label(
                            egui::RichText::new("拖拽视频文件到此处或点击打开文件")
                                .size(14.0)
                                .color(egui::Color32::GRAY)
                        );
                    }
                });
            });
        });
    }

    /// 渲染错误信息
    fn render_error_message(&self, ui: &mut Ui, rect: egui::Rect, message: &str) {
        ui.allocate_ui_at_rect(rect, |ui| {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("❌")
                            .size(48.0)
                            .color(egui::Color32::RED)
                    );
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(message)
                            .size(16.0)
                            .color(egui::Color32::LIGHT_RED)
                    );
                });
            });
        });
    }

    /// 渲染控制面板
    fn render_controls_panel(&mut self, ctx: &Context) {
        egui::TopBottomPanel::bottom("controls")
            .resizable(false)
            .height_range(64.0..=64.0)
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(29, 29, 29))
                    .stroke(egui::Stroke::new(0.0, egui::Color32::TRANSPARENT))
            )
            .show_separator_line(false)
            .show(ctx, |ui| {
                    // 时间轴（进度条）- 占据大部分宽度
                    ui.add_space(4.0); 
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = egui::Vec2::new(8.0, 0.0);
                        ui.add_space(20.0); 
                        let (duration, position) = {
                            let manager = self.playback_manager.read();
                            (
                                manager.get_duration().unwrap_or(0.0),
                                manager.get_position().unwrap_or(0.0),
                            )
                        };
                        
                        // 当前时间标签（左侧固定宽度）
                        let current_time_text = format_time(position);
                        let _left_label_response = ui.label(
                            egui::RichText::new(current_time_text)
                                .size(12.0)
                                .color(egui::Color32::WHITE)
                        );
                        
                        // 进度条 - 使用剩余所有空间
                        let mut seek_pos = if self.ui_state.seeking {
                            self.ui_state.seek_position
                        } else {
                            position
                        };
                        
                        // 计算右侧标签的预估宽度
                        let total_time_text = format_time(duration);
                        let estimated_total_time_width = 78.0; // "HH:MM:SS" 格式
                        
                        // 获取当前可用宽度（已减去左侧标签）
                        let remaining_width = ui.available_width();
                        
                        // 进度条应该占据大部分空间（减去右侧标签和间距）
                        let progress_width = remaining_width - estimated_total_time_width; 
                        
                        // 使用 allocate_ui_with_layout 来强制分配指定宽度
                        let progress_ui = ui.allocate_ui_with_layout(
                            egui::Vec2::new(progress_width, 20.0),
                           // egui::Layout::main_space_between(egui::Align::Center),
                            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                            |ui| {
                                ui.style_mut().spacing.slider_width = progress_width;
                                ui.style_mut().spacing.slider_rail_height = 2.0;
                                ui.add(
                                    egui::Slider::new(&mut seek_pos, 0.0..=duration.max(1.0))
                                        .show_value(false)
                                        .text("")
                                )
                            }
                        );
                        
                        let progress_response = progress_ui.inner;
                        
                        // 在进度条上设置鼠标手势指针
                        if progress_response.hovered() || progress_response.dragged() {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        
                        // 检测拖拽开始
                        if progress_response.drag_started() {
                            self.ui_state.seeking = true;
                            self.ui_state.seek_position = seek_pos;
                            self.ui_state.seek_executed = false;  // 重置执行标志
                            info!("开始拖拽进度条，位置: {:.2}s", seek_pos);
                        }
                        
                        // 更新拖拽中的位置
                        if progress_response.dragged() {
                            self.ui_state.seek_position = seek_pos;
                        }
                        
                        // 检测拖拽结束（只执行一次seek）
                        if self.ui_state.seeking && !self.ui_state.seek_executed {
                            // 方法1: 使用 drag_stopped() （最可靠）
                            let is_drag_stopped = progress_response.drag_stopped();
                            // 方法2: 检查鼠标按钮是否释放
                            let is_button_released = ctx.input(|i| i.pointer.primary_released());
                            // 方法3: 检查是否不再拖拽且没有按下按钮
                            let is_no_longer_dragging = !progress_response.dragged() && 
                                                         !progress_response.is_pointer_button_down_on();
                            
                            if is_drag_stopped || is_button_released || is_no_longer_dragging {
                                info!("拖拽结束，执行 seek 到: {:.2}s", self.ui_state.seek_position);
                                let mut manager = self.playback_manager.write();
                                if let Err(e) = manager.seek_to_seconds(self.ui_state.seek_position) {
                                    error!("Seek 失败: {}", e);
                                } else {
                                    info!("Seek 成功执行");
                                    // 重置当前帧 PTS，强制获取新帧（特别是向后 seek 时）
                                    self.current_frame_pts = None;
                                    // 标记seek已执行，防止重复
                                    self.ui_state.seek_executed = true;
                                    // 记录seek完成时间，延迟500ms后重置seeking状态
                                    // 这样进度条会继续显示目标位置，直到实际帧到达
                                    self.ui_state.seek_complete_time = Some(Instant::now());
                                }
                            }
                        }
                        
                        // 自动重置seeking状态（在seek完成500ms后）
                        if let Some(seek_time) = self.ui_state.seek_complete_time {
                            if seek_time.elapsed() > Duration::from_millis(500) {
                                self.ui_state.seeking = false;
                                self.ui_state.seek_complete_time = None;
                                self.ui_state.seek_executed = false;
                                debug!("Seek 状态已自动重置");
                            }
                        }
                        
                        // 总时长标签（右侧）
                        // ui.label(
                        //     egui::RichText::new(total_time_text)
                        //         .size(12.0)
                        //         .color(egui::Color32::WHITE)
                        // );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(20.0); // 右侧margin 20px
                            ui.label(
                                egui::RichText::new(total_time_text)
                                    .size(12.0)
                                    .color(egui::Color32::WHITE)
                            );
                        });
                    });

                ui.vertical(|ui| {
                    ui.add_space(2.0);
                    // 第一行：控制按钮和音量（水平居中，垂直对齐）
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing = egui::Vec2::new(12.0, 0.0);
                                ui.add_space(16.0);
                                
                                // 统一按钮尺寸常量
                                const BUTTON_SIZE: f32 = 26.0;
                                const ICON_SIZE: f32 = 22.0;
                                
                                // 打开文件按钮（文件夹图标）- 深色背景
                                if let Some(icons) = &self.icons {
                                    // 使用自定义绘制：先绘制深色背景，再绘制图标
                                    let button_rect = egui::Rect::from_min_size(ui.cursor().min, egui::Vec2::new(BUTTON_SIZE, BUTTON_SIZE));
                                    let response = ui.allocate_rect(button_rect, egui::Sense::click());
                                    
                                    // 设置鼠标手势指针
                                    if response.hovered() {
                                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    
                                    // 绘制深色背景
                                    ui.painter().rect_filled(
                                        button_rect,
                                        0.0,  // 无圆角
                                        egui::Color32::from_rgb(29, 29, 29)
                                    );
                                    
                                    // 绘制图标（居中）
                                    let icon_rect = egui::Rect::from_center_size(
                                        button_rect.center(),
                                        egui::Vec2::new(18.0, 18.0)
                                    );
                                    ui.painter().image(
                                        icons.open_file.id(),
                                        icon_rect,
                                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                        egui::Color32::WHITE
                                    );
                                    
                                    if response.clicked() {
                                        if let Some(path) = rfd::FileDialog::new()
                                            .add_filter("视频文件", &["mp4", "avi", "mkv", "mov", "wmv", "flv"])
                                            .pick_file()
                                        {
                                            if let Some(path_str) = path.to_str() {
                                                if let Err(e) = self.open_file(path_str.to_string()) {
                                                    error!("打开文件失败: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }
                                
                                // 打开网络流按钮 - 🌐 图标
                                {
                                    let button_rect = egui::Rect::from_min_size(ui.cursor().min, egui::Vec2::new(BUTTON_SIZE, BUTTON_SIZE));
                                    let response = ui.allocate_rect(button_rect, egui::Sense::click());
                                    
                                    // 设置鼠标手势指针
                                    if response.hovered() {
                                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    
                                    // 绘制深色背景
                                    ui.painter().rect_filled(
                                        button_rect,
                                        0.0,
                                        egui::Color32::from_rgb(29, 29, 29)
                                    );
                                    
                                    // 绘制 🌐 图标（使用文字）
                                    let text_pos = button_rect.center() - egui::Vec2::new(10.0, 10.0);
                                    ui.painter().text(
                                        text_pos,
                                        egui::Align2::LEFT_TOP,
                                        "🌐",
                                        egui::FontId::proportional(16.0),
                                        egui::Color32::WHITE
                                    );
                                    
                                    if response.clicked() {
                                        info!("🌐 网络流按钮被点击");
                                        self.ui_state.show_url_dialog = true;
                                        info!("show_url_dialog 设置为: {}", self.ui_state.show_url_dialog);
                                    }
                                }
                                
                                // 播放/暂停按钮 - 深色背景
                                let is_playing = self.playback_manager.read().is_playing();
                                if let Some(icons) = &self.icons {
                                    // 使用自定义绘制：先绘制深色背景，再绘制图标
                                    let button_rect = egui::Rect::from_min_size(ui.cursor().min, egui::Vec2::new(BUTTON_SIZE, BUTTON_SIZE));
                                    let response = ui.allocate_rect(button_rect, egui::Sense::click());
                                    
                                    // 设置鼠标手势指针
                                    if response.hovered() {
                                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    
                                    // 绘制深色背景
                                    ui.painter().rect_filled(
                                        button_rect,
                                        0.0,  // 无圆角
                                        egui::Color32::from_rgb(29, 29, 29)
                                    );
                                    
                                    // 绘制图标（居中）
                                    let icon_handle = if is_playing { &icons.pause } else { &icons.play };
                                    let icon_rect = egui::Rect::from_center_size(
                                        button_rect.center(),
                                        egui::Vec2::new(ICON_SIZE, ICON_SIZE)
                                    );
                                    ui.painter().image(
                                        icon_handle.id(),
                                        icon_rect,
                                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                        egui::Color32::WHITE
                                    );
                                    
                                    if response.clicked() {
                                        let mut manager = self.playback_manager.write();
                                        if is_playing {
                                            let _ = manager.pause();
                                        } else {
                                            if let Err(e) = manager.play() {
                                                error!("播放失败: {}", e);
                                            }
                                        }
                                    }
                                }

                                // 停止按钮 - 深色背景
                                if let Some(icons) = &self.icons {
                                    // 使用自定义绘制：先绘制深色背景，再绘制图标
                                    let button_rect = egui::Rect::from_min_size(ui.cursor().min, egui::Vec2::new(BUTTON_SIZE, BUTTON_SIZE));
                                    let response = ui.allocate_rect(button_rect, egui::Sense::click());
                                    
                                    // 设置鼠标手势指针
                                    if response.hovered() {
                                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    
                                    // 绘制深色背景
                                    ui.painter().rect_filled(
                                        button_rect,
                                        0.0,  // 无圆角
                                        egui::Color32::from_rgb(29, 29, 29)
                                    );
                                    
                                    // 绘制图标（居中）
                                    let icon_rect = egui::Rect::from_center_size(
                                        button_rect.center(),
                                        egui::Vec2::new(ICON_SIZE, ICON_SIZE)
                                    );
                                    ui.painter().image(
                                        icons.stop.id(),
                                        icon_rect,
                                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                        egui::Color32::WHITE
                                    );
                                    
                                    if response.clicked() {
                                        let mut manager = self.playback_manager.write();
                                        manager.stop();
                                        // 停止播放：重置到开头，清空当前帧
                                        self.current_frame_pts = None;
                                        // 清理视频渲染器的纹理缓存
                                        if let Some(renderer) = &mut self.video_renderer {
                                            renderer.cleanup();
                                        }
                                    }
                                }
                                
                                // 音量控制
                                ui.label(
                                    egui::RichText::new("音量:")
                                        .size(12.0)
                                        .color(egui::Color32::WHITE)
                                );
                                let volume_slider_response = ui.scope(|ui| {
                                    ui.style_mut().spacing.slider_rail_height = 2.0;
                                    ui.add_sized(
                                        egui::Vec2::new(100.0, 16.0),
                                        egui::Slider::new(&mut self.ui_state.volume, 0.0..=1.0)
                                            .show_value(false)
                                    )
                                });
                                // 在音量滑块上设置鼠标手势指针
                                if volume_slider_response.inner.hovered() || volume_slider_response.inner.dragged() {
                                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                // 检测音量变化，同步到播放管理器
                                if volume_slider_response.inner.changed() || volume_slider_response.inner.dragged() {
                                    if let Some(manager) = self.playback_manager.try_read() {
                                        manager.set_volume(self.ui_state.volume);
                                    }
                                }
                                ui.label(
                                    egui::RichText::new(format!("{:.0}%", self.ui_state.volume * 100.0))
                                        .size(12.0)
                                        .color(egui::Color32::WHITE)
                                );
                            });
                        });
                        
                        // 全屏提示文本（最右边，距离窗口边缘20px）
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(40.0); // 右侧margin 20px
                            ui.label(
                                egui::RichText::new("F11: 全屏/ESC: 退出全屏")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(69, 69, 69)) // 使用灰色作为提示文本
                            );
                        });
                    });
                    
                    ui.add_space(12.0);
                });
            });
    }

    /// 渲染信息面板
    fn render_info_panel(&self, ctx: &Context) {
        // 只在可见时才渲染
        if !self.ui_state.info_panel_visible {
            return;
        }
        
        egui::Window::new("Media Info")
            .anchor(egui::Align2::LEFT_TOP, egui::Vec2::new(10.0, 10.0))
            .resizable(false)
            .collapsible(true)
            .default_open(false)
            .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_black_alpha(200)))
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    if let Some(file) = &self.ui_state.current_file {
                        // 只显示文件名，避免路径中的中文字符乱码
                        let file_name = std::path::Path::new(file)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(file);
                        ui.label(
                            egui::RichText::new(format!("File: {}", file_name))
                                .size(12.0)
                                .color(egui::Color32::WHITE)
                        );
                    }
                    
                    let manager = self.playback_manager.read();
                    if let Some(info) = manager.get_media_info() {
                        ui.label(
                            egui::RichText::new(format!("Resolution: {}x{}", info.width, info.height))
                                .size(12.0)
                                .color(egui::Color32::WHITE)
                        );
                        ui.label(
                            egui::RichText::new(format!("Duration: {}", format_time(info.duration as f64 / 1000.0)))
                                .size(12.0)
                                .color(egui::Color32::WHITE)
                        );
                        ui.label(
                            egui::RichText::new(format!("Video: {}", info.video_codec))
                                .size(12.0)
                                .color(egui::Color32::WHITE)
                        );
                        ui.label(
                            egui::RichText::new(format!("Audio: {}", info.audio_codec))
                                .size(12.0)
                                .color(egui::Color32::WHITE)
                        );
                    }
                    
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!("FPS: {:.1}", self.perf_stats.fps))
                            .size(12.0)
                            .color(egui::Color32::WHITE)
                    );
                    ui.label(
                        egui::RichText::new(format!("Frame Time: {:.1}ms", self.perf_stats.frame_time.as_secs_f32() * 1000.0))
                            .size(12.0)
                            .color(egui::Color32::WHITE)
                    );
                });
            });
    }

    /// 检测是否处于全屏模式
    fn is_fullscreen(&self, ctx: &Context) -> bool {
        ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
    }
    
    /// 切换全屏模式
    fn toggle_fullscreen(&mut self, ctx: &Context) {
        let is_fullscreen = self.is_fullscreen(ctx);
        let will_be_fullscreen = !is_fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(will_be_fullscreen));
        self.ui_state.is_fullscreen = will_be_fullscreen;
        
        // 全屏时隐藏标题栏，退出全屏时恢复
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!will_be_fullscreen));
        
        // 进入全屏时，初始隐藏控制面板（提升观看体验）
        if will_be_fullscreen {
            self.ui_state.controls_visible = false;
            self.ui_state.controls_hide_timer = None;
        }
    }

    /// 渲染 URL 对话框（打开网络流）
    fn render_url_dialog(&mut self, ctx: &Context) {
        if !self.ui_state.show_url_dialog {
            return;
        }
        
        let mut should_close = false;  // 用于跟踪是否应该关闭对话框
        let mut should_open_url = false;  // 用于跟踪是否应该打开 URL
        
        let window_response = egui::Window::new("打开网络流")
            .collapsible(false)
            .resizable(false)
            .default_width(500.0)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new("请输入流地址：").size(14.0));
                    ui.add_space(10.0);
                    
                    // URL 输入框
                    let text_edit = egui::TextEdit::singleline(&mut self.ui_state.url_input)
                        .hint_text("例如: rtsp://example.com/stream")
                        .desired_width(460.0)
                        .font(egui::TextStyle::Monospace);
                    
                    let response = ui.add(text_edit);
                    
                    // 自动聚焦到输入框（只在第一帧）
                    response.request_focus();
                    
                    ui.add_space(15.0);
                    
                    // 协议说明（可折叠）
                    ui.collapsing("支持的协议", |ui| {
                        ui.add_space(5.0);
                        ui.label("• RTSP: rtsp://example.com/stream");
                        ui.label("• RTMP: rtmp://example.com/live/stream");
                        ui.label("• HLS: http://example.com/stream.m3u8");
                        ui.label("• HTTP: http://example.com/video.mp4");
                        ui.add_space(5.0);
                    });
                    
                    ui.add_space(15.0);
                    
                    // 按钮
                    let mut clicked_open = false;
                    let mut clicked_cancel = false;
                    
                    ui.horizontal(|ui| {
                        if ui.button(egui::RichText::new("  打开  ").size(14.0)).clicked() 
                            || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                            clicked_open = true;
                        }
                        
                        if ui.button(egui::RichText::new("  取消  ").size(14.0)).clicked() {
                            clicked_cancel = true;
                        }
                    });
                    
                    // 检测窗口关闭按钮（X）
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        clicked_cancel = true;
                    }
                    
                    // 返回按钮状态
                    (clicked_open, clicked_cancel)
                })
            });
        
        // 处理窗口响应
        if let Some(inner_response) = window_response {
            // inner_response.inner 是 Option<InnerResponse<(bool, bool)>>
            // 需要再次解包得到 (bool, bool)
            if let Some(vertical_response) = inner_response.inner {
                let (clicked_open, clicked_cancel) = vertical_response.inner;
                if clicked_open {
                    should_open_url = true;
                    should_close = true;
                }
                if clicked_cancel {
                    should_close = true;
                }
            }
        } else {
            // 窗口被关闭（用户点击了 X 按钮）
            should_close = true;
        }
        
        // 处理 Esc 键关闭
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            should_close = true;
        }
        
        // 统一关闭对话框（立即关闭，避免UI卡顿）
        if should_close {
            self.ui_state.show_url_dialog = false;
        }
        
        // 在闭包外部执行操作（避免借用冲突）
        // 在子线程中打开URL，避免阻塞主线程
        if should_open_url {
            self.open_url_async();
        }
    }
    
    /// 打开网络流（同步版本，保留用于兼容）
    fn open_url(&mut self) {
        if self.ui_state.url_input.trim().is_empty() {
            warn!("URL 为空，取消打开");
            return;
        }
        
        let url = self.ui_state.url_input.trim().to_string();
        info!("📡 尝试打开网络流: {}", url);
        
        // 解析 URL
        match MediaSource::from_url(&url) {
            Ok(source) => {
                if let Some(mut manager) = self.playback_manager.try_write() {
                    match manager.open_media_source(source) {
                        Ok(media_info) => {
                            info!("✅ 网络流打开成功: {:?}", media_info);
                            self.ui_state.current_file = Some(url);
                            
                            // 自动播放
                            if let Err(e) = manager.play() {
                                error!("❌ 自动播放失败: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("❌ 网络流打开失败: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                error!("❌ URL 解析失败: {}", e);
            }
        }
    }
    
    /// 异步打开网络流（使用新架构 - DemuxerFactory）
    fn open_url_async(&mut self) {
        if self.ui_state.url_input.trim().is_empty() {
            warn!("URL 为空，取消打开");
            return;
        }
        
        let url = self.ui_state.url_input.trim().to_string();
        
        info!("📡 使用新架构异步打开网络流: {}", url);
        
        // 设置加载状态
        self.loading_url = Some(url.clone());
        
        // 使用 DemuxerFactory 异步创建 Demuxer
        use crate::player::DemuxerFactory;
        
        let result_tx = self.demuxer_result_tx.clone();
        
        // 🔥 优化：在主线程中解析 URL（操作很快，不需要单独线程）
        info!("🔄 主线程解析 URL: {}", url);
        match MediaSource::from_url(&url) {
            Ok(source) => {
                info!("✅ URL 解析成功，在子线程中创建 Demuxer");
                
                // 使用 DemuxerFactory 在子线程中创建 Demuxer（这里会创建线程执行耗时的 Demuxer::open）
                DemuxerFactory::create_async(source, result_tx);
            }
            Err(e) => {
                error!("❌ URL 解析失败: {}", e);
                
                // 发送失败结果
                let _ = result_tx.send(crate::player::DemuxerCreationResult::Failed {
                    url: url.clone(),
                    error: e.to_string(),
                });
            }
        }
    }
    
    /// 渲染网络流状态
    fn render_stream_status(&self, ui: &mut Ui) {
        if let Some(manager) = self.playback_manager.try_read() {
            if let Some(state) = manager.get_stream_state() {
                match state {
                    StreamState::Connecting => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new("正在连接...").color(egui::Color32::YELLOW));
                        });
                    }
                    StreamState::Buffering { progress } => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new(format!("缓冲中... {:.0}%", progress * 100.0))
                                .color(egui::Color32::YELLOW));
                        });
                        
                        // 缓冲进度条
                        ui.add(egui::ProgressBar::new(progress)
                            .show_percentage());
                    }
                    StreamState::Reconnecting { attempt } => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new(format!("重新连接中... (尝试 {})", attempt))
                                .color(egui::Color32::from_rgb(255, 165, 0)));
                        });
                    }
                    StreamState::Failed { reason } => {
                        ui.colored_label(
                            egui::Color32::RED,
                            format!("❌ 连接失败: {}", reason)
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    /// 处理键盘输入
    fn handle_keyboard_input(&mut self, ctx: &Context) {
        // 使用标志位在闭包外处理需要 ctx 的操作，避免双重锁定
        let mut should_toggle_fullscreen = false;
        let mut current_fullscreen_state = false;
        let mut should_exit_fullscreen = false;
        let mut should_hide_info_panel = false;
        let mut should_toggle_info_panel = false;
        
        ctx.input(|i| {
            // 空格键：播放/暂停
            if i.key_pressed(egui::Key::Space) {
                let mut manager = self.playback_manager.write();
                if manager.is_playing() {
                    let _ = manager.pause();
                } else {
                    let _ = manager.play();
                }
            }
            
            // 左右箭头：快进/快退
            if i.key_pressed(egui::Key::ArrowLeft) {
                let mut manager = self.playback_manager.write();
                if let Ok(pos) = manager.get_position() {
                    let _ = manager.seek_to_seconds((pos - 10.0).max(0.0));
                }
            }
            
            if i.key_pressed(egui::Key::ArrowRight) {
                let mut manager = self.playback_manager.write();
                if let Ok(pos) = manager.get_position() {
                    let duration = manager.get_duration().unwrap_or(0.0);
                    let _ = manager.seek_to_seconds((pos + 10.0).min(duration));
                }
            }
            
            // F11: 全屏切换（标记为需要切换，在闭包外执行）
            if i.key_pressed(egui::Key::F11) {
                should_toggle_fullscreen = true;
                // 在闭包内获取当前全屏状态
                current_fullscreen_state = i.viewport().fullscreen.unwrap_or(false);
            }
            
            // Tab: 显示/隐藏信息面板
            if i.key_pressed(egui::Key::Tab) {
                should_toggle_info_panel = true;
            }
            
            // Escape: 检查是否需要退出全屏或隐藏信息面板
            if i.key_pressed(egui::Key::Escape) {
                // 在 input 闭包内直接检查 fullscreen 状态
                let is_fullscreen = i.viewport().fullscreen.unwrap_or(false);
                if is_fullscreen {
                    should_exit_fullscreen = true;
                } else {
                    should_hide_info_panel = true;
                }
            }
        });
        
        // 在闭包外执行需要 ctx 的操作，避免双重锁定
        if should_toggle_fullscreen {
            // F11: 切换全屏状态（使用闭包内获取的状态）
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!current_fullscreen_state));
            self.ui_state.is_fullscreen = !current_fullscreen_state;
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(current_fullscreen_state));
        } else if should_exit_fullscreen {
            // Esc（在全屏时）: 退出全屏
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            self.ui_state.is_fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        } else if should_hide_info_panel {
            // Esc（非全屏时）: 隐藏信息面板
            self.ui_state.info_panel_visible = false;
        }
        
        if should_toggle_info_panel {
            self.ui_state.info_panel_visible = !self.ui_state.info_panel_visible;
        }
    }
}

/// 格式化时间显示
fn format_time(seconds: f64) -> String {
    let total_seconds = seconds as u64;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;
    
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, secs)
    } else {
        format!("{:02}:{:02}", minutes, secs)
    }
}
