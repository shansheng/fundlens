// 记账页 — 本人持仓流水（事务账本为单一真相，单机单账户）
import { useCallback, useEffect, useState } from 'react';
import { Plus, Trash2, ArrowDownToLine, ArrowUpFromLine, TrendingUp, TrendingDown, Coins, FileUp, ScanLine, Upload, FileImage, TriangleAlert, ClipboardEdit } from 'lucide-react';
import { open } from '@tauri-apps/api/dialog';
import {
  listTransactions,
  addTransaction,
  deleteTransaction,
  importTransactions,
  importTxnScreenshots,
  getFundDetail,
  readImageDataUrl,
  isTauri,
  type TransactionOut,
  type TxnType,
  type ImportTxn,
  type ImportTxnPreview,
} from '../api';
import { PLATFORMS } from '../lib/mockData';
import { Card, EmptyState, PlatformBadge } from '../components/ui';

const TXN_META: Record<TxnType, { label: string; icon: typeof TrendingUp; inflow: boolean }> = {
  buy: { label: '买入', icon: TrendingUp, inflow: false },
  sell: { label: '卖出', icon: TrendingDown, inflow: true },
  dividend: { label: '分红', icon: Coins, inflow: true },
  reinvest_dividend: { label: '红利再投', icon: Coins, inflow: false },
  deposit: { label: '入金', icon: ArrowDownToLine, inflow: true },
  withdraw: { label: '出金', icon: ArrowUpFromLine, inflow: false },
  adjust: { label: '手工调整', icon: ClipboardEdit, inflow: false },
};

