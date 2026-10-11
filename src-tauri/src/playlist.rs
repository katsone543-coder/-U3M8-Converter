use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Playlist {
    pub path: PathBuf,
    pub segments: Vec<PathBuf>,
    pub declared_duration: f64,
}

pub fn parse_playlist(path: &Path) -> Result<Playlist, String> {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"))
    {
        return Err("请选择 .m3u8 文件。".into());
    }
    let path = path
        .canonicalize()
        .map_err(|_| "无法读取所选播放列表。".to_string())?;
    let base = path.parent().ok_or("无法确定播放列表所在文件夹。")?;
    let content =
        fs::read_to_string(&path).map_err(|_| "播放列表不是可读取的 UTF-8 文本。".to_string())?;
    let mut lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err("文件不是有效的 M3U8 播放列表。".into());
    }

    let mut segments = Vec::new();
    let mut duration = 0.0;
    let mut pending_duration = None;
    let mut ended = false;
    for line in lines {
        if ended {
            return Err("播放列表结束标记后仍有内容，无法安全转换。".into());
        }
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            if pending_duration.is_some() {
                return Err("播放列表的片段时长标记不完整。".into());
            }
            let seconds: f64 = value
                .split(',')
                .next()
                .unwrap_or("")
                .parse()
                .map_err(|_| "播放列表包含无效的片段时长。".to_string())?;
            if !seconds.is_finite() || seconds < 0.0 {
                return Err("播放列表包含无效的片段时长。".into());
            }
            pending_duration = Some(seconds);
        } else if line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if line.starts_with("#EXT-X-") {
            let tag = line.split(':').next().unwrap_or(line);
            if !matches!(
                tag,
                "#EXT-X-VERSION"
                    | "#EXT-X-TARGETDURATION"
                    | "#EXT-X-MEDIA-SEQUENCE"
                    | "#EXT-X-PLAYLIST-TYPE"
                    | "#EXT-X-INDEPENDENT-SEGMENTS"
            ) {
                return Err(format!("暂不支持播放列表标签：{tag}。"));
            }
        } else if line.starts_with('#') {
            return Err(format!(
                "暂不支持播放列表标签：{}。",
                line.split(':').next().unwrap_or(line)
            ));
        } else {
            let seconds = pending_duration
                .take()
                .ok_or("片段缺少 #EXTINF 时长标记。")?;
            if line.contains(['?', '#', '\\']) || line.contains("://") || line.contains('%') {
                return Err("片段路径包含 URL、参数或转义字符，暂不支持。".into());
            }
            let relative = Path::new(line);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err("片段路径必须是播放列表所在文件夹内的相对路径。".into());
            }
            if !relative
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
            {
                return Err("V0.1 仅支持本地 .ts 视频片段。".into());
            }
            let segment = base
                .join(relative)
                .canonicalize()
                .map_err(|_| format!("找不到视频片段：{line}。"))?;
            if !segment.starts_with(base) || !segment.is_file() {
                return Err(format!("片段超出播放列表文件夹或不是文件：{line}。"));
            }
            segments.push(segment);
            duration += seconds;
        }
    }
    if pending_duration.is_some() || segments.is_empty() || !ended {
        return Err("播放列表不完整：需要视频片段和 #EXT-X-ENDLIST 结束标记。".into());
    }
    Ok(Playlist {
        path,
        segments,
        declared_duration: duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn accepts_subdirectory_and_rejects_missing_segment() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("parts")).unwrap();
        fs::File::create(dir.path().join("parts/a.ts")).unwrap();
        let list = dir.path().join("video.m3u8");
        let mut file = fs::File::create(&list).unwrap();
        write!(file, "#EXTM3U\n#EXTINF:2,\nparts/a.ts\n#EXT-X-ENDLIST\n").unwrap();
        let parsed = parse_playlist(&list).unwrap();
        assert_eq!(parsed.segments.len(), 1);
        assert_eq!(parsed.declared_duration, 2.0);
        fs::remove_file(dir.path().join("parts/a.ts")).unwrap();
        assert!(parse_playlist(&list)
            .unwrap_err()
            .contains("找不到视频片段"));
    }

    #[test]
    fn rejects_unsupported_and_unsafe_playlists() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("video.m3u8");
        for contents in [
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nother.m3u8\n#EXT-X-ENDLIST\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n",
            "#EXTM3U\n#EXTINF:2,\n../a.ts\n#EXT-X-ENDLIST\n",
            "#EXTM3U\n#EXTINF:2,\nhttps://example.com/a.ts\n#EXT-X-ENDLIST\n",
            "#EXTM3U\n#EXTINF:2,\na.ts\n",
        ] {
            fs::write(&list, contents).unwrap();
            assert!(
                parse_playlist(&list).is_err(),
                "unexpectedly accepted {contents}"
            );
        }
    }
}
