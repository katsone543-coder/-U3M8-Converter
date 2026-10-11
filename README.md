# M3U8 转换器 V0.1

基于 Tauri 2、React、TypeScript 和随应用打包的 FFmpeg / FFprobe。仅处理完整的本地 `.m3u8` 媒体播放列表及其相对路径 `.ts` 片段，支持子目录。转换使用 `-c copy`，验证 MP4 后才创建正式输出文件；原始文件不会修改。

## 本地开发

需要 Node.js 20+、Rust stable，以及 [Tauri 2 对应平台的系统依赖](https://v2.tauri.app/start/prerequisites/)。

```bash
npm ci
npm run prepare:binaries
npm run tauri -- dev
```

Windows 当前提供 x64 构建。macOS 在 Apple Silicon 和 Intel 机器上分别执行 `npm run prepare:binaries`，可生成对应架构的随包二进制文件。

## Windows 11 打包

在 Windows 11 x64 的 PowerShell 中运行：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1
```

脚本会安装依赖、准备 FFmpeg / FFprobe、运行真实 TS 转换测试，并生成当前用户安装的 `.exe` 安装包。安装包位于 `src-tauri/target/release/bundle/nsis/`，无需单独安装 FFmpeg。GitHub Actions 的 Windows 工作流也会上传安装包。当前版本未进行代码签名，Windows 首次安装时可能显示发布者未知提示。

## 测试

```bash
cargo test --manifest-path src-tauri/Cargo.toml
npm run build
```

Rust 测试使用真实 FFmpeg 生成 TS 文件，检查成功转换、缺失与损坏片段、取消进程、拒绝覆盖和不支持的播放列表结构。

## macOS 打包

在 macOS 上运行：

```bash
bash scripts/build-macos.sh
```

脚本会安装依赖、准备本机架构的 FFmpeg / FFprobe、生成 `.icns` 图标、运行测试，然后构建未签名 `.dmg`。GitHub Actions 工作流可在 Apple Silicon 和 Intel macOS 运行相同步骤。未签名应用在其他 Mac 上打开时会触发系统安全提示；正式分发仍需 Apple Developer 签名与公证。

## V0.1 支持范围

支持有 `#EXT-X-ENDLIST` 的本地 TS 媒体播放列表。加密、在线 URI、主播放列表、字节范围、fMP4、复杂音轨及不连续片段会显示明确错误。输出已存在时要求另选文件名。

FFmpeg 二进制由 `ffmpeg-static` 提供，具体许可与来源见 [第三方说明](THIRD_PARTY_NOTICES.md)。发布前应核对对应二进制构建的许可和源码提供义务。
