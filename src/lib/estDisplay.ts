// 「估算收益」展示口径（与后端 valuation.rs 行情新鲜度三态对齐，v2.6.19+）：
// - 盘中：当日实时估算 dayPnlEst（描述今日盘中，日期由「当日」列「当日估算」标签承担）；
// - 其余时段（盘后/休市/盘前）：后端 SameDay 口径下当日估算不再归零（行情即最近收盘日，
//   用户口径「今日净值不影响今日估值」「没开盘前显示的都是今日收盘时候的估值」），
//   优先展示实时算出的「最近收盘估值」；实时值为 0（行情全陈旧/缺行情/QDII 海外交易中
//   抑制）时，回填上一交易日估算 lastDayPnlEst（position_daily 末条，null → 显示 —）。
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
}

export interface EstDisplay {
  /** 展示值：实时估算或回填的上一交易日估算；null=无数据（显示 —） */
  val: number | null;
  /** 展示收益率：实时行=dayPnlPctEst；回填行=lastDayPnlEst/(市值−当日实际)；null=不可计算 */
  pct: number | null;
  /** true=本次实时算出（描述 navDate 日收盘）；false=position_daily 种子回填（lastNavDate 日） */
  fromLive: boolean;
  /** 估算数据日期（YYYY-MM-DD）：实时行=官方净值日 navDate；回填行=种子日期 lastNavDate；盘中=null */
  date: string | null;
}

export function estDisplay(p: EstDisplayInput, marketSession: MarketSession): EstDisplay {
  if (marketSession === 'intraday' || Math.abs(p.dayPnlEst) > 1e-9) {
    // 盘中不重复带日期（「当日」列已标「当日估算」）；盘后/休市的实时值跟随官方净值日。
    const date = marketSession === 'intraday' ? null : p.navDate || p.lastNavDate || null;
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
