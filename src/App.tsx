import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open, save } from '@tauri-apps/plugin-dialog';
import { revealItemInDir } from '@tauri-apps/plugin-opener';

type Phase = 'empty' | 'inspecting' | 'ready' | 'converting' | 'cancelling' | 'success';
type MediaInfo = {
  inputPath: string;
  fileName: string;
  outputPath: string;
  durationSeconds: number;
  width: number;
  height: number;
  segmentCount: number;
  encrypted: boolean;
};
type Progress = { id: string; percent: number; seconds: number };

function formatDuration(seconds: number): string {
  const rounded = Math.round(seconds);
  const hours = Math.floor(rounded / 3600);
  const minutes = Math.floor((rounded % 3600) / 60);
  const rest = rounded % 60;
  return hours > 0
    ? `${hours} 小时 ${minutes} 分 ${rest} 秒`
    : `${minutes} 分 ${rest} 秒`;
}

function errorText(error: unknown): string {
  return typeof error === 'string' ? error : '操作失败，请检查文件后重试。';
}

function FilmIcon({ size = 24 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <rect x="2.5" y="4" width="19" height="16" rx="3" stroke="currentColor" strokeWidth="1.8" />
      <path d="M7.5 4v16M16.5 4v16M2.5 9h5M2.5 15h5M16.5 9h5M16.5 15h5" stroke="currentColor" strokeWidth="1.8" />
      <path d="m10 9 5 3-5 3V9Z" fill="currentColor" />
    </svg>
  );
}

