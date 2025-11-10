use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 播放时钟 - 用于音视频同步
#[derive(Clone)]
pub struct PlaybackClock {
    inner: Arc<Mutex<ClockInner>>,
}

struct ClockInner {
    base_pts: i64,              // 基准 PTS（毫秒）
    base_instant: Instant,      // 基准时刻
    playback_rate: f64,         // 播放速率（1.0 = 正常）
    paused: bool,
    paused_at: i64,             // 暂停时的位置
}

impl PlaybackClock {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ClockInner {
                base_pts: 0,
                base_instant: Instant::now(),
                playback_rate: 1.0,
                paused: true,
                paused_at: 0,
            })),
        }
    }

    /// 获取当前播放时间（毫秒）
    pub fn now(&self) -> i64 {
        let inner = self.inner.lock().unwrap();
        if inner.paused {
            inner.paused_at
        } else {
            let elapsed = inner.base_instant.elapsed().as_millis() as i64;
            inner.base_pts + (elapsed as f64 * inner.playback_rate) as i64
        }
    }

    /// 设置播放位置
    pub fn set_time(&self, pts: i64) {
        let mut inner = self.inner.lock().unwrap();
        inner.base_pts = pts;
        inner.base_instant = Instant::now();
        inner.paused_at = pts;
    }

    /// 开始播放
    pub fn play(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.paused {
            inner.base_pts = inner.paused_at;
            inner.base_instant = Instant::now();
            inner.paused = false;
        }
    }

    /// 暂停播放
    pub fn pause(&self) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.paused {
            inner.paused_at = self.now_unlocked(&inner);
            inner.paused = true;
        }
    }

    /// 设置播放速率
    pub fn set_rate(&self, rate: f64) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.paused {
            let current_time = self.now_unlocked(&inner);
            inner.base_pts = current_time;
            inner.base_instant = Instant::now();
        }
        inner.playback_rate = rate;
    }

    /// 是否暂停
    pub fn is_paused(&self) -> bool {
        self.inner.lock().unwrap().paused
    }

    fn now_unlocked(&self, inner: &ClockInner) -> i64 {
        if inner.paused {
            inner.paused_at
        } else {
            let elapsed = inner.base_instant.elapsed().as_millis() as i64;
            inner.base_pts + (elapsed as f64 * inner.playback_rate) as i64
        }
    }
}

impl Default for PlaybackClock {
    fn default() -> Self {
        Self::new()
    }
}

