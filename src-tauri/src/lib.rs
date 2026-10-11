mod playlist;

use playlist::{parse_playlist, Playlist};
use serde::Serialize;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{interval, Duration};

#[derive(Default)]
struct ConversionState(Mutex<Option<Arc<AtomicBool>>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaInfo {
    input_path: String,
    file_name: String,
    output_path: String,
    duration_seconds: f64,
    width: u64,
    height: u64,
    segment_count: usize,
    encrypted: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    id: String,
    percent: u8,
    seconds: f64,
}

#[cfg(debug_assertions)]
fn target_triple() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else {
        "unsupported"
    }
}

fn ffmpeg_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy();
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

fn tool_path(name: &str) -> Result<PathBuf, String> {
    let executable = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let installed = std::env::current_exe()
        .map_err(|_| "无法定位应用程序。")?
        .parent()
        .ok_or("无法定位应用程序。")?
        .join(&executable);
    if installed.is_file() {
        return Ok(installed);
    }
    #[cfg(debug_assertions)]
    {
        let development = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!(
                "{name}-{}{}",
                target_triple(),
                std::env::consts::EXE_SUFFIX
            ));
        if development.is_file() {
            return Ok(development);
        }
    }
    Err(format!("缺少随应用分发的 {name} 程序，请重新安装应用。"))
}

const PROBE_ENTRIES: &str = "format=duration:stream=codec_type,codec_name,width,height,channels,sample_rate,nb_read_packets";

