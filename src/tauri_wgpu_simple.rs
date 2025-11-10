/// 简化的 Tauri + wgpu 集成
/// 避免复杂的生命周期问题，使用更直接的方法

use crate::core::VideoFrame;
use log::info;
use std::sync::{Arc, Mutex};

/// 简化的 Tauri 应用状态
#[derive(Clone)]
pub struct TauriAppState {
    pub player: Arc<Mutex<crate::player::manager::PlaybackManager>>,
    pub current_frame: Arc<Mutex<Option<VideoFrame>>>,
}

impl TauriAppState {
    pub fn new() -> Self {
        Self {
            player: Arc::new(Mutex::new(crate::player::manager::PlaybackManager::new())),
            current_frame: Arc::new(Mutex::new(None)),
        }
    }
}

/// 启动渲染循环 - 将视频帧数据传递给前端
pub fn start_render_loop(app_state: Arc<TauriAppState>) {
    std::thread::spawn(move || {
        info!("启动简化渲染循环...");
        
        loop {
            // 检查播放状态
            let is_playing = {
                let player = app_state.player.lock().unwrap();
                let state = player.get_state();
                state.state == crate::core::PlaybackState::Playing
            };
            
            if is_playing {
                // 获取视频帧
                let frame = {
                    let player = app_state.player.lock().unwrap();
                    player.get_video_frame()
                };
                
                // 更新当前帧（每次都尝试获取最新帧）
                if let Some(frame) = frame {
                    let mut current_frame = app_state.current_frame.lock().unwrap();
                    *current_frame = Some(frame.clone());
                    // 释放锁后短暂休眠，让前端有机会读取
                    drop(current_frame);
                    
                    // 调试信息：显示帧更新
                    log::debug!("更新渲染帧: PTS={}ms", frame.pts);
                    std::thread::sleep(std::time::Duration::from_millis(16)); // ~60fps
                } else {
                    // 没有新帧时稍微休眠长一点
                    log::debug!("没有可用的视频帧，等待...");
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            } else {
                // 不在播放时休眠更长时间
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    });
}

/// 获取当前视频帧 (用于前端渲染)
#[tauri::command]
pub fn get_current_video_frame(state: tauri::State<TauriAppState>) -> std::result::Result<Option<VideoFrame>, String> {
    let current_frame = state.current_frame.lock().unwrap();
    Ok(current_frame.clone())
}

/// 获取队列状态 (调试用)
#[tauri::command]
pub fn get_queue_status(state: tauri::State<TauriAppState>) -> std::result::Result<String, String> {
    let _player = state.player.lock().unwrap();
    // 这里我们需要添加一个方法来获取队列状态
    Ok("队列状态监控".to_string())
}
