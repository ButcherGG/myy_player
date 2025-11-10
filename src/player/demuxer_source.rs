use crate::core::{MediaInfo, Result};
use ffmpeg_next as ffmpeg;
use ffmpeg::Packet;

/// Packet 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Video,
    Audio,
    Subtitle,
}

/// 媒体包（可跨线程传递）
pub struct MediaPacket {
    pub packet: Packet,
    pub packet_type: PacketType,
    pub stream_index: usize,
}

// 实现 Send，允许跨线程传递
unsafe impl Send for MediaPacket {}

/// Demuxer 数据源抽象接口
/// 
/// 这个 trait 定义了所有 Demuxer 实现必须提供的方法
/// 不同的媒体源（本地文件、网络流、内存流等）可以实现这个接口
pub trait DemuxerSource: Send {
    /// 读取下一个媒体包
    /// 
    /// 返回：
    /// - Ok(Some(packet)): 成功读取一个包
    /// - Ok(None): 到达文件末尾
    /// - Err(e): 读取错误
    fn read_packet(&mut self) -> Result<Option<MediaPacket>>;
    
    /// Seek 到指定位置（毫秒）
    fn seek(&mut self, timestamp_ms: i64) -> Result<()>;
    
    /// 获取媒体信息
    fn get_media_info(&self) -> &MediaInfo;
    
    /// 获取视频流索引
    fn video_stream_index(&self) -> Option<usize>;
    
    /// 获取音频流索引
    fn audio_stream_index(&self) -> Option<usize>;
    
    /// 获取字幕流索引
    fn subtitle_stream_index(&self) -> Option<usize>;
    
    /// 是否支持 seek
    fn is_seekable(&self) -> bool {
        true
    }
    
    /// 获取描述信息（用于调试）
    fn description(&self) -> String;
}