async fn probe(path: &Path, count_packets: bool) -> Result<Value, String> {
    let binary = tool_path("ffprobe")?;
    let mut command = Command::new(binary);
    command.args(["-v", "error", "-show_entries", PROBE_ENTRIES, "-of", "json"]);
    if count_packets {
        command.arg("-count_packets");
    }
    let output = command
        .arg(ffmpeg_path(path))
        .output()
        .await
        .map_err(|_| "无法启动视频检测程序，请重新安装应用。".to_string())?;
    if !output.status.success() {
        return Err("视频片段无法读取或文件已损坏。".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "FFprobe 返回了无效的视频信息。".to_string())
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn probe_encrypted_segment(
    playlist: &Playlist,
    segment: &Path,
    index: usize,
    key_hex: &str,
) -> Result<Value, String> {
    let encryption = playlist
        .encryption
        .as_ref()
        .ok_or("播放列表缺少密钥信息。")?;
    let base = playlist
        .path
        .parent()
        .ok_or("无法确定播放列表所在文件夹。")?;
    let relative = segment
        .strip_prefix(base)
        .map_err(|_| "视频片段超出播放列表文件夹。".to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    let iv_hex = hex_bytes(&encryption.iv_for_segment(index));
    let output = Command::new(tool_path("ffprobe")?)
        .current_dir(ffmpeg_path(base))
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file,crypto",
            "-decryption_key",
            key_hex,
            "-decryption_iv",
            &iv_hex,
            "-f",
            "mpegts",
            "-count_packets",
            "-show_entries",
            PROBE_ENTRIES,
            "-of",
            "json",
        ])
        .arg(format!("crypto:{relative}"))
        .output()
        .await
        .map_err(|_| "无法启动视频检测程序，请重新安装应用。".to_string())?;
    if !output.status.success() {
        return Err("AES-128 密钥错误，或视频片段无法解密/读取。".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "FFprobe 返回了无效的视频信息。".to_string())
}

fn video_stream(value: &Value) -> Option<&Value> {
    value
        .get("streams")?
        .as_array()?
        .iter()
        .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
}

async fn inspect(path: &Path, cancelled: Option<&AtomicBool>) -> Result<MediaInfo, String> {
    let playlist = parse_playlist(path)?;
    let key_hex = playlist
        .encryption
        .as_ref()
        .map(|encryption| -> Result<String, String> {
            let bytes = fs::read(&encryption.key_path)
                .map_err(|_| "无法读取 AES-128 密钥文件。".to_string())?;
            if bytes.len() != 16 {
                return Err("AES-128 密钥文件必须恰好为 16 字节。".into());
            }
            Ok(hex_bytes(&bytes))
        })
        .transpose()?;
    let mut expected_video: Option<(String, u64, u64)> = None;
    let mut expected_audio: Option<Option<(String, u64, String)>> = None;
    for (index, segment) in playlist.segments.iter().enumerate() {
        if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err("转换已取消。".into());
        }
        let data = match key_hex.as_deref() {
            Some(key) => probe_encrypted_segment(&playlist, segment, index, key).await,
            None => probe(segment, true).await,
        }
        .map_err(|error| format!("第 {} 个片段：{error}", index + 1))?;
        let streams = data
            .get("streams")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("第 {} 个片段缺少媒体流。", index + 1))?;
        let videos: Vec<_> = streams
            .iter()
            .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
            .collect();
        let audios: Vec<_> = streams
            .iter()
            .filter(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"))
            .collect();
        if videos.is_empty() {
            return Err(format!("第 {} 个片段没有可读取的视频流。", index + 1));
        }
        if videos.len() > 1 || audios.len() > 1 {
            return Err("视频包含多个视频或音轨，当前版本暂不支持。".into());
        }
        let audio = audios.first().map(|stream| {
            (
                stream
                    .get("codec_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                stream.get("channels").and_then(Value::as_u64).unwrap_or(0),
                stream
                    .get("sample_rate")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        });
        if expected_audio
            .as_ref()
            .is_some_and(|expected| expected != &audio)
        {
            return Err("视频片段的音频格式不一致，当前版本暂不支持。".into());
        }
        expected_audio = Some(audio);
        let stream = video_stream(&data)
            .ok_or_else(|| format!("第 {} 个片段没有可读取的视频流。", index + 1))?;
        let packets = stream
            .get("nb_read_packets")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        if packets == 0 {
            return Err(format!("第 {} 个片段没有可读取的视频帧。", index + 1));
        }
        let codec = stream
            .get("codec_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let width = stream.get("width").and_then(Value::as_u64).unwrap_or(0);
        let height = stream.get("height").and_then(Value::as_u64).unwrap_or(0);
        if width == 0 || height == 0 {
            return Err(format!("第 {} 个片段缺少有效分辨率。", index + 1));
        }
        let current = (codec, width, height);
        if expected_video
            .as_ref()
            .is_some_and(|expected| expected != &current)
        {
            return Err("视频片段的编码或分辨率不一致，当前版本暂不支持。".into());
        }
        expected_video = Some(current);
    }
    let (_, width, height) = expected_video.ok_or("播放列表没有视频片段。")?;
    let file_name = playlist
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let output_path = playlist.path.with_extension("mp4");
    Ok(MediaInfo {
        input_path: playlist.path.to_string_lossy().into_owned(),
        file_name,
        output_path: output_path.to_string_lossy().into_owned(),
        duration_seconds: playlist.declared_duration,
        width,
        height,
        segment_count: playlist.segments.len(),
        encrypted: playlist.encryption.is_some(),
    })
}

#[tauri::command]
async fn inspect_playlist(path: String) -> Result<MediaInfo, String> {
    inspect(Path::new(&path), None).await
}

fn seconds_from_timestamp(value: &str) -> Option<f64> {
    let parts: Vec<_> = value.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    Some(
        parts[0].parse::<f64>().ok()? * 3600.0
            + parts[1].parse::<f64>().ok()? * 60.0
            + parts[2].parse::<f64>().ok()?,
    )
}

async fn convert<F: Fn(ProgressEvent)>(
    report: &F,
    input: &Path,
    output: &Path,
    id: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<String, String> {
    let info = inspect(input, Some(&cancelled)).await?;
    if cancelled.load(Ordering::SeqCst) {
        return Err("转换已取消。".into());
    }
    if !output.is_absolute()
        || !output
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
    {
        return Err("输出路径必须是绝对路径，且扩展名为 .mp4。".into());
    }
    if output.exists() {
        return Err("输出文件已存在，请选择其他文件名。".into());
    }
    let parent = output.parent().ok_or("输出路径无效。")?;
    if !parent.is_dir() {
        return Err("输出文件夹不存在。".into());
    }
    let temp = parent.join(format!(
        ".m3u8-converter-{}.partial.mp4",
        uuid::Uuid::new_v4()
    ));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| "无法在输出文件夹创建临时文件，请检查写入权限。".to_string())?;
    let result = convert_to_temp(
        report,
        Path::new(&info.input_path),
        &temp,
        id,
        info.duration_seconds,
        info.encrypted,
        cancelled,
    )
    .await;
    let final_result = match result {
        Ok(()) => {
            async {
                let data = probe(&temp, false).await?;
                let stream = video_stream(&data).ok_or("生成的 MP4 没有视频流。")?;
                let actual_duration = data
                    .get("format")
                    .and_then(|format| format.get("duration"))
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let tolerance = 3.0;
                if actual_duration <= 0.0 || actual_duration + tolerance < info.duration_seconds {
                    Err("生成的 MP4 时长异常，已删除临时文件。".into())
                } else if stream.get("width").and_then(Value::as_u64) != Some(info.width)
                    || stream.get("height").and_then(Value::as_u64) != Some(info.height)
                {
                    Err("生成的 MP4 分辨率异常，已删除临时文件。".into())
                } else {
                    fs::hard_link(&temp, output).map_err(|_| {
                        if output.exists() {
                            "输出文件已存在，请选择其他文件名。".to_string()
                        } else {
                            "无法保存已验证的 MP4，请检查文件夹权限和磁盘空间。".to_string()
                        }
                    })?;
                    Ok(output.to_string_lossy().into_owned())
                }
            }
            .await
        }
        Err(error) => Err(error),
    };
    let _ = fs::remove_file(&temp);
    final_result
}

async fn convert_to_temp<F: Fn(ProgressEvent)>(
    report: &F,
    input: &Path,
    temp: &Path,
    id: &str,
    duration: f64,
    encrypted: bool,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    let binary = tool_path("ffmpeg")?;
    let mut command = Command::new(binary);
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-protocol_whitelist",
        "file,crypto",
    ]);
    if encrypted {
        let base = input.parent().ok_or("无法确定播放列表所在文件夹。")?;
        command.current_dir(ffmpeg_path(base));
        command.args(["-allowed_extensions", "ALL"]);
    }
    command
        .arg("-i")
        .arg(if encrypted {
            input.file_name().ok_or("播放列表文件名无效。")?.into()
        } else {
            ffmpeg_path(input)
        })
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a?",
            "-c",
            "copy",
            "-movflags",
            "+faststart",
            "-progress",
            "pipe:1",
            "-nostats",
            "-f",
            "mp4",
        ])
        .arg(ffmpeg_path(temp));
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "无法启动视频转换程序，请重新安装应用。".to_string())?;
    let stdout = child.stdout.take().ok_or("无法读取转换进度。")?;
    let mut stderr = child.stderr.take().ok_or("无法读取转换错误。")?;
    let stderr_reader = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        text
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut stdout_open = true;
    let mut ticker = interval(Duration::from_millis(150));
    loop {
        tokio::select! {
            next = lines.next_line(), if stdout_open => {
                match next {
                    Ok(Some(line)) => {
                        if let Some(time) = line.strip_prefix("out_time=").and_then(seconds_from_timestamp) {
                            let percent = if duration > 0.0 { ((time / duration) * 100.0).clamp(0.0, 99.0) as u8 } else { 0 };
                            report(ProgressEvent { id: id.to_string(), percent, seconds: time });
                        }
                    }
                    Ok(None) => stdout_open = false,
                    Err(_) => stdout_open = false,
                }
            }
            _ = ticker.tick() => {
                if cancelled.load(Ordering::SeqCst) {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let _ = stderr_reader.await;
                    return Err("转换已取消。".into());
                }
                if let Some(status) = child.try_wait().map_err(|_| "转换进程异常终止。".to_string())? {
                    let error_text = stderr_reader.await.unwrap_or_default();
                    if status.success() && error_text.trim().is_empty() { return Ok(()); }
                    return Err("转换失败，请检查视频片段的编码和完整性。".into());
                }
            }
        }
    }
}

