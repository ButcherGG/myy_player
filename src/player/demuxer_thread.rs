use crate::core::Result;
use crate::player::demuxer_source::DemuxerSource;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use ffmpeg_next as ffmpeg;
use log::{error, info, warn};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::process;

fn log_ctx() -> String {
    format!("[pid:{} tid:{:?}]", process::id(), thread::current().id())
}

/// Demuxer 线程命令
pub enum DemuxerCommand {
    Seek(i64), // ms
    Stop,
}

/// Demuxer 线程管理器
/// - packet 的传递从无界 SegQueue 改为有界 channel (Sender/Receiver)
/// - start() 返回的结构体保留接收端 (Receiver)，供解码线程使用
pub struct DemuxerThread {
    thread_handle: Option<JoinHandle<()>>,
    command_tx: Sender<DemuxerCommand>,

    // 保留发送端的 clone，stop() 会 drop 它们以让接收端退出
    video_packet_tx: Option<Sender<ffmpeg::Packet>>,
    audio_packet_tx: Option<Sender<ffmpeg::Packet>>,

    // 外部读包端（接收端），供解码线程使用（替代原先的 SegQueue）
    // 使用 Option 以便可以取出
    pub video_packet_queue: Option<Receiver<ffmpeg::Packet>>,
    pub audio_packet_queue: Option<Receiver<ffmpeg::Packet>>,
}

impl DemuxerThread {
    /// 启动 Demuxer 线程
    /// VIDEO_CAPACITY / AUDIO_CAPACITY 可调：根据目标缓冲时间（秒）与典型 bitrate 估算 packet 数
    pub fn start(mut demuxer_source: Box<dyn DemuxerSource>) -> Self {
        // 命令通道（unbounded 足够）
        let (command_tx, command_rx) = unbounded::<DemuxerCommand>();

        // 有界 packet 通道（背压）
        // 优化：减小容量，让背压更早生效，避免过度缓冲
        // 视频：200 packets ≈ 8秒（25fps），足够缓冲且及时背压
        // 音频：150 packets ≈ 3秒（48kHz），足够缓冲且及时背压
        const VIDEO_CAPACITY: usize = 200;
        const AUDIO_CAPACITY: usize = 150;

        let (video_tx, video_rx) = bounded::<ffmpeg::Packet>(VIDEO_CAPACITY);
        let (audio_tx, audio_rx) = bounded::<ffmpeg::Packet>(AUDIO_CAPACITY);

        // 为了在 stop() 时可以 drop 发送端，我们在结构体里保留一份 Sender clone
        let video_tx_clone_for_struct = video_tx.clone();
        let audio_tx_clone_for_struct = audio_tx.clone();

        // 启动线程：把 Sender (video_tx, audio_tx) 移动到线程中作为写端
        let thread_handle = thread::spawn(move || {
            Self::demux_loop(&mut *demuxer_source, command_rx, video_tx, audio_tx);
        });

        Self {
            thread_handle: Some(thread_handle),
            command_tx,
            video_packet_tx: Some(video_tx_clone_for_struct),
            audio_packet_tx: Some(audio_tx_clone_for_struct),
            video_packet_queue: Some(video_rx),
            audio_packet_queue: Some(audio_rx),
        }
    }

