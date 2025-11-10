use crate::core::{AudioFrame, PixelFormat, SampleFormat, SubtitleFrame, VideoFrame, Result};
use crate::player::hw_decoder::HWVideoDecoder;
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{codec, format, software, util};
use log::{debug, error, info, warn};
use std::ffi::CStr;
use ffmpeg_next::ffi::AVSubtitleType;

/// 视频解码器（支持硬件加速和软件解码）
pub struct VideoDecoder {
    inner: DecoderType,
}

/// 解码器内部类型
enum DecoderType {
    Hardware(HWVideoDecoder),
    Software(SoftwareVideoDecoder),
}

/// 软件视频解码器
struct SoftwareVideoDecoder {
    decoder: codec::decoder::Video,
    scaler: Option<software::scaling::Context>,
    time_base: f64,
}

// SwsContext 本身不是 Send，但我们确保只在单个线程中使用它
// 这是安全的，因为每个解码器实例只会在一个线程中使用
unsafe impl Send for SoftwareVideoDecoder {}

impl VideoDecoder {
    /// 从视频流创建解码器（自动选择硬件加速，失败则使用软件解码）
    pub fn from_stream(stream: format::stream::Stream) -> Result<Self> {
        info!("创建视频解码器（优先硬件加速）...");
        
        // 尝试硬件解码
        // 注意：HWVideoDecoder::from_stream_auto 会消耗 stream 的所有权
        // 如果硬件解码失败，我们需要重新获取流
        match HWVideoDecoder::from_stream_auto(stream) {
            Ok(hw_decoder) => {
                info!("✓ 使用硬件解码: {}", hw_decoder.info());
                Ok(Self {
                    inner: DecoderType::Hardware(hw_decoder),
                })
            }
            Err(e) => {
                // 硬件解码失败，返回错误
                // 调用者需要使用 from_stream_software 重试
                Err(e)
            }
        }
    }

    /// 强制使用软件解码
    pub fn from_stream_software(stream: format::stream::Stream) -> Result<Self> {
        info!("创建软件视频解码器...");
        let sw_decoder = SoftwareVideoDecoder::from_stream(stream)?;
        Ok(Self {
            inner: DecoderType::Software(sw_decoder),
        })
    }

    /// 解码数据包
    pub fn decode(&mut self, packet: &ffmpeg::Packet) -> Result<Vec<VideoFrame>> {
        match &mut self.inner {
            DecoderType::Hardware(decoder) => decoder.decode(packet),
            DecoderType::Software(decoder) => decoder.decode(packet),
        }
    }

    /// 刷新解码器（获取缓冲的帧）
    pub fn flush(&mut self) -> Result<Vec<VideoFrame>> {
        match &mut self.inner {
            DecoderType::Hardware(decoder) => decoder.flush(),
            DecoderType::Software(decoder) => decoder.flush(),
        }
    }

    /// 获取解码器类型信息
    pub fn info(&self) -> String {
        match &self.inner {
            DecoderType::Hardware(decoder) => decoder.info(),
            DecoderType::Software(_) => "软件解码".to_string(),
        }
    }

    /// 是否使用硬件加速
    pub fn is_hardware_accelerated(&self) -> bool {
        matches!(self.inner, DecoderType::Hardware(_))
    }
}

// ============= 软件解码器实现 =============

impl SoftwareVideoDecoder {
    /// 从视频流创建软件解码器
    fn from_stream(stream: format::stream::Stream) -> Result<Self> {
        let context = codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = context.decoder().video()?;

        let time_base = stream.time_base();
        let time_base = time_base.numerator() as f64 / time_base.denominator() as f64;

        debug!(
            "软件解码器: {}x{}, 格式: {:?}",
            decoder.width(),
            decoder.height(),
            decoder.format()
        );

        Ok(Self {
            decoder,
            scaler: None,
            time_base,
        })
    }

    /// 解码数据包
    fn decode(&mut self, packet: &ffmpeg::Packet) -> Result<Vec<VideoFrame>> {
        let mut frames = Vec::new();

        match self.decoder.send_packet(packet) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => {
                debug!("视频解码器收到 EOF（send_packet），执行 flush 并忽略本次包");
                self.decoder.flush();
                return Ok(frames);
            }
            Err(e) => return Err(e.into()),
        }

        loop {
            let mut decoded_frame = util::frame::Video::empty();
            match self.decoder.receive_frame(&mut decoded_frame) {
                Ok(_) => {
                    if let Some(frame) = self.convert_frame(decoded_frame)? {
                        frames.push(frame);
                    }
                }
                Err(ffmpeg::Error::Other { errno: 11 }) => break, // EAGAIN
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => {
                    // 对于网络流，某些解码错误是可以容忍的
                    warn!("解码错误（已跳过）: {}", e);
                    break;
                }
            }
        }

