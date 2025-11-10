use crate::core::{MediaInfo, PlayerError, Result};
use crate::player::demuxer_source::{DemuxerSource, MediaPacket, PacketType};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{format, media};
use log::{debug, info};

/// 解封装器 - 负责读取媒体文件并分离音视频流
pub struct Demuxer {
    input_ctx: format::context::Input,
    video_stream_index: Option<usize>,
    audio_stream_index: Option<usize>,
    subtitle_stream_index: Option<usize>,
    media_info: MediaInfo,  // 缓存媒体信息
    source_path: String,    // 媒体源路径（用于描述）
}

impl Demuxer {
    /// 打开媒体文件
    pub fn open(path: &str) -> Result<Self> {
        info!("正在打开文件: {}", path);

        // 🔥 检测 YouTube URL（FFmpeg 无法直接打开，需要先提取流 URL）
        let is_youtube = path.contains("youtube.com") || path.contains("youtu.be");
        if is_youtube {
            return Err(PlayerError::OpenError(format!(
                "YouTube URL 不支持直接播放。\n\n\
                YouTube 的网页 URL（如 {}) 不是直接的媒体流地址，FFmpeg 无法直接打开。\n\n\
                解决方案：\n\
                1. 使用 yt-dlp 提取实际的流 URL：\n\
                   yt-dlp -g \"{}\"\n\n\
                2. 将提取的流 URL 粘贴到播放器中播放\n\n\
                3. 或者使用支持 YouTube 的播放器（如 PotPlayer、VLC）",
                path, path
            )));
        }

        // 判断是否为网络流
        let is_network = path.starts_with("http://") 
            || path.starts_with("https://")
            || path.starts_with("rtsp://")
            || path.starts_with("rtmp://")
            || path.contains(".m3u8");
        
        // 为网络流设置选项
        let input_ctx = if is_network {
            info!("🌐 检测到网络流，应用优化选项");
            
            // 创建选项字典
            let mut options = ffmpeg::Dictionary::new();
            
            // 关键：组合多个 fflags（避免花屏和加速）
            // discardcorrupt: 丢弃损坏的帧
            // genpts: 生成 PTS（防止时间戳问题）
            // nobuffer: 减少缓冲延迟
            // igndts: 忽略 DTS（某些流的 DTS 不准确）
            options.set("fflags", "+discardcorrupt+genpts+nobuffer+igndts");
            
            // 降低分析时间（加快启动）
            options.set("analyzeduration", "5000000");  // 5秒（增加以获取更准确的流信息和关键帧）
            options.set("probesize", "10000000");       // 10MB（增加以确保找到关键帧）
            
            // 网络超时设置
            options.set("timeout", "15000000");  // 15秒超时
            
            // 🔥 增加网络缓冲（减少卡顿）
            options.set("buffer_size", "8388608");  // 8MB 缓冲区（大幅增加网络缓冲）
            
            // 启用低延迟模式
            options.set("max_delay", "500000");  // 最大延迟 0.5 秒
            
            // 重排序队列大小（减少以降低延迟）
            options.set("reorder_queue_size", "0");
            
            options.set("rw_timeout", "8000000");      // 读写操作 8s 超时
            options.set("stimeout", "8000000");        // socket 层超时
            options.set("http_multiple", "1");         // 每次重连不用复用旧连接
            options.set("reconnect", "1");             // 打开 FFmpeg 内部重连（若已默认可忽略）
            options.set("reconnect_streamed", "1");
            options.set("reconnect_delay_max", "4");

            // HLS 特定选项
            if path.contains(".m3u8") {
                info!("🎬 HLS 流检测，应用 HLS 优化");
                // 从最新片段开始（点播流使用 -1，直播流使用 -3）
                options.set("live_start_index", "-1");
                // 允许的最大重载次数
                options.set("max_reload", "10");  // 增加重试次数
                // HTTP 持久连接
                options.set("http_persistent", "1");
                // 🔥 HLS 分片缓冲（提前下载多个分片）
                options.set("hls_init_time", "5");  // 初始缓冲5秒
            }
            
            format::input_with_dictionary(&path, options)
                .map_err(|e| PlayerError::OpenError(format!("无法打开网络流: {}", e)))?
        } else {
            format::input(&path)
                .map_err(|e| PlayerError::OpenError(format!("无法打开文件: {}", e)))?
        };

        // 查找视频流和音频流
        let video_stream_index = input_ctx
            .streams()
            .best(media::Type::Video)
            .map(|s| s.index());

        let audio_stream_index = input_ctx
            .streams()
            .best(media::Type::Audio)
            .map(|s| s.index());

        // 查找字幕流（第一个字幕流）
        let subtitle_stream_index = input_ctx
            .streams()
            .filter(|s| s.parameters().medium() == media::Type::Subtitle)
            .next()
            .map(|s| s.index());

        if video_stream_index.is_none() {
            return Err(PlayerError::NoVideoStream);
        }

        debug!("视频流索引: {:?}", video_stream_index);
        debug!("音频流索引: {:?}", audio_stream_index);
        debug!("字幕流索引: {:?}", subtitle_stream_index);

        let mut demuxer = Self {
            input_ctx,
            video_stream_index,
            audio_stream_index,
            subtitle_stream_index,
            media_info: MediaInfo::default(),  // 临时默认值
            source_path: path.to_string(),
        };
        
        // 获取并缓存媒体信息
        demuxer.media_info = demuxer.extract_media_info()?;
        
        Ok(demuxer)
    }

