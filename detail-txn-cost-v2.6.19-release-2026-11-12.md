# FundLens v2.6.19 发版记录（2026-11-12）

## 背景

详情页三处连续反馈（同日两轮）：

1. 交易记录位置靠上、条数多时整表铺开，详情页被流水占满。
2. 走势区间最长只有「近6月」，看长期表现要切「全部」，且默认落在「全部」。
3. **「持仓成本线不应该是一条直线吧」** —— v9 把成本线改成「当前持仓均价水平参考线」，等于把加仓/减仓历史抹平。

## 改动 1：交易记录下移到走势图之后 + 分页

- 卡片顺序：净值走势 → 交易记录（原为交易记录在上）。
- 分页：`TXN_PAGE_SIZE = 20`，前端切片（详情接口本就一次返回该基金全部流水，不改后端）。
- 控件：首页/上页/页码/下页/末页 + 「第 a–b 条 · 共 n 条」，仅 `totalPages > 1` 时渲染。
- 两处边界处理：切基金时页码归零（`useEffect([code])`，否则沿用上一只基金页码）；删记录导致总页数变少时 `txnSafePage` 收敛到末页，不白屏。

## 改动 2：走势区间新增「近1年」并设为默认

- `RANGES` → 近1月 / 近3月 / 近6月 / **近1年** / 全部；`DEFAULT_RANGE = '1y'`。
- 后端 `range_cutoff` 增 `"1y" => 12`。
- 横轴无需改：`spanDays > 330` 自动从 `MM-DD` 切到 `YYYY-MM`，一年区间正好命中；区间涨跌/最高/最低均由 `navRows` 派生，自动适配。

## 改动 3：持仓成本线改为「成本台阶」（v9.1）

### 为什么不能正向重放流水

v9（`b36a1c0`）的顾虑成立：`positions` 是权威，流水只是账本（FundVal 导入 / 手工盘点 / 改成本价都不写流水）。真实库实测：

| 基金 | 实际持仓份额 | 正向重放流水 |
|---|---|---|
| 011506 | 5121.44 | **−1765.57** |
| 008929 | 7959.77 | 5591.39 |
| 014461 | 3247.67 | 83.40 |

### 采用「锚定当前持仓倒推」

从权威 `(shares, basis)` 出发，按 `apply_txn_to_position_conn` 的 forward 规则**逆运算**回放：buy 扣回份额与金额、sell 按当时均价回补、dividend 加回金额、reinvest 扣回份额。

- **右端点必然等于持仓页单位成本**（实测 008929→1.4811 / 003095→2.1986 / 011506→1.2995，与 `sum(cost_amount)/sum(shares)` 逐位一致）。
- 加仓/减仓当日竖跳，其余持平 → 真正的台阶。
- 逆推到份额或成本 ≤ 0 即账本覆盖不到更早（导入期初持仓），**就地截断**，页面标注「成本线自 X 起」，不伪装成水平线。
- 覆盖：193 只持仓中 **155 只可完整倒推 / 23 只部分 / 15 只纯导入无流水**（后者退回水平参考线，`cost_level` 兜底，属有意保留）。

### 顺带修掉取数错误

`get_position_basis` 原按（账户、基金）`query_row` 只取一行，同基金多平台持仓时成本线会偏离。改为 SUM 汇总（`get_position_basis_agg`），`get_fund_series` 增 `platform` 参数：从某平台持仓行进入按该平台算，否则跨平台合计。库中现有 8 只多平台持仓基金。

## 门禁与产物

| 项 | 结果 |
|---|---|
| `npx tsc -b` | 0 错 |
| `npx vitest run` | **131 passed / 17 文件**（原 125 → +6：分页 2、成本前向填充 3、成本台阶几何 3 − 交叉计） |
| `cargo test --lib --no-default-features` | **321 passed / 0 failed / 8 ignored**（原 317 → +4：区间 1y、成本台阶 4 − 重叠） |
| 版本 | 五处同步 2.6.19（package.json / package-lock.json ×2 / tauri.conf.json / Cargo.toml / Cargo.lock 仅 fundlens 段） |
| macOS | 构建 46s，部署 `/Applications`，sha256 `de37358e…` |
| Android | `FundLens-2.6.19-arm64.apk`，22,713,004 B，sha256 `814e8ae8…`，versionCode 2006019 |

### 几何断言（防回归的关键）

`navChartGeometry.test.tsx` 直接解析成本线的 SVG path：断言至少 2 个不同 y 层级、且存在连续 3 点等高的水平段。旧的「一条水平线」实现必挂。

### 决定性指纹探针

| 探针 | 新 | 旧 |
|---|---|---|
| `FundDetailPage-C5h6mjqA.js` | 1 | 0 |
| `FundDetailPage-2lwwRGCz.js`（macOS 上一版） | 0 | 1 |
| `FundDetailPage-p_xEUJJ8.js`（Android 2.6.18） | 0 | 1 |
| `FundDetailPage-`（共有串，证明方法有效） | 1 | 1 |

APK 侧同法在 `lib/arm64-v8a/libfundlens_lib.so` 内搜索；`.so` sha256 与大小均与 2.6.18 不同（11,860,488 vs 11,859,984），证明新 Rust 代码确已进入包内。

⚠️ **中文串不能当探针**：esbuild 默认 `charset=ascii`，非 ASCII 会被转成 `\uXXXX`，二进制里搜不到。

### 签名（覆盖安装三要素）

| 项 | 值 |
|---|---|
| 包名 | `com.fundlens.app`（同） |
| versionCode | 2006019 ≥ 2006018（不降） |
| 证书 SHA-256 | `78:7B:D9:31:06:83:40:F4:…:8E:E3:7C`（同 keystore，见 MEMORY 铁律 10） |

OCR 自检：`ppocrv6-tiny`，det/rec sha256 与 `src-tauri/resources/ocr` 逐字节一致。