#[tauri::command]
async fn start_conversion(
    app: tauri::AppHandle,
    state: tauri::State<'_, ConversionState>,
    input_path: String,
    output_path: String,
    id: String,
) -> Result<String, String> {
    let flag = Arc::new(AtomicBool::new(false));
    {
        let mut running = state.0.lock().map_err(|_| "任务状态不可用。")?;
        if running.is_some() {
            return Err("已有转换任务正在运行。".into());
        }
        *running = Some(flag.clone());
    }
    let result = convert(
        &|event| {
            let _ = app.emit("conversion-progress", event);
        },
        Path::new(&input_path),
        Path::new(&output_path),
        &id,
        flag,
    )
    .await;
    if let Ok(mut running) = state.0.lock() {
        *running = None;
    }
    result
}

#[tauri::command]
fn cancel_conversion(state: tauri::State<'_, ConversionState>) -> Result<(), String> {
    let running = state.0.lock().map_err(|_| "任务状态不可用。")?;
    if let Some(flag) = running.as_ref() {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(ConversionState::default())
        .invoke_handler(tauri::generate_handler![
            inspect_playlist,
            start_conversion,
            cancel_conversion
        ])
        .run(tauri::generate_context!())
        .expect("应用启动失败");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_segment(path: &Path, color: &str) {
        let result = std::process::Command::new(tool_path("ffmpeg").unwrap())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("color=c={color}:s=320x180:r=25"))
            .args([
                "-t", "1", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-f", "mpegts",
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let parts = directory.path().join("parts");
        fs::create_dir(&parts).unwrap();
        make_segment(&parts.join("001.ts"), "red");
        make_segment(&parts.join("002.ts"), "blue");
        let playlist = directory.path().join("样片.m3u8");
        fs::write(&playlist, "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nparts/001.ts\n#EXTINF:1,\nparts/002.ts\n#EXT-X-ENDLIST\n").unwrap();
        (directory, playlist)
    }

    fn encrypted_fixture(explicit_iv: bool) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let parts = directory.path().join("parts");
        fs::create_dir(&parts).unwrap();
        fs::write(parts.join("0.key"), [0x11u8; 16]).unwrap();
        for (index, color) in ["red", "blue"].iter().enumerate() {
            let raw = parts.join(format!("raw{index}.ts"));
            make_segment(&raw, color);
            let iv = if explicit_iv { 0 } else { 7 + index as u128 };
            let result = std::process::Command::new(tool_path("ffmpeg").unwrap())
                .current_dir(&parts)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
                .arg(format!("raw{index}.ts"))
                .args([
                    "-c",
                    "copy",
                    "-f",
                    "mpegts",
                    "-encryption_key",
                    "11111111111111111111111111111111",
                    "-encryption_iv",
                    &format!("{iv:032x}"),
                ])
                .arg(format!("crypto:{index:03}.ts"))
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            fs::remove_file(raw).unwrap();
        }
        let iv = if explicit_iv {
            ",IV=0x00000000000000000000000000000000"
        } else {
            ""
        };
        let playlist = directory.path().join("加密样片.m3u8");
        fs::write(
            &playlist,
            format!(
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-KEY:METHOD=AES-128,URI=\"parts/0.key\"{iv}\n#EXTINF:1,\nparts/000.ts\n#EXTINF:1,\nparts/001.ts\n#EXT-X-ENDLIST\n"
            ),
        )
        .unwrap();
        (directory, playlist)
    }

    #[tokio::test]
    async fn converts_local_aes128_with_explicit_and_sequence_ivs() {
        for explicit_iv in [true, false] {
            let (directory, playlist) = encrypted_fixture(explicit_iv);
            let info = inspect(&playlist, None).await.unwrap();
            assert!(info.encrypted);
            assert_eq!((info.width, info.height, info.segment_count), (320, 180, 2));
            let output = directory.path().join("解密结果.mp4");
            convert(
                &|_| {},
                &playlist,
                &output,
                "encrypted-test",
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
            let result = probe(&output, false).await.unwrap();
            assert!(video_stream(&result).is_some());
            assert!(output.metadata().unwrap().len() > 1000);
        }
    }

    #[tokio::test]
    async fn rejects_wrong_key_and_corrupt_ciphertext_without_mp4() {
        let (directory, playlist) = encrypted_fixture(true);
        let key = directory.path().join("parts/0.key");
        let output = directory.path().join("结果.mp4");
        fs::write(&key, [0x22u8; 16]).unwrap();
        let error = convert(
            &|_| {},
            &playlist,
            &output,
            "bad-key",
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
        assert!(error.contains("解密"));
        assert!(!output.exists());
        fs::write(&key, [0x11u8; 16]).unwrap();
        fs::write(directory.path().join("parts/001.ts"), b"broken ciphertext").unwrap();
        assert!(convert(
            &|_| {},
            &playlist,
            &output,
            "bad-segment",
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .is_err());
        assert!(!output.exists());
    }

    #[tokio::test]
    async fn cancels_a_running_aes128_conversion() {
        let (directory, playlist) = encrypted_fixture(true);
        let repeated = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"parts/0.key\",IV=0x00000000000000000000000000000000\n".to_string()
            + &"#EXTINF:1,\nparts/000.ts\n".repeat(1000)
            + "#EXT-X-ENDLIST\n";
        fs::write(&playlist, repeated).unwrap();
        let temp = directory.path().join("pending.partial.mp4");
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            signal.store(true, Ordering::SeqCst);
        });
        let result = convert_to_temp(
            &|_| {},
            &playlist,
            &temp,
            "encrypted-cancel",
            1000.0,
            true,
            cancelled,
        )
        .await;
        cancel_task.await.unwrap();
        assert!(result.unwrap_err().contains("取消"));
        assert!(!directory.path().join("结果.mp4").exists());
    }

    #[tokio::test]
    #[ignore = "需要通过 M3U8_TEST_INPUT 指定本地播放列表"]
    async fn inspects_external_playlist_from_environment() {
        let input = std::env::var_os("M3U8_TEST_INPUT").expect("M3U8_TEST_INPUT is required");
        let info = inspect(Path::new(&input), None).await.unwrap();
        println!(
            "segments={} duration={} resolution={}x{} encrypted={}",
            info.segment_count, info.duration_seconds, info.width, info.height, info.encrypted
        );
    }

    #[tokio::test]
    #[ignore = "需要通过 M3U8_TEST_INPUT 和 M3U8_TEST_OUTPUT_DIR 指定本地测试路径"]
    async fn converts_external_playlist_from_environment() {
        let input = std::env::var_os("M3U8_TEST_INPUT").expect("M3U8_TEST_INPUT is required");
        let directory =
            std::env::var_os("M3U8_TEST_OUTPUT_DIR").expect("M3U8_TEST_OUTPUT_DIR is required");
        let output =
            PathBuf::from(directory).join(format!("aes-validation-{}.mp4", uuid::Uuid::new_v4()));
        let saved = convert(
            &|_| {},
            Path::new(&input),
            &output,
            "external-test",
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(saved, output.to_string_lossy());
        let size = output.metadata().unwrap().len();
        let data = probe(&output, false).await.unwrap();
        assert!(video_stream(&data).is_some());
        println!("verified_mp4_bytes={size}");
        fs::remove_file(output).unwrap();
    }

    #[tokio::test]
    async fn converts_real_ts_and_verifies_mp4() {
        let (directory, playlist) = fixture();
        let info = inspect(&playlist, None).await.unwrap();
        assert_eq!((info.width, info.height, info.segment_count), (320, 180, 2));
        let output = directory.path().join("结果.mp4");
        let saved = convert(
            &|_| {},
            &playlist,
            &output,
            "test",
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(saved, output.to_string_lossy());
        assert!(video_stream(&probe(&output, false).await.unwrap()).is_some());
        assert!(!directory.path().read_dir().unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("partial.mp4")));
        let second = convert(
            &|_| {},
            &playlist,
            &output,
            "test",
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert!(second.unwrap_err().contains("已存在"));
    }

    #[tokio::test]
    async fn rejects_missing_and_corrupt_segments_without_output() {
        let (directory, playlist) = fixture();
        let output = directory.path().join("结果.mp4");
        fs::remove_file(directory.path().join("parts/002.ts")).unwrap();
        assert!(convert(
            &|_| {},
            &playlist,
            &output,
            "test",
            Arc::new(AtomicBool::new(false))
        )
        .await
        .is_err());
        assert!(!output.exists());
        fs::write(directory.path().join("parts/002.ts"), b"not video").unwrap();
        assert!(convert(
            &|_| {},
            &playlist,
            &output,
            "test",
            Arc::new(AtomicBool::new(false))
        )
        .await
        .is_err());
        assert!(!output.exists());
    }

    #[tokio::test]
    async fn cancellation_stops_process_and_leaves_no_final_mp4() {
        let (directory, playlist) = fixture();
        let output = directory.path().join("结果.mp4");
        let cancelled = Arc::new(AtomicBool::new(true));
        let result = convert(&|_| {}, &playlist, &output, "test", cancelled).await;
        assert!(result.unwrap_err().contains("取消"));
        assert!(!output.exists());
    }

    #[tokio::test]
    async fn cancellation_kills_a_running_ffmpeg_process() {
        let (directory, playlist) = fixture();
        let repeated = "#EXTM3U\n".to_string()
            + &"#EXTINF:1,\nparts/001.ts\n".repeat(500)
            + "#EXT-X-ENDLIST\n";
        fs::write(&playlist, repeated).unwrap();
        let temp = directory.path().join("pending.partial.mp4");
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            signal.store(true, Ordering::SeqCst);
        });
        let result =
            convert_to_temp(&|_| {}, &playlist, &temp, "test", 500.0, false, cancelled).await;
        cancel_task.await.unwrap();
        assert!(result.unwrap_err().contains("取消"));
        assert!(!directory.path().join("结果.mp4").exists());
    }

    #[tokio::test]
    async fn cancellation_interrupts_segment_checks() {
        let (directory, playlist) = fixture();
        fs::write(
            &playlist,
            "#EXTM3U\n".to_string()
                + &"#EXTINF:1,\nparts/001.ts\n".repeat(200)
                + "#EXT-X-ENDLIST\n",
        )
        .unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let signal = flag.clone();
        let cancellation = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            signal.store(true, Ordering::SeqCst);
        });
        let output = directory.path().join("结果.mp4");
        let result = convert(&|_| {}, &playlist, &output, "test", flag).await;
        cancellation.await.unwrap();
        assert!(result.unwrap_err().contains("取消"));
        assert!(!output.exists());
    }
}
