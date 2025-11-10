use crate::core::{AudioFrame, MediaInfo, PlaybackClock, PlaybackState, PlayerState, Result, SubtitleFrame, VideoFrame};
use crate::core::{MediaSource, StreamProtocol, StreamState};
use crate::player::{AudioDecoder, AudioOutput, Demuxer, SubtitleDecoder, VideoDecoder, ExternalSubtitleParser};
use crate::player::NetworkStreamManager;
use crossbeam::queue::SegQueue;
use crossbeam_channel::{Receiver, Sender, unbounded};
use ffmpeg_next as ffmpeg;
use log::{debug, error, info, warn};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::thread;
use std::time::{Duration, Instant};
use std::process;

fn log_ctx() -> String {
    format!("[pid:{}-tid:{:?}]", process::id(), thread::current().id())
}

/// 播放管理器 - 整体控制播放流程
pub struct PlaybackManager {
    state: Arc<Mutex<PlayerState>>,
    clock: PlaybackClock,
    running: Arc<AtomicBool>,
    is_first_audio_frame: Arc<AtomicBool>,  // 跟踪是否是第一个音频帧
    seek_position: Arc<Mutex<Option<(i64, Instant)>>>,  // Seek 目标位置和时间戳（用于防止首次音频帧覆盖时钟）
    need_flush_decoders: Arc<AtomicBool>,  // 标记是否需要 flush 解码器（Seek 后使用）
    current_file_path: Arc<Mutex<Option<String>>>,  // 当前打开的文件路径（用于停止后重新播放）
    demux_thread: Option<thread::JoinHandle<()>>,
    video_decode_thread: Option<thread::JoinHandle<()>>,
    audio_decode_thread: Option<thread::JoinHandle<()>>,
    audio_output: Option<AudioOutput>,
    audio_frame_queue: Arc<SegQueue<AudioFrame>>,
    video_frame_queue: Arc<SegQueue<VideoFrame>>,
    subtitle_frame_queue: Arc<SegQueue<SubtitleFrame>>,  // 字幕帧队列
    subtitle_decode_thread: Option<thread::JoinHandle<()>>,  // 字幕解码线程
    external_subtitle_frames: Arc<Mutex<Vec<SubtitleFrame>>>,  // 外部字幕帧缓存
    seek_tx: Option<Sender<i64>>,  // Seek 命令发送端
    
    // 网络流支持
    network_stream: Option<NetworkStreamManager>,  // 网络流管理器
    stream_state: Arc<RwLock<Option<StreamState>>>,  // 网络流状态（供 UI 读取）
    is_network_source: Arc<AtomicBool>,  // 标记当前是否为网络源（用于动态调整缓冲策略）
    
    // 新架构：DemuxerThread（用于网络流异步处理）
    demuxer_thread_handle: Option<crate::player::DemuxerThread>,  // 保存 DemuxerThread，防止被 drop
}

impl PlaybackManager {
    pub fn new() -> Self {
        info!("{} 🎮 创建播放管理器...", log_ctx());
        let manager = Self {
            state: Arc::new(Mutex::new(PlayerState::default())),
            clock: PlaybackClock::new(),
            running: Arc::new(AtomicBool::new(false)),
            is_first_audio_frame: Arc::new(AtomicBool::new(true)),
            seek_position: Arc::new(Mutex::new(None)),
            need_flush_decoders: Arc::new(AtomicBool::new(false)),
            current_file_path: Arc::new(Mutex::new(None)),
            demux_thread: None,
            video_decode_thread: None,
            audio_decode_thread: None,
            audio_output: None,
            audio_frame_queue: Arc::new(SegQueue::new()),
            video_frame_queue: Arc::new(SegQueue::new()),
            subtitle_frame_queue: Arc::new(SegQueue::new()),
            subtitle_decode_thread: None,
            external_subtitle_frames: Arc::new(Mutex::new(Vec::new())),
            seek_tx: None,
            network_stream: None,
            stream_state: Arc::new(RwLock::new(None)),
            is_network_source: Arc::new(AtomicBool::new(false)),
            demuxer_thread_handle: None,
        };
        info!("{} ✅ 播放管理器创建完成", log_ctx());
        manager
    }

    /// 打开媒体文件
    pub fn open_file(&mut self, path: &str) -> Result<MediaInfo> {
        self.open(path.to_string())
    }

    /// 打开媒体源（文件或网络流）
    pub fn open_media_source(&mut self, source: MediaSource) -> Result<MediaInfo> {
        match source {
            MediaSource::LocalFile(path) => {
                self.open(path.to_string_lossy().to_string())
            }
            MediaSource::NetworkStream { url, protocol } => {
                self.open_stream(&url, protocol)
            }
        }
    }
    
    /// 使用已创建的 Demuxer 启动播放（新架构）
    /// 
    /// 这个方法接收外部创建的 Demuxer，避免在主线程中阻塞创建过程
    /// 
    /// 参数：
    /// - demuxer: 已创建的 Demuxer（通常在子线程中创建）
    /// 
    /// 返回：
    /// - MediaInfo: 媒体信息
    pub fn attach_demuxer(&mut self, demuxer: crate::player::Demuxer) -> Result<MediaInfo> {
        info!("{} 📎 附加 Demuxer", log_ctx());
        
        // 停止当前播放
        self.stop();
        
        // 获取媒体信息
        let media_info = demuxer.get_media_info()?;
        
        // 判断是否为网络源（根据路径判断）
        let source_path = demuxer.description();
        let is_network = source_path.contains("rtsp://") 
            || source_path.contains("rtmp://")
            || source_path.contains("http://")
            || source_path.contains("https://");
        self.is_network_source.store(is_network, Ordering::SeqCst);
        
        // 重置首次音频帧标志
        self.is_first_audio_frame.store(true, Ordering::SeqCst);
        
        // 重置 seek 位置
        {
            let mut seek_pos = self.seek_position.lock().unwrap();
            *seek_pos = None;
        }
        
        // 更新状态
        {
            let mut state = self.state.lock().unwrap();
            state.state = PlaybackState::Opening;
            state.duration = media_info.duration;
            state.media_info = Some(media_info.clone());
        }
        
        info!("{} 媒体信息: {:?}", log_ctx(), media_info);
        
        // 创建视频解码器（自动选择硬件加速）
        let video_decoder = if let Some(stream) = demuxer.video_stream() {
            let decoder = match VideoDecoder::from_stream(stream) {
                Ok(decoder) => {
                    info!("视频解码器: {}", decoder.info());
                    if decoder.is_hardware_accelerated() {
                        info!("✓ 硬件加速已启用");
                    }
                    decoder
                }
                Err(e) => {
                    info!("硬件解码不可用: {}, 回退到软件解码", e);
                    let stream = demuxer.video_stream().unwrap();
                    let decoder = VideoDecoder::from_stream_software(stream)?;
                    info!("✓ 使用软件解码");
                    decoder
                }
            };
            Some(decoder)
        } else {
            None
        };
        
        // 创建音频输出（先创建，获取实际配置）
        self.audio_output = if media_info.audio_codec != "none" {
            match AudioOutput::new(media_info.sample_rate, media_info.channels) {
                Ok(mut output) => {
                    output.start()?;
                    Some(output)
                }
                Err(e) => {
                    error!("{} 创建音频输出失败: {}", log_ctx(), e);
                    None
                }
            }
        } else {
            None
        };
        
        // 获取音频输出的实际配置（用于解码器）
        let (actual_sample_rate, actual_channels) = if let Some(ref output) = self.audio_output {
            output.get_config()
        } else {
            (48000, 2) // 默认配置
        };
        
        // 创建音频解码器（使用音频输出的实际配置）
        let audio_decoder = if let Some(stream) = demuxer.audio_stream() {
            Some(AudioDecoder::from_stream_with_config(
                stream,
                actual_sample_rate,
                actual_channels,
            )?)
        } else {
            None
        };
        
        // 创建字幕解码器
        let subtitle_decoder = if let Some(stream) = demuxer.subtitle_stream() {
            match SubtitleDecoder::from_stream(stream) {
                Ok(decoder) => {
                    info!("{} 字幕解码器创建成功", log_ctx());
                    Some(decoder)
                }
                Err(e) => {
                    warn!("{} 创建字幕解码器失败: {}，继续播放（无字幕）", log_ctx(), e);
                    None
                }
            }
        } else {
            None
        };
        
        // 启动播放线程
        self.start_playback_threads(
            demuxer,
            video_decoder,
            audio_decoder,
            subtitle_decoder,
        );
        
        // 更新状态为暂停
        {
            let mut state = self.state.lock().unwrap();
            state.state = PlaybackState::Paused;
        }
        
        Ok(media_info)
    }
    
