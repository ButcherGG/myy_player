use anyhow::Result;
use egui::{Ui, Rect, TextureHandle, ColorImage, TextureOptions};
use log::{info, debug};
use std::collections::HashMap;
use std::sync::Arc;
use eframe::wgpu::{Device, Queue, Texture, TextureView, TextureDescriptor, TextureUsages, TextureDimension, TextureFormat, Extent3d, ImageCopyTexture, ImageDataLayout, Origin3d};

use crate::core::VideoFrame;

/// egui 视频渲染器 - 高性能零拷贝纹理更新
pub struct EguiVideoRenderer {
    /// wgpu 设备 (Arc 包装)
    device: Arc<Device>,
    /// wgpu 队列 (Arc 包装)
    queue: Arc<Queue>,
    /// 当前视频纹理
    video_texture: Option<VideoTexture>,
    /// egui 纹理句柄缓存
    texture_cache: HashMap<String, TextureHandle>,
    /// 渲染统计
    stats: RenderStats,
}

struct VideoTexture {
    /// wgpu 纹理
    wgpu_texture: Texture,
    /// 纹理视图
    texture_view: TextureView,
    /// egui 纹理句柄
    egui_handle: TextureHandle,
    /// 纹理尺寸
    width: u32,
    height: u32,
    /// 最后更新时间戳
    last_pts: i64,
}

#[derive(Default)]
struct RenderStats {
    frames_rendered: u64,
    texture_updates: u64,
    cache_hits: u64,
    cache_misses: u64,
}

impl EguiVideoRenderer {
    /// 创建新的 egui 视频渲染器
    pub fn new(wgpu_render_state: &eframe::egui_wgpu::RenderState) -> Result<Self> {
        info!("🎨 初始化 EguiVideoRenderer");

        let device = wgpu_render_state.device.clone();
        let queue = wgpu_render_state.queue.clone();

        Ok(Self {
            device,
            queue,
            video_texture: None,
            texture_cache: HashMap::new(),
            stats: RenderStats::default(),
        })
    }

    /// 更新纹理并渲染视频帧
    pub fn update_and_render(&mut self, ui: &mut Ui, frame: &VideoFrame, rect: Rect) -> Result<()> {
        // 检查是否需要更新纹理（只在PTS变化时更新，避免重复更新同一帧）
        let needs_update = self.video_texture.as_ref()
            .map(|tex| {
                // 只在以下情况更新：
                // 1. PTS不同（新帧）
                // 2. 尺寸变化
                tex.last_pts != frame.pts || tex.width != frame.width || tex.height != frame.height
            })
            .unwrap_or(true);

        if needs_update {
            debug!("📺 渲染视频帧: {}x{}, PTS: {}ms", frame.width, frame.height, frame.pts);
            self.update_video_texture(ui.ctx(), frame)?;
            self.stats.texture_updates += 1;
        } else {
            self.stats.cache_hits += 1;
        }

        // 渲染视频帧（即使没有更新纹理，也要渲染，因为egui可能重绘）
        self.render_video_frame(ui, rect)?;
        self.stats.frames_rendered += 1;

        Ok(())
    }

    /// 更新视频纹理
    fn update_video_texture(&mut self, ctx: &egui::Context, frame: &VideoFrame) -> Result<()> {
        debug!("🔄 更新视频纹理: {}x{}, PTS: {}ms", frame.width, frame.height, frame.pts);

        // 检查是否需要重新创建纹理
        let needs_recreate = self.video_texture.as_ref()
            .map(|tex| tex.width != frame.width || tex.height != frame.height)
            .unwrap_or(true);

        if needs_recreate {
            info!("🆕 创建新视频纹理: {}x{}", frame.width, frame.height);
            self.create_video_texture(ctx, frame)?;
        } else {
            // 只更新纹理数据
            self.update_texture_data(ctx, frame)?;
        }

        Ok(())
    }

