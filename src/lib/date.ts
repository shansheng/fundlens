/// 日期 / 时间口径统一入口。
///
/// **铁律：所有业务日期键与展示时间一律用「本地时区」。**
///
/// `Date.prototype.toISOString()` 返回的是 **UTC**，在 GMT+8 下 00:00–08:00 会跨日偏差一天：
/// 同一时刻本地是 1 日 00:30，UTC 却是 31 日 16:30，`slice(0, 10)` 得到的是「昨天」。
/// 本项目曾因此把记账表单的默认交易日、导入预览的默认日期写成昨天，
/// 而 `ReportsPage` 又早已单独修过同一个坑 —— 所以现在统一收口到这里，避免第三次。
///
/// 任何新的日期格式化都从这里取，**不要再写裸 `toISOString().slice(0, 10)`**。
/// 需要 UTC 的场景（如与后端约定的纯 UTC 字段）请显式说明，不要借用本模块。

/// 任意时刻 → 本地 `YYYY-MM-DD`（与后端 `nav_date` / `txn_date` 同格式）。
export function localDateKey(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

/// 今天的本地 `YYYY-MM-DD`。用于「是否今日」判定与表单默认值。
export function todayStr(): string {
  return localDateKey(new Date());
}

const pad2 = (n: number) => String(n).padStart(2, '0');

/// 本地 `YYYY-MM-DD HH:MM:SS`（与后端 `as_of`、快照导出时间同格式；展示用）。
export function localStamp(d: Date = new Date()): string {
  return `${localDateKey(d)} ${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`;
}

/// 本地 `YYYY-MM-DD-HH-MM-SS`，可直接进文件名 / 分享标题（冒号在多数文件系统与分享目标上非法）。
export function localFileStamp(d: Date = new Date()): string {
  return `${localDateKey(d)}-${pad2(d.getHours())}-${pad2(d.getMinutes())}-${pad2(d.getSeconds())}`;
}
