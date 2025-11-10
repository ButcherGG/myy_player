use crate::core::{AudioFrame, PlayerError, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig, SupportedStreamConfigRange};
use crossbeam::queue::SegQueue;
use log::{debug, info, warn};
use std::sync::{Arc, Mutex};

/// 音频输出 - 使用 cpal 播放音频
pub struct AudioOutput {
    device: Device,
    config: StreamConfig,
    stream: Option<Stream>,
    buffer: Arc<SegQueue<f32>>,
    volume: Arc<Mutex<f32>>,
}

// cpal::Stream 本身不是 Send，但在 PlaybackManager 中我们确保它只在创建它的线程中使用
// PlaybackManager 在 Tauri 的主线程中创建和使用，不会跨线程传递
unsafe impl Send for AudioOutput {}

impl AudioOutput {
    /// 创建音频输出（支持非标准配置自动回退）
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self> {
        info!("初始化音频输出: {} Hz, {} 声道", sample_rate, channels);

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| PlayerError::AudioError("无法找到音频输出设备".to_string()))?;

        debug!("使用音频设备: {}", device.name().unwrap_or_default());

        // 尝试使用请求的配置
        let mut config = StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // 检查设备是否支持该配置，如果不支持则回退到标准配置
        let supported_configs = device.supported_output_configs()
            .map_err(|e| PlayerError::AudioError(format!("无法获取支持的音频配置: {}", e)))?;

        let mut is_supported = false;
        for supported_config in supported_configs {
            if Self::is_config_compatible(&config, &supported_config) {
                is_supported = true;
                break;
            }
        }

        // 如果不支持，回退到标准配置
        if !is_supported {
            warn!("⚠️  音频设备不支持 {} Hz, {} 声道配置，回退到标准配置", sample_rate, channels);
            
            // 尝试标准配置（48000 Hz, 2 声道）
            let fallback_configs = vec![
                (48000, 2),  // 最常见
                (44100, 2),  // CD 音质
                (48000, 1),  // 单声道高质量
                (44100, 1),  // 单声道 CD 质量
                (22050, 1),  // 原配置单声道（可能支持）
                (22050, 2),  // 原采样率立体声
            ];

            let mut found_fallback = false;
            for (fb_rate, fb_channels) in fallback_configs {
                let fb_config = StreamConfig {
                    channels: fb_channels,
                    sample_rate: cpal::SampleRate(fb_rate),
                    buffer_size: cpal::BufferSize::Default,
                };

                // 重新检查支持的配置
                let supported_configs = device.supported_output_configs()
                    .map_err(|e| PlayerError::AudioError(format!("无法获取支持的音频配置: {}", e)))?;

                for supported_config in supported_configs {
                    if Self::is_config_compatible(&fb_config, &supported_config) {
                        info!("✅ 使用回退配置: {} Hz, {} 声道", fb_rate, fb_channels);
                        config = fb_config;
                        found_fallback = true;
                        break;
                    }
                }

                if found_fallback {
                    break;
                }
            }

            if !found_fallback {
                return Err(PlayerError::AudioError(
                    format!("音频设备不支持任何标准配置 (原请求: {} Hz, {} 声道)", sample_rate, channels)
                ));
            }
        }

        Ok(Self {
            device,
            config,
            stream: None,
            buffer: Arc::new(SegQueue::new()),
            volume: Arc::new(Mutex::new(1.0)),
        })
    }

    /// 检查配置是否兼容
    fn is_config_compatible(config: &StreamConfig, supported: &SupportedStreamConfigRange) -> bool {
        let rate_in_range = config.sample_rate.0 >= supported.min_sample_rate().0
            && config.sample_rate.0 <= supported.max_sample_rate().0;
        
        let channels_match = config.channels == supported.channels();
        
        rate_in_range && channels_match
    }

    /// 开始播放
    pub fn start(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }

        let buffer = self.buffer.clone();
        let volume = self.volume.clone();

        let stream = self
            .device
            .build_output_stream(
                &self.config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let vol = *volume.lock().unwrap();
                    for sample in data.iter_mut() {
                        if let Some(value) = buffer.pop() {
                            *sample = value * vol;
                        } else {
                            *sample = 0.0;
                        }
                    }
                },
                move |err| {
                    eprintln!("音频流错误: {}", err);
                },
                None,
            )
            .map_err(|e| PlayerError::AudioError(format!("创建音频流失败: {}", e)))?;

        stream
            .play()
            .map_err(|e| PlayerError::AudioError(format!("启动音频流失败: {}", e)))?;

        self.stream = Some(stream);
        info!("音频输出已启动");

        Ok(())
    }

    /// 停止播放
    pub fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            drop(stream);
            info!("音频输出已停止");
        }
    }

    /// 写入音频帧
    pub fn write_frame(&self, frame: &AudioFrame) {
        for sample in &frame.data {
            self.buffer.push(*sample);
        }
    }

    /// 设置音量 (0.0 - 1.0)
    pub fn set_volume(&self, volume: f32) {
        *self.volume.lock().unwrap() = volume.clamp(0.0, 1.0);
    }

    /// 获取缓冲区大小（采样数）
    pub fn buffer_size(&self) -> usize {
        self.buffer.len()
    }

    /// 清空缓冲区
    pub fn clear_buffer(&self) {
        while self.buffer.pop().is_some() {}
    }
    
    /// 获取实际使用的音频配置
    pub fn get_config(&self) -> (u32, u16) {
        (self.config.sample_rate.0, self.config.channels)
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.stop();
    }
}

