// 播放器核心模块

pub mod demuxer;
pub mod demuxer_source;  // 新增：Demuxer 抽象接口
pub mod demuxer_thread;  // 新增：Demuxer 线程管理
pub mod demuxer_factory; // 新增：Demuxer 工厂（异步创建）
pub mod decoder;
pub mod hw_decoder;
// pub mod renderer;  // 暂时注释，后续版本实现
pub mod audio_output;
pub mod manager;
pub mod external_subtitle;
pub mod network_stream;

pub use demuxer::Demuxer;
// pub use demuxer_source::{DemuxerSource, MediaPacket, PacketType};  // 导出接口（暂时未使用，如需要可取消注释）
pub use demuxer_thread::DemuxerThread;  // 导出线程管理
pub use demuxer_factory::{DemuxerFactory, DemuxerCreationResult};  // 导出工厂
pub use decoder::{VideoDecoder, AudioDecoder, SubtitleDecoder};
// pub use renderer::Renderer;
pub use audio_output::AudioOutput;
// pub use manager::PlaybackManager;
pub use external_subtitle::ExternalSubtitleParser;
pub use network_stream::NetworkStreamManager;