    /// Demuxer 循环（在独立线程中运行）
    ///
    /// 关键点：
    /// - 使用 send() 将 packet 发到有界通道。当通道满时 send() 会阻塞，从而自然背压。
    /// - 处理命令使用 try_recv()（非阻塞），以保证尽快响应 Seek/Stop。
    fn demux_loop(
        demuxer: &mut dyn DemuxerSource,
        command_rx: Receiver<DemuxerCommand>,
        video_tx: Sender<ffmpeg::Packet>,
        audio_tx: Sender<ffmpeg::Packet>,
    ) {
        info!("{} 🎬 Demuxer 线程启动: {}", log_ctx(), demuxer.description());

        let mut running = true;
        let mut packet_count: usize = 0;
        let mut video_packet_count: usize = 0;
        let mut audio_packet_count: usize = 0;

        // 阈值（仅用于日志 & startup buffering 判断）
        const LOG_FIRST_N: usize = 5;

        while running {
            // 优先处理所有命令（非阻塞）
            loop {
                match command_rx.try_recv() {
                    Ok(cmd) => {
                        match cmd {
                            DemuxerCommand::Seek(timestamp_ms) => {
                                info!("{} ⏩ Demuxer 线程收到 Seek 命令: {}ms", log_ctx(), timestamp_ms);
                                
                                // 清空 packet channel 中的旧包，避免解码线程处理旧数据
                                // 注意：这里只能清空发送端，接收端会在解码线程中自然消费完
                                // 实际的清空需要通过背压机制：让 channel 阻塞，然后在解码线程中跳过旧包
                                // 更好的方法是：在 Seek 后，解码线程会跳过旧包，这里只需要执行 seek
                                
                                if let Err(e) = demuxer.seek(timestamp_ms) {
                                    error!("{} ❌ Seek 失败: {}", log_ctx(), e);
                                } else {
                                    info!("{} 🧹 Seek 成功（Demuxer 已 Seek），请在解码端清空并 flush 解码器", log_ctx());
                                    // 注意：packet channel 中的旧包会在解码线程中被跳过（通过 seek_pos 过滤）
                                    // 不需要在这里清空 channel，因为 channel 是有界的，新包会自然填充
                                }
                            }
                            DemuxerCommand::Stop => {
                                info!("{} ⏹ Demuxer 线程收到停止命令", log_ctx());
                                running = false;
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        // 没有更多命令
                        break;
                    }
                }
            }

            if !running {
                break;
            }

            // 读取包（阻塞返回 None 表示 EOF）
            match demuxer.read_packet() {
                Ok(Some(media_packet)) => {
                    packet_count += 1;

                    match media_packet.packet_type {
                        crate::player::demuxer_source::PacketType::Video => {
                            video_packet_count += 1;
                            if video_packet_count <= LOG_FIRST_N || video_packet_count % 100 == 0 {
                                info!("{} 📦 Demuxer 读取视频包 #{}（total packets {}）", log_ctx(), video_packet_count, packet_count);
                            }

                            // 发送到视频通道（send 会在通道满时阻塞，起到背压）
                            if let Err(_e) = video_tx.send(media_packet.packet) {
                                error!("{} ❌ 发送视频包失败，接收端可能已关闭", log_ctx());
                                break;
                            }
                        }
                        crate::player::demuxer_source::PacketType::Audio => {
                            audio_packet_count += 1;
                            if audio_packet_count <= LOG_FIRST_N || audio_packet_count % 100 == 0 {
                                info!("{} 🔊 Demuxer 读取音频包 #{}（total packets {}）", log_ctx(), audio_packet_count, packet_count);
                            }

                            if let Err(_e) = audio_tx.send(media_packet.packet) {
                                error!("{} ❌ 发送音频包失败，接收端可能已关闭", log_ctx());
                                break;
                            }
                        }
                        _ => {
                            // 忽略字幕/数据包
                        }
                    }
                }
                Ok(None) => {
                    // 到达 EOF：保持线程存活，等待 Seek/Stop
                    info!("{} 📄 Demuxer 到达文件末尾，等待命令（Seek/Stop）...", log_ctx());
                    // 不忙等：短睡眠，避免 CPU 空转
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e) => {
                    error!("{} ❌ 读取包失败: {}", log_ctx(), e);
                    break;
                }
            }
        }

        info!("{} 🛑 Demuxer 线程退出（共读取 {} 个包：{} 视频，{} 音频）",
              log_ctx(),
              packet_count, video_packet_count, audio_packet_count);
        // 当退出时，发送端 (video_tx/audio_tx) 会被 drop（线程作用域结束），
        // 这样接收端的 recv() 会返回 Err，相关解码线程可以退出。
    }

    /// 发送 Seek 命令
    pub fn seek(&self, timestamp_ms: i64) -> Result<()> {
        self.command_tx
            .send(DemuxerCommand::Seek(timestamp_ms))
            .map_err(|e| crate::core::error::PlayerError::Other(format!("发送 Seek 命令失败: {}", e)))
    }

    /// 暂停读取（占位：若要在 demux 保存 paused 状态，可实现 Pause 命令）
    pub fn pause(&self) -> Result<()> {
        // TODO: 实现 pause/resume 命令处理
        Ok(())
    }

    /// 恢复读取（占位）
    pub fn resume(&self) -> Result<()> {
        Ok(())
    }

    /// 停止线程（可被外部调用）
    /// - 发送 Stop 命令
    /// - drop 发送端（让接收端退出 recv）
    /// - join 线程
    pub fn stop(&mut self) {
        info!("{} 🛑 DemuxerThread::stop() called", log_ctx());
        let _ = self.command_tx.send(DemuxerCommand::Stop);

        // drop the packet senders so receivers get disconnected and recv() returns Err
        self.video_packet_tx.take();
        self.audio_packet_tx.take();

        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
    
    /// 取出接收端（用于传递给解码线程）
    /// 注意：调用此方法后，DemuxerThread 将不再持有 Receiver
    pub fn take_receivers(&mut self) -> (Receiver<ffmpeg::Packet>, Receiver<ffmpeg::Packet>) {
        (
            self.video_packet_queue.take().expect("video_packet_queue already taken"),
            self.audio_packet_queue.take().expect("audio_packet_queue already taken"),
        )
    }
}

impl Drop for DemuxerThread {
    fn drop(&mut self) {
        if self.thread_handle.is_some() {
            warn!("{} ⚠ DemuxerThread 被 drop，但可能未调用 stop()，正在尝试优雅停止", log_ctx());
            let _ = self.command_tx.send(DemuxerCommand::Stop);

            // drop senders
            self.video_packet_tx.take();
            self.audio_packet_tx.take();

            if let Some(handle) = self.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}