/// 解析交易记录 CSV/TSV（支持表头或固定列序）。返回规范后的导入项与逐行错误。
function parseTxnCsv(text: string): { items: ImportTxn[]; errors: string[] } {
  const errors: string[] = [];
  const items: ImportTxn[] = [];
  const rawLines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  if (rawLines.length === 0) return { items, errors };

  const HEADER_KEYS: Record<string, keyof ColMap> = {
    日期: 'date', date: 'date', 时间: 'date', 交易日期: 'date',
    基金代码: 'code', code: 'code', 代码: 'code', 基金: 'code',
    类型: 'type', type: 'type', 操作: 'type', 买卖: 'type', 方向: 'type',
    份额: 'shares', shares: 'shares', 份数: 'shares', 份: 'shares',
    金额: 'amount', amount: 'amount', 成交金额: 'amount', 总额: 'amount', 总价: 'amount',
    价格: 'price', price: 'price', 单价: 'price', 净值: 'price', 价: 'price',
  };
  type ColMap = { date: number; code: number; type: number; shares: number; amount: number; price: number };

  let colMap: ColMap | null = null;
  let dataLines = rawLines;
  // 表头探测：首行含任一表头关键字则视为表头
  const firstHasHeader = rawLines[0].split(/[,;\t]/).some((c) => HEADER_KEYS[c.trim().toLowerCase()]);
  if (firstHasHeader) {
    const header = rawLines[0].split(/[,;\t]/).map((c) => c.trim().toLowerCase());
    const map: Partial<ColMap> = {};
    header.forEach((h, i) => {
      const key = HEADER_KEYS[h];
      if (key) (map as Record<string, number>)[key] = i;
    });
    colMap = {
      date: map.date ?? 0,
      code: map.code ?? 1,
      type: map.type ?? 2,
      shares: map.shares ?? 3,
      amount: map.amount ?? 4,
      price: map.price ?? 5,
    };
    dataLines = rawLines.slice(1);
  } else {
    colMap = { date: 0, code: 1, type: 2, shares: 3, amount: 4, price: 5 };
  }

  const normDate = (s: string): string => {
    const t = s.trim().replace(/\//g, '-');
    if (/^\d{8}$/.test(t)) return `${t.slice(0, 4)}-${t.slice(4, 6)}-${t.slice(6, 8)}`;
    return t;
  };
  const normType = (s: string): TxnType | null => {
    const t = s.trim().toLowerCase();
    if (['buy', '买入', '申购', '买进'].includes(t)) return 'buy';
    if (['sell', '卖出', '赎回'].includes(t)) return 'sell';
    if (['dividend', '分红', '现金分红'].includes(t)) return 'dividend';
    if (['reinvest_dividend', '红利再投', '分红再投', '再投'].includes(t)) return 'reinvest_dividend';
    return null;
  };

  dataLines.forEach((line, idx) => {
    const ln = (firstHasHeader ? idx + 2 : idx + 1);
    const parts = line.split(/[,;\t]/).map((p) => p.trim());
    const date = normDate(parts[colMap!.date] ?? '');
    const code = (parts[colMap!.code] ?? '').trim();
    const type = normType(parts[colMap!.type] ?? '');
    const sharesRaw = (parts[colMap!.shares] ?? '').trim();
    const amountRaw = (parts[colMap!.amount] ?? '').trim();
    const priceRaw = (parts[colMap!.price] ?? '').trim();

    if (!type) {
      errors.push(`第 ${ln} 行：交易类型无法识别（支持 买入/卖出/分红/红利再投）`);
      return;
    }
    if (!code) {
      errors.push(`第 ${ln} 行：缺少基金代码`);
      return;
    }
    const amount = Number(amountRaw);
    if (!amountRaw || Number.isNaN(amount) || amount <= 0) {
      errors.push(`第 ${ln} 行：金额无效`);
      return;
    }
    const needsShares = type === 'buy' || type === 'sell' || type === 'reinvest_dividend';
    let shares: number | null = null;
    if (needsShares) {
      shares = Number(sharesRaw);
      if (!sharesRaw || Number.isNaN(shares) || shares <= 0) {
        const label = type === 'buy' ? '买入' : type === 'sell' ? '卖出' : '红利再投';
        errors.push(`第 ${ln} 行：${label}需要有效份额`);
        return;
      }
    }
    const price = priceRaw ? Number(priceRaw) : null;
    items.push({
      fundCode: code,
      txnType: type,
      shares,
      amount,
      price: price != null && !Number.isNaN(price) ? price : null,
      txnDate: date || new Date().toISOString().slice(0, 10),
    });
  });

  return { items, errors };
}

function todayStr(): string {
  return new Date().toISOString().slice(0, 10);
}

function TxnBadge({ type }: { type: TxnType }) {
  const m = TXN_META[type];
  const Icon = m.icon;
  const cls = m.inflow ? 'text-success bg-success/10' : 'text-danger bg-danger/10';
  return (
    <span className={`inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs font-medium ${cls}`}>
      <Icon size={13} aria-hidden />
      {m.label}
    </span>
  );
}

/// 截图 OCR 识别后可手改的交易记录行（字段用字符串以便编辑）
interface EditableTxn {
  txnType: TxnType | '';
  date: string;
  time: string;       // 交易时间 HH:MM（可选）
  code: string;
  name: string;
  shares: string;
  amount: string;
  price: string;
  confidence: number;
}

/// 根据交易时间给出净值结算口径提示：
/// 交易日下午 15:00 前按当日净值、15:00 后按下一交易日净值结算。
function navSettlementHint(time: string): string {
  if (!time) return '';
  const [h, m] = time.split(':').map((x) => Number(x));
  if (Number.isNaN(h)) return '';
  const mins = h * 60 + (Number.isNaN(m) ? 0 : m);
  return mins >= 15 * 60 ? '15:00后·下一交易日净值' : '15:00前·当日净值';
}

export default function LedgerPage() {
  const [txns, setTxns] = useState<TransactionOut[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  // 记账表单
  const [txnType, setTxnType] = useState<TxnType>('buy');
  const [fundCode, setFundCode] = useState('');
  const [shares, setShares] = useState('');
  const [price, setPrice] = useState('');
  const [amount, setAmount] = useState('');
  const [txnDate, setTxnDate] = useState(todayStr());
  const [txnTime, setTxnTime] = useState('');
  const [note, setNote] = useState('');
  // 手动记账所属平台：决定流水按哪个平台累计（避免落到空「无平台」幻影持仓）
  const [manualPlatform, setManualPlatform] = useState<string>('alipay');
  const [formErr, setFormErr] = useState<string | null>(null);

  // 交易记录 CSV 导入（增量合并）
  const [csvText, setCsvText] = useState('');
  // 默认批次标签带时间戳（秒级唯一）：同一天多次导入不会互相覆盖。
  // 后端对「同 source_ref」批次是先清后写（幂等重导）；若沿用纯日期标签，
  // 第二次导入会把第一次的记录整批删除（2026-08-21 事故：8 条导入被后续导入覆盖）。
  // 手动输入与已有批次相同的标签时，仍走「重导修正」语义（先清后写）。
  const [batchLabel, setBatchLabel] = useState(() => {
    const now = new Date();
    const pad = (n: number) => String(n).padStart(2, '0');
    return `导入-${todayStr()} ${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`;
  });
  const [parseResult, setParseResult] = useState<{ items: ImportTxn[]; errors: string[] } | null>(null);
  const [importErr, setImportErr] = useState<string | null>(null);
  const [importBusy, setImportBusy] = useState(false);

  // 交易记录截图导入（识别 + 可编辑预览 + 落地为真实流水）
  const [txnPlatform, setTxnPlatform] = useState<string>('alipay');
  const [txnFiles, setTxnFiles] = useState<string[]>([]);
  const [txnPreviews, setTxnPreviews] = useState<string[]>([]);
  const [txnPreview, setTxnPreview] = useState<ImportTxnPreview | null>(null);
  const [txnRows, setTxnRows] = useState<EditableTxn[]>([]);
  const [txnScanBusy, setTxnScanBusy] = useState(false);
  const [txnImportBusy, setTxnImportBusy] = useState(false);
  const [txnImportErr, setTxnImportErr] = useState<string | null>(null);
  const [txnShowRaw, setTxnShowRaw] = useState(false);

  const loadTxns = useCallback(async () => {
    setLoading(true);
    try {
      const list = await listTransactions();
      list.sort((a, b) => (a.txnDate < b.txnDate ? 1 : a.txnDate > b.txnDate ? -1 : b.id - a.id));
      setTxns(list);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadTxns();
  }, [loadTxns]);

  // 输入基金代码后（失焦）回退到该基金已有持仓的平台，确保手动记账落在正确平台累加。
  // 若该基金无持仓或查询失败，则保留用户当前选择，不影响保存。
  const handleFundCodeBlur = useCallback(async () => {
    const code = fundCode.trim();
    if (!code) return;
    try {
      const detail = await getFundDetail(code);
      const p = detail.fund.platform;
      if (p && p !== '' && PLATFORMS[p]) setManualPlatform(p);
    } catch {
      /* 忽略：保留当前平台选择 */
    }
  }, [fundCode]);

  const handleAdd = useCallback(async () => {
    setFormErr(null);
    const isFundTyped = txnType === 'buy' || txnType === 'sell' || txnType === 'dividend' || txnType === 'reinvest_dividend';
    const needsShares = txnType === 'buy' || txnType === 'sell' || txnType === 'reinvest_dividend';
    let amt = amount ? Number(amount) : NaN;
    if (needsShares && (Number.isNaN(amt) || amt <= 0)) {
      const sh = Number(shares);
      const pr = Number(price);
      if (!Number.isNaN(sh) && !Number.isNaN(pr) && sh > 0 && pr > 0) {
        amt = Math.round(sh * pr * 100) / 100;
      }
    }
    if (isFundTyped && !fundCode.trim()) return setFormErr('买入/卖出/分红需要填写基金代码');
    if (Number.isNaN(amt) || amt <= 0) return setFormErr('金额无效（买入/卖出将按 份额×价格 自动计算，也可手动填写）');
    if (needsShares && (Number.isNaN(Number(shares)) || Number(shares) <= 0)) return setFormErr('买入/卖出需要填写有效份额');

    setBusy(true);
    try {
      await addTransaction(
        txnType,
        isFundTyped ? fundCode.trim() : null,
        needsShares ? Number(shares) : null,
        amt,
        needsShares && price ? Number(price) : null,
        txnDate,
        txnTime.trim() || undefined,
        note.trim() || undefined,
        manualPlatform,
      );
      setFundCode('');
      setShares('');
      setPrice('');
      setAmount('');
      setNote('');
      await loadTxns();
    } catch (e) {
      setFormErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [txnType, fundCode, shares, price, amount, txnDate, note, manualPlatform, loadTxns]);

  const handleDelete = useCallback(
    async (id: number) => {
      if (!confirm('确定删除这条流水吗？删除后持仓成本将按剩余流水重算。')) return;
      try {
        await deleteTransaction(id);
        await loadTxns();
      } catch (e) {
        alert(`删除失败：${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [loadTxns],
  );

  const handleParseCsv = useCallback(() => {
    setImportErr(null);
    const r = parseTxnCsv(csvText);
    setParseResult(r);
  }, [csvText]);

  const handleImportCsv = useCallback(async () => {
    if (!parseResult || parseResult.items.length === 0) {
      setImportErr('没有可导入的有效记录');
      return;
    }
    const label = batchLabel.trim() || null;
    setImportBusy(true);
    setImportErr(null);
    try {
      const n = await importTransactions(parseResult.items, label);
      setCsvText('');
      setParseResult(null);
      await loadTxns();
      alert(`成功导入 ${n} 条交易记录（批次：${label ?? '无'}，同批次重复导入将幂等替换）`);
    } catch (e) {
      setImportErr(e instanceof Error ? e.message : String(e));
    } finally {
      setImportBusy(false);
    }
  }, [parseResult, batchLabel, loadTxns]);

  // ---- 交易记录截图识别 + 可编辑预览 ----
  const pickTxnFiles = useCallback(async () => {
    const selected = await open({
      multiple: true,
      filters: [{ name: '图片', extensions: ['png', 'jpg', 'jpeg', 'bmp', 'webp'] }],
    });
    if (Array.isArray(selected) && selected.length > 0) {
      const paths = selected as string[];
      setTxnFiles(paths);
      setTxnPreview(null);
      setTxnRows([]);
      setTxnShowRaw(false);
      const urls = await Promise.all(
        paths.map(async (p) => {
          try {
            return await readImageDataUrl(p);
          } catch {
            return '';
          }
        }),
      );
      setTxnPreviews(urls);
    }
  }, []);

  const scanTxn = useCallback(async () => {
    if (txnFiles.length === 0) return;
    setTxnScanBusy(true);
    setTxnImportErr(null);
    try {
      const r = await importTxnScreenshots(txnPlatform, txnFiles);
      setTxnPreview(r);
      setTxnRows(
        r.txns.map((t) => ({
          txnType: (['buy', 'sell', 'dividend', 'reinvest_dividend'].includes(t.txnType) ? t.txnType : 'buy') as TxnType,
          date: t.date,
          time: t.time ?? '',
          code: t.code,
          name: t.name,
          shares: t.shares ? String(t.shares) : '',
          amount: t.amount ? String(t.amount) : '',
          price: t.price ? String(t.price) : '',
          confidence: t.confidence,
        })),
      );
    } catch (e) {
      setTxnPreview(null);
      setTxnRows([]);
      setTxnImportErr(`识别失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setTxnScanBusy(false);
    }
  }, [txnFiles, txnPlatform]);

  const updateTxnRow = useCallback((idx: number, patch: Partial<EditableTxn>) => {
    setTxnRows((rows) => rows.map((r, i) => (i === idx ? { ...r, ...patch } : r)));
  }, []);

  // 从预览中移除某行（如低置信度、不想导入的记录）—— 仅前端剔除，不落库
  const removeTxnRow = useCallback((idx: number) => {
    setTxnRows((rows) => rows.filter((_, i) => i !== idx));
  }, []);

  const importTxnRows = useCallback(async () => {
    if (txnRows.length === 0) return;
    const items: ImportTxn[] = [];
    const missingShares: { row: number; code: string }[] = [];
    for (let i = 0; i < txnRows.length; i++) {
      const r = txnRows[i];
      const amount = Number(r.amount);
      if (!r.txnType) {
        setTxnImportErr(`第 ${i + 1} 行：缺少交易类型`);
        return;
      }
      if (!amount || Number.isNaN(amount) || amount <= 0) {
        setTxnImportErr(`第 ${i + 1} 行：金额无效`);
        return;
      }
      const sharesRaw = r.shares ? Number(r.shares) : null;
      // 买入/卖出无份额：不再硬拦截——后端会按「交易日(15:00 分界)确认净值」从本地净值
      // 自动反推份额；本地无该日净值时暂存待补，待净值到位后自动回填（见下方汇总提示）。
      if ((r.txnType === 'buy' || r.txnType === 'sell') && (sharesRaw == null || Number.isNaN(sharesRaw) || sharesRaw <= 0)) {
        missingShares.push({ row: i + 1, code: r.code });
      }
      items.push({
        fundCode: r.code.trim(),
        fundName: r.name.trim() || null,
        txnType: r.txnType as TxnType,
        shares: sharesRaw != null && !Number.isNaN(sharesRaw) ? sharesRaw : null,
        amount,
        price: r.price ? Number(r.price) : null,
        txnDate: r.date || todayStr(),
        txnTime: r.time.trim() || undefined,
      });
    }
    // 无份额行汇总确认：让用户知情自动反推/待回填机制（15:00 前按当日净值、15:00 后按下一交易日净值）。
    if (missingShares.length > 0) {
      const list = missingShares.map((m) => `第 ${m.row} 行（${m.code}）`).join('、');
      const ok = window.confirm(
        `以下 ${missingShares.length} 笔买入/卖出未识别份额：\n${list}\n\n` +
          '将按交易日期（15:00 前=当日净值 / 15:00 后=下一交易日净值）自动反推份额；\n' +
          '若本地暂无对应日净值，份额将暂存待补，获取净值后自动更新。\n\n继续导入？',
      );
      if (!ok) return;
    }
    const label = batchLabel.trim() || null;
    setTxnImportBusy(true);
    setTxnImportErr(null);
    try {
      const n = await importTransactions(items, label, txnPreview?.platform || null);
      setTxnPreview(null);
      setTxnRows([]);
      setTxnFiles([]);
      setTxnPreviews([]);
      await loadTxns();
      alert(`成功导入 ${n} 条交易记录（批次：${label ?? '无'}）`);
    } catch (e) {
      setTxnImportErr(e instanceof Error ? e.message : String(e));
    } finally {
      setTxnImportBusy(false);
    }
  }, [txnRows, batchLabel, loadTxns]);

  const isFundTyped = txnType === 'buy' || txnType === 'sell' || txnType === 'dividend';
  const needsShares = txnType === 'buy' || txnType === 'sell';

  return (
    <div className="p-6 space-y-5">
      <header>
        <h1 className="text-xl font-semibold">记账</h1>
        <p className="text-xs text-muted mt-0.5">
          事务账本为单一真相：买卖/出入金流水重建真实持仓成本与精确盈亏（本人全部平台合并统计）
        </p>
      </header>

      {/* 记一笔 */}
      <Card title="记一笔">
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          <label className="text-sm">
            <span className="block text-xs text-muted mb-1">类型</span>
            <select
              value={txnType}
              onChange={(e) => setTxnType(e.target.value as TxnType)}
              className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm"
            >
              <option value="buy">买入</option>
              <option value="sell">卖出</option>
              <option value="dividend">分红</option>
              <option value="deposit">入金</option>
              <option value="withdraw">出金</option>
            </select>
          </label>

          <label className="text-sm">
            <span className="block text-xs text-muted mb-1">平台</span>
            <select
              value={manualPlatform}
              onChange={(e) => setManualPlatform(e.target.value)}
              className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm"
            >
              {Object.values(PLATFORMS).map((p) => (
                <option key={p.code} value={p.code}>
                  {p.name}
                </option>
              ))}
            </select>
          </label>

          {isFundTyped && (
            <label className="text-sm">
              <span className="block text-xs text-muted mb-1">基金代码</span>
              <input
                value={fundCode}
                onChange={(e) => setFundCode(e.target.value)}
                onBlur={() => void handleFundCodeBlur()}
                placeholder="如 110011"
                className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm"
              />
            </label>
          )}

          {needsShares && (
            <label className="text-sm">
              <span className="block text-xs text-muted mb-1">份额</span>
              <input
                value={shares}
                onChange={(e) => setShares(e.target.value)}
                type="number"
                min="0"
                step="0.01"
                placeholder="份"
                className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm tnum"
              />
            </label>
          )}

          {needsShares && (
            <label className="text-sm">
              <span className="block text-xs text-muted mb-1">价格（可选）</span>
              <input
                value={price}
                onChange={(e) => setPrice(e.target.value)}
                type="number"
                min="0"
                step="0.0001"
                placeholder="元/份"
                className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm tnum"
              />
            </label>
          )}

          <label className="text-sm">
            <span className="block text-xs text-muted mb-1">金额（元）</span>
            <input
              value={amount}
              onChange={(e) => setAmount(e.target.value)}
              type="number"
              min="0"
              step="0.01"
              placeholder={needsShares ? '留空自动算' : '必填'}
              className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm tnum"
            />
          </label>

          <label className="text-sm">
            <span className="block text-xs text-muted mb-1">日期</span>
            <input
              value={txnDate}
              onChange={(e) => setTxnDate(e.target.value)}
              type="date"
              className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm tnum"
            />
          </label>

          <label className="text-sm">
            <span className="block text-xs text-muted mb-1">时间（可选）</span>
            <input
              value={txnTime}
              onChange={(e) => setTxnTime(e.target.value)}
              type="time"
              className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm tnum"
            />
          </label>

          <label className="text-sm md:col-span-2">
            <span className="block text-xs text-muted mb-1">备注（可选）</span>
            <input
              value={note}
              onChange={(e) => setNote(e.target.value)}
              placeholder="如 建仓 / 减仓 / 工资转入"
              className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm"
            />
          </label>
        </div>

        {formErr && <p className="mt-2 text-xs text-danger">{formErr}</p>}

        <button
          onClick={() => void handleAdd()}
          disabled={busy}
          className="mt-3 inline-flex items-center gap-1.5 rounded-md bg-primary px-4 py-2 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
        >
          <Plus size={16} aria-hidden />
          {busy ? '保存中…' : '保存流水'}
        </button>
      </Card>

      {/* 导入交易记录（增量合并） */}
      <Card title={<span className="flex items-center gap-1.5"><FileUp size={16} aria-hidden />导入交易记录（买/卖/分红）</span>}>
        <p className="text-xs text-muted mb-3">
          粘贴券商导出的 CSV/TSV（列支持：日期、基金代码、类型、份额、金额、价格；首行可为表头或用固定列序）。
          同一「批次标签」重复导入会幂等替换，不同批次与手动流水互不干扰，实现流水增量合并。
        </p>
        <textarea
          value={csvText}
          onChange={(e) => { setCsvText(e.target.value); setParseResult(null); }}
          placeholder={'日期,基金代码,类型,份额,金额,价格\n2026-01-05,110011,买入,1000,4196,4.196\n2026-04-12,110011,分红,,120,'}
          className="w-full h-28 rounded-md border border-border bg-background px-2 py-1.5 text-xs font-mono"
        />
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <label className="text-sm flex items-center gap-1.5">
            <span className="text-xs text-muted">批次标签</span>
            <input
              value={batchLabel}
              onChange={(e) => setBatchLabel(e.target.value)}
              className="rounded-md border border-border bg-background px-2 py-1.5 text-sm w-44"
            />
          </label>
          <button
            onClick={() => void handleParseCsv()}
            disabled={!csvText.trim()}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-border/60 disabled:opacity-50"
          >
            <FileUp size={15} aria-hidden />
            解析预览
          </button>
          <button
            onClick={() => void handleImportCsv()}
            disabled={importBusy || !parseResult || parseResult.items.length === 0}
            className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
          >
            <Plus size={15} aria-hidden />
            {importBusy ? '导入中…' : '导入'}
          </button>
        </div>

        {parseResult && (
          <div className="mt-3 space-y-2">
            {parseResult.errors.length > 0 && (
              <p className="text-xs text-danger">解析错误 {parseResult.errors.length} 条：{parseResult.errors.slice(0, 5).join('；')}{parseResult.errors.length > 5 ? '…' : ''}</p>
            )}
            <p className="text-xs text-success">可导入 {parseResult.items.length} 条</p>
            {parseResult.items.length > 0 && (
              <div className="overflow-x-auto max-h-40 overflow-y-auto">
                <table className="w-full text-xs">
                  <thead>
                    <tr className="text-left text-muted border-b border-border">
                      <th className="py-1 pr-2">日期</th>
                      <th className="py-1 pr-2">类型</th>
                      <th className="py-1 pr-2">代码</th>
                      <th className="py-1 pr-2 text-right">份额</th>
                      <th className="py-1 pr-2 text-right">金额</th>
                    </tr>
                  </thead>
                  <tbody>
                    {parseResult.items.slice(0, 20).map((it, i) => (
                      <tr key={i} className="border-b border-border/50">
                        <td className="py-1 pr-2 tnum">{it.txnDate}</td>
                        <td className="py-1 pr-2">{TXN_META[it.txnType].label}</td>
                        <td className="py-1 pr-2">{it.fundCode}</td>
                        <td className="py-1 pr-2 text-right tnum">{it.shares != null ? it.shares : '—'}</td>
                        <td className="py-1 pr-2 text-right tnum">¥{it.amount.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        )}
        {importErr && <p className="mt-2 text-xs text-danger">{importErr}</p>}

        {/* 交易记录截图识别（与上方 CSV 粘贴并列，截图为主、CSV 为辅） */}
        <div className="mt-5 border-t border-border pt-4">
          <div className="flex items-center gap-1.5 text-sm font-medium mb-1">
            <ScanLine size={16} aria-hidden />
            上传交易记录截图（支付宝 / 京东金融 / 腾讯理财通）
          </div>
          <p className="text-xs text-muted mb-3">
            本地 PaddleOCR 识别买/卖/分红流水，先预览（类型/日期/份额/金额/价格均可手改），确认后写入真实流水。数据不上传云端。
          </p>

          {/* 平台选择 */}
          <div className="grid grid-cols-3 gap-2 mb-3">
            {Object.values(PLATFORMS).map((p) => (
              <button
                key={p.code}
                type="button"
                onClick={() => setTxnPlatform(p.code)}
                className={`rounded-md border px-3 py-2 text-left text-xs transition-colors ${
                  txnPlatform === p.code ? 'border-primary bg-primary/5' : 'border-border hover:bg-background'
                }`}
              >
                <PlatformBadge code={p.code} />
              </button>
            ))}
          </div>

          {/* 文件选择 */}
          <button
            type="button"
            onClick={() => void pickTxnFiles()}
            className="flex w-full flex-col items-center justify-center gap-2 rounded-md border border-dashed border-border py-8 cursor-pointer hover:bg-background"
          >
            <Upload size={24} className="text-muted" aria-hidden />
            <span className="text-sm text-foreground">点击选择交易记录截图（可多选）</span>
            <span className="text-xs text-muted">支持 支付宝 / 京东金融 / 腾讯理财通 交易列表</span>
          </button>
          {txnFiles.length > 0 && (
            <ul className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
              {txnFiles.map((f, i) => (
                <li key={f} className="overflow-hidden rounded-md border border-border">
                  {txnPreviews[i] ? (
                    <img src={txnPreviews[i]} alt={f} className="h-20 w-full object-cover" />
                  ) : !isTauri ? (
                    <div className="flex h-20 items-center gap-2 px-2 text-sm text-muted">
                      <FileImage size={16} aria-hidden />
                      <span className="truncate">{f}</span>
                    </div>
                  ) : (
                    <div className="flex h-20 items-center justify-center text-xs text-muted">预览加载中…</div>
                  )}
                  <div className="truncate px-2 py-1 text-xs text-muted">{f.split('/').pop()}</div>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            onClick={() => void scanTxn()}
            disabled={txnScanBusy || txnFiles.length === 0}
            className="mt-3 inline-flex items-center gap-1.5 rounded-md bg-primary px-4 py-2 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
          >
            <ScanLine size={16} aria-hidden />
            {txnScanBusy ? '识别中…' : '识别截图'}
          </button>

          {/* OCR 未就绪提示 */}
          {txnPreview && !txnPreview.ocrReady && (
            <div className="mt-3 flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
              <TriangleAlert size={16} aria-hidden />
              OCR 未就绪：请运行 src-tauri/download_ocr_models.sh 下载模型，并以 --features ocr 构建。
            </div>
          )}

          {/* 可编辑预览 */}
          {txnRows.length > 0 ? (
            <div className="mt-3 space-y-2">
              <p className="text-xs text-success">识别到 {txnRows.length} 条交易记录（可手改后导入；低置信度行可点「删除」剔除）</p>
              <p className="text-[11px] text-muted">净值结算口径：交易时间 15:00 前按当日净值、15:00 后按下一交易日净值（预览「时间」列下方会标注）。</p>
              <div className="overflow-x-auto max-h-72 overflow-y-auto border border-border rounded-md">
                <table className="w-full text-xs">
                  <thead>
                    <tr className="text-left text-muted border-b border-border bg-background sticky top-0">
                      <th className="py-1.5 px-2 font-medium">类型</th>
                      <th className="py-1.5 px-2 font-medium">日期</th>
                      <th className="py-1.5 px-2 font-medium">时间</th>
                      <th className="py-1.5 px-2 font-medium">代码</th>
                      <th className="py-1.5 px-2 font-medium">名称</th>
                      <th className="py-1.5 px-2 font-medium text-right">份额</th>
                      <th className="py-1.5 px-2 font-medium text-right">金额</th>
                      <th className="py-1.5 px-2 font-medium text-right">价格</th>
                      <th className="py-1.5 px-2 font-medium text-right">置信度</th>
                      <th className="py-1.5 px-2 font-medium text-right">操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {txnRows.map((r, i) => (
                      <tr key={i} className="border-b border-border/50 align-top">
                        <td className="py-1 px-2">
                          <select
                            value={r.txnType}
                            onChange={(e) => updateTxnRow(i, { txnType: e.target.value as TxnType })}
                            className="rounded border border-border bg-background px-1 py-0.5 text-xs"
                          >
                            <option value="buy">买入</option>
                            <option value="sell">卖出</option>
                            <option value="dividend">分红</option>
                          </select>
                        </td>
                        <td className="py-1 px-2">
                          <input
                            value={r.date}
                            onChange={(e) => updateTxnRow(i, { date: e.target.value })}
                            className="w-24 rounded border border-border bg-background px-1 py-0.5 text-xs tnum"
                          />
                          {txnPreview?.txns[i] && !txnPreview.txns[i].hasYear && (
                            <div className="text-[10px] text-warning leading-tight">年份?</div>
                          )}
                        </td>
                        <td className="py-1 px-2">
                          <input
                            value={r.time}
                            onChange={(e) => updateTxnRow(i, { time: e.target.value })}
                            placeholder="HH:MM"
                            className="w-16 rounded border border-border bg-background px-1 py-0.5 text-xs tnum"
                          />
                          {r.time && (
                            <div
                              className={
                                'text-[10px] leading-tight ' +
                                (r.time >= '15:00' ? 'text-warning' : 'text-muted')
                              }
                            >
                              {navSettlementHint(r.time)}
                            </div>
                          )}
                        </td>
                        <td className="py-1 px-2">
                          <input
                            value={r.code}
                            onChange={(e) => updateTxnRow(i, { code: e.target.value })}
                            className="w-16 rounded border border-border bg-background px-1 py-0.5 text-xs tnum"
                          />
                        </td>
                        <td className="py-1 px-2">
                          <input
                            value={r.name}
                            onChange={(e) => updateTxnRow(i, { name: e.target.value })}
                            className="w-28 rounded border border-border bg-background px-1 py-0.5 text-xs"
                          />
                        </td>
                        <td className="py-1 px-2">
                          <input
                            value={r.shares}
                            onChange={(e) => updateTxnRow(i, { shares: e.target.value })}
                            className="w-16 rounded border border-border bg-background px-1 py-0.5 text-xs tnum text-right"
                          />
                        </td>
                        <td className="py-1 px-2">
                          <input
                            value={r.amount}
                            onChange={(e) => updateTxnRow(i, { amount: e.target.value })}
                            className="w-20 rounded border border-border bg-background px-1 py-0.5 text-xs tnum text-right"
                          />
                        </td>
                        <td className="py-1 px-2">
                          <input
                            value={r.price}
                            onChange={(e) => updateTxnRow(i, { price: e.target.value })}
                            className="w-16 rounded border border-border bg-background px-1 py-0.5 text-xs tnum text-right"
                          />
                        </td>
                        <td className="py-1 px-2 text-right tnum text-muted">{Math.round(r.confidence * 100)}%</td>
                        <td className="py-1 px-2 text-right">
                          <button
                            type="button"
                            onClick={() => removeTxnRow(i)}
                            title="从导入清单移除（不落库）"
                            className="inline-flex items-center justify-center rounded border border-border px-1.5 py-0.5 text-xs text-muted hover:bg-danger/10 hover:text-danger"
                          >
                            删除
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>

              {txnPreview?.note && <p className="text-xs text-muted">{txnPreview.note}</p>}

              <button
                type="button"
                onClick={() => void importTxnRows()}
                disabled={txnImportBusy}
                className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
              >
                <Plus size={15} aria-hidden />
                {txnImportBusy ? '导入中…' : '导入交易记录'}
              </button>
            </div>
          ) : (
            txnPreview &&
            txnPreview.ocrReady && (
              <p className="mt-3 text-xs text-muted">{txnPreview.note}</p>
            )
          )}

          {/* 未识别到记录时给出原始文本，便于核对/微调 */}
          {txnPreview && txnPreview.ocrReady && txnRows.length === 0 && txnPreview.rawLines.length > 0 && (
            <div className="mt-3">
              <button
                type="button"
                onClick={() => setTxnShowRaw((v) => !v)}
                className="flex items-center gap-1 text-xs text-muted hover:text-foreground"
              >
                {txnShowRaw ? '▾' : '▸'} OCR 原始文本（{txnPreview.rawLines.length} 行，用于核对/微调）
              </button>
              {txnShowRaw && (
                <pre className="mt-2 max-h-60 overflow-auto rounded-md border border-border bg-background p-3 text-xs leading-relaxed text-muted">
                  {txnPreview.rawLines.join('\n')}
                </pre>
              )}
            </div>
          )}

          {txnImportErr && <p className="mt-2 text-xs text-danger">{txnImportErr}</p>}
        </div>
      </Card>

      {/* 流水列表 */}
      <Card title={`流水记录（${txns.length}）`}>
        {loading ? (
          <EmptyState title="加载中…" />
        ) : txns.length === 0 ? (
          <EmptyState title="暂无流水" hint="用上方表单记录第一笔买卖或出入金" />
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs text-muted border-b border-border">
                  <th className="py-2 pr-3 font-medium">日期</th>
                  <th className="py-2 pr-3 font-medium">时间</th>
                  <th className="py-2 pr-3 font-medium">类型</th>
                  <th className="py-2 pr-3 font-medium">基金</th>
                  <th className="py-2 pr-3 font-medium text-right">份额</th>
                  <th className="py-2 pr-3 font-medium text-right">金额</th>
                  <th className="py-2 pr-3 font-medium">批次</th>
                  <th className="py-2 pr-3 font-medium">备注</th>
                  <th className="py-2 pr-3 font-medium text-right">操作</th>
                </tr>
              </thead>
              <tbody>
                {txns.map((t) => {
                  return (
                    <tr key={t.id} className="border-b border-border/60 last:border-0">
                      <td className="py-2 pr-3 tnum">{t.txnDate}</td>
                      <td className="py-2 pr-3 tnum text-muted">{t.txnTime || '—'}</td>
                      <td className="py-2 pr-3"><TxnBadge type={t.txnType} /></td>
                      <td className="py-2 pr-3">
                        {t.fundCode ? (
                          <span>
                            <span className="font-medium">{t.fundName ?? t.fundCode}</span>
                            <span className="ml-1 text-xs text-muted tnum">{t.fundCode}</span>
                          </span>
                        ) : (
                          <span className="text-muted">—</span>
                        )}
                      </td>
                      <td className="py-2 pr-3 text-right tnum">{t.shares != null ? t.shares.toLocaleString('zh-CN', { maximumFractionDigits: 2 }) : '—'}</td>
                      <td className="py-2 pr-3 text-right tnum">¥{t.amount.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}</td>
                      <td className="py-2 pr-3 text-muted max-w-[8rem] truncate">
                        {t.sourceRef ?? (t.source === 'manual_txn' ? '手动' : t.source)}
                      </td>
                      <td className="py-2 pr-3 text-muted max-w-[12rem] truncate">{t.note ?? ''}</td>
                      <td className="py-2 pr-3 text-right">
                        <button
                          onClick={() => void handleDelete(t.id)}
                          className="inline-flex items-center justify-center rounded p-1.5 text-muted hover:bg-border/60 hover:text-danger"
                          title="删除"
                        >
                          <Trash2 size={15} aria-hidden />
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  );
}
