// 「估算收益」展示口径（与后端 valuation.rs 行情新鲜度三态对齐，v2.6.19+）：
// - 盘中：当日实时估算 dayPnlEst（描述今日盘中，日期由「当日」列「当日估算」标签承担）；
// - 其余时段（盘后/休市/盘前）：后端 SameDay 口径下当日估算不再归零（行情即最近收盘日，
//   用户口径「今日净值不影响今日估值」「没开盘前显示的都是今日收盘时候的估值」），
//   优先展示实时算出的「最近收盘估值」；实时值为 0（行情全陈旧/缺行情/QDII 海外交易中
//   抑制）时，回填上一交易日估算 lastDayPnlEst（position_daily 末条，null → 显示 —）。
//
// 日期标签口径（2026-10-30 用户报障修复）：
//   估算的**数据日期** = 后端 `estDate`（本次估算实际描述的交易日），**不是**官方净值日
//   `navDate`。盘后（15:00 收盘后）实时估算描述的是**当日收盘**（行情日），而官方净值
//   通常当晚/次日才发布（navDate 落后一天）——早前用 navDate 当标签，导致 15:00 后
//   10-30 的估值被打上「10-29」标签。
//   - 实时行：date = estDate（空则回退 navDate/lastNavDate）；
//   - 回填行：date = lastNavDate（种子日，即该条 position_daily 记录的净值日）。
//
// 总览表（PositionTable）、总览头部（OverviewPage）、基金详情页（FundDetailPage）三处
// 共用本函数，杜绝「行级显实时、头部加种子」的口径漂移（2026-10-24 用户报障根因）。
export type MarketSession = 'intraday' | 'post_close' | 'closed';

/** estDisplay 的结构入参（PositionRow 与 FundPosition 均满足，避免与具体类型强耦合） */
export interface EstDisplayInput {
  dayPnlEst: number;
  dayPnlPctEst: number;
  lastDayPnlEst?: number | null;
  marketValue: number;
  /** 当日实际收益（真实官方口径；PositionRow.dayPnlAct 与详情页 FundPosition.dayPnl 同源） */
  dayPnl: number;
  navDate?: string | null;
  lastNavDate?: string | null;
  /** 本次估算实际描述的交易日（后端 estDate）；空/缺失时回退 navDate/lastNavDate */
  estDate?: string | null;
}

export interface EstDisplay {
  /** 展示值：实时估算或回填的上一交易日估算；null=无数据（显示 —） */
  val: number | null;
  /** 展示收益率：实时行=dayPnlPctEst；回填行=lastDayPnlEst/(市值−当日实际)；null=不可计算 */
  pct: number | null;
  /** true=本次实时算出（描述 estDate 日收盘）；false=position_daily 种子回填（lastNavDate 日） */
  fromLive: boolean;
  /** 估算数据日期（YYYY-MM-DD）：实时行=estDate（回退 navDate/lastNavDate）；回填行=种子日 lastNavDate */
  date: string | null;
}

export function estDisplay(p: EstDisplayInput, marketSession: MarketSession): EstDisplay {
  if (marketSession === 'intraday' || Math.abs(p.dayPnlEst) > 1e-9) {
    // 盘中不重复带日期（「当日」列已标「当日估算」）；盘后/休市的实时值跟随**本次估算描述的
    // 交易日**（estDate），而非官方净值日 —— 后者盘后落后一天，打标签会误导。
    const date = marketSession === 'intraday' ? null : p.estDate || p.navDate || p.lastNavDate || null;
    return { val: p.dayPnlEst, pct: p.dayPnlPctEst, fromLive: true, date };
  }
  const base = p.marketValue - p.dayPnl;
  return {
    val: p.lastDayPnlEst ?? null,
    pct: p.lastDayPnlEst != null && base !== 0 ? p.lastDayPnlEst / base : null,
    fromLive: false,
    date: p.lastNavDate || p.navDate || null,
  };
}