    /// 创建新的视频纹理
    fn create_video_texture(&mut self, ctx: &egui::Context, frame: &VideoFrame) -> Result<()> {
        // 创建 wgpu 纹理
        let texture_desc = TextureDescriptor {
            label: Some("Video Texture"),
            size: Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb, // RGBA8 格式
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        };

        let wgpu_texture = self.device.as_ref().create_texture(&texture_desc);
        let texture_view = wgpu_texture.create_view(&Default::default());

        // 上传初始纹理数据
        self.queue.as_ref().write_texture(
            ImageCopyTexture {
                texture: &wgpu_texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: eframe::wgpu::TextureAspect::All,
            },
            &frame.data,
            ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * frame.width), // RGBA = 4 bytes per pixel
                rows_per_image: Some(frame.height),
            },
            texture_desc.size,
        );

        // 创建 egui 纹理句柄
        let egui_handle = self.create_egui_texture_handle(ctx, frame)?;

        // 保存纹理信息
        self.video_texture = Some(VideoTexture {
            wgpu_texture,
            texture_view,
            egui_handle,
            width: frame.width,
            height: frame.height,
            last_pts: frame.pts,
        });

        Ok(())
    }

    /// 创建 egui 纹理句柄
    fn create_egui_texture_handle(&self, ctx: &egui::Context, frame: &VideoFrame) -> Result<TextureHandle> {
        // 将 RGBA 数据转换为 egui ColorImage
        let color_image = ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.data,
        );

        // 创建纹理句柄
        let handle = ctx.load_texture(
            "video_frame",
            color_image,
            TextureOptions::LINEAR, // 线性过滤获得更好的缩放质量
        );

        Ok(handle)
    }

    /// 更新现有纹理数据
    fn update_texture_data(&mut self, _ctx: &egui::Context, frame: &VideoFrame) -> Result<()> {
        if let Some(video_texture) = &mut self.video_texture {
            // 只更新 egui 纹理句柄（不更新 wgpu 纹理，因为 egui 有自己的渲染管线）
            // 将 RGBA 数据转换为 egui ColorImage 并更新纹理
            let color_image = ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.data,
            );
            
            // 更新现有纹理（egui 会处理实际的 GPU 上传）
            video_texture.egui_handle.set(color_image, TextureOptions::LINEAR);

            video_texture.last_pts = frame.pts;
        }

        Ok(())
    }

    /// 渲染视频帧到 UI
    fn render_video_frame(&self, ui: &mut Ui, rect: Rect) -> Result<()> {
        self.render_video_frame_only(ui, rect)
    }

    /// 仅渲染视频帧（不更新纹理），用于避免重复更新导致的闪烁
    pub fn render_video_frame_only(&self, ui: &mut Ui, rect: Rect) -> Result<()> {
        if let Some(video_texture) = &self.video_texture {
            // 计算视频的显示尺寸，保持宽高比
            let video_aspect = video_texture.width as f32 / video_texture.height as f32;
            let rect_aspect = rect.width() / rect.height();

            let display_size = if video_aspect > rect_aspect {
                // 视频更宽，以宽度为准
                egui::Vec2::new(rect.width(), rect.width() / video_aspect)
            } else {
                // 视频更高，以高度为准
                egui::Vec2::new(rect.height() * video_aspect, rect.height())
            };

            // 居中显示
            let display_rect = Rect::from_center_size(rect.center(), display_size);

            // 渲染视频帧
            ui.allocate_ui_at_rect(display_rect, |ui| {
                ui.add(
                    egui::Image::from_texture(&video_texture.egui_handle)
                        .fit_to_exact_size(display_size)
                        .rounding(egui::Rounding::same(4.0)) // 圆角
                );
            });

            // 调试信息 (可选)
            // if ui.ctx().debug_on_hover() {
            //     ui.allocate_ui_at_rect(
            //         Rect::from_min_size(rect.left_top() + egui::Vec2::new(10.0, 10.0), egui::Vec2::new(200.0, 60.0)),
            //         |ui| {
            //             ui.label(format!("视频: {}x{}", video_texture.width, video_texture.height));
            //             ui.label(format!("PTS: {}ms", video_texture.last_pts));
            //             ui.label(format!("渲染: {} 帧", self.stats.frames_rendered));
            //         }
            //     );
            // }
        }

        Ok(())
    }

    /// 获取渲染统计信息
    pub fn get_stats(&self) -> &RenderStats {
        &self.stats
    }

    /// 检查是否有纹理（用于判断是否应该显示占位符）
    pub fn has_texture(&self) -> bool {
        self.video_texture.is_some()
    }

    /// 清理资源
    pub fn cleanup(&mut self) {
        info!("🧹 清理 EguiVideoRenderer 资源");
        self.video_texture = None;
        self.texture_cache.clear();
    }
}

impl Drop for EguiVideoRenderer {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// 性能优化的纹理更新策略
impl EguiVideoRenderer {
    /// 零拷贝纹理更新 (高级优化)
    /// 
    /// 这个方法尝试直接更新 GPU 纹理而不经过 CPU 拷贝
    /// 适用于硬件解码的场景
    #[allow(dead_code)]
    fn zero_copy_texture_update(&mut self, ctx: &egui::Context, frame: &VideoFrame) -> Result<()> {
        // TODO: 实现零拷贝更新
        // 1. 如果视频帧来自 GPU (硬件解码)，直接使用 GPU 纹理
        // 2. 使用 wgpu 的 copy_texture_to_texture
        // 3. 避免 CPU-GPU 数据传输

        debug!("🚀 零拷贝纹理更新 (未实现)");
        
        // 当前回退到常规更新
        self.update_texture_data(ctx, frame)
    }

    /// 纹理池管理 (内存优化)
    /// 
    /// 重用纹理对象以减少分配开销
    #[allow(dead_code)]
    fn get_pooled_texture(&mut self, _width: u32, _height: u32) -> Result<&mut VideoTexture> {
        // TODO: 实现纹理池
        // 1. 维护不同尺寸的纹理池
        // 2. 重用相同尺寸的纹理
        // 3. 定期清理未使用的纹理

        todo!("纹理池未实现")
    }
}
