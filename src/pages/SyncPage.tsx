// 数据同步页（多设备同步 M2/M3）：云通道推送/拉取 + 手动文件导出/导入 + 冲突对比与裁决 + 状态总览。
//
// 传输无关：云通道（M2）与文件（M3）走同一份「设备快照」载荷与同一套 LWW 合并语义，
// 因此「换后端不改界面」——目录通道、自建 relay、CloudBase PG 直连在界面上只体现为配置项差异。
// 口径：快照 = 全部存活行的最新状态 + 删除墓碑；导入/拉取按行 LWW 合并（更新的那方获胜），
// 不做整库覆盖，因此可反复执行、多设备收敛。
// M3 冲突裁决：LWW 判负的远端版本会落 sync_conflicts；本页展开任意一条即可逐字段对比
// 「本地（当前保留）」和「远端（被拒）」，并选择保留本地（丢弃远端）或采用远端（覆盖本地）。
// 采用远端会把裁决结果作为**新版本**写回并记入变更流水，从而传播给其它设备。
import { useEffect, useState } from 'react';
import { save, open } from '@tauri-apps/api/dialog';
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
  Cloud,
  FolderOpen,
  ChevronRight,
  ChevronDown,
} from 'lucide-react';
import {
  isTauri,
  isMobile,
  syncStatus,
  syncListConflicts,
  syncConflictDetail,
  syncConflictResolve,
  syncConflictsResolveAll,
  syncExportSnapshot,
  syncExportSnapshotB64,
  syncImportSnapshot,
  syncImportSnapshotB64,
  syncCreateBackup,
  syncListBackups,
  syncSetBackupKeep,
  syncRestoreBackup,
  syncDeleteBackup,
  syncCloudConfigGet,
  syncCloudConfigSet,
  syncCloudCheck,
  syncCloudPush,
  syncCloudPull,
  type SyncStatus,
  type SyncConflictRow,
  type SyncConflictDetail,
  type SyncConflictChoice,
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
  if (tag === 'before-restore') return '恢复前自动';
  return tag || '—';
}

function cloudModeLabel(mode: string): string {
  if (mode === 'dir') return '本地目录';
  if (mode === 'cloud') return 'HTTP 服务';
  if (mode === 'pg') return 'CloudBase（PG）';
  return '未启用';
}

function stamp(): string {
  return new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-');
}

/** 冲突里 rowKey 是业务主键的 JSON 数组文本（如 `["000001"]`）→ 展示成 `000001`。 */
function rowKeyLabel(rowKey: string): string {
  try {
    const arr: unknown = JSON.parse(rowKey);
    if (Array.isArray(arr)) return arr.map((x) => String(x)).join(' / ');
  } catch {
    // 非法 JSON：原样展示，便于排障
  }
  return rowKey || '—';
}

/** 字段值展示：null/空串显式区分，避免「看不出是空还是没值」。 */
function formatFieldValue(v: unknown): string {
  if (v === null || v === undefined) return '—';
  if (typeof v === 'string') return v === '' ? '(空)' : v;
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  return JSON.stringify(v);
}

/** 冲突的远端意图文案。 */
function conflictOpLabel(op: SyncConflictDetail['op']): string {
  if (op === 'delete') return '远端删除了这条记录';
  if (op === 'corrupt') return '远端数据无法解析';
  return '远端修改了这条记录';
}

