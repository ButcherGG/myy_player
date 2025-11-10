use thiserror::Error;

#[derive(Error, Debug)]
pub enum PlayerError {
    #[error("FFmpeg 错误: {0}")]
    FFmpegError(#[from] ffmpeg_next::Error),

    #[error("IO 错误: {0}")]
    IoError(#[from] std::io::Error),

    #[error("无法打开文件: {0}")]
    OpenError(String),

    #[error("无法找到视频流")]
    NoVideoStream,

    #[error("无法找到音频流")]
    NoAudioStream,

    #[error("解码错误: {0}")]
    DecodeError(String),

    #[error("渲染错误: {0}")]
    RenderError(String),

    #[error("音频输出错误: {0}")]
    AudioError(String),

    #[error("网络错误: {0}")]
    NetworkError(String),

    #[error("其他错误: {0}")]
    Other(String),

    #[error("Anyhow 错误: {0}")]
    AnyhowError(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, PlayerError>;

