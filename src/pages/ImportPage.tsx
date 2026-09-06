// 截图导入页 — 选择平台模板，上传持仓/交易截图，本地 PaddleOCR 识别后预览持仓
import { useState } from 'react';
import { Upload, ScanLine, CheckCircle2, FileImage, TriangleAlert, ChevronDown, ChevronRight } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { importScreenshots, importScreenshotsB64, readImageDataUrl, isTauri, isMobile, type ImportPreview } from '../api';
import { pickImagesMobile } from '../lib/fileChain';
import { PLATFORMS } from '../lib/mockData';
import { Card, PlatformBadge, EmptyState } from '../components/ui';

const PLATFORM_LIST = Object.values(PLATFORMS);

export default function ImportPage() {
  const [platform, setPlatform] = useState<string>('alipay');
  const [files, setFiles] = useState<string[]>([]);
  const [previews, setPreviews] = useState<string[]>([]); // base64 data URL，与 files 一一对应
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [busy, setBusy] = useState(false);
  const [showRaw, setShowRaw] = useState(false);

  const onPick = (code: string) => {
    setPlatform(code);
    setPreview(null);
  };

  const onPickFiles = async () => {
    // 移动端：dialog 返回 content:// URI（std::fs 不可读），改用 <input type=file> 读字节；
    // files 槽位存 base64（桌面存路径，语义随环境），previews 用前端 data URL。
    if (isMobile) {
      const picks = await pickImagesMobile();
      if (picks.length === 0) return;
      setFiles(picks.map((p) => p.b64));
      setPreviews(picks.map((p) => p.dataUrl));
      setPreview(null);
      return;
    }
    // 真实环境用 Tauri 原生文件对话框选择本地截图路径
    const selected = await open({
      multiple: true,
      filters: [{ name: '图片', extensions: ['png', 'jpg', 'jpeg', 'bmp', 'webp'] }],
    });
    if (Array.isArray(selected) && selected.length > 0) {
      const paths = selected as string[];
      setFiles(paths);
      setPreview(null);
      // 并行读取为 base64 data URL（后端读取，规避 asset 协议作用域限制）
      const urls = await Promise.all(
        paths.map(async (p) => {
          try {
            return await readImageDataUrl(p);
          } catch {
            return '';
          }
        }),
      );
      setPreviews(urls);
    }
  };

  const onImport = async () => {
    if (files.length === 0) return;
    setBusy(true);
    try {
      const r = isMobile
        ? await importScreenshotsB64(platform, files)
        : await importScreenshots(platform, files);
      setPreview(r);
    } catch (e) {
      setPreview({
        platform,
        platformName: PLATFORMS[platform]?.name ?? platform,
        detectedCount: 0,
        funds: [],
        ocrReady: false,
        note: `识别失败：${String(e)}`,
        rawLines: [],
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="p-6 space-y-5">
      <header>
        <h1 className="text-xl font-semibold">截图导入</h1>
        <p className="text-xs text-muted mt-0.5">本地 PaddleOCR 识别持仓截图，数据不上传云端</p>
      </header>

      <Card title="1 · 选择来源平台">
        <div className="grid grid-cols-3 gap-3">
          {PLATFORM_LIST.map((p) => (
            <button
              key={p.code}
              onClick={() => onPick(p.code)}
              className={`rounded-md border px-4 py-3 text-left text-sm transition-colors ${
                platform === p.code ? 'border-primary bg-primary/5' : 'border-border hover:bg-background'
              }`}
            >
              <PlatformBadge code={p.code} />
              <div className="mt-2 text-xs text-muted">识别持仓/交易列表截图</div>
            </button>
          ))}
        </div>
      </Card>

      <Card title="2 · 上传截图">
        <button
          type="button"
          onClick={() => void onPickFiles()}
          className="flex w-full flex-col items-center justify-center gap-2 rounded-md border border-dashed border-border py-10 cursor-pointer hover:bg-background"
        >
          <Upload size={28} className="text-muted" aria-hidden />
          <span className="text-sm text-foreground">点击选择截图（可多选）</span>
          <span className="text-xs text-muted">支持 支付宝 / 京东金融 / 腾讯理财通 持仓与交易列表</span>
        </button>
        {files.length > 0 && (
          <ul className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
            {files.map((f, i) => (
              <li key={f} className="overflow-hidden rounded-md border border-border">
                {previews[i] ? (
                  <img src={previews[i]} alt={f} className="h-24 w-full object-cover" />
                ) : !isTauri ? (
                  <div className="flex h-24 items-center gap-2 px-2 text-sm text-muted">
                    <FileImage size={16} aria-hidden />
                    <span className="truncate">{f}</span>
                  </div>
                ) : (
                  <div className="flex h-24 items-center justify-center text-xs text-muted">
                    预览加载中…
                  </div>
                )}
                <div className="truncate px-2 py-1 text-xs text-muted">{f.split('/').pop()}</div>
              </li>
            ))}
          </ul>
        )}
        <p className="mt-4 text-xs text-muted">
          导入将写入本人持仓（平台：<span className="font-medium text-foreground">{PLATFORMS[platform]?.name ?? platform}</span>）
        </p>
        <button
          onClick={() => void onImport()}
          disabled={busy || files.length === 0}
          className="mt-2 inline-flex items-center gap-1.5 rounded-md bg-primary px-4 py-2 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
        >
          <ScanLine size={16} aria-hidden />
          {busy ? '识别中…' : '开始识别'}
        </button>
      </Card>

      {preview && (
        <Card title="3 · 识别结果预览">
          {!preview.ocrReady && (
            <div className="mb-3 flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
              <TriangleAlert size={16} aria-hidden />
              OCR 未就绪：请运行 src-tauri/download_ocr_models.sh 下载模型，并以 --features ocr 构建。
            </div>
          )}
          <div className="flex items-center gap-2 text-sm text-success mb-3">
            <CheckCircle2 size={16} aria-hidden />
            识别到 {preview.detectedCount} 条持仓 · {preview.platformName}
          </div>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs text-muted border-b border-border">
                  <th className="py-2 pr-3 font-medium">基金名称</th>
                  <th className="py-2 pr-3 font-medium text-right">持仓金额</th>
                  <th className="py-2 pr-3 font-medium text-right">持有收益</th>
                  <th className="py-2 pr-3 font-medium text-right">昨日收益</th>
                  <th className="py-2 pr-3 font-medium text-right">收益率</th>
                </tr>
              </thead>
              <tbody>
                {preview.funds.length === 0 ? (
                  <tr>
                    <td colSpan={5} className="py-4 text-center text-muted">未识别到持仓条目</td>
                  </tr>
                ) : (
                  preview.funds.map((f) => (
                    <tr key={f.code || f.name} className="border-b border-border/60 last:border-0">
                      <td className="py-2.5 pr-3">{f.name}</td>
                      <td className="py-2.5 pr-3 text-right tnum">{f.holdingAmount.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}</td>
                      <td className={`py-2.5 pr-3 text-right tnum ${f.holdingProfit >= 0 ? 'text-gain' : 'text-loss'}`}>
                        {f.holdingProfit >= 0 ? '+' : ''}{f.holdingProfit.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}
                      </td>
                      <td className={`py-2.5 pr-3 text-right tnum ${f.yesterdayProfit >= 0 ? 'text-gain' : 'text-loss'}`}>
                        {f.yesterdayProfit >= 0 ? '+' : ''}{f.yesterdayProfit.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}
                      </td>
                      <td className={`py-2.5 pr-3 text-right tnum ${f.profitRate >= 0 ? 'text-gain' : 'text-loss'}`}>
                        {f.profitRate >= 0 ? '+' : ''}{f.profitRate.toFixed(2)}%
                      </td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          </div>
          <p className="mt-3 text-xs text-muted">{preview.note}</p>

          {preview.rawLines.length > 0 && (
            <div className="mt-4">
              <button
                type="button"
                onClick={() => setShowRaw((v) => !v)}
                className="flex items-center gap-1 text-xs text-muted hover:text-foreground"
              >
                {showRaw ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                OCR 原始文本（{preview.rawLines.length} 行，用于核对/微调）
              </button>
              {showRaw && (
                <pre className="mt-2 max-h-60 overflow-auto rounded-md border border-border bg-background p-3 text-xs leading-relaxed text-muted">
                  {preview.rawLines.join('\n')}
                </pre>
              )}
            </div>
          )}
        </Card>
      )}

      {!preview && files.length === 0 && (
        <EmptyState title="尚未选择截图" hint="选择平台后上传截图即可预览识别结果" />
      )}
    </div>
  );
}