export default function SyncPage() {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [conflicts, setConflicts] = useState<SyncConflictRow[]>([]);
  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [keep, setKeep] = useState(7);
  const [msg, setMsg] = useState<{ text: string; kind: 'ok' | 'err' } | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  /** 顶部常驻状态条：成功/提示用 ok，失败用 err（失败以 text-primary 强调）。 */
  function ok(t: string) {
    setMsg({ text: t, kind: 'ok' });
  }
  function fail(t: string) {
    setMsg({ text: t, kind: 'err' });
  }

  // 备份列表：默认展示前 8 条，超出可展开查看全部（避免老备份无法恢复/删除）。
  const [showAllBackups, setShowAllBackups] = useState(false);

  // M3 冲突裁决：差异按需拉取（列表不携带 payload，避免一次拉 200 条大字段）
  const [openConflict, setOpenConflict] = useState<number | null>(null);
  const [conflictDetail, setConflictDetail] = useState<SyncConflictDetail | null>(null);
  const [detailBusy, setDetailBusy] = useState(false);
  const [resolving, setResolving] = useState(false);

  // 云通道配置表单（令牌不回显：留空=不修改，另有「清除令牌」按钮）
  const [cloudMode, setCloudMode] = useState('off');
  const [cloudEndpoint, setCloudEndpoint] = useState('');
  const [cloudDir, setCloudDir] = useState('');
  const [cloudToken, setCloudToken] = useState('');
  const [cloudTokenSet, setCloudTokenSet] = useState(false);

  async function refresh() {
    try {
      const [s, c, b, cfg] = await Promise.all([
        syncStatus(),
        syncListConflicts(),
        syncListBackups(),
        syncCloudConfigGet(),
      ]);
      setStatus(s);
      setConflicts(c);
      setBackups(b);
      setKeep(s.backupKeep);
      setCloudMode(cfg.mode);
      setCloudEndpoint(cfg.endpoint);
      setCloudDir(cfg.dir);
      setCloudTokenSet(cfg.tokenSet);
      setCloudToken('');
    } catch (e) {
      fail(`读取同步状态失败：${errText(e)}`);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  /** 展开/收起某条冲突的字段级差异。 */
  async function toggleConflict(id: number) {
    if (openConflict === id) {
      setOpenConflict(null);
      setConflictDetail(null);
      return;
    }
    setOpenConflict(id);
    setConflictDetail(null);
    setDetailBusy(true);
    try {
      setConflictDetail(await syncConflictDetail(id));
    } catch (e) {
      fail(`读取冲突详情失败：${errText(e)}`);
      setOpenConflict(null);
    } finally {
      setDetailBusy(false);
    }
  }

  /** 裁决一条冲突。采用远端会改写本地数据，先让用户确认。 */
  async function resolveOne(c: SyncConflictRow, choice: SyncConflictChoice) {
    if (
      choice === 'remote' &&
      !window.confirm('采用远端会用远端版本覆盖本地这条记录，且不可撤销。确定继续？')
    ) {
      return;
    }
    setResolving(true);
    try {
      const out = await syncConflictResolve(c.id, choice);
      ok(
        `${c.tableLabel} · ${rowKeyLabel(c.rowKey)}：已${
          choice === 'remote' ? '采用远端' : '保留本地'
        }（写回 ${out.applied} 行）`,
      );
      setOpenConflict(null);
      setConflictDetail(null);
      await refresh();
    } catch (e) {
      fail(`裁决失败：${errText(e)}`);
    } finally {
      setResolving(false);
    }
  }

  /** 批量裁决全部未解冲突。 */
  async function resolveAll(choice: SyncConflictChoice) {
    const n = status?.conflictCount ?? 0;
    if (n === 0) return;
    const verb = choice === 'remote' ? '采用远端（用远端版本覆盖本地）' : '保留本地（丢弃远端版本）';
    if (!window.confirm(`将对全部 ${n} 条未解冲突执行「${verb}」，且不可撤销。确定继续？`)) return;
    setResolving(true);
    try {
      const out = await syncConflictsResolveAll(choice);
      const extra =
        out.failed > 0 ? `；${out.failed} 条因远端数据损坏未能处理，仍保留在列表中` : '';
      ok(`已处理 ${out.resolved} 条冲突${extra}。`);
      setOpenConflict(null);
      setConflictDetail(null);
      await refresh();
    } catch (e) {
      fail(`批量裁决失败：${errText(e)}`);
    } finally {
      setResolving(false);
    }
  }

  async function handleExport() {
    if (!isTauri) {
      ok('浏览器预览模式不支持真实导出，请在桌面端或多端 App 中使用。');
      return;
    }
    setBusy(true);
    try {
      if (isMobile) {
        const out = await syncExportSnapshotB64();
        const res = await shareFileMobile(out.fileName, 'application/jsonl', out.data);
        ok(
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
        ok(`已导出快照：${info.path}（${info.count} 条 / ${formatSize(info.size)}）`);
      }
      await refresh();
    } catch (e) {
      fail(`导出失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleImport() {
    if (!isTauri) {
      ok('浏览器预览模式不支持真实导入，请在桌面端或多端 App 中使用。');
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
        ok(importSummary(out.applied, out.conflicts, out.total, out.device, out.backupFile));
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
        ok(importSummary(out.applied, out.conflicts, out.total, out.device, out.backupFile));
      }
      await refresh();
    } catch (e) {
      fail(`导入失败：${errText(e)}`);
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
      ok(`已生成备份：${b.file}（${formatSize(b.size)}）`);
      await refresh();
    } catch (e) {
      fail(`备份失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleSaveKeep(next: number) {
    setBusy(true);
    try {
      const v = await syncSetBackupKeep(next);
      setKeep(v);
      ok(`已把自动备份保留份数设为 ${v} 份，超出的最旧备份已清理。`);
      await refresh();
    } catch (e) {
      fail(`设置保留份数失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  /** 从整库备份恢复：会先用 before-restore 标签自动备份当前整库，再用所选备份整库覆盖回去（破坏性、不可撤销）。 */
  async function handleRestoreBackup(b: BackupEntry) {
    if (
      !window.confirm(
        `将用备份「${b.file}」（${b.at}）整库覆盖当前全部数据，此操作不可撤销。\n` +
          '恢复前会自动再备份一份当前数据作为安全网。确定继续？',
      )
    ) {
      return;
    }
    setBusy(true);
    try {
      const out = await syncRestoreBackup(b.file);
      let text = `已从 ${out.file} 恢复整库`;
      if (out.safetyBackup) {
        text += `；恢复前已自动备份当前数据为 ${out.safetyBackup}`;
      } else {
        text += '。⚠ 警告：未能生成恢复前的安全备份，当前数据已被覆盖且无法还原。';
      }
      ok(text);
      await refresh();
    } catch (e) {
      fail(`恢复失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  /** 删除一份整库备份文件（不可撤销）。 */
  async function handleDeleteBackup(b: BackupEntry) {
    if (!window.confirm(`确定删除备份文件 ${b.file}？此操作不可撤销。`)) return;
    setBusy(true);
    try {
      await syncDeleteBackup(b.file);
      ok(`已删除备份：${b.file}`);
      await refresh();
    } catch (e) {
      fail(`删除失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleSaveCloud(clearToken = false) {
    setBusy(true);
    try {
      const cfg = await syncCloudConfigSet(
        cloudMode,
        cloudEndpoint,
        cloudDir,
        clearToken ? '' : cloudToken.trim() === '' ? null : cloudToken,
      );
      setCloudMode(cfg.mode);
      setCloudEndpoint(cfg.endpoint);
      setCloudDir(cfg.dir);
      setCloudTokenSet(cfg.tokenSet);
      setCloudToken('');
      ok(
        clearToken
          ? '已清除同步令牌。'
          : cfg.mode === 'off'
            ? '已关闭云通道，本机不再自动同步。'
            : cfg.ready
              ? '云通道配置已保存，可以开始推送或拉取。'
              : '配置已保存，但还不完整：请补齐所选模式需要的地址/令牌/目录。',
      );
      await refresh();
    } catch (e) {
      fail(`保存云通道配置失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handlePickDir() {
    if (!isTauri) {
      ok('浏览器预览模式无法选择目录，请在桌面端操作。');
      return;
    }
    try {
      const picked = await open({ directory: true, multiple: false });
      if (typeof picked === 'string' && picked) setCloudDir(picked);
    } catch (e) {
      fail(`选择目录失败：${errText(e)}`);
    }
  }

  async function handleCloudCheck() {
    setBusy(true);
    try {
      const c = await syncCloudCheck();
      ok(
        `连接正常：远端共 ${c.items} 份快照，其中来自其它设备 ${c.others} 份` +
          `（本机标识 ${c.deviceId}）。`,
      );
    } catch (e) {
      fail(`连接失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleCloudPush() {
    setBusy(true);
    try {
      const p = await syncCloudPush();
      ok(`已推送本机快照：${p.count} 条记录 / ${formatSize(p.size)}。`);
      await refresh();
    } catch (e) {
      fail(`推送失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleCloudPull() {
    const confirmText =
      '从云端拉取会把其他设备的记录与本地数据按行合并：同一条记录以更新时间更晚的一方为准，' +
      '不会整库覆盖本地数据。合并前会自动备份一份整库文件。确定继续？';
    if (!window.confirm(confirmText)) return;
    setBusy(true);
    try {
      const r = await syncCloudPull();
      if (r.planned === 0) {
        ok('已是最新：云端没有本机尚未合并的其他设备快照。');
      } else {
        const base = `拉取完成：合并了 ${r.pulled} 台设备的快照，应用 ${r.applied} 条`;
        ok(
          r.conflicts > 0
            ? `${base}；有 ${r.conflicts} 条因本地记录更新更晚而保留本地版本，已在下方冲突列表列出。`
            : `${base}。`,
        );
      }
      await refresh();
    } catch (e) {
      fail(`拉取失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  }

  const pending = status?.pendingChanges ?? 0;
  const unresolved = status?.conflictCount ?? 0;
  const cloudReady = status?.cloudReady ?? false;
  const visibleBackups = showAllBackups ? backups : backups.slice(0, 8);

  return (
    <div className="p-4 sm:p-6 space-y-5 max-w-3xl">
      <header>
        <div className="flex items-center gap-2">
          <ArrowLeftRight size={22} className="text-primary" aria-hidden />
          <h1 className="text-xl font-semibold">数据同步</h1>
        </div>
        <p className="mt-2 text-sm text-muted leading-relaxed">
          把本机的持仓、基金、交易、快照与设置等用户数据打包成一份「设备快照」，在另一台设备合并即可多端一致。
          合并按行取更新时间更晚的一方（LWW），可重复执行、多设备逐步收敛；不包含净值历史与行情缓存等派
          生数据——那些由各设备自行从官方源重取。快照的搬运方式可选「云端同步」（一键推送/拉取）或「手动文件」。
        </p>
      </header>

      {msg && (
        <div
          role="status"
          className={`rounded-md border border-border p-3 text-sm leading-relaxed ${
            msg.kind === 'err' ? 'text-primary' : 'text-muted'
          }`}
        >
          {msg.text}
        </div>
      )}

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
        <p className="mt-3 text-xs text-muted leading-relaxed">
          说明：文件通道适合偶尔一次性搬运；日常多设备同步请用下方「云端同步」，一键推送/拉取即可。
        </p>
      </section>

      <section className="bg-surface border border-border rounded-md p-4 shadow-ring">
        <div className="flex items-center gap-2 mb-2">
          <Cloud size={18} className="text-primary" aria-hidden />
          <h2 className="text-base font-semibold">云端同步</h2>
          <span className="text-xs text-muted">
            {status?.cloudMode === 'off' ? '未启用' : status?.cloudReady ? '已启用' : '配置待补全'}
          </span>
        </div>
        <p className="text-xs text-muted leading-relaxed mb-3">
          配置一个「远端」后，本机可一键推送自己的快照、拉取其他设备的快照并按行合并；
          合并规则与文件导入完全一致（更新时间更晚的一方获胜，不整库覆盖）。
          远端只保存用户数据快照，不含净值历史与行情缓存等派生数据。
        </p>

        <div className="space-y-3">
          <div>
            <label className="block text-xs text-muted mb-1" htmlFor="cloud-mode">
              同步通道
            </label>
            <select
              id="cloud-mode"
              value={cloudMode}
              disabled={busy}
              onChange={(e) => setCloudMode(e.target.value)}
              className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm text-foreground disabled:opacity-50"
            >
              <option value="off">关闭（本机不主动同步）</option>
              <option value="dir">本地目录 / 网盘同步盘</option>
              <option value="cloud">HTTP 服务（自建 relay 或云函数）</option>
              <option value="pg">CloudBase（PostgreSQL 直连）</option>
            </select>
          </div>

          {cloudMode === 'dir' && (
            <div>
              <label className="block text-xs text-muted mb-1" htmlFor="cloud-dir">
                同步目录
              </label>
              <div className="flex gap-2">
                <input
                  id="cloud-dir"
                  value={cloudDir}
                  disabled={busy}
                  onChange={(e) => setCloudDir(e.target.value)}
                  placeholder="例如 ~/OneDrive/fundlens-sync"
                  className="flex-1 rounded-md border border-border bg-background px-2 py-1.5 text-sm text-foreground tnum disabled:opacity-50"
                />
                <button
                  onClick={() => void handlePickDir()}
                  disabled={busy}
                  className="inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs hover:text-primary disabled:opacity-50"
                >
                  <FolderOpen size={14} aria-hidden /> 选择
                </button>
              </div>
              <p className="mt-1 text-xs text-muted leading-relaxed">
                把该目录放进网盘同步盘（或局域网共享盘），多台设备指向同一目录即可互相看见对方的快照。
              </p>
            </div>
          )}

          {(cloudMode === 'cloud' || cloudMode === 'pg') && (
            <>
              <div>
                <label className="block text-xs text-muted mb-1" htmlFor="cloud-endpoint">
                  {cloudMode === 'pg' ? 'CloudBase REST 基址' : '服务地址'}
                </label>
                <input
                  id="cloud-endpoint"
                  value={cloudEndpoint}
                  disabled={busy}
                  onChange={(e) => setCloudEndpoint(e.target.value)}
                  placeholder={
                    cloudMode === 'pg'
                      ? 'https://<环境ID>.api.tcloudbasegateway.com/v1/rdb/rest'
                      : 'https://sync.example.com/fundlens'
                  }
                  className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm text-foreground tnum disabled:opacity-50"
                />
                {cloudMode === 'pg' && (
                  <p className="mt-1 text-xs text-muted leading-relaxed">
                    直连环境自带的 PostgreSQL，无需部署任何服务或云函数；远端只存快照文本。
                  </p>
                )}
              </div>
              <div>
                <label className="block text-xs text-muted mb-1" htmlFor="cloud-token">
                  {cloudMode === 'pg' ? 'CloudBase API Key' : '同步令牌'}
                </label>
                <input
                  id="cloud-token"
                  type="password"
                  value={cloudToken}
                  disabled={busy}
                  onChange={(e) => setCloudToken(e.target.value)}
                  placeholder={cloudTokenSet ? '已设置（留空表示不修改）' : '尚未设置'}
                  className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm text-foreground disabled:opacity-50"
                />
                <p className="mt-1 text-xs text-muted leading-relaxed">
                  {cloudMode === 'pg'
                    ? '在 CloudBase 控制台「身份认证 → API Key」创建（角色 service_role）。密钥只保存在本机同步元数据里，不会随快照上传到远端。'
                    : '令牌只保存在本机数据库的同步元数据里，不会随快照上传到远端。'}
                </p>
              </div>
            </>
          )}

          <div className="flex flex-wrap gap-2">
            <button
              onClick={() => void handleSaveCloud()}
              disabled={busy}
              className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
            >
              <Check size={15} aria-hidden /> 保存配置
            </button>
            <button
              onClick={() => void handleCloudCheck()}
              disabled={busy || !cloudReady}
              className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:text-primary disabled:opacity-50"
            >
              <RefreshCw size={15} aria-hidden /> 测试连接
            </button>
            <button
              onClick={() => void handleCloudPush()}
              disabled={busy || !cloudReady}
              className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:text-primary disabled:opacity-50"
            >
              <Upload size={15} aria-hidden /> 立即推送
            </button>
            <button
              onClick={() => void handleCloudPull()}
              disabled={busy || !cloudReady}
              className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:text-primary disabled:opacity-50"
            >
              <Download size={15} aria-hidden /> 立即拉取
            </button>
            {(cloudMode === 'cloud' || cloudMode === 'pg') && cloudTokenSet ? (
              <button
                onClick={() => void handleSaveCloud(true)}
                disabled={busy}
                className="inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-muted hover:text-primary disabled:opacity-50"
              >
                清除令牌
              </button>
            ) : null}
          </div>

          <dl className="grid grid-cols-2 sm:grid-cols-3 gap-3 text-sm pt-3 border-t border-border">
            <div>
              <dt className="text-xs text-muted mb-0.5">通道</dt>
              <dd>{cloudModeLabel(status?.cloudMode ?? 'off')}</dd>
            </div>
            <div>
              <dt className="text-xs text-muted mb-0.5">最近推送</dt>
              <dd className="tnum">{status?.cloudLastPush ?? '尚未推送'}</dd>
            </div>
            <div>
              <dt className="text-xs text-muted mb-0.5">最近拉取</dt>
              <dd className="tnum">{status?.cloudLastPull ?? '尚未拉取'}</dd>
            </div>
            <div>
              <dt className="text-xs text-muted mb-0.5">已合并设备</dt>
              <dd className="tnum">{status?.cloudPeers ?? 0} 台</dd>
            </div>
            <div className="col-span-2">
              <dt className="text-xs text-muted mb-0.5">远端位置</dt>
              <dd className="tnum break-all">
                {status?.cloudMode === 'off' || !status?.cloudMode
                  ? '—'
                  : status?.cloudMode === 'dir'
                    ? status?.cloudDir || '—'
                    : status?.cloudEndpoint || '—'}
              </dd>
            </div>
          </dl>
        </div>
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
          <span className="text-xs text-muted">当前生效 {status?.backupKeep ?? keep} 份</span>
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
                  <th className="py-1.5 pr-4 font-medium">大小</th>
                  <th className="py-1.5 font-medium">操作</th>
                </tr>
              </thead>
              <tbody>
                {visibleBackups.map((b) => (
                  <tr key={b.file} className="border-t border-border">
                    <td className="py-1.5 pr-4 tnum whitespace-nowrap">{b.at}</td>
                    <td className="py-1.5 pr-4 tnum break-all">{b.file}</td>
                    <td className="py-1.5 pr-4">{backupTagLabel(b.tag)}</td>
                    <td className="py-1.5 pr-4 tnum whitespace-nowrap">{formatSize(b.size)}</td>
                    <td className="py-1.5">
                      <div className="flex gap-1.5">
                        <button
                          type="button"
                          onClick={() => void handleRestoreBackup(b)}
                          disabled={busy}
                          className="rounded-md border border-border px-2.5 py-1 text-xs hover:text-primary disabled:opacity-50"
                        >
                          恢复
                        </button>
                        <button
                          type="button"
                          onClick={() => void handleDeleteBackup(b)}
                          disabled={busy}
                          className="rounded-md border border-border px-2.5 py-1 text-xs hover:text-primary disabled:opacity-50"
                        >
                          删除
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {backups.length > 8 && (
              <button
                type="button"
                onClick={() => setShowAllBackups((v) => !v)}
                className="mt-2 rounded-md border border-border px-2.5 py-1 text-xs hover:text-primary"
              >
                {showAllBackups ? `收起（共 ${backups.length} 条）` : `展开全部 ${backups.length} 条`}
              </button>
            )}
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
          {unresolved > 0 && (
            <div className="ml-auto flex items-center gap-1.5">
              <button
                type="button"
                onClick={() => void resolveAll('local')}
                disabled={resolving}
                className="rounded-md border border-border px-2.5 py-1 text-xs hover:text-primary disabled:opacity-50"
              >
                全部保留本地
              </button>
              <button
                type="button"
                onClick={() => void resolveAll('remote')}
                disabled={resolving}
                className="rounded-md border border-border px-2.5 py-1 text-xs hover:text-primary disabled:opacity-50"
              >
                全部采用远端
              </button>
            </div>
          )}
        </div>

        {conflicts.length === 0 ? (
          <p className="text-sm text-muted">
            暂无冲突。同一记录在多台设备上被先后修改时，这里会列出两份版本供你选择保留哪一份。
          </p>
        ) : (
          <>
            <p className="mb-3 text-xs text-muted">
              展开任意一条可逐字段对比「本地」与「远端」；选择保留本地即丢弃远端那一版，
              选择采用远端会用远端版本覆盖本地。
            </p>
            <div className="space-y-2">
              {conflicts.map((c) => {
                const open = openConflict === c.id;
                const done = c.resolved !== 0;
                return (
                  <div key={c.id} className="rounded-md border border-border">
                    <button
                      type="button"
                      onClick={() => void toggleConflict(c.id)}
                      aria-expanded={open}
                      className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:text-primary"
                    >
                      {open ? (
                        <ChevronDown size={16} className="shrink-0" aria-hidden />
                      ) : (
                        <ChevronRight size={16} className="shrink-0" aria-hidden />
                      )}
                      <span className="font-medium">{c.tableLabel || c.tbl || '—'}</span>
                      <span className="tnum break-all text-muted">{rowKeyLabel(c.rowKey)}</span>
                      <span className="ml-auto shrink-0 text-xs text-muted">
                        {done ? '已解' : '未解'} · 来自 {c.device || '未知'}
                      </span>
                      <span className="tnum shrink-0 whitespace-nowrap text-xs text-muted">
                        {c.createdAt || '—'}
                      </span>
                    </button>

                    {open && (
                      <div className="border-t border-border px-3 py-3">
                        {detailBusy && <p className="text-sm text-muted">正在读取差异…</p>}
                        {!detailBusy && conflictDetail && conflictDetail.id === c.id && (
                          <>
                            <div className="mb-2 flex flex-wrap items-center gap-2 text-xs text-muted">
                              <span>{conflictOpLabel(conflictDetail.op)}</span>
                              {conflictDetail.op === 'upsert' && (
                                <span>· 本地{conflictDetail.localExists ? '存在该记录' : '已无该记录'}</span>
                              )}
                              {conflictDetail.identical && (
                                <span className="text-primary">· 两份内容已一致，无需改动数据</span>
                              )}
                            </div>

                            {conflictDetail.payloadError && (
                              <p className="mb-2 text-sm text-primary">
                                远端数据无法解析：{conflictDetail.payloadError}
                              </p>
                            )}

                            {conflictDetail.fields.length > 0 ? (
                              <div className="overflow-x-auto">
                                <table className="w-full text-sm">
                                  <thead>
                                    <tr className="text-left text-xs text-muted">
                                      <th className="py-1.5 pr-4 font-medium">字段</th>
                                      <th className="py-1.5 pr-4 font-medium">本地（当前保留）</th>
                                      <th className="py-1.5 font-medium">远端（被拒）</th>
                                    </tr>
                                  </thead>
                                  <tbody>
                                    {conflictDetail.fields.map((f) => (
                                      <tr key={f.col} className="border-t border-border align-top">
                                        <td className="py-1.5 pr-4 font-medium">{f.col}</td>
                                        <td className="py-1.5 pr-4 break-all">
                                          {formatFieldValue(f.local)}
                                        </td>
                                        <td className="py-1.5 break-all text-muted">
                                          {formatFieldValue(f.remote)}
                                        </td>
                                      </tr>
                                    ))}
                                  </tbody>
                                </table>
                              </div>
                            ) : (
                              <p className="text-sm text-muted">
                                {conflictDetail.op === 'delete'
                                  ? '远端意图删除这条记录，没有可对比的字段。'
                                  : '没有字段差异。'}
                              </p>
                            )}

                            {!done && (
                              <div className="mt-3 flex items-center gap-2">
                                <button
                                  type="button"
                                  onClick={() => void resolveOne(c, 'local')}
                                  disabled={resolving}
                                  className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:text-primary disabled:opacity-50"
                                >
                                  <Check size={14} aria-hidden />
                                  保留本地
                                </button>
                                <button
                                  type="button"
                                  onClick={() => void resolveOne(c, 'remote')}
                                  disabled={resolving || conflictDetail.op === 'corrupt'}
                                  title={
                                    conflictDetail.op === 'corrupt'
                                      ? '远端数据无法解析，不能采用'
                                      : undefined
                                  }
                                  className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:text-primary disabled:opacity-50"
                                >
                                  <ArrowLeftRight size={14} aria-hidden />
                                  采用远端
                                </button>
                              </div>
                            )}
                          </>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </>
        )}
      </section>
    </div>
  );
}
