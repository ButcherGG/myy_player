use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 媒体源类型
#[derive(Debug, Clone)]
pub enum MediaSource {
    /// 本地文件路径
    LocalFile(PathBuf),
    
    /// 网络流 URL
    NetworkStream {
        url: String,
        protocol: StreamProtocol,
    },
}

impl MediaSource {
    /// 从 URL 字符串解析媒体源
    pub fn from_url(url: &str) -> anyhow::Result<Self> {
        if url.starts_with("rtsp://") {
            Ok(MediaSource::NetworkStream {
                url: url.to_string(),
                protocol: StreamProtocol::RTSP,
            })
        } else if url.starts_with("rtmp://") {
            Ok(MediaSource::NetworkStream {
                url: url.to_string(),
                protocol: StreamProtocol::RTMP,
            })
        } else if url.ends_with(".m3u8") || url.contains("/hls/") {
            Ok(MediaSource::NetworkStream {
                url: url.to_string(),
                protocol: StreamProtocol::HLS,
            })
        } else if url.starts_with("http://") || url.starts_with("https://") {
            Ok(MediaSource::NetworkStream {
                url: url.to_string(),
                protocol: StreamProtocol::HTTP,
            })
        } else {
            // 默认当作本地文件
            Ok(MediaSource::LocalFile(PathBuf::from(url)))
        }
    }
    
    /// 判断是否为网络流
    pub fn is_network_stream(&self) -> bool {
        matches!(self, MediaSource::NetworkStream { .. })
    }
}

/// 流媒体协议类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamProtocol {
    /// RTSP - 实时流协议（监控摄像头）
    RTSP,
    /// RTMP - 实时消息协议（直播流）
    RTMP,
    /// HLS - HTTP Live Streaming
    HLS,
    /// HTTP - 普通 HTTP 流
    HTTP,
}

impl StreamProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            StreamProtocol::RTSP => "RTSP",
            StreamProtocol::RTMP => "RTMP",
            StreamProtocol::HLS => "HLS",
            StreamProtocol::HTTP => "HTTP",
        }
    }
}

/// 网络流连接状态
#[derive(Debug, Clone, PartialEq)]
pub enum StreamState {
    /// 未连接
    Disconnected,
    
    /// 连接中
    Connecting,
    
    /// 已连接，缓冲中
    Buffering { 
        progress: f32  // 0.0 - 1.0
    },
    
    /// 播放中
    Playing,
    
    /// 重新连接中
    Reconnecting { 
        attempt: u32 
    },
    
    /// 连接失败
    Failed { 
        reason: String 
    },
}

/// 缓冲状态信息（用于监控和调试）
#[derive(Debug, Clone, Default)]
pub struct BufferStatus {
    /// 视频数据包队列长度
    pub video_packets: usize,
    
    /// 音频数据包队列长度
    pub audio_packets: usize,
    
    /// 视频帧队列长度
    pub video_frames: usize,
    
    /// 音频帧队列长度
    pub audio_frames: usize,
    
    /// 是否正在缓冲
    pub is_buffering: bool,
    
    /// 缓冲进度 (0.0 - 1.0)
    pub buffer_progress: f32,
}

/// 像素格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    RGBA,
    RGB,
    YUV420P,
    NV12,
}

/// 音频采样格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    I16,
}

/// 视频帧数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrame {
    pub pts: i64,           // 显示时间戳（毫秒）
    pub duration: i64,      // 帧持续时间（毫秒）
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,      // CPU 内存数据
}

/// 音频帧数据
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub pts: i64,           // 显示时间戳（毫秒）
    pub sample_rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    pub data: Vec<f32>,     // 统一使用 f32 格式
}

/// 字幕帧数据
#[derive(Debug, Clone)]
pub struct SubtitleFrame {
    pub pts: i64,           // 开始显示时间戳（毫秒）
    pub duration: i64,      // 显示持续时间（毫秒）
    pub text: String,        // 字幕文本
    pub end_pts: i64,       // 结束显示时间戳（毫秒）
}

/// 播放状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    Idle,
    Opening,
    Playing,
    Paused,
    Seeking,
    Buffering,
    Stopped,
    Error,
}

/// 媒体信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration: i64,          // 总时长（毫秒）
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    pub audio_codec: String,
    pub sample_rate: u32,
    pub channels: u16,
}

impl Default for MediaInfo {
    fn default() -> Self {
        Self {
            duration: 0,
            width: 0,
            height: 0,
            fps: 0.0,
            video_codec: String::new(),
            audio_codec: String::new(),
            sample_rate: 0,
            channels: 0,
        }
    }
}

/// 播放器状态信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerState {
    pub state: PlaybackState,
    pub position: i64,          // 当前位置（毫秒）
    pub duration: i64,          // 总时长（毫秒）
    pub volume: f32,            // 音量 0.0 - 1.0
    pub media_info: Option<MediaInfo>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            state: PlaybackState::Idle,
            position: 0,
            duration: 0,
            volume: 1.0,
            media_info: None,
        }
    }
}

