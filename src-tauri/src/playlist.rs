use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Playlist {
    pub path: PathBuf,
    pub segments: Vec<PathBuf>,
    pub declared_duration: f64,
    pub encryption: Option<Encryption>,
}

#[derive(Debug, Clone)]
pub struct Encryption {
    pub key_path: PathBuf,
    pub iv: Option<[u8; 16]>,
    pub media_sequence: u64,
}

impl Encryption {
    pub fn iv_for_segment(&self, index: usize) -> [u8; 16] {
        self.iv
            .unwrap_or_else(|| (u128::from(self.media_sequence) + index as u128).to_be_bytes())
    }
}

fn local_path(base: &Path, value: &str, label: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.contains(['?', '#', '\\', '%', ':']) {
        return Err(format!("{label}必须是文件夹内的本地相对路径。"));
    }
    let relative = Path::new(value);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("{label}必须是文件夹内的本地相对路径。"));
    }
    let path = base
        .join(relative)
        .canonicalize()
        .map_err(|_| format!("找不到{label}：{value}。"))?;
    if !path.starts_with(base) || !path.is_file() {
        return Err(format!("{label}超出播放列表文件夹或不是文件：{value}。"));
    }
    Ok(path)
}

fn key_attributes(value: &str) -> Result<Vec<(String, String)>, String> {
    let mut attributes = Vec::new();
    let mut remaining = value;
    while !remaining.is_empty() {
        let (name, rest) = remaining
            .split_once('=')
            .ok_or("AES-128 密钥标签格式无效。")?;
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_uppercase() || c == '-') {
            return Err("AES-128 密钥标签格式无效。".into());
        }
        let (value, next) = if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"').ok_or("AES-128 密钥标签格式无效。")?;
            let value = &quoted[..end];
            let tail = &quoted[end + 1..];
            let next = if tail.is_empty() {
                ""
            } else {
                tail.strip_prefix(',')
                    .filter(|remaining| !remaining.is_empty())
                    .ok_or("AES-128 密钥标签格式无效。")?
            };
            (value, next)
        } else {
            match rest.split_once(',') {
                Some((value, tail)) if !tail.is_empty() => (value, tail),
                Some(_) => return Err("AES-128 密钥标签格式无效。".into()),
                None => (rest, ""),
            }
        };
        if value.is_empty() || attributes.iter().any(|(existing, _)| existing == name) {
            return Err("AES-128 密钥标签格式无效或属性重复。".into());
        }
        attributes.push((name.to_string(), value.to_string()));
        remaining = next;
    }
    Ok(attributes)
}

fn parse_key(base: &Path, value: &str, media_sequence: u64) -> Result<Encryption, String> {
    let attributes = key_attributes(value)?;
    let attribute = |name: &str| {
        attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if attribute("METHOD") != Some("AES-128") {
        return Err("仅支持本地 AES-128 密钥，不支持其他加密方式。".into());
    }
    if attribute("KEYFORMAT").is_some_and(|format| format != "identity")
        || attribute("KEYFORMATVERSIONS").is_some_and(|versions| versions != "1")
        || attributes.iter().any(|(name, _)| {
            !matches!(
                name.as_str(),
                "METHOD" | "URI" | "IV" | "KEYFORMAT" | "KEYFORMATVERSIONS"
            )
        })
    {
        return Err("暂不支持此 AES-128 密钥格式。".into());
    }
    let uri = attribute("URI").ok_or("AES-128 密钥缺少 URI。")?;
    let key_path = local_path(base, uri, "密钥文件")?;
    let size = fs::metadata(&key_path)
        .map_err(|_| "无法读取 AES-128 密钥文件。".to_string())?
        .len();
    if size != 16 {
        return Err("AES-128 密钥文件必须恰好为 16 字节。".into());
    }
    let iv = attribute("IV")
        .map(|value| {
            let hex = value.strip_prefix("0x").ok_or("AES-128 IV 格式无效。")?;
            if hex.is_empty() || hex.len() > 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("AES-128 IV 必须是最多 32 位的十六进制数。".to_string());
            }
            u128::from_str_radix(hex, 16)
                .map(u128::to_be_bytes)
                .map_err(|_| "AES-128 IV 格式无效。".to_string())
        })
        .transpose()?;
    Ok(Encryption {
        key_path,
        iv,
        media_sequence,
    })
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
    let mut key: Option<String> = None;
    let mut media_sequence = 0;
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
        } else if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            if key.is_some() || !segments.is_empty() || pending_duration.is_some() {
                return Err("暂不支持播放列表中途更换密钥。".into());
            }
            key = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            if !segments.is_empty() || pending_duration.is_some() {
                return Err("媒体序号必须位于视频片段之前。".into());
            }
            media_sequence = value
                .parse::<u64>()
                .map_err(|_| "播放列表包含无效的媒体序号。".to_string())?;
        } else if line.starts_with("#EXT-X-") {
            let tag = line.split(':').next().unwrap_or(line);
            if !matches!(
                tag,
                "#EXT-X-VERSION"
                    | "#EXT-X-TARGETDURATION"
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
            if !Path::new(line)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
            {
                return Err("当前版本仅支持本地 .ts 视频片段。".into());
            }
            let segment = local_path(base, line, "视频片段")?;
            segments.push(segment);
            duration += seconds;
        }
    }
    if pending_duration.is_some() || segments.is_empty() || !ended {
        return Err("播放列表不完整：需要视频片段和 #EXT-X-ENDLIST 结束标记。".into());
    }
    let encryption = key
        .as_deref()
        .map(|value| parse_key(base, value, media_sequence))
        .transpose()?;
    Ok(Playlist {
        path,
        segments,
        declared_duration: duration,
        encryption,
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
            "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key\"\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n",
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

    #[test]
    fn accepts_local_aes128_key_and_sequence_iv() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("parts")).unwrap();
        fs::write(dir.path().join("parts/secret.key"), [1u8; 16]).unwrap();
        fs::write(dir.path().join("parts/000.ts"), [0u8; 16]).unwrap();
        let list = dir.path().join("video.m3u8");
        fs::write(
            &list,
            "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-KEY:METHOD=AES-128,URI=\"parts/secret.key\"\n#EXTINF:1,\nparts/000.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        let parsed = parse_playlist(&list).unwrap();
        let encryption = parsed.encryption.unwrap();
        assert_eq!(encryption.media_sequence, 7);
        assert_eq!(encryption.iv_for_segment(0), 7u128.to_be_bytes());
        assert_eq!(encryption.iv_for_segment(2), 9u128.to_be_bytes());
    }

    #[test]
    fn rejects_unsafe_or_incomplete_aes128_keys() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.ts"), [0u8; 16]).unwrap();
        fs::write(dir.path().join("key.bin"), [1u8; 15]).unwrap();
        let list = dir.path().join("video.m3u8");
        for tag in [
            "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"../key.bin\"",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"https://example.com/key\"",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"missing.key\"",
        ] {
            fs::write(
                &list,
                format!("#EXTM3U\n{tag}\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n"),
            )
            .unwrap();
            assert!(
                parse_playlist(&list).is_err(),
                "unexpectedly accepted {tag}"
            );
        }
        fs::write(dir.path().join("key.bin"), [1u8; 16]).unwrap();
        fs::write(
            &list,
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0xZZ\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        assert!(parse_playlist(&list).unwrap_err().contains("IV"));
        fs::write(
            &list,
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:1,\na.ts\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        assert!(parse_playlist(&list).unwrap_err().contains("更换密钥"));
    }
}
