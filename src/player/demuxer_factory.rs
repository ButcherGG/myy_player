use crate::core::MediaSource;
use crate::player::Demuxer;
use crossbeam_channel::Sender;
use log::{error, info};
use std::thread;

/// Demuxer 创建结果
pub enum DemuxerCreationResult {
    /// 创建成功
    Success {
        demuxer: Demuxer,  // 改为具体类型
        url: String,
    },
    /// 创建失败
    Failed {
        url: String,
        error: String,
    },
}

/// Demuxer 工厂 - 负责异步创建 Demuxer
/// 
/// 使用方法：
/// ```
/// let (tx, rx) = unbounded();
/// DemuxerFactory::create_async(source, tx);
/// 
/// // 在 update() 中接收结果
/// if let Ok(result) = rx.try_recv() {
///     match result {
///         DemuxerCreationResult::Success { demuxer, .. } => {
///             manager.attach_demuxer(demuxer)?;
///         }
///         DemuxerCreationResult::Failed { error, .. } => {
///             error!("创建失败: {}", error);
///         }
///     }
/// }
/// ```
pub struct DemuxerFactory;

impl DemuxerFactory {
    /// 异步创建 Demuxer（在子线程中）
    /// 
    /// 参数：
    /// - source: 媒体源
    /// - result_tx: 结果发送通道
    pub fn create_async(
        source: MediaSource,
        result_tx: Sender<DemuxerCreationResult>,
    ) {
        thread::spawn(move || {
            info!("🔨 开始在子线程中创建 Demuxer");
            
            let result = match source {
                MediaSource::LocalFile(path) => {
                    let path_str = path.to_string_lossy().to_string();
                    info!("📁 创建本地文件 Demuxer: {}", path_str);
                    
                    match Demuxer::open(&path_str) {
                        Ok(demuxer) => DemuxerCreationResult::Success {
                            demuxer,  // 直接返回，不装箱
                            url: path_str,
                        },
                        Err(e) => DemuxerCreationResult::Failed {
                            url: path_str,
                            error: e.to_string(),
                        },
                    }
                }
                MediaSource::NetworkStream { url, protocol } => {
                    info!("🌐 创建网络流 Demuxer: {} ({})", url, protocol.as_str());
                    
                    // 网络流的耗时操作在这里执行
                    match Demuxer::open(&url) {
                        Ok(demuxer) => DemuxerCreationResult::Success {
                            demuxer,  // 直接返回，不装箱
                            url: url.clone(),
                        },
                        Err(e) => DemuxerCreationResult::Failed {
                            url: url.clone(),
                            error: e.to_string(),
                        },
                    }
                }
            };
            
            // 发送结果
            if let Err(e) = result_tx.send(result) {
                error!("❌ 发送 Demuxer 创建结果失败: {}", e);
            } else {
                info!("✅ Demuxer 创建结果已发送");
            }
        });
    }
}