    /// 使用已创建的 Demuxer 启动播放（网络流专用 - 使用 DemuxerThread 异步模式）
    /// 
    /// 这个方法专门用于网络流，使用 DemuxerThread 在独立线程中运行 Demuxer
    /// 
    /// 参数：
    /// - demuxer: 已创建的 Demuxer（通常在子线程中创建）
    /// 
    /// 返回：
    /// - MediaInfo: 媒体信息
    pub fn attach_demuxer_async(&mut self, demuxer: crate::player::Demuxer) -> Result<MediaInfo> {
        use crate::player::DemuxerThread;
        
        info!("{} 📎 附加 Demuxer（异步模式 - 网络流）", log_ctx());
        
            // 停止当前播放（注意 stop 应该能停止所有线程并 join）
    self.stop();

    // 获取媒体信息
    let media_info = demuxer.get_media_info()?;

    // 标记为网络源
    self.is_network_source.store(true, Ordering::SeqCst);
    // 重置首次音频帧标志
    self.is_first_audio_frame.store(true, Ordering::SeqCst);
    // 重置 seek 位置
    {
        let mut seek_pos = self.seek_position.lock().unwrap();
        *seek_pos = None;
    }

    // 更新状态（Opening）
    {
        let mut state = self.state.lock().unwrap();
        state.state = PlaybackState::Opening;
        state.duration = media_info.duration;
        state.media_info = Some(media_info.clone());
    }

    info!("{} 📎 媒体信息: {:?}", log_ctx(), media_info);

    // 创建解码器（保持你现有逻辑）
    let video_decoder = if let Some(stream) = demuxer.video_stream() {
        let decoder = match VideoDecoder::from_stream(stream) {
            Ok(decoder) => {
                info!("{} 📎 视频解码器: {}", log_ctx(), decoder.info());
                if decoder.is_hardware_accelerated() {
                    info!("{} ✓ 硬件加速已启用", log_ctx());
                }
                decoder
            }
            Err(e) => {
                info!("{} 硬件解码不可用: {}, 回退到软件解码", log_ctx(), e);
                let stream = demuxer.video_stream().unwrap();
                let decoder = VideoDecoder::from_stream_software(stream)?;
                info!("{} ✓ 使用软件解码", log_ctx());
                decoder
            }
        };
        Some(decoder)
    } else {
        None
    };

    // 创建音频输出
    self.audio_output = if media_info.audio_codec != "none" {
        match AudioOutput::new(media_info.sample_rate, media_info.channels) {
            Ok(mut output) => {
                output.start()?;
                Some(output)
            }
            Err(e) => {
                error!("{} ❌ 创建音频输出失败: {}", log_ctx(), e);
                None
            }
        }
    } else {
        None
    };

    // 获取实际音频输出配置
    let (actual_sample_rate, actual_channels) = if let Some(ref output) = self.audio_output {
        output.get_config()
    } else {
        (48000, 2)
    };

    // 创建音频解码器
    let audio_decoder = if let Some(stream) = demuxer.audio_stream() {
        Some(AudioDecoder::from_stream_with_config(stream, actual_sample_rate, actual_channels)?)
    } else {
        None
    };

    // 创建字幕解码器（保持原逻辑）
    let subtitle_decoder = if let Some(stream) = demuxer.subtitle_stream() {
        match SubtitleDecoder::from_stream(stream) {
            Ok(decoder) => {
                info!("{} 📎 字幕解码器创建成功", log_ctx());
                Some(decoder)
            }
            Err(e) => {
                warn!("{} ❌ 创建字幕解码器失败: {}，继续播放（无字幕）", log_ctx(), e);
                None
            }
        }
    } else {
        None
    };

    // 启动 DemuxerThread（使用新实现）
    info!("{} 🚀 启动 DemuxerThread", log_ctx());
    let demuxer_thread = DemuxerThread::start(Box::new(demuxer));

    // 启动播放线程（使用 DemuxerThread）
    self.start_playback_threads_with_demuxer_thread(
        demuxer_thread,
        video_decoder,
        audio_decoder,
        subtitle_decoder,
    );

    // 进入缓冲阶段（Buffering），直到 packet 队列满足阈值或超时
    {
        let mut state = self.state.lock().unwrap();
        state.state = PlaybackState::Buffering;
    }

    // 缓冲目标：可根据网络/分辨率动态调整。这里使用 packet 数量阈值示例。
    const TARGET_VIDEO_PACKETS: usize = 40; // 例如约 1-2 秒数据，需自行调试
    const TARGET_AUDIO_PACKETS: usize = 80;
    const BUFFER_TIMEOUT_MS: u64 = 8000; // 最长等待 8 秒

    let start = Instant::now();
    let mut buffered = false;

    // 获取 Receiver.len() 方法（crossbeam::channel::Receiver 有 len()）
    while start.elapsed() < Duration::from_millis(BUFFER_TIMEOUT_MS) {
        if let Some(ref demux_thread) = self.demuxer_thread_handle {
            let vlen = demux_thread.video_packet_queue.as_ref().map(|r| r.len()).unwrap_or(0);
            let alen = demux_thread.audio_packet_queue.as_ref().map(|r| r.len()).unwrap_or(0);
            if vlen >= TARGET_VIDEO_PACKETS && alen >= TARGET_AUDIO_PACKETS {
                buffered = true;
                break;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }

    if buffered {
        info!("{} ✅ 缓冲完成：开始播放", log_ctx());
    } else {
        warn!("{} ❌ 缓冲超时（{}ms），将尽量开始播放以避免长时间等待", log_ctx(), BUFFER_TIMEOUT_MS);
    }

    // 将状态设为 Paused（与原逻辑一致），外部 UI 可以触发 Play
    {
        let mut state = self.state.lock().unwrap();
        state.state = PlaybackState::Paused;
    }

    Ok(media_info)
    }

    /// 打开媒体文件
    pub fn open(&mut self, path: String) -> Result<MediaInfo> {
        info!("{} � 打开媒体文件: {}", log_ctx(), path);

        // 停止当前播放
        self.stop();
        
        // 标记为本地文件（非网络源）
        self.is_network_source.store(false, Ordering::SeqCst);
        
        // 重置首次音频帧标志
        self.is_first_audio_frame.store(true, Ordering::SeqCst);
        
        // 重置 seek 位置（避免旧文件的 seek 位置影响新文件）
        {
            let mut seek_pos = self.seek_position.lock().unwrap();
            *seek_pos = None;
        }

        // 更新状态
        {
            let mut state = self.state.lock().unwrap();
            state.state = PlaybackState::Opening;
        }

        // 保存文件路径（用于停止后重新播放）
        {
            let mut file_path = self.current_file_path.lock().unwrap();
            *file_path = Some(path.clone());
        }
        
        // 打开解封装器
        let demuxer = Demuxer::open(&path)?;
        let media_info = demuxer.get_media_info()?;

        info!("{} 📎 媒体信息: {:?}", log_ctx(), media_info);

        // 更新状态
        {
            let mut state = self.state.lock().unwrap();
            state.duration = media_info.duration;
            state.media_info = Some(media_info.clone());
            state.state = PlaybackState::Paused;
        }

        // 创建视频解码器（自动选择硬件加速）
        let video_decoder = if let Some(stream) = demuxer.video_stream() {
            // 先尝试硬件解码
            let decoder = match VideoDecoder::from_stream(stream) {
                Ok(decoder) => {
            info!("{} 📎 视频解码器: {}", log_ctx(), decoder.info());
            if decoder.is_hardware_accelerated() {
                info!("{} ✓ 硬件加速已启用", log_ctx());
                    }
                    decoder
                }
                Err(e) => {
                    info!("{} 硬件解码不可用: {}, 回退到软件解码", log_ctx(), e);
                    // 硬件解码失败，使用软件解码
                    let stream = demuxer.video_stream().unwrap();
                    let decoder = VideoDecoder::from_stream_software(stream)?;
                    info!("{} ✓ 使用软件解码", log_ctx());
                    decoder
                }
            };
            Some(decoder)
        } else {
            None
        };

        // 创建音频输出（先创建，获取实际配置）
        self.audio_output = if media_info.audio_codec != "none" {
            match AudioOutput::new(media_info.sample_rate, media_info.channels) {
                Ok(mut output) => {
                    output.start()?;
                    Some(output)
                }
                Err(e) => {
                    error!("{} ❌ 创建音频输出失败: {}", log_ctx(), e);
                    None
                }
            }
        } else {
            None
        };
        
        // 获取音频输出的实际配置（用于解码器）
        let (actual_sample_rate, actual_channels) = if let Some(ref output) = self.audio_output {
            output.get_config()
        } else {
            (48000, 2) // 默认配置
        };

        // 创建音频解码器（使用音频输出的实际配置）
        let audio_decoder = if let Some(stream) = demuxer.audio_stream() {
            Some(AudioDecoder::from_stream_with_config(
                stream,
                actual_sample_rate,
                actual_channels,
            )?)
        } else {
            None
        };

        // 创建字幕解码器
        let subtitle_decoder = if let Some(stream) = demuxer.subtitle_stream() {
            match SubtitleDecoder::from_stream(stream) {
                Ok(decoder) => {
                    info!("{} 📎 字幕解码器创建成功", log_ctx());
                    Some(decoder)
                }
                Err(e) => {
                    warn!("{} ❌ 创建字幕解码器失败: {}，继续播放（无字幕）", log_ctx(), e);
                    None
                }
            }
        } else {
            None
        };

        // 加载外部字幕文件
        self.load_external_subtitles(&path);

        // 启动播放线程
        self.start_playback_threads(
            demuxer,
            video_decoder,
            audio_decoder,
            subtitle_decoder,
        );

        Ok(media_info)
    }

    /// 播放
    pub fn play(&mut self) -> Result<()> {
        let current_state = {
            let state = self.state.lock().unwrap();
            state.state
        };
        
        // 如果处于停止状态，需要重新打开文件
        if current_state == PlaybackState::Stopped {
            // 先获取文件路径并释放锁
            let file_path = {
                let file_path_guard = self.current_file_path.lock().unwrap();
                file_path_guard.clone()
            };
            
            if let Some(path) = file_path {
                info!("{} 从停止状态恢复播放，重新打开文件: {}", log_ctx(), path);
                // 重新打开文件（这会重新启动线程）
                self.open_file(&path)?;
                // 打开后状态是 Paused，继续执行下面的 play 逻辑
            } else {
                return Err(crate::core::PlayerError::Other("没有打开的文件，无法播放".to_string()).into());
            }
        }
        
        info!("{} 🎬 播放", log_ctx());
        self.clock.play();
        let mut state = self.state.lock().unwrap();
        state.state = PlaybackState::Playing;
        Ok(())
    }

    /// 暂停播放
    /// 
    /// # 音画同步机制
    /// - 暂停时钟：停止时间推进
    /// - 清空音频缓冲区：立即停止声音输出
    /// - 更新播放状态：标记为暂停
    pub fn pause(&self) {
        info!("{} 🎬 暂停", log_ctx());
        
        // ========== 暂停时钟 ==========
        // 停止时间推进，视频帧也会停止更新
        self.clock.pause();
        
        // ========== 清空音频输出缓冲区 ==========
        // 立即停止音频播放，避免"拖尾"
        if let Some(ref output) = self.audio_output {
            output.clear_buffer();
            debug!("{} ✓ 暂停时清空音频输出缓冲区", log_ctx());
        }
        
        // ========== 更新播放状态 ==========
        let mut state = self.state.lock().unwrap();
        state.state = PlaybackState::Paused;
    }

    /// ==================== 音画同步核心: Seek 跳转 ====================
    /// 
    /// # 功能说明
    /// 跳转到指定播放位置，确保音画同步
    /// 
    /// # 音画同步机制
    /// 
    /// ## 核心原理
    /// - **音频作为主时钟**：所有同步以音频时间为基准
    /// - **多线程协调**：UI线程、解封装线程、音视频解码线程需要协同工作
    /// - **状态清理**：清除旧数据，避免残留帧影响播放
    /// 
    /// ## Seek 步骤（7步流程）
    /// 
    /// ### 1. 设置 seek 标记
    /// - 通知音视频解码线程跳过不合适的旧帧
    /// - 音频阈值：目标 ±500ms（音频帧密集）
    /// - 视频阈值：目标 ±1000ms（视频帧稀疏，24fps ≈ 42ms/帧）
    /// - 附带时间戳用于超时检测（2秒后强制清除，防止卡住）
    /// 
    /// ### 2. 重置首次音频帧标志
    /// - 确保音频解码线程将下一个有效帧视为"新的开始"
    /// - 但不会覆盖 seek 设置的时钟（时钟已在步骤5预设）
    /// 
    /// ### 3. 清空音频输出缓冲区
    /// - 立即停止播放旧音频，避免"拖尾"现象
    /// - 用户听到的声音立即切换到新位置
    /// 
    /// ### 4. 清空所有帧队列
    /// - 丢弃已解码但未消费的旧帧（视频、音频、字幕）
    /// - 避免旧帧影响新位置的播放
    /// 
    /// ### 5. 立即更新播放时钟
    /// - 设置为目标位置（预设值）
    /// - UI 基于此显示进度
    /// - 实际时钟会在第一个音频帧到达时微调确认
    /// 
    /// ### 6. 更新播放状态
    /// - 记录新位置供日志、统计使用
    /// 
    /// ### 7. 通知解封装线程
    /// - 发送 seek 命令，从文件新位置开始读取
    /// - 使用阻塞发送（send），确保命令不会丢失
    /// - 解封装线程会合并多个 seek 命令，只执行最后一个
    pub fn seek(&self, position_ms: i64) {
        info!("{} 🎯 Seek 到: {} ms", log_ctx(), position_ms);
        
        // ========== 步骤1: 设置 seek 标记 ==========
        // 让音视频解码线程知道需要跳过不合适的旧帧
        // 附带时间戳，用于2秒超时检测（防止卡在 seek 状态）
        {
            let mut seek_pos = self.seek_position.lock().unwrap();
            *seek_pos = Some((position_ms, Instant::now()));
        }
        
        // ========== 步骤2: 重置首次音频帧标志 ==========
        // 让音频解码线程将下一个有效帧视为"新的开始"
        // 注意：不会覆盖步骤5预设的时钟值
        self.is_first_audio_frame.store(true, Ordering::SeqCst);
        
        // ========== 步骤3: 清空音频输出缓冲区 ==========
        // 立即停止播放旧音频，避免"拖尾"
        if let Some(ref output) = self.audio_output {
            output.clear_buffer();
            debug!("✓ 清空音频输出缓冲区");
        }
        
        // ========== 步骤4: 设置 flush 标志 ==========
        // 通知解码线程需要 flush 解码器，清除内部缓冲的旧帧
        self.need_flush_decoders.store(true, Ordering::SeqCst);
        info!("🔄 Seek 设置 flush 标志，通知解码线程 flush 解码器");
        
        // ========== 步骤5: 清空所有帧队列 ==========
        // 丢弃所有已解码但未消费的旧帧（关键：seek后必须立即清空，避免显示旧帧）
        let mut video_count = 0;
        while self.video_frame_queue.pop().is_some() {
            video_count += 1;
        }
        
        let mut audio_count = 0;
        while self.audio_frame_queue.pop().is_some() {
            audio_count += 1;
        }

        let mut subtitle_count = 0;
        while self.subtitle_frame_queue.pop().is_some() {
            subtitle_count += 1;
        }
        
        if video_count > 0 || audio_count > 0 || subtitle_count > 0 {
            info!("{} 🧹 Seek 清空帧队列: {} 视频帧, {} 音频帧, {} 字幕帧", log_ctx(), video_count, audio_count, subtitle_count);
        }
        
        // ========== 步骤6: 立即更新播放时钟 ==========
        // 预设时钟为目标位置，UI会基于此显示进度
        // 实际时钟会在第一个音频帧到达时微调确认
        self.clock.set_time(position_ms);
        
        // ========== 步骤7: 更新播放状态 ==========
        // 记录新位置（供日志、统计使用）
        {
            let mut state = self.state.lock().unwrap();
            state.position = position_ms;
        }
        
        // ========== 步骤8: 通知解封装线程执行文件级 seek ==========
        // 分两种情况：
        // 1. DemuxerThread 模式：直接调用 DemuxerThread 的 seek 方法，并立即清空包队列
        // 2. 旧架构模式：通过 seek_tx channel 发送命令
        if let Some(ref demuxer_thread) = self.demuxer_thread_handle {
            // DemuxerThread 模式
            // 注意：Receiver 不能直接在主线程中清空，因为它在解码线程中使用
            // Seek 时，解码线程会继续接收包，但会在解码时丢弃旧的包
            // 清空操作应该在解码线程中处理，或者在 demuxer 线程 seek 后自动清空
            // 这里我们只发送 seek 命令
            
            if let Err(e) = demuxer_thread.seek(position_ms) {
                error!("{} ❌ 发送 seek 命令到 DemuxerThread 失败: {}", log_ctx(), e);
            } else {
                info!("{} ✅ Seek 命令已发送到 DemuxerThread: {}ms（队列清空由 demuxer 线程处理）", log_ctx(), position_ms);
            }
        } else if let Some(ref tx) = self.seek_tx {
            // 旧架构模式：通过 channel 发送
            if let Err(e) = tx.send(position_ms) {
                error!("{} ❌ 发送 seek 命令失败: {}", log_ctx(), e);
            } else {
                debug!("{} ✓ Seek 命令已发送到 demuxer 线程", log_ctx());
            }
        } else {
            warn!("{} ⚠️  Seek 命令无法发送：既没有 DemuxerThread 也没有 seek_tx", log_ctx());
        }
        
        info!("{} ✅ Seek 准备完成: {}ms", log_ctx(), position_ms);
    }

    /// 停止播放
    pub fn stop(&mut self) {
        info!("{} ⏹️  停止播放", log_ctx());
        self.running.store(false, Ordering::SeqCst);

        // 等待线程结束（对于打开新文件时正确重置状态很重要）
        // 线程应该在收到 running=false 后很快退出，因为它们在循环中检查这个标志
        
        // 停止 DemuxerThread（新架构）
        if let Some(mut demuxer_thread) = self.demuxer_thread_handle.take() {
            info!("{} ⏹️  停止 DemuxerThread", log_ctx());
            demuxer_thread.stop();
            info!("{} ✅ DemuxerThread 已停止", log_ctx());
        }
        
        // 等待解封装线程结束
        if let Some(thread) = self.demux_thread.take() {
            let _ = thread.join();
            info!("{} ✅ 解封装线程已结束", log_ctx());
        }
        
        // 等待视频解码线程结束
        if let Some(thread) = self.video_decode_thread.take() {
            let _ = thread.join();
            info!("{} ✅ 视频解码线程已结束", log_ctx());
        }
        
        // 等待音频解码线程结束
        if let Some(thread) = self.audio_decode_thread.take() {
            let _ = thread.join();
            info!("{} ✅ 音频解码线程已结束", log_ctx());
        }
        
        // 等待字幕解码线程结束
        if let Some(thread) = self.subtitle_decode_thread.take() {
            let _ = thread.join();
            info!("{} ✅ 字幕解码线程已结束", log_ctx());
        }
        
        // 停止并清理音频输出
        if let Some(mut output) = self.audio_output.take() {
            info!("{} 🔊 停止音频输出", log_ctx());
            output.stop();
        }

        // 清空帧队列
        let mut audio_count = 0;
        while self.audio_frame_queue.pop().is_some() {
            audio_count += 1;
        }
        if audio_count > 0 {
            info!("{} 🗑️  清空音频帧队列: {} 帧", log_ctx(), audio_count);
        }
        
        let mut video_count = 0;
        while self.video_frame_queue.pop().is_some() {
            video_count += 1;
        }
        if video_count > 0 {
            info!("{} 🗑️  清空视频帧队列: {} 帧", log_ctx(), video_count);
        }

        // 清空字幕帧队列
        let mut subtitle_count = 0;
        while self.subtitle_frame_queue.pop().is_some() {
            subtitle_count += 1;
        }
        if subtitle_count > 0 {
            info!("{} 🗑️  清空字幕帧队列: {} 帧", log_ctx(), subtitle_count);
        }

        // 清空外部字幕缓存
        {
            let mut external_frames = self.external_subtitle_frames.lock().unwrap();
            let external_count = external_frames.len();
            external_frames.clear();
            if external_count > 0 {
                info!("{} 🗑️  清空外部字幕缓存: {} 条", log_ctx(), external_count);
            }
        }

        // 重置播放时钟（重要：打开新文件前必须重置时钟）
        self.clock.set_time(0);
        
        // 重置 seek 通道（清理旧通道）
        self.seek_tx = None;
        
        // 重置 flush 标志
        self.need_flush_decoders.store(false, Ordering::SeqCst);
        
        // 重置状态
        let mut state = self.state.lock().unwrap();
        state.state = PlaybackState::Stopped;
        state.position = 0;
        
        info!("{} ✅ 停止播放完成，所有线程已清理", log_ctx());
    }

    /// 设置音量
    pub fn set_volume(&self, volume: f32) {
        let mut state = self.state.lock().unwrap();
        state.volume = volume.clamp(0.0, 1.0);
    }

    /// 获取当前状态
    pub fn get_state(&self) -> PlayerState {
        let mut state = self.state.lock().unwrap();
        state.position = self.clock.now();
        state.clone()
    }

    /// 更新音频输出（从队列中取出帧并写入）
    /// 应该定期调用此方法以保持音频播放流畅
    /// 
    /// # 音画同步机制
    /// - **仅在播放状态下更新音频**：暂停时不从队列取帧
    /// - 避免暂停后音频继续播放的问题
    pub fn update_audio(&mut self) {
        // ========== 检查播放状态 ==========
        // 仅在播放状态下更新音频，暂停/停止时不处理
        let is_playing = {
            let state = self.state.lock().unwrap();
            state.state == PlaybackState::Playing
        };
        
        if !is_playing {
            return;  // 暂停或停止状态，不更新音频
        }
        
        // ========== 从队列取出音频帧并写入输出 ==========
        if let Some(ref mut output) = self.audio_output {
            // 处理所有可用的音频帧
            while let Some(frame) = self.audio_frame_queue.pop() {
                output.write_frame(&frame);
                
                // 更新音量
                let vol = self.state.lock().unwrap().volume;
                output.set_volume(vol);
                
                // 限制缓冲区大小，避免延迟过大
                if output.buffer_size() > 96000 {
                    break;
                }
            }
        }
    }

    /// 获取当前视频帧
    /// 返回最新的视频帧用于渲染
    pub fn get_video_frame(&self) -> Option<VideoFrame> {
        self.video_frame_queue.pop()
    }
    
    /// 获取媒体信息
    pub fn get_media_info(&self) -> Option<MediaInfo> {
        let state = self.state.lock().unwrap();
        state.media_info.clone()
    }

    /// 获取当前视频帧（简单版本，直接取队列中的第一个）
    /// 注意：这个方法不做时间同步，只是简单地取出队列中的第一个帧
    /// 同时会清理队列中过期的帧
    pub fn get_current_frame(&self) -> Option<VideoFrame> {
        // 如果队列过大，先清理过期帧
        let queue_len = self.video_frame_queue.len();
        if queue_len > 80 {
            let clock = self.clock.clone();
            let current_time = clock.now();
            const DROP_THRESHOLD_MS: i64 = 1000; // 丢弃1秒前的帧
            const MAX_KEEP: usize = 50; // 最多保留50帧
            
            let mut kept_frames = Vec::new();
            let mut processed = 0;
            const MAX_PROCESS: usize = 300; // 限制处理数量
            
            // 清理过期帧，保留最新的帧
            while processed < MAX_PROCESS {
                if let Some(frame) = self.video_frame_queue.pop() {
                    processed += 1;
                    // 只保留未过期且最近的帧
                    if frame.pts >= current_time - DROP_THRESHOLD_MS {
                        if kept_frames.len() < MAX_KEEP {
                            kept_frames.push(frame);
                        }
                        // 超出保留数量的帧也丢弃
                    }
                    // 过期帧直接丢弃
                } else {
                    break;
                }
            }
            
            // 按PTS排序并放回（最新的在前）
            kept_frames.sort_by_key(|f| f.pts);
            for frame in kept_frames {
                self.video_frame_queue.push(frame);
            }
        }
        
        self.video_frame_queue.pop()
    }

    /// 获取当前字幕（根据播放时间）
    /// 
    /// 算法说明：
    /// 1. 遍历字幕队列，查找所有在当前时间范围内的字幕
    /// 2. 选择时间戳最新的字幕（用于处理重叠字幕）
    /// 3. 保留未到时间和未使用的字幕回队列
    /// 4. 丢弃过期字幕以避免内存泄漏
    pub fn get_current_subtitle(&self, current_time_ms: i64) -> Option<SubtitleFrame> {
        let mut best_subtitle: Option<SubtitleFrame> = None;
        let mut pending_frames = Vec::new();
        let mut checked_count = 0;
        const MAX_CHECK_COUNT: usize = 100; // 限制检查数量，防止无限循环

        // 遍历队列查找合适的字幕
        while let Some(frame) = self.subtitle_frame_queue.pop() {
            checked_count += 1;
            
            // 防止无限循环（队列可能很大）
            if checked_count > MAX_CHECK_COUNT {
                // 将剩余帧放回队列
                pending_frames.push(frame);
                break;
            }
            
            if current_time_ms >= frame.pts && current_time_ms < frame.end_pts {
                // 找到匹配的字幕（在当前时间范围内）
                // 选择时间戳最新的字幕（处理重叠字幕的情况）
                if best_subtitle.as_ref().map(|b| frame.pts > b.pts).unwrap_or(true) {
                    // 如果之前有候选字幕，将其放回队列
                    if let Some(old) = best_subtitle.take() {
                        pending_frames.push(old);
                    }
                    best_subtitle = Some(frame.clone());
                    // 当前帧也要放回队列，因为它可能还需要继续显示
                    pending_frames.push(frame);
                } else {
                    // 这个字幕不如当前最佳字幕，放回队列
                    pending_frames.push(frame);
                }
            } else if current_time_ms < frame.pts {
                // 未到时间的字幕，保留
                pending_frames.push(frame);
            } else {
                // 过期字幕（current_time_ms >= frame.end_pts）直接丢弃，避免内存泄漏
                // 不放入 pending_frames，让它被回收
            }
        }

        // 将未使用的字幕放回队列
        // 注意：如果找到了最佳字幕，它也在 pending_frames 中，会被放回队列
        // 这样可以支持字幕在时间范围内持续显示
        for frame in pending_frames {
            // 如果是最佳字幕，或者不是最佳字幕且未过期，则放回队列
            let should_keep = best_subtitle.as_ref()
                .map(|best| {
                    // 如果是最佳字幕本身，保留
                    frame.pts == best.pts
                    // 或者不是最佳字幕，但是未到时间的字幕
                    || (current_time_ms < frame.pts)
                })
                .unwrap_or(true);
            
            if should_keep {
                self.subtitle_frame_queue.push(frame);
            }
        }

        // 如果没有找到内嵌字幕，尝试外部字幕
        if best_subtitle.is_none() {
            best_subtitle = self.get_external_subtitle(current_time_ms);
        }

        best_subtitle
    }

    /// 加载外部字幕文件
    fn load_external_subtitles(&self, video_path: &str) {
        info!("🔍 查找外部字幕文件: {}", video_path);
        
        // 查找同目录下的字幕文件
        let subtitle_files = ExternalSubtitleParser::find_subtitle_files(video_path);
        
        if subtitle_files.is_empty() {
            info!("未找到外部字幕文件");
            return;
        }

        let mut all_frames = Vec::new();
        
        // 解析所有找到的字幕文件（优先级：第一个找到的）
        for subtitle_file in subtitle_files.iter().take(1) { // 目前只加载第一个字幕文件
            info!("📝 加载外部字幕文件: {}", subtitle_file.display());
            
            match ExternalSubtitleParser::parse_subtitle_file(subtitle_file) {
                Ok(frames) => {
                    info!("✅ 成功解析外部字幕，共 {} 条", frames.len());
                    all_frames.extend(frames);
                    break; // 成功加载一个就够了
                }
                Err(e) => {
                    error!("{} ❌ 解析外部字幕文件失败: {} - {}", log_ctx(), subtitle_file.display(), e);
                }
            }
        }

        // 按时间戳排序
        all_frames.sort_by_key(|frame| frame.pts);

        // 存储到外部字幕缓存
        {
            let mut external_frames = self.external_subtitle_frames.lock().unwrap();
            *external_frames = all_frames;
            info!("{} 📝 外部字幕加载完成，共 {} 条字幕", log_ctx(), external_frames.len());
        }
    }

    /// 从外部字幕中获取当前时间应显示的字幕
    fn get_external_subtitle(&self, current_time_ms: i64) -> Option<SubtitleFrame> {
        let external_frames = self.external_subtitle_frames.lock().unwrap();
        
        // 查找当前时间范围内的字幕
        for frame in external_frames.iter() {
            if current_time_ms >= frame.pts && current_time_ms < frame.end_pts {
                return Some(frame.clone());
            }
            
            // 如果字幕还没到时间，后面的也不会到时间（已排序）
            if current_time_ms < frame.pts {
                break;
            }
        }
        
        None
    }

    /// 根据播放时钟获取应该显示的视频帧（音视频同步）
    /// 返回 PTS <= 当前播放时间的最近一帧
    /// 
    /// 优化：限制检查数量，避免一次性处理所有帧导致内存爆炸
    pub fn get_frame_for_time(&self, current_time_ms: i64) -> Option<VideoFrame> {
        // 从队列中找到最接近但不超过当前时间的帧
        let mut best_frame: Option<VideoFrame> = None;
        let mut frames_to_keep = Vec::new();
        let mut future_frames = Vec::new();
        
        // 限制检查数量，防止队列过大时内存爆炸
        const MAX_CHECK_COUNT: usize = 200; // 最多检查200帧
        const MAX_FUTURE_FRAMES: usize = 30; // 最多保留30个未来帧（减少）
        let mut checked_count = 0;
        let mut discarded_old_frames = 0;
        
        // 丢弃阈值：如果帧的 PTS 比当前时间早 1 秒，直接丢弃（更激进）
        const DROP_THRESHOLD_MS: i64 = 1000;
        
        // 第一遍：收集帧（限制数量）
        while checked_count < MAX_CHECK_COUNT {
            if let Some(frame) = self.video_frame_queue.pop() {
                checked_count += 1;
                
                // 丢弃过期的帧（PTS 远小于当前时间）
                if frame.pts < current_time_ms - DROP_THRESHOLD_MS {
                    discarded_old_frames += 1;
                    continue; // 直接丢弃，不保留
                }
                
                if frame.pts <= current_time_ms {
                    // 这个帧的时间戳合适，保留它（如果有更好的就替换）
                    if best_frame.as_ref().map(|f| f.pts < frame.pts).unwrap_or(true) {
                        // 丢弃之前的best_frame（如果时间戳更早）
                        if let Some(old) = best_frame.take() {
                            frames_to_keep.push(old);
                        }
                        best_frame = Some(frame);
                    } else {
                        // 这个帧不如best_frame好，保留它到队列
                        frames_to_keep.push(frame);
                    }
                } else {
                    // 这个帧的时间戳太新，暂时保留
                    // 但限制未来帧的数量
                    if future_frames.len() < MAX_FUTURE_FRAMES {
                        future_frames.push(frame);
                    } else {
                        // 未来帧已满，丢弃最旧的未来帧
                        discarded_old_frames += 1;
                    }
                }
            } else {
                // 队列为空
                break;
            }
        }
        
        if discarded_old_frames > 0 {
            debug!("🗑️ 丢弃了 {} 个过期视频帧", discarded_old_frames);
        }
        
        // 将未使用的帧放回队列
        // 先放回过去的帧（按PTS排序），然后放回未来的帧（按PTS排序）
        frames_to_keep.sort_by_key(|f| f.pts);
        future_frames.sort_by_key(|f| f.pts);
        
        for frame in frames_to_keep {
            self.video_frame_queue.push(frame);
        }
        for frame in future_frames {
            self.video_frame_queue.push(frame);
        }
        
        best_frame
    }

    /// 获取播放时长（秒）
    pub fn get_duration(&self) -> Result<f64> {
        let state = self.state.lock().unwrap();
        if let Some(info) = &state.media_info {
            // duration 是毫秒，转换为秒
            Ok(info.duration as f64 / 1000.0)
        } else {
            Ok(0.0)
        }
    }

    /// 获取当前播放位置（秒）
    pub fn get_position(&self) -> Result<f64> {
        // clock.now() 返回毫秒，转换为秒
        Ok(self.clock.now() as f64 / 1000.0)
    }

    /// 跳转到指定位置（秒）
    pub fn seek_to_seconds(&mut self, position: f64) -> Result<()> {
        info!("{} ⏩ 跳转到位置: {:.2}s", log_ctx(), position);
        // 转换为毫秒
        let position_ms = (position * 1000.0) as i64;
        self.seek(position_ms);
        Ok(())
    }

    /// 检查是否正在播放
    pub fn is_playing(&self) -> bool {
        let state = self.state.lock().unwrap();
        matches!(state.state, PlaybackState::Playing)
    }

    /// 启动播放线程
    fn start_playback_threads(
        &mut self,
        mut demuxer: Demuxer,
        video_decoder: Option<VideoDecoder>,
        audio_decoder: Option<AudioDecoder>,
        subtitle_decoder: Option<SubtitleDecoder>,
    ) {
        self.running.store(true, Ordering::SeqCst);

        // 创建数据包队列
        let video_packet_queue = Arc::new(SegQueue::new());
        let audio_packet_queue = Arc::new(SegQueue::new());
        let subtitle_packet_queue = Arc::new(SegQueue::new());

        // 使用 manager 的视频、音频和字幕帧队列
        let video_frame_queue = self.video_frame_queue.clone();
        let audio_frame_queue = self.audio_frame_queue.clone();
        let subtitle_frame_queue = self.subtitle_frame_queue.clone();

        let running = self.running.clone();
        let clock = self.clock.clone();
        let is_first_audio_frame = self.is_first_audio_frame.clone();

        // 创建 seek 通道
        let (seek_tx, seek_rx): (Sender<i64>, Receiver<i64>) = unbounded();
        self.seek_tx = Some(seek_tx);

        // 解封装线程
        let video_pq = video_packet_queue.clone();
        let audio_pq = audio_packet_queue.clone();
        let subtitle_pq = subtitle_packet_queue.clone();
        let demux_running = running.clone();
        let is_network = self.is_network_source.clone();

        self.demux_thread = Some(thread::spawn(move || {
            info!("解封装线程启动");
            let mut packet_count = 0;
            while demux_running.load(Ordering::SeqCst) {
                // 检查是否有 seek 命令（处理所有待处理的seek命令，只执行最后一个）
                let mut last_seek_pos: Option<i64> = None;
                while let Ok(seek_pos_ms) = seek_rx.try_recv() {
                    // 如果有多个seek命令堆积，只记录最后一个
                    if let Some(old_pos) = last_seek_pos {
                        debug!("跳过旧的 seek 命令: {} ms", old_pos);
                    }
                    last_seek_pos = Some(seek_pos_ms);
                }
                
                if let Some(seek_pos_ms) = last_seek_pos {
                    info!("🎯 Demuxer 收到 seek 命令: {} ms，清空队列并执行 seek", seek_pos_ms);
                    
                    // 清空所有包队列（确保没有旧数据）
                    let mut cleared_video = 0;
                    let mut cleared_audio = 0;
                    let mut cleared_subtitle = 0;
                    while video_pq.pop().is_some() { cleared_video += 1; }
                    while audio_pq.pop().is_some() { cleared_audio += 1; }
                    while subtitle_pq.pop().is_some() { cleared_subtitle += 1; }
                    
                    if cleared_video > 0 || cleared_audio > 0 || cleared_subtitle > 0 {
                        debug!("清空包队列: 视频{} 音频{} 字幕{}", cleared_video, cleared_audio, cleared_subtitle);
                    }
                    
                    // 执行 seek
                    if let Err(e) = demuxer.seek(seek_pos_ms) {
                        error!("{} ❌ Demuxer seek 失败: {}", log_ctx(), e);
                    } else {
                        info!("✅ Demuxer seek 成功: {} ms", seek_pos_ms);
                    }
                    packet_count = 0; // 重置计数
                    
                    // 短暂等待，确保队列被其他线程清空
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                
                match demuxer.read_packet() {
                    Ok(Some((packet, is_video, is_subtitle))) => {
                        packet_count += 1;
                        if is_video {
                            video_pq.push(packet);
                            if packet_count % 100 == 0 {
                                debug!("解封装视频包: {} (队列: {})", packet_count, video_pq.len());
                            }
                        } else if is_subtitle {
                            // 字幕包推入字幕队列
                            subtitle_pq.push(packet);
                        } else {
                            audio_pq.push(packet);
                        }
                    }
                    Ok(None) => {
                        info!("文件读取完毕，共处理 {} 个包", packet_count);
                        break;
                    }
                    Err(e) => {
                        error!("{} 读取数据包失败: {} (已处理 {} 个包)", log_ctx(), e, packet_count);
                        break;
                    }
                }

                // 智能缓冲策略：根据媒体源类型动态调整队列大小
                // 本地文件: 磁盘 I/O 快速稳定，使用较小缓冲节省内存
                // 网络流: 网络 I/O 不稳定，使用较大缓冲应对抖动
                let is_network_source = is_network.load(Ordering::SeqCst);
                let max_queue_size = if is_network_source {
                    1000  // 网络流: 1000 包（约 20-40 秒，应对网络抖动）
                } else {
                    300   // 本地文件: 300 包（约 6-12 秒，足够流畅）
                };
                
                while (video_pq.len() > max_queue_size || audio_pq.len() > max_queue_size)
                    && demux_running.load(Ordering::SeqCst)
                {
                    if video_pq.len() > max_queue_size || audio_pq.len() > max_queue_size {
                        debug!("队列满，等待消费 (视频: {}/{}, 音频: {}/{}, 类型: {})", 
                               video_pq.len(), max_queue_size, audio_pq.len(), max_queue_size,
                               if is_network_source { "网络流" } else { "本地文件" });
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
            info!("解封装线程结束");
        }));

        // 视频解码线程
        if let Some(mut decoder) = video_decoder {
            let video_pq = video_packet_queue.clone();
            let video_fq = video_frame_queue.clone();
            let decode_running = running.clone();
            let _video_clock = clock.clone();
            let seek_pos = self.seek_position.clone();
            let is_network = self.is_network_source.clone();

            self.video_decode_thread = Some(thread::spawn(move || {
                info!("🎬 视频解码线程启动");
                // ==================== 视频解码线程：跟随音频时钟 ====================
                // 职责：
                // 1. 解码视频包为视频帧
                // 2. 跟随音频时钟，不主动控制播放节奏
                // 3. Seek后跳过不合适的旧帧
                // 4. 提前解码帧以保证播放流畅
                while decode_running.load(Ordering::SeqCst) {
                    // ========== 队列限流：防止过度解码 ==========
                    // 智能缓冲策略：根据媒体源类型调整视频帧缓冲
                    // 本地文件模式：更激进的队列控制，提前减速
                    let is_network_source = is_network.load(Ordering::SeqCst);
                    
                    if !is_network_source {
                        // 本地文件：提前减速，避免队列过大
                        let queue_len = video_fq.len();
                        const LOCAL_MAX_FRAMES: usize = 20;  // 本地文件最大帧数（从15增加到20，但提前控制）
                        const LOCAL_HIGH_WATER: usize = 12;  // 高水位：开始减速
                        
                        if queue_len > LOCAL_MAX_FRAMES {
                            // 队列过大，减速解码
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        } else if queue_len > LOCAL_HIGH_WATER {
                            // 接近上限，轻微减速
                            thread::sleep(Duration::from_millis(2));
                        }
                    } else {
                        // 网络流：使用更大的缓冲（在网络流模式中处理，这里不做特殊处理）
                        let max_video_frames = 30;  // 网络流: 30帧
                        if video_fq.len() > max_video_frames {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                    }

                    if let Some(packet) = video_pq.pop() {
                        match decoder.decode(&packet) {
                            Ok(frames) => {
                                for frame in frames {
                                    // ========== Seek 后帧过滤逻辑 ==========
                                    // 目的：跳过不合适的旧帧，快速定位到 seek 目标位置
                                    // 返回：should_skip（是否跳过当前帧）
                                    let should_skip = {
                                        let mut seek_pos_guard = seek_pos.lock().unwrap();
                                        if let Some((seek_target, seek_time)) = *seek_pos_guard {
                                            // --- 超时检测：防止卡在 seek 状态 ---
                                            if seek_time.elapsed() > Duration::from_secs(2) {
                                                warn!("{} 🎬 Seek 超时（2秒），强制清除视频seek标志", log_ctx());
                                                *seek_pos_guard = None;
                                                false  // 不跳过
                                            } else {
                                                // --- 帧 PTS 范围检查 ---
                                                // 太旧的帧：PTS < 目标 - 1000ms
                                                // 比音频阈值更宽松，因为视频帧间隔更大（24fps ≈ 42ms/帧）
                                                if frame.pts < seek_target - 1000 {
                                                    debug!("🎬 跳过旧视频帧: PTS={}ms < Seek目标={}ms", frame.pts, seek_target);
                                                    true  // 跳过
                                                }
                                                // 太新的帧：PTS > 目标 + 10s（可能是旧的残留帧）
                                                else if frame.pts > seek_target + 10000 {
                                                    debug!("🎬 跳过异常视频帧: PTS={}ms > Seek目标+10s={}ms", frame.pts, seek_target + 10000);
                                                    true  // 跳过
                                                } else {
                                                    false  // 在合理范围内，不跳过
                                                }
                                            }
                                        } else {
                                            false  // 没有 seek，正常处理
                                        }
                                    };
                                    
                                    // 在释放锁后再执行 continue（避免持有锁时跳转）
                                    if should_skip {
                                        continue;
                                    }
                                    
                                    // ========== 推入视频帧队列 ==========
                                    // 供 UI 线程消费（根据音频时钟选择合适的帧显示）
                                    debug!("🎬 解码视频帧: PTS={}ms", frame.pts);
                                    video_fq.push(frame);
                                }
                            }
                            Err(e) => {
                                match e {
                                    crate::core::error::PlayerError::FFmpegError(ffmpeg::Error::Eof) => {
                                        debug!("{} 🎬 解码器返回 EOF（视频），忽略", log_ctx());
                                    }
                                    crate::core::error::PlayerError::FFmpegError(ffmpeg::Error::Other { errno: 11 }) => {
                                        debug!("{} 🎬 解码器返回 EAGAIN（视频），忽略", log_ctx());
                                    }
                                    _ => {
                                        error!("{} ❌ 视频解码失败: {}", log_ctx(), e);
                                    }
                                }
                            }
                        }
                    } else {
                        // 没有包时稍微休眠，避免空转消耗 CPU
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                info!("🎬 视频解码线程结束");
            }));
        }

        // 音频解码线程
        if let Some(mut decoder) = audio_decoder {
            let audio_pq = audio_packet_queue.clone();
            let audio_fq = audio_frame_queue.clone();
            let decode_running = running.clone();
            let audio_clock = clock.clone();
            let first_audio_flag = is_first_audio_frame.clone();
            let seek_pos = self.seek_position.clone();
            let is_network = self.is_network_source.clone();

            self.audio_decode_thread = Some(thread::spawn(move || {
                info!("🔊 音频解码线程启动");
                // ==================== 音频解码线程：主时钟源 ====================
                // 职责：
                // 1. 解码音频包为音频帧
                // 2. 作为主时钟源，控制整个播放节奏
                // 3. Seek后跳过不合适的旧帧
                // 4. 设置初始音频时钟基准
                while decode_running.load(Ordering::SeqCst) {
                    if let Some(packet) = audio_pq.pop() {
                        debug!("🔊 音频解码线程获取到包，队列剩余: {}", audio_pq.len());
                        match decoder.decode(&packet) {
                            Ok(frames) => {
                                for frame in frames {
                                    // ========== Seek 后帧过滤逻辑 ==========
                                    // 目的：跳过不合适的旧帧，快速定位到 seek 目标位置
                                    // 返回：(should_skip, is_first_valid_frame)
                                    let (should_skip, is_first_valid_frame) = {
                                        let mut seek_pos_guard = seek_pos.lock().unwrap();
                                        if let Some((seek_target, seek_time)) = *seek_pos_guard {
                                            // --- 超时检测：防止卡在 seek 状态 ---
                                            if seek_time.elapsed() > Duration::from_secs(2) {
                                                warn!("{} 🔊 Seek 超时（2秒），强制清除seek标志", log_ctx());
                                                *seek_pos_guard = None;
                                                (false, false)  // 不跳过，不是首个有效帧
                                            } else {
                                                // --- 帧 PTS 范围检查 ---
                                                // 太旧的帧：PTS < 目标 - 500ms
                                                if frame.pts < seek_target - 500 {
                                                    debug!("🔊 跳过旧音频帧: PTS={}ms < Seek目标={}ms", frame.pts, seek_target);
                                                    (true, false)  // 跳过
                                                }
                                                // 太新的帧：PTS > 目标 + 10s（可能是旧的残留帧）
                                                else if frame.pts > seek_target + 10000 {
                                                    debug!("🔊 跳过异常音频帧: PTS={}ms > Seek目标+10s={}ms", frame.pts, seek_target + 10000);
                                                    (true, false)  // 跳过
                                                } 
                                                // 合适的帧：在目标 ±500ms 范围内
                                                else {
                                                    info!("🔊 找到 Seek 后的首个有效音频帧: PTS={}ms (目标={}ms)", frame.pts, seek_target);
                                                    *seek_pos_guard = None;  // 清除 seek 标志
                                                    (false, true)  // 不跳过，是首个有效帧
                                                }
                                            }
                                        } else {
                                            (false, false)  // 没有 seek，正常处理
                                        }
                                    };
                                    
                                    // 在释放锁后再执行 continue（避免持有锁时跳转）
                                    if should_skip {
                                        continue;
                                    }
                                    
                                    // ========== 音频时钟基准设置 ==========
                                    // 分两种场景：
                                    // 1. Seek 后：时钟已在 seek() 中预设，这里只需清除标志
                                    // 2. 正常播放开始：使用第一个音频帧的 PTS 作为时钟基准
                                    
                                    if is_first_valid_frame {
                                        // --- Seek 场景 ---
                                        // seek() 已经设置好时钟，这里只需清除 first_audio_flag
                                        // 防止后续帧再次设置时钟（避免时钟跳动）
                                        first_audio_flag.store(false, Ordering::SeqCst);
                                        debug!("🔊 Seek 后首个有效帧，时钟已由 seek() 设置，清除 first_audio_flag");
                                    }
                                    else if first_audio_flag.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                                        // --- 正常播放场景 ---
                                        // 第一个音频帧，使用其 PTS 作为时钟基准
                                        // 音频作为主时钟，视频会跟随音频时钟
                                        info!("🔊 首次音频帧: 设置音频时钟基准 PTS={}ms", frame.pts);
                                        audio_clock.set_time(frame.pts);
                                    }
                                    
                                    // ========== 推入音频帧队列 ==========
                                    // 供音频输出线程消费
                                    audio_fq.push(frame.clone());
                                    debug!("🔊 音频帧推入队列: PTS={}ms, 队列长度={}", frame.pts, audio_fq.len());
                                }
                            }
                            Err(e) => {
                                match e {
                                    crate::core::error::PlayerError::FFmpegError(ffmpeg::Error::Eof) => {
                                        debug!("{} 🔊 解码器返回 EOF（音频），忽略", log_ctx());
                                    }
                                    crate::core::error::PlayerError::FFmpegError(ffmpeg::Error::Other { errno: 11 }) => {
                                        debug!("{} 🔊 解码器返回 EAGAIN（音频），忽略", log_ctx());
                                    }
                                    _ => {
                                        error!("{} ❌ 音频解码失败: {}", log_ctx(), e);
                                    }
                                }
                            }
                        }
                    } else {
                        debug!("🔊 音频解码线程: 没有包可处理，音频队列长度: {}", audio_pq.len());
                        thread::sleep(Duration::from_millis(5));
                    }

                    // 控制帧队列大小：智能缓冲策略
                    // 本地文件模式：提前减速，避免队列过大
                    let is_network_source = is_network.load(Ordering::SeqCst);
                    
                    if !is_network_source {
                        // 本地文件：提前减速控制
                        let queue_len = audio_fq.len();
                        const LOCAL_MAX_AUDIO_FRAMES: usize = 80;  // 本地文件最大音频帧（从150降到80）
                        const LOCAL_AUDIO_HIGH_WATER: usize = 50;  // 高水位：开始减速
                        
                        if queue_len > LOCAL_MAX_AUDIO_FRAMES {
                            // 队列过大，减速解码
                            thread::sleep(Duration::from_millis(15));
                        } else if queue_len > LOCAL_AUDIO_HIGH_WATER {
                            // 接近上限，轻微减速
                            thread::sleep(Duration::from_millis(5));
                        }
                    } else {
                        // 网络流：使用更大的缓冲
                        let max_audio_frames = 300;  // 网络流: 300帧（约 6-7 秒，应对网络抖动）
                        while audio_fq.len() > max_audio_frames && decode_running.load(Ordering::SeqCst) {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
                info!("🔊 音频解码线程结束");
            }));
        }

        // 字幕解码线程
        if let Some(mut decoder) = subtitle_decoder {
            let subtitle_pq = subtitle_packet_queue.clone();
            let subtitle_fq = subtitle_frame_queue.clone();
            let decode_running = running.clone();

            self.subtitle_decode_thread = Some(thread::spawn(move || {
                info!("📝 字幕解码线程启动");
                while decode_running.load(Ordering::SeqCst) {
                    if let Some(packet) = subtitle_pq.pop() {
                        debug!("📝 字幕解码线程获取到包，队列剩余: {}", subtitle_pq.len());
                        match decoder.decode(&packet) {
                            Ok(frames) => {
                                for frame in frames {
                                    subtitle_fq.push(frame.clone());
                                    debug!("📝 字幕帧推入队列: PTS={}ms, 文本=\"{}\"", frame.pts, frame.text);
                                }
                            }
                            Err(e) => {
                                error!("{} ❌ 字幕解码失败: {}", log_ctx(), e);
                            }
                        }
                    } else {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                info!("📝 字幕解码线程结束");
            }));
        }
        
        // 音频输出说明：
        // AudioOutput 包含 cpal::Stream，不是 Send，无法跨线程传递
        // 因此音频输出必须在主线程中处理，通过定期调用 update_audio() 方法
        // 来从 audio_frame_queue 中取出帧并写入 AudioOutput
        if self.audio_output.is_some() {
            info!("🔊 音频输出已准备，需要在主线程中定期调用 update_audio() 方法");
            info!("🔊 音频帧队列已准备，解码线程将推送帧到队列");
        }
        
        // 在主线程的更新循环中处理音频帧
        // 注意：这需要定期调用 update() 方法来从队列中取出音频帧并写入 AudioOutput

        // 注意：视频渲染需要在主线程或有窗口上下文的线程中进行
        // 这里我们只是解码,实际渲染需要在 Tauri 的窗口事件循环中处行
        // 可以通过共享的 video_frame_queue 来获取解码后的帧
    }
    
    /// 启动播放线程（使用 DemuxerThread - 网络流专用）
    /// 
    /// 这个方法专门用于网络流，使用 DemuxerThread 在独立线程中运行 Demuxer
    /// DemuxerThread 会持续读取 MediaPacket 并发送到 channel
    fn start_playback_threads_with_demuxer_thread(
        &mut self,
        demuxer_thread: crate::player::DemuxerThread,
        video_decoder: Option<VideoDecoder>,
        audio_decoder: Option<AudioDecoder>,
        subtitle_decoder: Option<SubtitleDecoder>,
    ) {
        self.running.store(true, Ordering::SeqCst);
    
        info!("{} 🚀 启动播放线程（DemuxerThread 模式）", log_ctx());
    
        // frame queues（保持你原来的 SegQueue）
        let video_frame_queue = self.video_frame_queue.clone();
        let audio_frame_queue = self.audio_frame_queue.clone();
    
        let running = self.running.clone();
        let clock = self.clock.clone();
        let is_first_audio_frame = self.is_first_audio_frame.clone();
    
        // 保存 demuxer_thread 到 manager，防止被 drop
        self.demuxer_thread_handle = Some(demuxer_thread);
        
        // 取出接收端（Receiver 不能 clone，需要移动）
        let (video_packet_rx, audio_packet_rx) = self.demuxer_thread_handle.as_mut().unwrap().take_receivers();
    
        // 视频解码线程：使用 recv() 阻塞接收 packet
        if let Some(mut decoder) = video_decoder {
            let video_rx = video_packet_rx;
            let video_fq = video_frame_queue.clone();
            let decode_running = running.clone();
            let video_clock = clock.clone(); // 克隆 clock 供视频解码线程使用
            let need_flush = self.need_flush_decoders.clone();
            let seek_pos = self.seek_position.clone();
    
            self.video_decode_thread = Some(thread::spawn(move || {
                info!("{} 🎬 视频解码线程启动（DemuxerThread 模式）", log_ctx());
    
                let mut video_packet_count: usize = 0;
                let mut decoded_frame_count: usize = 0;
                let mut last_seek_time: Option<Instant> = None; // 记录最后一次 Seek 的时间
                const SEEK_CLEANUP_DISABLE_DURATION: Duration = Duration::from_millis(500); // Seek 后500ms内禁用队列清理
                const VIDEO_QUEUE_SOFT_LIMIT: usize = 36;
                const VIDEO_QUEUE_HARD_LIMIT: usize = 48;
    
                while decode_running.load(Ordering::SeqCst) {
                    // ========== 检查是否需要 flush 解码器 ==========
                    if need_flush.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                        info!("{} 🔄 视频解码线程：执行 flush 解码器", log_ctx());
                        match decoder.flush() {
                            Ok(flushed_frames) => {
                                // 丢弃 flush 出来的旧帧（它们已经过时了）
                                if !flushed_frames.is_empty() {
                                    info!("{} 🔄 视频解码器 flush: 丢弃 {} 个旧帧", log_ctx(), flushed_frames.len());
                                }
                            }
                            Err(e) => {
                                error!("{} ❌ 视频解码器 flush 失败: {}", log_ctx(), e);
                            }
                        }
                        // 记录 Seek 时间，用于暂时禁用队列清理
                        last_seek_time = Some(Instant::now());
                    }
                    
                    // 在取新包前，等待渲染线程消费，避免队列无限增长
                    while decode_running.load(Ordering::SeqCst) && video_fq.len() >= VIDEO_QUEUE_HARD_LIMIT {
                        thread::sleep(Duration::from_millis(5));
                    }

                    // 阻塞等待一个包；当发送端被 drop 时 recv() 返回 Err，退出循环
                    match video_rx.recv() {
                        Ok(packet) => {
                            video_packet_count += 1;
                            if video_packet_count % 100 == 0 {
                                debug!("{} 📦 已接收 {} 个视频包", log_ctx(), video_packet_count);
                            }
    
                            match decoder.decode(&packet) {
                                Ok(frames) => {
                                    for frame in frames {
                                        // Seek 后帧过滤：跳过太旧的帧
                                        let should_skip = {
                                            let seek_pos_guard = seek_pos.lock().unwrap();
                                            if let Some((seek_target, seek_time)) = *seek_pos_guard {
                                                // 超时检测
                                                if seek_time.elapsed() > Duration::from_secs(2) {
                                                    false // 超时，不再跳过
                                                } else {
                                                    // 跳过太旧的帧（PTS < 目标 - 1秒）
                                                    frame.pts < seek_target - 1000
                                                }
                                            } else {
                                                false
                                            }
                                        };
                                        
                                        if should_skip {
                                            debug!("{} 🎬 Seek 后跳过旧视频帧: PTS={}ms", log_ctx(), frame.pts);
                                            continue;
                                        }
                                        
                                        decoded_frame_count += 1;
                                        if decoded_frame_count <= 5 || decoded_frame_count % 100 == 0 {
                                            info!("{} 🎬 解码视频帧 #{}: PTS={}ms",log_ctx(), decoded_frame_count, frame.pts);
                                        }
                                        video_fq.push(frame);
                                    }
    
                                    // 队列大小控制：通过等待方式做温和背压
                                    if last_seek_time.map(|t| t.elapsed() < SEEK_CLEANUP_DISABLE_DURATION).unwrap_or(false) {
                                        // Seek 后保护期内不额外等待，尽快填充新帧
                                    } else {
                                        let queue_len = video_fq.len();
                                        if queue_len >= VIDEO_QUEUE_HARD_LIMIT {
                                            let mut backoff = 6u64;
                                            while decode_running.load(Ordering::SeqCst) && video_fq.len() >= VIDEO_QUEUE_SOFT_LIMIT {
                                                thread::sleep(Duration::from_millis(backoff));
                                                backoff = (backoff + 2).min(20);
                                            }
                                        } else if queue_len >= VIDEO_QUEUE_SOFT_LIMIT {
                                            thread::sleep(Duration::from_millis(4));
                                        }
                                    }
                                }
                                Err(e) => {
                                    match e {
                                        crate::core::error::PlayerError::FFmpegError(ffmpeg::Error::Eof) => {
                                            debug!("{} 🎬 解码器返回 EOF（视频），忽略", log_ctx());
                                        }
                                        crate::core::error::PlayerError::FFmpegError(ffmpeg::Error::Other { errno: 11 }) => {
                                            debug!("{} 🎬 解码器返回 EAGAIN（视频），忽略", log_ctx());
                                        }
                                        _ => {
                                            error!("{} ❌ 视频解码失败: {}", log_ctx(), e);
                                        }
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            // 发送端已关闭（Stop），退出解码线程
                            info!("{} 🎬 视频解码线程检测到发送端关闭，准备退出", log_ctx());
                            break;
                        }
                    }
                }
    
                info!("{} 🎬 视频解码线程结束", log_ctx());
            }));
        }
    
        // 音频解码线程：audio 为主时钟
        if let Some(mut decoder) = audio_decoder {
            let audio_rx = audio_packet_rx;
            let audio_fq = audio_frame_queue.clone();
            let decode_running = running.clone();
            let audio_clock = clock.clone();
            let first_audio_flag = is_first_audio_frame.clone();
            let need_flush = self.need_flush_decoders.clone();
            let seek_pos = self.seek_position.clone();
            let mut decoded_frame_count: usize = 0;

            self.audio_decode_thread = Some(thread::spawn(move || {
                info!("{} 🔊 音频解码线程启动（DemuxerThread 模式）", log_ctx());
    
                let mut last_seek_time: Option<Instant> = None; // 记录最后一次 Seek 的时间
                const SEEK_CLEANUP_DISABLE_DURATION: Duration = Duration::from_millis(500); // Seek 后500ms内禁用队列清理
                const AUDIO_QUEUE_SOFT_LIMIT: usize = 80;
                const AUDIO_QUEUE_HARD_LIMIT: usize = 120;
    
                while decode_running.load(Ordering::SeqCst) {
                    // ========== 检查是否需要 flush 解码器 ==========
                    if need_flush.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                        info!("{} 🔄 音频解码线程：执行 flush 解码器", log_ctx());
                        match decoder.flush() {
                            Ok(flushed_frames) => {
                                // 丢弃 flush 出来的旧帧（它们已经过时了）
                                if !flushed_frames.is_empty() {
                                    info!("{} 🔄 音频解码器 flush: 丢弃 {} 个旧帧", log_ctx(), flushed_frames.len());
                                }
                            }
                            Err(e) => {
                                warn!("{} ⚠️ 音频解码器 flush 失败: {}", log_ctx(), e);
                            }
                        }
                        // 记录 Seek 时间，用于暂时禁用队列清理
                        last_seek_time = Some(Instant::now());
                    }
                    
                    while decode_running.load(Ordering::SeqCst) && audio_fq.len() >= AUDIO_QUEUE_HARD_LIMIT {
                        thread::sleep(Duration::from_millis(5));
                    }

                    match audio_rx.recv() {
                        Ok(packet) => {
                            match decoder.decode(&packet) {
                                Ok(frames) => {
                                    for frame in frames {
                                        // Seek 后帧过滤：跳过太旧的帧
                                        let should_skip = {
                                            let seek_pos_guard = seek_pos.lock().unwrap();
                                            if let Some((seek_target, seek_time)) = *seek_pos_guard {
                                                // 超时检测
                                                if seek_time.elapsed() > Duration::from_secs(2) {
                                                    false // 超时，不再跳过
                                                } else {
                                                    // 跳过太旧的帧（PTS < 目标 - 500ms）
                                                    frame.pts < seek_target - 500
                                                }
                                            } else {
                                                false
                                            }
                                        };
                                        
                                        if should_skip {
                                            debug!("{} 🔊 Seek 后跳过旧音频帧: PTS={}ms", log_ctx(), frame.pts);  
                                            continue;
                                        }
                                        
                                        // 第一帧音频：初始化时钟
                                        if first_audio_flag.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                                            // 使用 frame.pts 初始化时钟（Seek 后时钟已经在 seek() 中设置）
                                            info!("{} 🕐 音频时钟已初始化（首帧 PTS: {} ms）", log_ctx(), frame.pts);
                                            audio_clock.set_time(frame.pts);
                                        }
                                        decoded_frame_count += 1;
                                        if decoded_frame_count <= 5 || decoded_frame_count % 100 == 0 {
                                            info!("{} 🕐 解码音频帧 #{}: PTS={}ms",log_ctx(), decoded_frame_count, frame.pts);
                                        }
                                        audio_fq.push(frame);
                                    }
    
                                    // 音频队列大小控制：通过等待方式做温和背压
                                    if last_seek_time.map(|t| t.elapsed() < SEEK_CLEANUP_DISABLE_DURATION).unwrap_or(false) {
                                        // Seek 后保护期内不额外等待，尽快填充新帧
                                    } else {
                                        let queue_len = audio_fq.len();
                                        if queue_len >= AUDIO_QUEUE_HARD_LIMIT {
                                            let mut backoff = 6u64;
                                            while decode_running.load(Ordering::SeqCst) && audio_fq.len() >= AUDIO_QUEUE_SOFT_LIMIT {
                                                thread::sleep(Duration::from_millis(backoff));
                                                backoff = (backoff + 2).min(15);
                                            }
                                        } else if queue_len >= AUDIO_QUEUE_SOFT_LIMIT {
                                            thread::sleep(Duration::from_millis(4));
                                        }
                                    }
                                }
                                Err(e) => {
                                    match e {
                                        crate::core::error::PlayerError::FFmpegError(ffmpeg::Error::Eof) => {
                                            debug!("{} 🔊 解码器返回 EOF（音频），忽略", log_ctx());
                                        }
                                        crate::core::error::PlayerError::FFmpegError(ffmpeg::Error::Other { errno: 11 }) => {
                                            debug!("{} 🔊 解码器返回 EAGAIN（音频），忽略", log_ctx());
                                        }
                                        _ => {
                                            error!("{} ❌ 音频解码失败: {}", log_ctx(), e);
                                        }
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            info!("{} 🔊 音频解码线程检测到发送端关闭，准备退出", log_ctx());
                            break;
                        }
                    }
                }
    
                info!("{} 🔊 音频解码线程结束", log_ctx());
            }));
        }
    
        // 字幕：暂未改动（和你原来一致）
        if let Some(_decoder) = subtitle_decoder {
            warn!("{} ⚠️  DemuxerThread 模式暂不支持字幕解码", log_ctx());
        }
    
        // 音频输出在主线程中处理（保持原逻辑）
        if self.audio_output.is_some() {
            info!("{} 🔊 音频输出已准备（DemuxerThread 模式）", log_ctx());
        }
    
        info!("{} ✅ 所有播放线程已启动（DemuxerThread 模式）", log_ctx());
    }    
    
    /// 打开网络流
    fn open_stream(&mut self, url: &str, protocol: StreamProtocol) -> Result<MediaInfo> {
        info!("📡 打开网络流: {} (协议: {})", url, protocol.as_str());
        
        // 停止当前播放
        self.stop();
        
        // 标记为网络源
        self.is_network_source.store(true, Ordering::SeqCst);
        
        // 重置首次音频帧标志
        self.is_first_audio_frame.store(true, Ordering::SeqCst);
        
        // 重置 seek 位置
        {
            let mut seek_pos = self.seek_position.lock().unwrap();
            *seek_pos = None;
        }
        
        // 更新状态
        {
            let mut state = self.state.lock().unwrap();
            state.state = PlaybackState::Opening;
        }
        
        // 保存 URL（用于停止后重新播放）
        {
            let mut file_path = self.current_file_path.lock().unwrap();
            *file_path = Some(url.to_string());
        }
        
        // 创建网络流管理器
        let mut stream_manager = NetworkStreamManager::new(url.to_string(), protocol);
        
        // 连接到流
        stream_manager.connect()?;
        
        // 更新流状态
        {
            let state = stream_manager.get_state();
            let mut self_stream_state = self.stream_state.write().unwrap();
            *self_stream_state = Some(state);
        }
        
        // 从流管理器获取 FFmpeg 输入上下文
        // 注意：这里我们需要直接使用 FFmpeg 的输入上下文，类似于 Demuxer
        // 但网络流不能使用本地文件的 Demuxer，需要直接处理
        
        // 创建一个临时的 Demuxer 来包装网络流
        // FFmpeg 会自动处理网络协议
        let demuxer = Demuxer::open(url)?;
        let media_info = demuxer.get_media_info()?;
        
        info!("网络流媒体信息: {:?}", media_info);
        
        // 更新状态
        {
            let mut state = self.state.lock().unwrap();
            state.duration = media_info.duration;
            state.media_info = Some(media_info.clone());
            state.state = PlaybackState::Paused;
        }
        
        // 创建视频解码器
        let video_decoder = if let Some(stream) = demuxer.video_stream() {
            match VideoDecoder::from_stream(stream) {
                Ok(decoder) => {
                    info!("视频解码器: {}", decoder.info());
                    if decoder.is_hardware_accelerated() {
                        info!("✓ 硬件加速已启用");
                    }
                    Some(decoder)
                }
                Err(e) => {
                    info!("硬件解码不可用: {}, 回退到软件解码", e);
                    let stream = demuxer.video_stream().unwrap();
                    let decoder = VideoDecoder::from_stream_software(stream)?;
                    info!("✓ 使用软件解码");
                    Some(decoder)
                }
            }
        } else {
            None
        };
        
        // 创建音频输出（先创建，获取实际配置）
        self.audio_output = if media_info.audio_codec != "none" {
            match AudioOutput::new(media_info.sample_rate, media_info.channels) {
                Ok(mut output) => {
                    output.start()?;
                    Some(output)
                }
                Err(e) => {
                    error!("{} 创建音频输出失败: {}", log_ctx(), e);
                    None
                }
            }
        } else {
            None
        };
        
        // 获取音频输出的实际配置（用于解码器）
        let (actual_sample_rate, actual_channels) = if let Some(ref output) = self.audio_output {
            output.get_config()
        } else {
            (48000, 2) // 默认配置
        };
        
        // 创建音频解码器（使用音频输出的实际配置）
        let audio_decoder = if let Some(stream) = demuxer.audio_stream() {
            Some(AudioDecoder::from_stream_with_config(
                stream,
                actual_sample_rate,
                actual_channels,
            )?)
        } else {
            None
        };
        
        // 创建字幕解码器
        let subtitle_decoder = if let Some(stream) = demuxer.subtitle_stream() {
            match SubtitleDecoder::from_stream(stream) {
                Ok(decoder) => {
                    info!("字幕解码器创建成功");
                    Some(decoder)
                }
                Err(e) => {
                    warn!("{} 创建字幕解码器失败: {}，继续播放（无字幕）", log_ctx(), e);
                    None
                }
            }
        } else {
            None
        };
        
        // 网络流不支持外部字幕
        
        // 保存网络流管理器
        self.network_stream = Some(stream_manager);
        
        // 启动播放线程
        self.start_playback_threads(
            demuxer,
            video_decoder,
            audio_decoder,
            subtitle_decoder,
        );
        
        Ok(media_info)
    }
    
    /// 获取网络流状态（供 UI 使用）
    pub fn get_stream_state(&self) -> Option<StreamState> {
        self.stream_state.read().ok()?.clone()
    }
    
    /// 检查是否正在播放网络流
    pub fn is_network_stream(&self) -> bool {
        self.network_stream.is_some()
    }
}

impl Default for PlaybackManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PlaybackManager {
    fn drop(&mut self) {
        // 发送停止信号
        self.running.store(false, Ordering::SeqCst);
        
        // 等待线程结束
        if let Some(thread) = self.demux_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.video_decode_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.audio_decode_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.subtitle_decode_thread.take() {
            let _ = thread.join();
        }
    }
}