        Ok(frames)
    }

    /// 刷新解码器
    fn flush(&mut self) -> Result<Vec<VideoFrame>> {
        let mut frames = Vec::new();

        self.decoder.send_eof()?;

        loop {
            let mut decoded_frame = util::frame::Video::empty();
            match self.decoder.receive_frame(&mut decoded_frame) {
                Ok(_) => {
                    if let Some(frame) = self.convert_frame(decoded_frame)? {
                        frames.push(frame);
                    }
                }
                Err(_) => break,
            }
        }

        self.decoder.flush();

        Ok(frames)
    }

    /// 转换帧格式为 RGBA
    fn convert_frame(&mut self, frame: util::frame::Video) -> Result<Option<VideoFrame>> {
        let width = frame.width();
        let height = frame.height();

        // 初始化 scaler（YUV -> RGBA）
        if self.scaler.is_none() {
            self.scaler = Some(
                software::scaling::Context::get(
                    frame.format(),
                    width,
                    height,
                    util::format::Pixel::RGBA,
                    width,
                    height,
                    software::scaling::Flags::BILINEAR,
                )?,
            );
        }

        let mut rgba_frame = util::frame::Video::empty();
        self.scaler.as_mut().unwrap().run(&frame, &mut rgba_frame)?;

        // 计算 PTS（毫秒）
        let pts = if let Some(timestamp) = frame.timestamp() {
            (timestamp as f64 * self.time_base * 1000.0) as i64
        } else {
            0
        };

        // 复制数据到连续内存
        let data_size = (width * height * 4) as usize;
        let mut data = vec![0u8; data_size];

        let stride = rgba_frame.stride(0);
        let frame_data = rgba_frame.data(0);

        for y in 0..height as usize {
            let src_offset = y * stride;
            let dst_offset = y * (width as usize * 4);
            let row_size = width as usize * 4;
            data[dst_offset..dst_offset + row_size]
                .copy_from_slice(&frame_data[src_offset..src_offset + row_size]);
        }

        Ok(Some(VideoFrame {
            pts,
            duration: 0,
            width,
            height,
            format: PixelFormat::RGBA,
            data,
        }))
    }
}

/// 音频解码器
pub struct AudioDecoder {
    decoder: codec::decoder::Audio,
    resampler: Option<software::resampling::Context>,
    time_base: f64,
    target_channels: u16,      // 目标声道数（用于声道转换）
    target_sample_rate: u32,   // 目标采样率
}

impl AudioDecoder {
    /// 从音频流创建解码器（使用默认输出配置）
    pub fn from_stream(stream: format::stream::Stream) -> Result<Self> {
        let context = codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = context.decoder().audio()?;

        let time_base = stream.time_base();
        let time_base = time_base.numerator() as f64 / time_base.denominator() as f64;

        debug!(
            "音频解码器: {} Hz, {} 声道, 格式: {:?}",
            decoder.rate(),
            decoder.channels(),
            decoder.format()
        );

        Ok(Self {
            decoder,
            resampler: None,
            time_base,
            target_channels: 2,      // 默认立体声
            target_sample_rate: 48000, // 默认 48kHz
        })
    }
    
    /// 从音频流创建解码器（指定目标配置）
    pub fn from_stream_with_config(
        stream: format::stream::Stream,
        target_sample_rate: u32,
        target_channels: u16,
    ) -> Result<Self> {
        let context = codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = context.decoder().audio()?;

        let time_base = stream.time_base();
        let time_base = time_base.numerator() as f64 / time_base.denominator() as f64;

        debug!(
            "音频解码器: {} Hz, {} 声道 → 目标: {} Hz, {} 声道",
            decoder.rate(),
            decoder.channels(),
            target_sample_rate,
            target_channels
        );

        Ok(Self {
            decoder,
            resampler: None,
            time_base,
            target_channels,
            target_sample_rate,
        })
    }

    /// 解码数据包
    pub fn decode(&mut self, packet: &ffmpeg::Packet) -> Result<Vec<AudioFrame>> {
        let mut frames = Vec::new();

        match self.decoder.send_packet(packet) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => {
                debug!("音频解码器收到 EOF（send_packet），执行 flush 并忽略本次包");
                self.decoder.flush();
                return Ok(frames);
            }
            Err(e) => return Err(e.into()),
        }

