// 关于页 — 品牌承诺与合规说明（P0-3：与养基宝「黑箱+导流」形成差异化）
import { useState } from 'react';
import { ShieldCheck, Ban, Database, BellOff, Calculator, ArrowLeft, Download, Upload } from 'lucide-react';
import { Link } from 'react-router-dom';
import { save, open } from '@tauri-apps/plugin-dialog';
import { exportDb, importDb, isTauri } from '../api';

const PROMISES = [
  {
    icon: ShieldCheck,
    title: '不碰交易',
    desc: 'FundLens 仅做持仓分析与估值复盘，不提供买卖入口、不开户导流、不收交易佣金。你的投资决策始终在你自己的账户里完成。',
  },
  {
    icon: Ban,
    title: '不做导流',
    desc: '没有券商开户链接、没有红包拉群、没有达人带货。我们不靠把用户导去别处赚钱，只靠把工具本身做好。',
  },
  {
    icon: Database,
    title: '数据本地优先',
    desc: '所有持仓、披露与估值计算都在你本机完成，数据默认不上云。你随时可以导出或清空，隐私始终在你手中。',
  },
  {
    icon: BellOff,
    title: '无广告',
    desc: '界面里没有开屏广告、没有弹窗营销、没有信息流。打开就是你的资产，干净纯粹。',
  },
];

export default function AboutPage() {
  const [backupMsg, setBackupMsg] = useState<string>('');

  function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
  }

  async function handleExport() {
    if (!isTauri) {
      setBackupMsg('浏览器预览模式不支持真实导出，请使用桌面端。');
      return;
    }
    const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-');
    const target = await save({
      defaultPath: `fundlens-backup-${stamp}.db`,
      filters: [{ name: 'SQLite 数据库', extensions: ['db'] }],
    });
    if (!target) return; // 用户取消
    try {
      const info = await exportDb(target as string);
      setBackupMsg(`已导出备份：${info.path}（${formatSize(info.size)}）`);
    } catch (e) {
      setBackupMsg(`导出失败：${(e as Error).message ?? String(e)}`);
    }
  }

  async function handleImport() {
    if (!isTauri) {
      setBackupMsg('浏览器预览模式不支持真实恢复，请使用桌面端。');
      return;
    }
    const selected = await open({
      multiple: false,
      filters: [{ name: 'SQLite 数据库', extensions: ['db'] }],
    });
    if (!selected || typeof selected !== 'string') return;
    if (!window.confirm('从备份恢复会覆盖当前全部本地数据，且不可撤销。确定继续？')) return;
    try {
      const info = await importDb(selected);
      setBackupMsg(`已从备份恢复：${info.path}（${formatSize(info.size)}）。建议重启应用以刷新内存缓存。`);
    } catch (e) {
      setBackupMsg(`恢复失败：${(e as Error).message ?? String(e)}`);
    }
  }

  return (
    <div className="p-6 space-y-5 max-w-3xl">
      <Link to="/overview" className="inline-flex items-center gap-1 text-sm text-muted hover:text-primary">
        <ArrowLeft size={16} aria-hidden /> 返回总览
      </Link>

      <header>
        <div className="flex items-center gap-2">
          <ShieldCheck size={22} className="text-primary" aria-hidden />
          <h1 className="text-xl font-semibold">关于 FundLens</h1>
        </div>
        <p className="mt-2 text-sm text-muted leading-relaxed">
          FundLens 是一个<strong className="text-foreground">本地优先的个人基金持仓穿透分析工具</strong>。
          它把你在多个平台持有的基金，按「披露持仓 + 公开个股行情」在本地透明地计算估值与收益，
          帮你一眼看清「我的组合到底长什么样、赚在哪、亏在哪」——而不是又一个催你交易、给你导流的应用。
        </p>
      </header>

      <section>
        <h2 className="text-sm font-semibold text-muted mb-3">我们的四个承诺</h2>
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
          {PROMISES.map(({ icon: Icon, title, desc }) => (
            <div key={title} className="bg-surface border border-border rounded-md p-4 shadow-ring">
              <div className="flex items-center gap-2 mb-1.5">
                <Icon size={18} className="text-primary" aria-hidden />
                <span className="font-medium">{title}</span>
              </div>
              <p className="text-xs text-muted leading-relaxed">{desc}</p>
            </div>
          ))}
        </div>
      </section>

      <section className="bg-surface border border-border rounded-md p-4 shadow-ring">
        <div className="flex items-center gap-2 mb-2">
          <Calculator size={18} className="text-primary" aria-hidden />
          <h2 className="text-base font-semibold">透明计算，而非黑箱</h2>
        </div>
        <p className="text-xs text-muted leading-relaxed">
          市面上的「实时估值」多是云端黑箱拟合，你不知道它怎么算的。FundLens 把口径摊开给你看：
        </p>
        <div className="mt-3 rounded-md bg-background/60 border border-border px-3 py-2 text-sm tnum">
          估算净值 = 官方净值 × (1 + Σ 披露持仓占比ᵢ × 个股当日涨跌ᵢ)
        </div>
        <ul className="mt-3 text-xs text-muted leading-relaxed list-disc pl-5 space-y-1">
          <li><strong className="text-foreground">本地计算</strong>：公式与数据都在你本机，不依赖任何第三方「估值服务」。</li>
          <li><strong className="text-foreground">覆盖可见</strong>：每只基金会标明「估算覆盖度」（前几大重仓占净值比例），未覆盖部分按零波动近似，绝不假装精确。</li>
          <li><strong className="text-foreground">时段清晰</strong>：盘中为<strong className="text-foreground">估算</strong>，盘后即为<strong className="text-foreground">当日实际</strong>，休市显示上一交易日实际——绝不混淆。</li>
        </ul>
      </section>

      <section className="bg-surface border border-border rounded-md p-4 shadow-ring">
        <div className="flex items-center gap-2 mb-2">
          <Database size={18} className="text-primary" aria-hidden />
          <h2 className="text-base font-semibold">数据备份与恢复</h2>
        </div>
        <p className="text-xs text-muted leading-relaxed mb-3">
          你的全部持仓、交易与估值数据都保存在本机这一个 SQLite 文件里（文件权限已收紧为仅本人可读写）。
          建议定期导出备份；换设备或重装前，可用备份完整恢复。
        </p>
        <div className="flex flex-wrap gap-2">
          <button
            onClick={() => void handleExport()}
            className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover"
          >
            <Download size={15} aria-hidden /> 导出数据库备份
          </button>
          <button
            onClick={() => void handleImport()}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-border/60"
          >
            <Upload size={15} aria-hidden /> 从备份恢复
          </button>
        </div>
        {backupMsg && (
          <p className="mt-3 text-xs text-muted bg-background/60 border border-border rounded-md px-3 py-2 leading-relaxed">
            {backupMsg}
          </p>
        )}
      </section>

      <section className="rounded-md border border-warning/40 bg-warning/10 px-4 py-3 text-xs text-warning leading-relaxed">
        <strong>免责声明</strong>：FundLens 提供的估值为基于公开数据的本地计算参考，<strong>不构成任何投资建议</strong>。
        基金有风险，投资需谨慎；过往业绩不代表未来表现。本工具不提供买卖推荐，也不对估算误差承担责任。
      </section>
    </div>
  );
}
