// 数据同步页（多设备同步 M3）：手动导出/导入设备快照 + 冲突提示 + 状态总览。
//
// 传输无关：本页只调用「快照导出/导入/冲突/状态」四个命令面，文件只是当前的 interim 通道；
// 云通道（M2）落位后复用同一批封装与同一套界面，无需返工。
// 口径：快照 = 全部存活行的最新状态 + 删除墓碑；导入按行 LWW 合并（更新的那方获胜），
// 不做整库覆盖，因此可反复导入、多设备收敛。
import { useEffect, useState } from 'react';
import { save, open } from '@tauri-apps/plugin-dialog';
import {
  RefreshCw,
  Download,
  Upload,
  TriangleAlert,
  Database,
  CircleAlert,
  ArrowLeftRight,
  HardDriveDownload,
  Check,
} from 'lucide-react';
import {
  isTauri,
  isMobile,
  syncStatus,
  syncListConflicts,
  syncExportSnapshot,
  syncExportSnapshotB64,
  syncImportSnapshot,
  syncImportSnapshotB64,
  syncCreateBackup,
  syncListBackups,
  syncSetBackupKeep,
  type SyncStatus,
  type SyncConflictRow,
  type BackupEntry,
} from '../api';
import { pickSingleFileMobile, shareFileMobile } from '../lib/fileChain';

const SNAPSHOT_EXT = ['jsonl'];