        loop {
            let mut decoded_frame = util::frame::Audio::empty();
            match self.decoder.receive_frame(&mut decoded_frame) {
                Ok(_) => {
                    if let Some(frame) = self.convert_frame(decoded_frame)? {
                        frames.push(frame);
                    }
                }
                Err(ffmpeg::Error::Other { errno: 11 }) => break, // EAGAIN
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }

        Ok(frames)
    }

    /// 刷新解码器（获取缓冲的帧）
    pub fn flush(&mut self) -> Result<Vec<AudioFrame>> {
        let mut frames = Vec::new();

        self.decoder.send_eof()?;

        loop {
            let mut decoded_frame = util::frame::Audio::empty();
            match self.decoder.receive_frame(&mut decoded_frame) {
                Ok(_) => {
                    if let Some(frame) = self.convert_frame(decoded_frame)? {
                        frames.push(frame);
                    }
                }
                Err(_) => break,
            }
        }

        self.decoder.flush();

        Ok(frames)
    }

    /// 转换音频帧为 f32 格式（支持声道转换和重采样）
    fn convert_frame(&mut self, frame: util::frame::Audio) -> Result<Option<AudioFrame>> {
        let source_rate = frame.rate();
        let source_channels = frame.channels();

        // 初始化 resampler（支持声道转换和重采样）
        if self.resampler.is_none() {
            // 计算目标声道布局
            let target_layout = match self.target_channels {
                1 => util::channel_layout::ChannelLayout::MONO,
                2 => util::channel_layout::ChannelLayout::STEREO,
                6 => util::channel_layout::ChannelLayout::_5POINT1,
                _ => util::channel_layout::ChannelLayout::STEREO, // 默认立体声
            };
            
            debug!(
                "🔧 初始化音频重采样器: {}Hz/{}ch → {}Hz/{}ch",
                source_rate, source_channels,
                self.target_sample_rate, self.target_channels
            );
            
            self.resampler = Some(
                software::resampling::Context::get(
                    frame.format(),
                    frame.channel_layout(),
                    source_rate,
                    util::format::Sample::F32(util::format::sample::Type::Packed),
                    target_layout,
                    self.target_sample_rate,
                )?,
            );
        }

        let mut resampled = util::frame::Audio::empty();
        self.resampler
            .as_mut()
            .unwrap()
            .run(&frame, &mut resampled)?;

        // 计算 PTS（毫秒）
        let pts = if let Some(timestamp) = frame.timestamp() {
            (timestamp as f64 * self.time_base * 1000.0) as i64
        } else {
            0
        };

        // 复制音频数据（使用目标声道数）
        let samples = resampled.samples();
        let data_size = samples * self.target_channels as usize;
        let mut data = vec![0f32; data_size];

        let frame_data = resampled.data(0);
        let byte_slice = unsafe {
            std::slice::from_raw_parts(frame_data.as_ptr() as *const f32, data_size)
        };
        data.copy_from_slice(byte_slice);

        Ok(Some(AudioFrame {
            pts,
            sample_rate: self.target_sample_rate,
            channels: self.target_channels,
            format: SampleFormat::F32,
            data,
        }))
    }
}

/// 字幕解码器
pub struct SubtitleDecoder {
    decoder: codec::decoder::Subtitle,
    time_base: f64,
}

impl SubtitleDecoder {
    /// 从字幕流创建解码器
    pub fn from_stream(stream: format::stream::Stream) -> Result<Self> {
        let context = codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = context.decoder().subtitle()?;

        let tb = stream.time_base();
        let time_base = tb.numerator() as f64 / tb.denominator() as f64;

        debug!("字幕解码器初始化: time_base = {}", time_base);

        Ok(Self { decoder, time_base })
    }