    /// 提取媒体信息（内部使用）
    fn extract_media_info(&self) -> Result<MediaInfo> {
        let video_stream = self
            .input_ctx
            .stream(self.video_stream_index.unwrap())
            .ok_or(PlayerError::NoVideoStream)?;

        let video_codec = video_stream.parameters();
        
        // 先获取编解码器名称（在 video_codec 被移动前）
        let video_codec_name = video_codec
            .id()
            .name()
            .to_string();
        
        let decoder = ffmpeg::codec::context::Context::from_parameters(video_codec)?;
        let video_decoder = decoder.decoder().video()?;

        let width = video_decoder.width();
        let height = video_decoder.height();
        let fps = video_stream.avg_frame_rate();
        let fps = fps.numerator() as f64 / fps.denominator() as f64;

        let duration = self.input_ctx.duration() / 1000; // 微秒转毫秒

        let (audio_codec_name, sample_rate, channels) = if let Some(audio_idx) = self.audio_stream_index {
            let audio_stream = self.input_ctx.stream(audio_idx).unwrap();
            let audio_codec = audio_stream.parameters();
            
            // 先获取编解码器名称（在 audio_codec 被移动前）
            let codec_name = audio_codec.id().name().to_string();
            
            let decoder = ffmpeg::codec::context::Context::from_parameters(audio_codec)?;
            let audio_decoder = decoder.decoder().audio()?;

            (
                codec_name,
                audio_decoder.rate(),
                audio_decoder.channels(),
            )
        } else {
            ("none".to_string(), 0, 0)
        };

        Ok(MediaInfo {
            duration,
            width,
            height,
            fps,
            video_codec: video_codec_name,
            audio_codec: audio_codec_name,
            sample_rate,
            channels,
        })
    }

    /// 获取视频流索引
    pub fn video_stream_index(&self) -> Option<usize> {
        self.video_stream_index
    }

    /// 获取音频流索引
    pub fn audio_stream_index(&self) -> Option<usize> {
        self.audio_stream_index
    }

    /// 获取视频流
    pub fn video_stream(&self) -> Option<format::stream::Stream> {
        self.video_stream_index
            .map(|idx| self.input_ctx.stream(idx).unwrap())
    }

