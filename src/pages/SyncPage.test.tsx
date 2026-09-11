import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import SyncPage from './SyncPage';
import { ThemeProvider } from '../theme';
import * as api from '../api';

vi.mock('@tauri-apps/api/dialog', () => ({
  save: vi.fn(),
  open: vi.fn(),
}));

vi.mock('../api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api')>();
  return {
    ...actual,
    isTauri: true,
    syncStatus: vi.fn(),
    syncListConflicts: vi.fn(),
    syncConflictDetail: vi.fn(),
    syncConflictResolve: vi.fn(),
    syncConflictsResolveAll: vi.fn(),
    syncExportSnapshot: vi.fn(),
    syncImportSnapshot: vi.fn(),
    syncListBackups: vi.fn(),
    syncCreateBackup: vi.fn(),
    syncSetBackupKeep: vi.fn(),
    syncRestoreBackup: vi.fn(),
    syncDeleteBackup: vi.fn(),
    syncCloudConfigGet: vi.fn(),
    syncCloudConfigSet: vi.fn(),
    syncCloudCheck: vi.fn(),
    syncCloudPush: vi.fn(),
    syncCloudPull: vi.fn(),
  };
});

const mockedStatus = vi.mocked(api.syncStatus);
const mockedConflicts = vi.mocked(api.syncListConflicts);
const mockedBackups = vi.mocked(api.syncListBackups);
const mockedCloudConfig = vi.mocked(api.syncCloudConfigGet);
const mockedDetail = vi.mocked(api.syncConflictDetail);
const mockedResolve = vi.mocked(api.syncConflictResolve);
const mockedResolveAll = vi.mocked(api.syncConflictsResolveAll);
const mockedRestore = vi.mocked(api.syncRestoreBackup);
const mockedDelete = vi.mocked(api.syncDeleteBackup);
const mockedCreateBackup = vi.mocked(api.syncCreateBackup);

/** 状态卡默认值：云通道未启用。 */
function statusFixture(over: Partial<api.SyncStatus> = {}): api.SyncStatus {
  return {
    deviceId: 'dev-abc-123',
    tablesSynced: 13,
    pendingChanges: 4,
    totalChanges: 120,
    lastExportAt: '2026-09-10 20:00:00',
    lastImportAt: null,
    conflictCount: 0,
    backupKeep: 7,
    backupCount: 1,
    lastBackupAt: '2026-09-10 19:00:00',
    backupDir: '/tmp/data/backups',
    cloudMode: 'off',
    cloudReady: false,
    cloudEndpoint: '',
    cloudDir: '',
    cloudTokenSet: false,
    cloudLastPush: null,
    cloudLastPull: null,
    cloudPeers: 0,
    ...over,
  };
}

function cloudConfigFixture(over: Partial<api.CloudConfigInfo> = {}): api.CloudConfigInfo {
  return { mode: 'off', endpoint: '', dir: '', tokenSet: false, ready: false, ...over };
}

/** 一条未解的持仓冲突（LWW 判负的远端版本）。 */
function conflictRowFixture(over: Partial<api.SyncConflictRow> = {}): api.SyncConflictRow {
  return {
    id: 1,
    tbl: 'positions',
    tableLabel: '持仓',
    rowKey: '["7"]',
    device: 'dev-other-9',
    resolved: 0,
    createdAt: '2026-09-10 21:00:00',
    ...over,
  };
}

/** 冲突详情：shares 数值不同、cost 本地为空。 */
function conflictDetailFixture(over: Partial<api.SyncConflictDetail> = {}): api.SyncConflictDetail {
  return {
    id: 1,
    tbl: 'positions',
    tableLabel: '持仓',
    rowKey: '["7"]',
    device: 'dev-other-9',
    createdAt: '2026-09-10 21:00:00',
    resolved: false,
    op: 'upsert',
    localExists: true,
    fields: [
      { col: 'shares', local: 1000, remote: 1200 },
      { col: 'cost', local: null, remote: 1.5 },
    ],
    identical: false,
    payloadError: null,
    ...over,
  };
}