    /// 解码数据包 → 输出 0~n 条字幕帧
    pub fn decode(&mut self, packet: &ffmpeg::Packet) -> Result<Vec<SubtitleFrame>> {
        let mut frames = Vec::new();
        let mut subtitle = ffmpeg::codec::subtitle::Subtitle::default();

        if let Err(e) = self.decoder.decode(packet, &mut subtitle) {
            // EAGAIN 时不视为错误
            if !matches!(e, ffmpeg::Error::Other { errno: 11 }) {
                error!("字幕解码失败: {}", e);
                return Err(e.into());
            }
            return Ok(frames);
        }

        // 计算 PTS（毫秒）
        let pts = subtitle.pts().unwrap_or(0) as f64 * self.time_base * 1000.0;
        let start_pts = pts as i64;

        // 尝试从 FFmpeg subtitle 获取结束时间
        // AVSubtitle 结构中有 end_display_time 字段（以毫秒为单位）
        let duration = unsafe {
            let raw_subtitle = subtitle.as_ptr();
            let end_display_time_ms = (*raw_subtitle).end_display_time;
            if end_display_time_ms > 0 {
                end_display_time_ms as i64
            } else {
                3000 // 默认 3 秒
            }
        };
        let end_pts = start_pts + duration;

        // 解析字幕内容
        let mut text = String::new();

        for rect in subtitle.rects() {
            unsafe {
                let raw = rect.as_ptr();
                match (*raw).type_ {
                    AVSubtitleType::SUBTITLE_TEXT => {
                        if !(*raw).text.is_null() {
                            let s = CStr::from_ptr((*raw).text).to_string_lossy().into_owned();
                            text.push_str(&s);
                            text.push('\n');
                        }
                    }
                    AVSubtitleType::SUBTITLE_ASS => {
                        if !(*raw).ass.is_null() {
                            let s = CStr::from_ptr((*raw).ass).to_string_lossy().into_owned();
                            text.push_str(&s);
                            text.push('\n');
                        }
                    }
                    AVSubtitleType::SUBTITLE_BITMAP => {
                        // TODO: 后续可处理位图字幕
                        debug!("跳过位图字幕（当前仅支持文本字幕）");
                    }
                    _ => {}
                }
            }
        }
        
        // ✅ 必须释放 FFmpeg subtitle，否则泄漏
        unsafe {
            ffmpeg_next::ffi::avsubtitle_free(subtitle.as_mut_ptr());
        }

        if !text.trim().is_empty() {
            frames.push(SubtitleFrame {
                pts: start_pts,
                duration,
                end_pts,
                text: Self::clean_subtitle_text(&text),
            });
        }

        Ok(frames)
    }

    /// 清理字幕文本：移除 ASS 标签、格式化换行
    /// 
    /// 支持的清理功能：
    /// - 移除 ASS/SSA 标签（如 {\an8}, {\pos(100,200)}, {\r} 等）
    /// - 处理换行符（\N, \n）
    /// - 移除控制字符
    /// - 规范化空白字符
    fn clean_subtitle_text(text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }

        let mut result = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        let mut in_ass_tag = false;

        while let Some(ch) = chars.next() {
            match ch {
                '{' => {
                    // ASS 标签开始
                    in_ass_tag = true;
                }
                '}' => {
                    // ASS 标签结束
                    in_ass_tag = false;
                }
                '<' => {
                    // 可能是简单标签 <i>, <b>, <u>, <font> 等
                    if !in_ass_tag {
                        // 检查是否是标签
                        let mut tag_chars = vec![ch];
                        let mut found_tag = false;
                        while let Some(&next_ch) = chars.peek() {
                            if next_ch == '>' {
                                tag_chars.push(chars.next().unwrap());
                                found_tag = true;
                                break;
                            } else if next_ch.is_whitespace() || next_ch == '/' {
                                tag_chars.push(chars.next().unwrap());
                            } else if next_ch.is_ascii_alphabetic() {
                                tag_chars.push(chars.next().unwrap());
                            } else {
                                break;
                            }
                        }
                        if !found_tag {
                            // 不是标签，是普通字符
                            result.push(ch);
                        }
                        // 标签已跳过
                    } else {
                        result.push(ch);
                    }
                }
                '\\' => {
                    // 处理转义序列
                    if !in_ass_tag {
                        match chars.peek() {
                            Some(&'N') => {
                                chars.next();
                                result.push('\n');
                            }
                            Some(&'n') => {
                                chars.next();
                                result.push('\n');
                            }
                            Some(&'r') => {
                                chars.next();
                                // 忽略 \r
                            }
                            Some(&'t') => {
                                chars.next();
                                result.push('\t');
                            }
                            _ => {
                                // 其他转义序列，保留反斜杠
                                result.push(ch);
                            }
                        }
                    } else {
                        result.push(ch);
                    }
                }
                '\r' => {
                    // 移除回车符（保留换行符）
                    // 不做任何处理
                }
                _ch if in_ass_tag => {
                    // ASS 标签内部内容，忽略
                }
                _ => {
                    result.push(ch);
                }
            }
        }

        // 规范化空白字符和换行
        result = result
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n");

        // 移除多余的空白行（超过2个连续换行）
        let mut normalized = String::with_capacity(result.len());
        let mut consecutive_newlines = 0;
        for ch in result.chars() {
            if ch == '\n' {
                consecutive_newlines += 1;
                if consecutive_newlines <= 2 {
                    normalized.push(ch);
                }
            } else {
                consecutive_newlines = 0;
                normalized.push(ch);
            }
        }

        normalized.trim().to_string()
    }
}

