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
  };
});

const mockedStatus = vi.mocked(api.syncStatus);
const mockedConflicts = vi.mocked(api.syncListConflicts);
const mockedBackups = vi.mocked(api.syncListBackups);

describe('SyncPage', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedStatus.mockResolvedValue({
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
    });
    mockedConflicts.mockResolvedValue([]);
    mockedBackups.mockResolvedValue([
      { file: 'fundlens-20260910-190000-auto.db', size: 2048, at: '2026-09-10 19:00:00', tag: 'auto' },
    ]);
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
    mockedStatus.mockResolvedValue({
      deviceId: 'dev-abc-123',
      tablesSynced: 13,
      pendingChanges: 0,
      totalChanges: 120,
      lastExportAt: null,
      lastImportAt: '2026-09-10 21:00:00',
      conflictCount: 1,
      backupKeep: 7,
      backupCount: 0,
      lastBackupAt: null,
      backupDir: '/tmp/data/backups',
    });
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
});
