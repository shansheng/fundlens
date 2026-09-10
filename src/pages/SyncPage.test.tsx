import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import SyncPage from './SyncPage';
import { ThemeProvider } from '../theme';
import * as api from '../api';

vi.mock('@tauri-apps/plugin-dialog', () => ({
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
    syncExportSnapshot: vi.fn(),
    syncImportSnapshot: vi.fn(),
    syncListBackups: vi.fn(),
    syncCreateBackup: vi.fn(),
    syncSetBackupKeep: vi.fn(),
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

  it('存在冲突时列出数据表与来源设备', async () => {
    mockedStatus.mockResolvedValue(statusFixture({ lastExportAt: null, lastImportAt: '2026-09-10 21:00:00', conflictCount: 1 }));
    mockedConflicts.mockResolvedValue([
      {
        id: 1,
        tbl: 'positions',
        rowKey: '["7"]',
        device: 'dev-other-9',
        resolved: 0,
        createdAt: '2026-09-10 21:00:00',
      },
    ]);

    render(
      <ThemeProvider>
        <SyncPage />
      </ThemeProvider>,
    );

    expect(await screen.findByText('positions')).toBeInTheDocument();
    expect(screen.getByText('dev-other-9')).toBeInTheDocument();
    expect(screen.queryByText(/暂无冲突/)).not.toBeInTheDocument();
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
});
