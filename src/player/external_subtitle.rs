use crate::core::{Result, SubtitleFrame};
use log::{info, warn};
use std::fs;
use std::path::{Path, PathBuf};

/// 外部字幕文件解析器
pub struct ExternalSubtitleParser;

impl ExternalSubtitleParser {
    /// 查找与视频文件同目录下的字幕文件
    /// 支持的字幕文件格式：.srt, .ass, .ssa, .vtt
    pub fn find_subtitle_files(video_path: &str) -> Vec<PathBuf> {
        let video_path = Path::new(video_path);
        let mut subtitle_files = Vec::new();

        // 获取视频文件的目录和文件名（不含扩展名）
        if let Some(parent_dir) = video_path.parent() {
            if let Some(file_stem) = video_path.file_stem() {
                let file_stem = file_stem.to_string_lossy();
                
                // 支持的字幕文件扩展名
                let subtitle_extensions = ["srt", "ass", "ssa", "vtt"];
                
                // 方法1: 精确匹配 - video_name.srt, video_name.ass 等
                for ext in &subtitle_extensions {
                    let subtitle_path = parent_dir.join(format!("{}.{}", file_stem, ext));
                    if subtitle_path.exists() {
                        info!("找到精确匹配字幕文件: {}", subtitle_path.display());
                        subtitle_files.push(subtitle_path);
                    }
                }
                
                // 方法2: 语言标识匹配 - video_name.zh.srt, video_name.en.srt
                let language_codes = ["zh", "en", "chs", "cht", "zh-cn", "zh-tw", "ja", "ko", "chs-eng"];
                for lang in &language_codes {
                    for ext in &subtitle_extensions {
                        let subtitle_path = parent_dir.join(format!("{}.{}.{}", file_stem, lang, ext));
                        if subtitle_path.exists() {
                            info!("找到语言标识字幕文件: {}", subtitle_path.display());
                            subtitle_files.push(subtitle_path);
                        }
                    }
                }

                // 方法3: 智能模糊匹配 - 查找同目录下包含相似名称的字幕文件
                if subtitle_files.is_empty() {
                    if let Ok(entries) = std::fs::read_dir(parent_dir) {
                        // 提取视频文件名的关键部分用于匹配
                        let video_keywords = Self::extract_keywords(&file_stem);
                        
                        for entry in entries.flatten() {
                            if let Some(entry_name) = entry.file_name().to_str() {
                                // 检查是否是字幕文件
                                let is_subtitle = subtitle_extensions.iter().any(|ext| {
                                    entry_name.to_lowercase().ends_with(&format!(".{}", ext))
                                });
                                
                                if is_subtitle {
                                    // 检查文件名是否包含视频的关键词
                                    let entry_lower = entry_name.to_lowercase();
                                    let mut match_score = 0;
                                    
                                    for keyword in &video_keywords {
                                        if entry_lower.contains(&keyword.to_lowercase()) {
                                            match_score += 1;
                                        }
                                    }
                                    
                                    // 如果匹配度足够高，认为是对应的字幕文件
                                    if match_score >= (video_keywords.len() / 2).max(1) {
                                        let subtitle_path = entry.path();
                                        info!("找到模糊匹配字幕文件: {} (匹配度: {}/{})", 
                                              subtitle_path.display(), match_score, video_keywords.len());
                                        subtitle_files.push(subtitle_path);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 按优先级排序：精确匹配 > 语言标识 > 模糊匹配
        subtitle_files.sort_by(|a, b| {
            let a_name = a.file_name().unwrap_or_default().to_string_lossy();
            let b_name = b.file_name().unwrap_or_default().to_string_lossy();
            
            // 优先选择包含 "chs" 或 "zh" 的中文字幕
            let a_is_chinese = a_name.contains("chs") || a_name.contains("zh");
            let b_is_chinese = b_name.contains("chs") || b_name.contains("zh");
            
            match (a_is_chinese, b_is_chinese) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a_name.cmp(&b_name),
            }
        });

        subtitle_files
    }

    /// 从文件名中提取关键词用于模糊匹配
    fn extract_keywords(filename: &str) -> Vec<String> {
        let mut keywords = Vec::new();
        
        // 按常见分隔符分割文件名
        let parts: Vec<&str> = filename
            .split(&['.', '-', '_', ' ', '[', ']', '(', ')'])
            .filter(|s| !s.is_empty() && s.len() > 2) // 过滤掉太短的部分
            .collect();
        
        for part in parts {
            let part_lower = part.to_lowercase();
            
            // 跳过常见的无意义词汇
            if !["web", "dl", "ddp", "atmos", "h264", "h265", "mkv", "mp4", "avi", 
                 "1080p", "2160p", "720p", "480p", "bluray", "bdrip", "webrip",
                 "x264", "x265", "aac", "ac3", "dts", "flac", "mp3"].contains(&part_lower.as_str()) {
                keywords.push(part.to_string());
            }
        }
        
        // 如果关键词太少，添加原始文件名的前几个字符
        if keywords.len() < 2 && filename.len() > 10 {
            keywords.push(filename[..10.min(filename.len())].to_string());
        }
        
        keywords
    }

    /// 解析外部字幕文件
    pub fn parse_subtitle_file(file_path: &Path) -> Result<Vec<SubtitleFrame>> {
        let content = fs::read_to_string(file_path)
            .map_err(|e| anyhow::anyhow!("读取字幕文件失败: {}", e))?;

        let extension = file_path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();

        match extension.as_str() {
            "srt" => Self::parse_srt(&content),
            "ass" | "ssa" => Self::parse_ass(&content),
            "vtt" => Self::parse_vtt(&content),
            _ => Err(anyhow::anyhow!("不支持的字幕文件格式: {}", extension).into()),
        }
    }

    /// 解析 SRT 格式字幕
    fn parse_srt(content: &str) -> Result<Vec<SubtitleFrame>> {
        let mut frames = Vec::new();
        let mut current_frame: Option<(i64, i64, String)> = None;
        let mut lines = content.lines();
        let mut line_num = 0;

        while let Some(line) = lines.next() {
            line_num += 1;
            let line = line.trim();

            if line.is_empty() {
                // 空行，完成当前字幕条目
                if let Some((start_pts, end_pts, text)) = current_frame.take() {
                    if !text.trim().is_empty() {
                        frames.push(SubtitleFrame {
                            pts: start_pts,
                            duration: end_pts - start_pts,
                            end_pts,
                            text: text.trim().to_string(),
                        });
                    }
                }
                continue;
            }

            // 尝试解析序号行（忽略）
            if line.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }

            // 尝试解析时间行
            if line.contains("-->") {
                if let Some((start_time, end_time)) = Self::parse_srt_time_line(line) {
                    current_frame = Some((start_time, end_time, String::new()));
                } else {
                    warn!("无法解析 SRT 时间行 (第{}行): {}", line_num, line);
                }
                continue;
            }

            // 文本行
            if let Some((_, _, ref mut text)) = current_frame {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(line);
            }
        }

        // 处理最后一个字幕条目
        if let Some((start_pts, end_pts, text)) = current_frame {
            if !text.trim().is_empty() {
                frames.push(SubtitleFrame {
                    pts: start_pts,
                    duration: end_pts - start_pts,
                    end_pts,
                    text: text.trim().to_string(),
                });
            }
        }

        info!("解析 SRT 字幕完成，共 {} 条字幕", frames.len());
        Ok(frames)
    }

    /// 解析 SRT 时间行：00:01:30,500 --> 00:01:33,400
    fn parse_srt_time_line(line: &str) -> Option<(i64, i64)> {
        let parts: Vec<&str> = line.split("-->").map(|s| s.trim()).collect();
        if parts.len() != 2 {
            return None;
        }

        let start_time = Self::parse_srt_timestamp(parts[0])?;
        let end_time = Self::parse_srt_timestamp(parts[1])?;

        Some((start_time, end_time))
    }

    /// 解析 SRT 时间戳：00:01:30,500 -> 90500ms
    fn parse_srt_timestamp(timestamp: &str) -> Option<i64> {
        // 格式：HH:MM:SS,mmm
        let parts: Vec<&str> = timestamp.split(',').collect();
        if parts.len() != 2 {
            return None;
        }

        let time_part = parts[0];
        let ms_part: i64 = parts[1].parse().ok()?;

        let time_components: Vec<&str> = time_part.split(':').collect();
        if time_components.len() != 3 {
            return None;
        }

        let hours: i64 = time_components[0].parse().ok()?;
        let minutes: i64 = time_components[1].parse().ok()?;
        let seconds: i64 = time_components[2].parse().ok()?;

        Some(hours * 3600000 + minutes * 60000 + seconds * 1000 + ms_part)
    }

    /// 解析 ASS/SSA 格式字幕（简化版本）
    fn parse_ass(content: &str) -> Result<Vec<SubtitleFrame>> {
        let mut frames = Vec::new();
        let mut in_events_section = false;

        for line in content.lines() {
            let line = line.trim();

            // 检查是否进入 Events 段
            if line.eq_ignore_ascii_case("[Events]") {
                in_events_section = true;
                continue;
            }

            // 检查是否离开 Events 段
            if line.starts_with('[') && line.ends_with(']') && !line.eq_ignore_ascii_case("[Events]") {
                in_events_section = false;
                continue;
            }

            // 只处理 Events 段中的 Dialogue 行
            if in_events_section && line.starts_with("Dialogue:") {
                if let Some(frame) = Self::parse_ass_dialogue_line(line) {
                    frames.push(frame);
                }
            }
        }

        info!("解析 ASS 字幕完成，共 {} 条字幕", frames.len());
        Ok(frames)
    }

    /// 解析 ASS Dialogue 行
    fn parse_ass_dialogue_line(line: &str) -> Option<SubtitleFrame> {
        // Dialogue: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text
        let parts: Vec<&str> = line.splitn(10, ',').collect();
        if parts.len() < 10 {
            return None;
        }

        let start_time = Self::parse_ass_timestamp(parts[1].trim())?;
        let end_time = Self::parse_ass_timestamp(parts[2].trim())?;
        let text = parts[9].trim();

        // 清理 ASS 标签
        let cleaned_text = Self::clean_ass_text(text);

        if !cleaned_text.trim().is_empty() {
            Some(SubtitleFrame {
                pts: start_time,
                duration: end_time - start_time,
                end_pts: end_time,
                text: cleaned_text,
            })
        } else {
            None
        }
    }

    /// 解析 ASS 时间戳：0:01:30.50 -> 90500ms
    fn parse_ass_timestamp(timestamp: &str) -> Option<i64> {
        // 格式：H:MM:SS.cc
        let parts: Vec<&str> = timestamp.split('.').collect();
        if parts.len() != 2 {
            return None;
        }

        let time_part = parts[0];
        let centiseconds: i64 = parts[1].parse().ok()?;

        let time_components: Vec<&str> = time_part.split(':').collect();
        if time_components.len() != 3 {
            return None;
        }

        let hours: i64 = time_components[0].parse().ok()?;
        let minutes: i64 = time_components[1].parse().ok()?;
        let seconds: i64 = time_components[2].parse().ok()?;

        Some(hours * 3600000 + minutes * 60000 + seconds * 1000 + centiseconds * 10)
    }

    /// 清理 ASS 文本标签
    fn clean_ass_text(text: &str) -> String {
        let mut result = String::new();
        let mut in_tag = false;
        let mut chars = text.chars();

        while let Some(ch) = chars.next() {
            match ch {
                '{' => in_tag = true,
                '}' => in_tag = false,
                '\\' if in_tag => {
                    // 跳过转义序列
                    if let Some(next_ch) = chars.next() {
                        match next_ch {
                            'N' | 'n' => {
                                if !in_tag {
                                    result.push('\n');
                                }
                            }
                            _ => {} // 忽略其他转义序列
                        }
                    }
                }
                _ if !in_tag => result.push(ch),
                _ => {} // 在标签内，忽略
            }
        }

        result.trim().to_string()
    }

    /// 解析 WebVTT 格式字幕（简化版本）
    fn parse_vtt(content: &str) -> Result<Vec<SubtitleFrame>> {
        let mut frames = Vec::new();
        let mut lines = content.lines();
        let mut line_num = 0;

        // 跳过 WEBVTT 头部
        if let Some(first_line) = lines.next() {
            line_num += 1;
            if !first_line.trim().starts_with("WEBVTT") {
                warn!("VTT 文件缺少 WEBVTT 头部");
            }
        }

        let mut current_frame: Option<(i64, i64, String)> = None;

        while let Some(line) = lines.next() {
            line_num += 1;
            let line = line.trim();

            if line.is_empty() {
                // 空行，完成当前字幕条目
                if let Some((start_pts, end_pts, text)) = current_frame.take() {
                    if !text.trim().is_empty() {
                        frames.push(SubtitleFrame {
                            pts: start_pts,
                            duration: end_pts - start_pts,
                            end_pts,
                            text: text.trim().to_string(),
                        });
                    }
                }
                continue;
            }

            // 尝试解析时间行
            if line.contains("-->") {
                if let Some((start_time, end_time)) = Self::parse_vtt_time_line(line) {
                    current_frame = Some((start_time, end_time, String::new()));
                } else {
                    warn!("无法解析 VTT 时间行 (第{}行): {}", line_num, line);
                }
                continue;
            }

            // 跳过 NOTE 和其他指令
            if line.starts_with("NOTE") || line.starts_with("STYLE") {
                continue;
            }

            // 文本行
            if let Some((_, _, ref mut text)) = current_frame {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(line);
            }
        }

        // 处理最后一个字幕条目
        if let Some((start_pts, end_pts, text)) = current_frame {
            if !text.trim().is_empty() {
                frames.push(SubtitleFrame {
                    pts: start_pts,
                    duration: end_pts - start_pts,
                    end_pts,
                    text: text.trim().to_string(),
                });
            }
        }

        info!("解析 VTT 字幕完成，共 {} 条字幕", frames.len());
        Ok(frames)
    }

    /// 解析 VTT 时间行：00:01:30.500 --> 00:01:33.400
    fn parse_vtt_time_line(line: &str) -> Option<(i64, i64)> {
        let parts: Vec<&str> = line.split("-->").map(|s| s.trim()).collect();
        if parts.len() != 2 {
            return None;
        }

        let start_time = Self::parse_vtt_timestamp(parts[0])?;
        let end_time = Self::parse_vtt_timestamp(parts[1])?;

        Some((start_time, end_time))
    }

    /// 解析 VTT 时间戳：00:01:30.500 -> 90500ms
    fn parse_vtt_timestamp(timestamp: &str) -> Option<i64> {
        // 格式：HH:MM:SS.mmm 或 MM:SS.mmm
        let parts: Vec<&str> = timestamp.split('.').collect();
        if parts.len() != 2 {
            return None;
        }

        let time_part = parts[0];
        let ms_part: i64 = parts[1].parse().ok()?;

        let time_components: Vec<&str> = time_part.split(':').collect();
        
        match time_components.len() {
            2 => {
                // MM:SS 格式
                let minutes: i64 = time_components[0].parse().ok()?;
                let seconds: i64 = time_components[1].parse().ok()?;
                Some(minutes * 60000 + seconds * 1000 + ms_part)
            }
            3 => {
                // HH:MM:SS 格式
                let hours: i64 = time_components[0].parse().ok()?;
                let minutes: i64 = time_components[1].parse().ok()?;
                let seconds: i64 = time_components[2].parse().ok()?;
                Some(hours * 3600000 + minutes * 60000 + seconds * 1000 + ms_part)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_srt_timestamp() {
        assert_eq!(ExternalSubtitleParser::parse_srt_timestamp("00:01:30,500"), Some(90500));
        assert_eq!(ExternalSubtitleParser::parse_srt_timestamp("01:23:45,123"), Some(5025123));
    }

    #[test]
    fn test_parse_ass_timestamp() {
        assert_eq!(ExternalSubtitleParser::parse_ass_timestamp("0:01:30.50"), Some(90500));
        assert_eq!(ExternalSubtitleParser::parse_ass_timestamp("1:23:45.12"), Some(5025120));
    }

    #[test]
    fn test_clean_ass_text() {
        assert_eq!(ExternalSubtitleParser::clean_ass_text("{\\b1}Hello{\\b0} World"), "Hello World");
        assert_eq!(ExternalSubtitleParser::clean_ass_text("Line 1\\NLine 2"), "Line 1\nLine 2");
    }
}