    /// 获取音频流
    pub fn audio_stream(&self) -> Option<format::stream::Stream> {
        self.audio_stream_index
            .map(|idx| self.input_ctx.stream(idx).unwrap())
    }

    /// 获取字幕流索引
    pub fn subtitle_stream_index(&self) -> Option<usize> {
        self.subtitle_stream_index
    }

    /// 获取字幕流
    pub fn subtitle_stream(&self) -> Option<format::stream::Stream> {
        self.subtitle_stream_index
            .map(|idx| self.input_ctx.stream(idx).unwrap())
    }

    /// 读取下一个数据包
    /// 返回 (packet, is_video, is_subtitle)
    pub fn read_packet(&mut self) -> Result<Option<(ffmpeg::Packet, bool, bool)>> {
        match self.input_ctx.packets().next() {
            Some((stream, packet)) => {
                let is_video = Some(stream.index()) == self.video_stream_index;
                let is_audio = Some(stream.index()) == self.audio_stream_index;
                let is_subtitle = Some(stream.index()) == self.subtitle_stream_index;

                if is_video || is_audio || is_subtitle {
                    Ok(Some((packet, is_video, is_subtitle)))
                } else {
                    // 跳过其他流
                    self.read_packet()
                }
            }
            None => Ok(None),
        }
    }

    /// Seek 到指定位置（毫秒）
    fn seek_internal(&mut self, timestamp_ms: i64) -> Result<()> {
        let timestamp = timestamp_ms * 1000; // 毫秒转微秒
        self.input_ctx
            .seek(timestamp, ..timestamp)?;
        Ok(())
    }
    
    /// Seek 到指定位置（毫秒）- 公开接口
    pub fn seek(&mut self, timestamp_ms: i64) -> Result<()> {
        self.seek_internal(timestamp_ms)
    }
    
    /// 获取媒体信息（公开接口）
    pub fn get_media_info(&self) -> Result<MediaInfo> {
        Ok(self.media_info.clone())
    }
    
    /// 获取源路径描述
    pub fn description(&self) -> String {
        self.source_path.clone()
    }
}

// 实现 DemuxerSource trait
impl DemuxerSource for Demuxer {
    fn read_packet(&mut self) -> Result<Option<MediaPacket>> {
        loop {
            match self.input_ctx.packets().next() {
                Some((stream, packet)) => {
                    let stream_index = stream.index();
                    
                    // 判断包类型
                    if Some(stream_index) == self.video_stream_index {
                        return Ok(Some(MediaPacket {
                            packet,  // ✅ 使用 SegQueue，无需 clone
                            packet_type: PacketType::Video,
                            stream_index,
                        }));
                    } else if Some(stream_index) == self.audio_stream_index {
                        return Ok(Some(MediaPacket {
                            packet,
                            packet_type: PacketType::Audio,
                            stream_index,
                        }));
                    } else if Some(stream_index) == self.subtitle_stream_index {
                        return Ok(Some(MediaPacket {
                            packet,
                            packet_type: PacketType::Subtitle,
                            stream_index,
                        }));
                    }
                    // 否则跳过这个包，继续循环
                }
                None => return Ok(None),
            }
        }
    }
    
    fn seek(&mut self, timestamp_ms: i64) -> Result<()> {
        self.seek_internal(timestamp_ms)
    }
    
    fn get_media_info(&self) -> &MediaInfo {
        &self.media_info
    }
    
    fn video_stream_index(&self) -> Option<usize> {
        self.video_stream_index
    }
    
    fn audio_stream_index(&self) -> Option<usize> {
        self.audio_stream_index
    }
    
    fn subtitle_stream_index(&self) -> Option<usize> {
        self.subtitle_stream_index
    }
    
    fn is_seekable(&self) -> bool {
        // 本地文件和大多数网络流都支持 seek
        true
    }
    
    fn description(&self) -> String {
        format!("FFmpeg Demuxer: {}", self.source_path)
    }
}

