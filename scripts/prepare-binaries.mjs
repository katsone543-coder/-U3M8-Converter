import { chmodSync, copyFileSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
const triples = {
  'win32-x64': 'x86_64-pc-windows-msvc',
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
};
const triple = triples[`${process.platform}-${process.arch}`];
if (!triple) throw new Error('当前平台暂不支持打包。');
const ffmpeg = require('ffmpeg-static');
const ffprobe = require('ffprobe-static').path;
if (!ffmpeg || !ffprobe) throw new Error('未找到 FFmpeg 或 FFprobe 二进制文件。');
const destination = join(import.meta.dirname, '..', 'src-tauri', 'binaries');
mkdirSync(destination, { recursive: true });
for (const [name, source] of [['ffmpeg', ffmpeg], ['ffprobe', ffprobe]]) {
  const target = join(destination, `${name}-${triple}${process.platform === 'win32' ? '.exe' : ''}`);
  copyFileSync(source, target);
  if (process.platform !== 'win32') chmodSync(target, 0o755);
  console.log(`${name}: ${target}`);
}