export default function App() {
  const [phase, setPhase] = useState<Phase>('empty');
  const [info, setInfo] = useState<MediaInfo | null>(null);
  const [outputPath, setOutputPath] = useState('');
  const [progress, setProgress] = useState(0);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [dragging, setDragging] = useState(false);
  const activeId = useRef('');
  const phaseRef = useRef<Phase>('empty');

  function updatePhase(next: Phase) {
    phaseRef.current = next;
    setPhase(next);
  }

  async function loadFile(path: string) {
    if (phaseRef.current === 'inspecting' || phaseRef.current === 'converting' || phaseRef.current === 'cancelling') return;
    updatePhase('inspecting');
    setError('');
    setNotice('');
    setInfo(null);
    setProgress(0);
    try {
      const media = await invoke<MediaInfo>('inspect_playlist', { path });
      setInfo(media);
      setOutputPath(media.outputPath);
      updatePhase('ready');
    } catch (cause) {
      setError(errorText(cause));
      updatePhase('empty');
    }
  }

  useEffect(() => {
    let disposed = false;
    const subscriptions: Array<() => void> = [];
    getCurrentWindow().onDragDropEvent((event) => {
      if (event.payload.type === 'over') setDragging(true);
      if (event.payload.type === 'leave') setDragging(false);
      if (event.payload.type === 'drop') {
        setDragging(false);
        const [first] = event.payload.paths;
        if (first) void loadFile(first);
      }
    }).then((off) => disposed ? off() : subscriptions.push(off));
    listen<Progress>('conversion-progress', ({ payload }) => {
      if (payload.id === activeId.current) setProgress(payload.percent);
    }).then((off) => disposed ? off() : subscriptions.push(off));
    return () => {
      disposed = true;
      subscriptions.forEach((off) => off());
    };
  }, []);

  async function chooseFile() {
    try {
      const selected = await open({ multiple: false, directory: false, filters: [{ name: 'M3U8 播放列表', extensions: ['m3u8'] }] });
      if (typeof selected === 'string') await loadFile(selected);
    } catch {
      setError('无法打开文件选择窗口，请重试。');
    }
  }

  async function chooseOutput() {
    if (!info) return;
    try {
      const selected = await save({ defaultPath: outputPath, filters: [{ name: 'MP4 视频', extensions: ['mp4'] }] });
      if (selected) {
        setOutputPath(selected);
        setError('');
      }
    } catch {
      setError('无法选择保存位置，请重试。');
    }
  }

  async function start() {
    if (!info || phaseRef.current !== 'ready') return;
    const id = crypto.randomUUID();
    activeId.current = id;
    updatePhase('converting');
    setProgress(0);
    setError('');
    setNotice('');
    try {
      const saved = await invoke<string>('start_conversion', { inputPath: info.inputPath, outputPath, id });
      setOutputPath(saved);
      setProgress(100);
      updatePhase('success');
      setNotice('转换完成，MP4 已通过视频检测。');
    } catch (cause) {
      const message = errorText(cause);
      if (message.includes('已取消')) setNotice(message);
      else setError(message);
      updatePhase('ready');
    } finally {
      activeId.current = '';
    }
  }

  async function cancel() {
    if (phaseRef.current !== 'converting') return;
    updatePhase('cancelling');
    setNotice('正在取消转换…');
    try {
      await invoke('cancel_conversion');
    } catch {
      setError('取消任务失败，请稍后重试。');
      updatePhase('converting');
    }
  }

  async function reveal() {
    try { await revealItemInDir(outputPath); }
    catch { setError('无法打开保存位置，请在文件管理器中手动查找。'); }
  }

  const working = phase === 'converting' || phase === 'cancelling';

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand-mark"><FilmIcon size={23} /></div>
        <div className="brand-copy"><strong>M3U8 转换器</strong><span>本地视频，安心转换</span></div>
        <span className="version">V0.2</span>
      </header>

      <main className="main-content">
        <div className="intro">
          <div className="eyebrow"><span className="eyebrow-dot" /> 本地转换工具</div>
          <h1>将 M3U8 转为 MP4</h1>
          <p>选择本地播放列表，检查视频片段，然后无损封装为 MP4。</p>
        </div>

        <section className={`drop-card ${dragging ? 'is-dragging' : ''} ${info ? 'has-file' : ''}`} aria-label="选择 M3U8 文件">
          <div className="drop-symbol"><FilmIcon size={34} /></div>
          <div className="drop-copy">
            <h2>{info ? info.fileName : phase === 'inspecting' ? '正在检查视频片段…' : '拖入 M3U8 文件'}</h2>
            <p>{info ? (info.encrypted ? '本地密钥和视频片段检查完成，可以开始转换' : '文件检查完成，可以开始转换') : '或从电脑中选择本地 .m3u8 播放列表'}</p>
          </div>
          <button type="button" className="button button-secondary" onClick={chooseFile} disabled={working || phase === 'inspecting'}>
            {info ? '更换文件' : '选择文件'}
          </button>
        </section>

        {info && <section className="details-card" aria-label="视频信息">
          <div className="section-heading"><h2>视频信息</h2><span className="verified"><span>✓</span> 已检查 {info.segmentCount} 个片段</span></div>
          <div className="stats">
            <div><span className="stat-label">播放列表时长</span><strong>{formatDuration(info.durationSeconds)}</strong></div>
            <div><span className="stat-label">视频分辨率</span><strong>{info.width} × {info.height}</strong></div>
            <div><span className="stat-label">转换方式</span><strong>{info.encrypted ? 'AES-128 解密＋无损封装' : '无损封装'}</strong></div>
          </div>
          <div className="separator" />
          <div className="output-heading"><label htmlFor="output-path">保存位置</label><span>验证成功后才会保存为 MP4</span></div>
          <div className="output-row">
            <input id="output-path" value={outputPath} readOnly title={outputPath} />
            <button type="button" className="button button-quiet" onClick={chooseOutput} disabled={working || phase === 'success'}>更改位置</button>
          </div>
        </section>}

        {working && <section className="progress-card" aria-live="polite">
          <div className="progress-header"><strong>{phase === 'cancelling' ? '正在取消…' : '正在转换视频…'}</strong><span>{progress}%</span></div>
          <div className="progress-track"><div style={{ width: `${progress}%` }} /></div>
          <p>{info?.encrypted ? '正在使用本地密钥解密并无损封装，完成后会自动验证文件。' : '正在无损封装片段，完成后会自动验证文件。'}</p>
        </section>}

        {error && <div className="message error" role="alert"><span>!</span>{error}</div>}
        {notice && <div className={`message ${phase === 'success' ? 'success' : 'neutral'}`} role="status"><span>{phase === 'success' ? '✓' : '•'}</span>{notice}</div>}

        <div className="actions">
          {phase === 'success' ? <button type="button" className="button button-primary" onClick={reveal}>打开保存位置 <span aria-hidden="true">↗</span></button>
            : working ? <button type="button" className="button button-danger" onClick={cancel} disabled={phase === 'cancelling'}>取消转换</button>
              : <button type="button" className="button button-primary" onClick={start} disabled={phase !== 'ready'}>开始转换 <span aria-hidden="true">→</span></button>}
        </div>
      </main>

      <footer className="footer"><span>仅处理本地 TS 视频片段</span><span>原始文件不会被修改</span></footer>
    </div>
  );
}