function errText(e: unknown): string {
  return (e as Error)?.message ?? String(e);
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

function backupTagLabel(tag: string): string {
  if (tag === 'pre-import') return '导入前自动';
  if (tag === 'auto') return '每日自动';
  if (tag === 'manual') return '手动';
  return tag || '—';
}

function stamp(): string {
  return new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-');
}

export default function SyncPage() {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [conflicts, setConflicts] = useState<SyncConflictRow[]>([]);
  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [keep, setKeep] = useState(7);
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);

  async function refresh() {
    try {
      const [s, c, b] = await Promise.all([syncStatus(), syncListConflicts(), syncListBackups()]);
      setStatus(s);
      setConflicts(c);
      setBackups(b);
      setKeep(s.backupKeep);
    } catch (e) {
      setMsg(`读取同步状态失败：${errText(e)}`);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function handleExport() {
    if (!isTauri) {
      setMsg('浏览器预览模式不支持真实导出，请在桌面端或多端 App 中使用。');
      return;
    }
    setBusy(true);
    try {
      if (isMobile) {
        const out = await syncExportSnapshotB64();
        const res = await shareFileMobile(out.fileName, 'application/jsonl', out.data);
        setMsg(
          res === 'shared'
            ? `已生成快照（${out.count} 条 / ${formatSize(out.size)}）并调起系统分享，可发送到另一台设备。`
            : res === 'aborted'
              ? '已取消分享，快照未保存。'
              : res === 'downloadAttempted'
                ? `已生成快照（${out.count} 条 / ${formatSize(out.size)}）并尝试下载保存；若文件未出现，请改用桌面端导出。`
                : '导出失败：当前系统不支持文件分享/下载。',
        );
      } else {
        const target = await save({
          defaultPath: `fundlens-sync-${stamp()}.jsonl`,
          filters: [{ name: '同步快照', extensions: SNAPSHOT_EXT }],
        });
        if (!target) return;
        const info = await syncExportSnapshot(target);
        setMsg(`已导出快照：${info.path}（${info.count} 条 / ${formatSize(info.size)}）`);
      }
      await refresh();
    } catch (e) {
      setMsg(`导出失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleImport() {
    if (!isTauri) {
      setMsg('浏览器预览模式不支持真实导入，请在桌面端或多端 App 中使用。');
      return;
    }
    const confirmText =
      '导入快照会把其中的记录与本地数据按行合并：同一条记录以更新时间更晚的一方为准，' +
      '不会整库覆盖本地数据。确定继续？';
    setBusy(true);
    try {
      if (isMobile) {
        const f = await pickSingleFileMobile('.jsonl,application/jsonl,text/plain');
        if (!f) {
          setBusy(false);
          return;
        }
        if (!window.confirm(confirmText)) {
          setBusy(false);
          return;
        }
        const out = await syncImportSnapshotB64(f.b64);
        setMsg(importSummary(out.applied, out.conflicts, out.total, out.device, out.backupFile));
      } else {
        const selected = await open({
          multiple: false,
          filters: [{ name: '同步快照', extensions: SNAPSHOT_EXT }],
        });
        if (!selected || typeof selected !== 'string') {
          setBusy(false);
          return;
        }
        if (!window.confirm(confirmText)) {
          setBusy(false);
          return;
        }
        const out = await syncImportSnapshot(selected);
        setMsg(importSummary(out.applied, out.conflicts, out.total, out.device, out.backupFile));
      }
      await refresh();
    } catch (e) {
      setMsg(`导入失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  function importSummary(
    applied: number,
    conflicts: number,
    total: number,
    device: string,
    backupFile: string | null,
  ): string {
    const base = `导入完成：共 ${total} 条，应用 ${applied} 条`;
    const tail = backupFile ? `（导入前的整库备份：${backupFile}）` : '';
    return conflicts > 0
      ? `${base}；有 ${conflicts} 条因本地记录更新更晚而保留本地版本，已在下方冲突列表列出${tail}`
      : `${base}；来源设备 ${device}${tail}`;
  }

  async function handleCreateBackup() {
    setBusy(true);
    try {
      const b = await syncCreateBackup();
      setMsg(`已生成备份：${b.file}（${formatSize(b.size)}）`);
      await refresh();
    } catch (e) {
      setMsg(`备份失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleSaveKeep(next: number) {
    setBusy(true);
    try {
      const v = await syncSetBackupKeep(next);
      setKeep(v);
      setMsg(`已把自动备份保留份数设为 ${v} 份，超出的最旧备份已清理。`);
      await refresh();
    } catch (e) {
      setMsg(`设置保留份数失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  const pending = status?.pendingChanges ?? 0;
  const unresolved = status?.conflictCount ?? 0;

  return (
    <div className="p-6 space-y-5 max-w-3xl">
      <header>
        <div className="flex items-center gap-2">
          <ArrowLeftRight size={22} className="text-primary" aria-hidden />
          <h1 className="text-xl font-semibold">数据同步</h1>
        </div>
        <p className="mt-2 text-sm text-muted leading-relaxed">
          把本机的持仓、基金、交易、快照与设置等用户数据导出为一份「设备快照」，在另一台设备导入即可合并。
          合并按行取更新时间更晚的一方（LWW），可重复导入、多设备逐步收敛；不包含净值历史与行情缓存等派
          生数据——那些由各设备自行从官方源重取。
        </p>
      </header>

      <section className="bg-surface border border-border rounded-md p-4 shadow-ring">
        <div className="flex items-center gap-2 mb-3">
          <Database size={18} className="text-primary" aria-hidden />
          <h2 className="text-base font-semibold">同步状态</h2>
          <button
            onClick={() => void refresh()}
            disabled={busy}
            className="ml-auto inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1 text-xs text-muted hover:text-primary disabled:opacity-50"
          >
            <RefreshCw size={14} aria-hidden /> 刷新
          </button>
        </div>
        {loading ? (
          <p className="text-sm text-muted">正在读取同步状态…</p>
        ) : (
          <dl className="grid grid-cols-2 sm:grid-cols-3 gap-3 text-sm">
            <div>
              <dt className="text-xs text-muted mb-0.5">本设备标识</dt>
              <dd className="tnum truncate">{status?.deviceId ?? '—'}</dd>
            </div>
            <div>
              <dt className="text-xs text-muted mb-0.5">参与同步的表</dt>
              <dd className="tnum">{status?.tablesSynced ?? '—'} 张</dd>
            </div>
            <div>
              <dt className="text-xs text-muted mb-0.5">待同步变更</dt>
              <dd className="tnum">
                {pending} 条
                {(status?.totalChanges ?? 0) > 0 ? (
                  <span className="text-xs text-muted"> / 累计 {status?.totalChanges}</span>
                ) : null}
              </dd>
            </div>
            <div>
              <dt className="text-xs text-muted mb-0.5">最近导出</dt>
              <dd className="tnum">{status?.lastExportAt ?? '尚未导出'}</dd>
            </div>
            <div>
              <dt className="text-xs text-muted mb-0.5">最近导入</dt>
              <dd className="tnum">{status?.lastImportAt ?? '尚未导入'}</dd>
            </div>
            <div>
              <dt className="text-xs text-muted mb-0.5">未解冲突</dt>
              <dd className="tnum">{unresolved} 条</dd>
            </div>
          </dl>
        )}
      </section>

      <section className="bg-surface border border-border rounded-md p-4 shadow-ring">
        <h2 className="text-base font-semibold mb-2">手动同步</h2>
        <p className="text-xs text-muted leading-relaxed mb-3">
          在一台设备导出快照，把文件传到另一台设备后导入即可。导入是合并而非覆盖，两台设备的数据都会保留。
        </p>
        <div className="flex flex-wrap gap-2">
          <button
            onClick={() => void handleExport()}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
          >
            <Download size={15} aria-hidden /> 导出设备快照
          </button>
          <button
            onClick={() => void handleImport()}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:text-primary disabled:opacity-50"
          >
            <Upload size={15} aria-hidden /> 从文件导入快照
          </button>
        </div>
        {msg && (
          <p className="mt-3 text-xs text-muted leading-relaxed" role="status">
            {msg}
          </p>
        )}
        <p className="mt-3 text-xs text-muted leading-relaxed">
          说明：云端自动同步与备份仍在后续里程碑中，届时本页操作方式保持不变。
        </p>
      </section>

      <section className="bg-surface border border-border rounded-md p-4 shadow-ring">
        <div className="flex items-center gap-2 mb-2">
          <HardDriveDownload size={18} className="text-primary" aria-hidden />
          <h2 className="text-base font-semibold">自动备份</h2>
          <span className="text-xs text-muted">共 {status?.backupCount ?? backups.length} 份</span>
        </div>
        <p className="text-xs text-muted leading-relaxed mb-3">
          每次导入快照前会先自动备份一份整库文件，应用每天首次启动时也会备份一次；
          超出保留份数的部分按时间从最旧开始清理。备份保存在：
          <span className="tnum break-all"> {status?.backupDir ?? '—'}</span>
        </p>
        <div className="flex flex-wrap items-end gap-3">
          <button
            onClick={() => void handleCreateBackup()}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:text-primary disabled:opacity-50"
          >
            <Database size={15} aria-hidden /> 立即备份
          </button>
          <label className="flex items-center gap-2 text-xs text-muted">
            保留份数
            <input
              type="number"
              min={1}
              max={60}
              value={keep}
              disabled={busy}
              onChange={(e) => setKeep(Number(e.target.value) || 1)}
              className="w-16 rounded-md border border-border bg-background px-2 py-1 text-sm text-foreground tnum"
              aria-label="自动备份保留份数"
            />
          </label>
          <button
            onClick={() => void handleSaveKeep(keep)}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs hover:text-primary disabled:opacity-50"
          >
            <Check size={14} aria-hidden /> 保存
          </button>
        </div>
        {backups.length === 0 ? (
          <p className="mt-3 text-sm text-muted">还没有备份。点击「立即备份」生成第一份。</p>
        ) : (
          <div className="mt-3 overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs text-muted">
                  <th className="py-1.5 pr-4 font-medium">时间</th>
                  <th className="py-1.5 pr-4 font-medium">文件</th>
                  <th className="py-1.5 pr-4 font-medium">来源</th>
                  <th className="py-1.5 font-medium">大小</th>
                </tr>
              </thead>
              <tbody>
                {backups.slice(0, 8).map((b) => (
                  <tr key={b.file} className="border-t border-border">
                    <td className="py-1.5 pr-4 tnum whitespace-nowrap">{b.at}</td>
                    <td className="py-1.5 pr-4 tnum break-all">{b.file}</td>
                    <td className="py-1.5 pr-4">{backupTagLabel(b.tag)}</td>
                    <td className="py-1.5 tnum whitespace-nowrap">{formatSize(b.size)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section className="bg-surface border border-border rounded-md p-4 shadow-ring">
        <div className="flex items-center gap-2 mb-3">
          {unresolved > 0 ? (
            <TriangleAlert size={18} className="text-primary" aria-hidden />
          ) : (
            <CircleAlert size={18} className="text-muted" aria-hidden />
          )}
          <h2 className="text-base font-semibold">冲突记录</h2>
          <span className="text-xs text-muted">未解 {unresolved} 条</span>
        </div>
        {conflicts.length === 0 ? (
          <p className="text-sm text-muted">暂无冲突。同一记录在多台设备上被先后修改时，这里会列出被保留的本地版本。</p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs text-muted">
                  <th className="py-1.5 pr-4 font-medium">数据表</th>
                  <th className="py-1.5 pr-4 font-medium">记录主键</th>
                  <th className="py-1.5 pr-4 font-medium">来源设备</th>
                  <th className="py-1.5 font-medium">记录时间</th>
                </tr>
              </thead>
              <tbody>
                {conflicts.map((c) => (
                  <tr key={c.id} className="border-t border-border">
                    <td className="py-1.5 pr-4">{c.tbl || '—'}</td>
                    <td className="py-1.5 pr-4 tnum break-all">{c.rowKey || '—'}</td>
                    <td className="py-1.5 pr-4 tnum">{c.device || '未知'}</td>
                    <td className="py-1.5 tnum whitespace-nowrap">{c.createdAt || '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