describe('SyncPage', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedStatus.mockResolvedValue(statusFixture());
    mockedConflicts.mockResolvedValue([]);
    mockedBackups.mockResolvedValue([
      { file: 'fundlens-20260910-190000-auto.db', size: 2048, at: '2026-09-10 19:00:00', tag: 'auto' },
    ]);
    mockedCloudConfig.mockResolvedValue(cloudConfigFixture());
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('渲染同步状态与手动操作入口，无冲突时给出空态说明', async () => {
    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(screen.getByRole('heading', { name: '数据同步' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /导出设备快照/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /从文件导入快照/ })).toBeInTheDocument();

    await waitFor(() => {
      expect(mockedStatus).toHaveBeenCalled();
    });
    expect(await screen.findByText('dev-abc-123')).toBeInTheDocument();
    expect(screen.getByText('13 张')).toBeInTheDocument();
    expect(screen.getByText('2026-09-10 20:00:00')).toBeInTheDocument();
    expect(screen.getByText('尚未导入')).toBeInTheDocument();
    expect(await screen.findByText(/暂无冲突/)).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: '自动备份' })).toBeInTheDocument();
    expect(await screen.findByText('fundlens-20260910-190000-auto.db')).toBeInTheDocument();
    expect(screen.getByText('每日自动')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /立即备份/ })).toBeInTheDocument();
  });

  it('存在冲突时列出表名标签、主键与来源设备', async () => {
    mockedStatus.mockResolvedValue(
      statusFixture({ lastExportAt: null, lastImportAt: '2026-09-10 21:00:00', conflictCount: 1 }),
    );
    mockedConflicts.mockResolvedValue([conflictRowFixture()]);

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(await screen.findByText('持仓')).toBeInTheDocument();
    // rowKey 是 JSON 数组文本，展示时解析成可读主键
    expect(screen.getByText('7')).toBeInTheDocument();
    expect(screen.getByText(/来自 dev-other-9/)).toBeInTheDocument();
    expect(screen.queryByText(/暂无冲突/)).not.toBeInTheDocument();
    // 有未解冲突时出现批量入口
    expect(screen.getByRole('button', { name: '全部保留本地' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '全部采用远端' })).toBeInTheDocument();
  });

  it('M3：展开冲突显示字段级差异（本地 vs 远端），空值显式区分', async () => {
    mockedStatus.mockResolvedValue(statusFixture({ conflictCount: 1 }));
    mockedConflicts.mockResolvedValue([conflictRowFixture()]);
    mockedDetail.mockResolvedValue(conflictDetailFixture());

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: /持仓/ }));

    expect(await screen.findByText('远端修改了这条记录')).toBeInTheDocument();
    expect(screen.getByText('字段')).toBeInTheDocument();
    expect(screen.getByText('本地（当前保留）')).toBeInTheDocument();
    expect(screen.getByText('远端（被拒）')).toBeInTheDocument();
    expect(screen.getByText('shares')).toBeInTheDocument();
    expect(screen.getByText('1000')).toBeInTheDocument();
    expect(screen.getByText('1200')).toBeInTheDocument();
    expect(screen.getByText('cost')).toBeInTheDocument();
    // 本地值为 null → 该行显示占位符而非空白（页面上别处也有 '—'，故限定在 cost 所在行内断言）
    const costRow = screen.getByText('cost').closest('tr');
    expect(costRow?.textContent).toContain('—');
    expect(mockedDetail).toHaveBeenCalledWith(1);
  });

  it('M3：保留本地直接调用后端并刷新，无需二次确认', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    mockedStatus.mockResolvedValue(statusFixture({ conflictCount: 1 }));
    mockedConflicts.mockResolvedValue([conflictRowFixture()]);
    mockedDetail.mockResolvedValue(conflictDetailFixture());
    mockedResolve.mockResolvedValue({ resolved: 1, applied: 0, failed: 0 });

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: /持仓/ }));
    fireEvent.click(await screen.findByRole('button', { name: '保留本地' }));

    await waitFor(() => expect(mockedResolve).toHaveBeenCalledWith(1, 'local'));
    expect(confirmSpy).not.toHaveBeenCalled();
    expect(await screen.findByText(/已保留本地/)).toBeInTheDocument();
  });

  it('M3：采用远端会二次确认；用户取消则不调用后端', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
    mockedStatus.mockResolvedValue(statusFixture({ conflictCount: 1 }));
    mockedConflicts.mockResolvedValue([conflictRowFixture()]);
    mockedDetail.mockResolvedValue(conflictDetailFixture());

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: /持仓/ }));
    fireEvent.click(await screen.findByRole('button', { name: '采用远端' }));

    expect(confirmSpy).toHaveBeenCalled();
    expect(mockedResolve).not.toHaveBeenCalled();
  });

  it('M3：远端载荷损坏时禁用「采用远端」，只允许保留本地', async () => {
    mockedStatus.mockResolvedValue(statusFixture({ conflictCount: 1 }));
    mockedConflicts.mockResolvedValue([conflictRowFixture()]);
    mockedDetail.mockResolvedValue(
      conflictDetailFixture({
        op: 'corrupt',
        localExists: true,
        fields: [],
        payloadError: '冲突载荷解析失败: expected value',
      }),
    );

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: /持仓/ }));

    // 「远端数据无法解析」既是意图标签也是错误段落开头，带冒号才能唯一定位到段落
    expect(await screen.findByText(/远端数据无法解析：/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '采用远端' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '保留本地' })).toBeEnabled();
  });

  it('M3：批量「全部采用远端」确认后调用后端，并报告未能处理的条数', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    mockedStatus.mockResolvedValue(statusFixture({ conflictCount: 3 }));
    mockedConflicts.mockResolvedValue([
      conflictRowFixture({ id: 1 }),
      conflictRowFixture({ id: 2, rowKey: '["8"]' }),
      conflictRowFixture({ id: 3, rowKey: '["9"]' }),
    ]);
    mockedResolveAll.mockResolvedValue({ resolved: 2, applied: 2, failed: 1 });

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: '全部采用远端' }));

    await waitFor(() => expect(mockedResolveAll).toHaveBeenCalledWith('remote'));
    expect(confirmSpy).toHaveBeenCalled();
    expect(await screen.findByText(/1 条因远端数据损坏未能处理/)).toBeInTheDocument();
  });

  it('M3：无未解冲突时不显示批量裁决入口', async () => {
    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(await screen.findByText(/暂无冲突/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '全部保留本地' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '全部采用远端' })).not.toBeInTheDocument();
  });

  it('云通道未配置时：推送/拉取/测试连接均不可点，通道显示未启用', async () => {
    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(await screen.findByRole('heading', { name: '云端同步' })).toBeInTheDocument();
    await waitFor(() => expect(mockedCloudConfig).toHaveBeenCalled());

    // 「未启用」在区块角标与「通道」字段各出现一次
    expect((await screen.findAllByText('未启用')).length).toBeGreaterThanOrEqual(1);
    expect(screen.getByRole('button', { name: /立即推送/ })).toBeDisabled();
    expect(screen.getByRole('button', { name: /立即拉取/ })).toBeDisabled();
    expect(screen.getByRole('button', { name: /测试连接/ })).toBeDisabled();
    expect(screen.getByRole('button', { name: /保存配置/ })).toBeEnabled();
    expect(screen.getByText('尚未推送')).toBeInTheDocument();
    expect(screen.getByText('尚未拉取')).toBeInTheDocument();
  });

  it('云通道为本地目录且已配置时：渲染目录输入、远端位置与已合并设备数，操作可用', async () => {
    mockedCloudConfig.mockResolvedValue(
      cloudConfigFixture({ mode: 'dir', dir: '/tmp/share/fundlens-sync', ready: true }),
    );
    mockedStatus.mockResolvedValue(
      statusFixture({
        cloudMode: 'dir',
        cloudReady: true,
        cloudDir: '/tmp/share/fundlens-sync',
        cloudLastPush: '2026-09-10 22:00:00',
        cloudLastPull: '2026-09-10 22:05:00',
        cloudPeers: 2,
      }),
    );

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(await screen.findByText('已启用')).toBeInTheDocument();
    // 目录模式的输入框与远端位置都应显示配置值
    const dirInputs = await screen.findAllByDisplayValue('/tmp/share/fundlens-sync');
    expect(dirInputs.length).toBeGreaterThan(0);
    expect(screen.getByText('本地目录')).toBeInTheDocument();
    expect(screen.getByText('2026-09-10 22:00:00')).toBeInTheDocument();
    expect(screen.getByText('2 台')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /立即推送/ })).toBeEnabled();
    expect(screen.getByRole('button', { name: /立即拉取/ })).toBeEnabled();
    expect(screen.getByRole('button', { name: /选择/ })).toBeInTheDocument();
  });

  it('HTTP 模式不显示明文令牌，仅提示已设置或未设置', async () => {
    mockedCloudConfig.mockResolvedValue(
      cloudConfigFixture({
        mode: 'cloud',
        endpoint: 'https://relay.example.com/sync',
        tokenSet: true,
        ready: true,
      }),
    );

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    const token = (await screen.findByLabelText('同步令牌')) as HTMLInputElement;
    expect(token.value).toBe('');
    expect(token.getAttribute('placeholder')).toContain('已设置');
    expect(screen.getByRole('button', { name: /清除令牌/ })).toBeInTheDocument();
    expect(screen.getByLabelText('服务地址')).toHaveValue('https://relay.example.com/sync');
  });

  it('CloudBase PG 模式：显示 REST 基址与 API Key 字段，密钥不回显', async () => {
    const base = 'https://env-1.api.tcloudbasegateway.com/v1/rdb/rest';
    mockedCloudConfig.mockResolvedValue(
      cloudConfigFixture({ mode: 'pg', endpoint: base, tokenSet: true, ready: true }),
    );
    mockedStatus.mockResolvedValue(
      statusFixture({ cloudMode: 'pg', cloudReady: true, cloudEndpoint: base }),
    );

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(await screen.findByText('已启用')).toBeInTheDocument();
    // 通道标签与字段文案走 PG 专属分支
    expect(screen.getByText('CloudBase（PG）')).toBeInTheDocument();
    expect(screen.getByLabelText('CloudBase REST 基址')).toHaveValue(base);
    const key = (await screen.findByLabelText('CloudBase API Key')) as HTMLInputElement;
    expect(key.value).toBe('');
    expect(key.getAttribute('placeholder')).toContain('已设置');
    expect(screen.getByRole('button', { name: /清除令牌/ })).toBeInTheDocument();
  });

  it('备份行渲染「恢复」「删除」操作按钮', async () => {
    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(await screen.findByRole('button', { name: '恢复' })).toBeInTheDocument();
    expect(await screen.findByRole('button', { name: '删除' })).toBeInTheDocument();
  });

  it('恢复必须二次确认；用户取消则不调用 syncRestoreBackup', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: '恢复' }));

    expect(confirmSpy).toHaveBeenCalled();
    expect(mockedRestore).not.toHaveBeenCalled();
  });

  it('恢复成功：消息包含恢复文件与安全备份文件名', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    mockedRestore.mockResolvedValue({ file: 'fundlens-restore.db', safetyBackup: 'fundlens-safety.db' });

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: '恢复' }));

    await waitFor(() =>
      expect(mockedRestore).toHaveBeenCalledWith('fundlens-20260910-190000-auto.db'),
    );
    expect(await screen.findByText(/已从 fundlens-restore\.db 恢复整库/)).toBeInTheDocument();
    expect(screen.getByText(/fundlens-safety\.db/)).toBeInTheDocument();
  });

  it('恢复成功但 safetyBackup 为 null：消息给出未能生成安全备份的警告', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    mockedRestore.mockResolvedValue({ file: 'fundlens-restore.db', safetyBackup: null });

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: '恢复' }));

    await waitFor(() => expect(mockedRestore).toHaveBeenCalled());
    expect(await screen.findByText(/未能生成恢复前的安全备份/)).toBeInTheDocument();
  });

  it('删除必须二次确认，确认后调用 syncDeleteBackup 并刷新列表', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    mockedDelete.mockResolvedValue(undefined);

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: '删除' }));

    await waitFor(() =>
      expect(mockedDelete).toHaveBeenCalledWith('fundlens-20260910-190000-auto.db'),
    );
    // 删除后 refresh() 会再次拉取备份列表（首次渲染已拉取一次）
    expect(mockedBackups).toHaveBeenCalledTimes(2);
  });

  it('操作「立即备份」后，消息出现在 header 下方常驻状态条，且不再出现在「手动同步」区块内', async () => {
    mockedCreateBackup.mockResolvedValue({
      file: 'fundlens-new.db',
      size: 1024,
      at: '2026-09-10 19:30:00',
      tag: 'manual',
    });

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    fireEvent.click(await screen.findByRole('button', { name: /立即备份/ }));

    const statusBar = await screen.findByRole('status');
    expect(statusBar.textContent).toContain('fundlens-new.db');

    const manualSection = screen.getByRole('heading', { name: '手动同步' }).closest('section');
    expect(manualSection?.textContent).not.toContain('fundlens-new.db');
  });

  it('备份列表默认展示前 8 条，可展开查看全部', async () => {
    const many = Array.from({ length: 9 }, (_, i) => ({
      file: `bk-${i}.db`,
      size: 100,
      at: `2026-09-10 1${i}:00:00`,
      tag: 'auto',
    }));
    mockedBackups.mockResolvedValue(many);

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    // 默认仅前 8 条可见，第 9 条不在
    expect(screen.queryByText('bk-8.db')).not.toBeInTheDocument();
    fireEvent.click(await screen.findByRole('button', { name: /展开全部 9 条/ }));
    expect(await screen.findByText('bk-8.db')).toBeInTheDocument();
    fireEvent.click(await screen.findByRole('button', { name: /收起/ }));
    await waitFor(() => expect(screen.queryByText('bk-8.db')).not.toBeInTheDocument());
  });

  it('保留份数旁显示当前生效值', async () => {
    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(await screen.findByText('当前生效 7 份')).toBeInTheDocument();
  });
});
