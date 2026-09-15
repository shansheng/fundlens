# 图表重做 v2.6.15（2026-09-15）

## 背景（用户反馈）

> 「请重新设计本应用中所有图表，重点解决净值走势图不直观的问题。
> ①横坐标时间轴改为按日期等距刻度，而非当前按买入卖出密度分布；
> ②买入卖出标记不要使用现有图例，改为用圆点表示，且圆点必须准确落在对应的净值线上，而非偏离净值线较远。」

## 根因（已实测复现，非推测）

净值走势图此前是 `<ComposedChart data={navPoints}>` + `<XAxis dataKey="date">`（默认 **分类轴**）
+ 三个带**独立 `data`** 的 `<Scatter data={buyData}>`。

recharts 的 Scatter 取坐标规则（`node_modules/recharts/es6/cartesian/Scatter.js:334`）：

```js
var xAxisDataKey = isNil(xAxis.dataKey) ? item.props.dataKey : xAxis.dataKey;
var yAxisDataKey = isNil(yAxis.dataKey) ? item.props.dataKey : yAxis.dataKey;
```

分类轴的**分类域**会被带独立数据的子系列改写。实测（jsdom，`XAxis dataKey="date"` + 2 个买入点）：

```
[category] x 轴刻度 = 2026-09-02@x=65  2026-09-04@x=495     ← 刻度只剩「两个买入日」！
[category] 圆点      = r2.5:(65.0,138.3) r2.5:(495.0,105.0)  ← 净值线的两个点
                       r4:(65.0,105.0)   r4:(495.0,71.7)     ← 买入点：x 撞在首末分类上，y 与曲线不同
```

⇒ 两条抱怨其实是**同一个 bug**：

1. **横轴按买入卖出密度分布**：分类域被 Scatter 的 data（交易点）接管 → 刻度只剩交易日期；
2. **圆点偏离净值线**：点的 x 落在与自身日期无关的位置，y 又按比例单独求值 → 悬在曲线外。

## 修复方案

### 1. 时间轴改「数值型真时间轴」

每行带数值型时间键 `t`（UTC 毫秒，避免 GMT+8 跨日偏移），XAxis 改为：

```tsx
<XAxis dataKey="t" type="number" domain={['dataMin', 'dataMax']} ticks={xTicks}
       tickFormatter={fmtTimeTick} padding={{ left: 8, right: 8 }} tickLine={false} />
```

`xTicks` 由 `evenlySpacedTicks(tMin, tMax, 3|5)` 生成 —— 刻度位置只由日期窗口决定，与数据点/成交点疏密无关。
刻度格式随跨度自适应（>330 天显示 `YYYY-MM`，否则 `MM-DD`）。

### 2. 交易/分红圆点与净值线「共用同一行」

`buildNavChartRows(navPoints, markers)` 把标记**写进对应日期那一行**（值 = 该行 `nav`），
Scatter 的 data 直接取这些行 → 点与线的 x 都走 `xAxis.scale(row.t)`、y 都走 `scale(row.nav)`，**数学上必然重合**。

- 周末/非交易日成交 → 归到「当日或之前最近一个净值点」（二分 `nearestNavIndex`）；
- 早于区间首日 → 归到首行（不画到域外）；
- `shares<=0` 且非分红的占位流水不打点（沿用旧口径）。

### 3. 标记形状：三角形/菱形 → 圆点

| 语义 | 形状 | 说明 |
|---|---|---|
| 买入 | 实心圆 r=4（红） | 外描边用 surface 色，压在线上仍可辨 |
| 卖出 | **空心圆** r=4（绿） | 实心/空心承载语义，不依赖红绿 → 色觉障碍可区分 |
| 分红 | 小实心圆 r=3（琥珀） | 体量最小，不抢净值线 |

图例形符（`KeySwatch`）同步改为同款圆点，删除 UpTriangle/DownTriangle/Diamond。

### 4. 其余可读性优化（净值走势图）

- 卡片顶部新增**区间概览**：区间涨跌 %（GainLossBadge）+ 区间最高/最低净值 + 净值日数；
- 净值线由 `monotone` 改 `linear`：平滑样条会画出实际不存在的取值；
- 自定义 Tooltip：除单位/累计净值外，直接标出「当天买入/卖出/分红」，不必再回表格对照；
- **Y 轴取整齐步长 + 显式 yTicks**：recharts 在自定义域上会在两端补不等距刻度 → 网格线忽宽忽窄。
  现按 `niceStep(span/5)`（1/2/2.5/5/10 × 10^n）取步长，并把域扩到步长整数倍，**网格线严格等距**；
  小数位随步长自适应（≥1 元 → 2 位 / ≥0.05 → 3 位 / 更小 → 4 位）+ 轴标题「净值（元）」；
- **同日既买又卖**：两个圆点圆心原本完全重合、其中一枚被遮住 → 卖出改画「只描边不填充」的外环
  （r=6.8）套住买入实心点（r=4），两点同时可见；图注说明；
- 网格线只留横向（`vertical={false}`），减少图表噪声；
- 图表抽成 `NavChart` 组件（`ResponsiveContainer` 注入宽高），**使几何可被单测断言**。

## 其余图表审查结论（同一批次）

| 文件 | 图表 | 问题 | 处理 |
|---|---|---|---|
| `StatsPage.tsx` | 资产配置全景（环形） | 段角度难比较；列表顺序与扇区无关；悬停无反馈；方点图例 | 按市值降序；列表加**比例条**（以最大类为满格）；悬停/点击高亮对应扇区（其余淡出）；圆心总市值保留；`role="img"` + aria-label |
| `ReportsPage.tsx` | 单线迷你折线 `Sparkline` | `preserveAspectRatio="none"` 非等比拉伸 → 描边被横向拉粗 | 全套 `vectorEffect="non-scaling-stroke"`；加区间最低值基准线；末点用绝对定位 div 画圆（circle 会被拉成椭圆）；补 aria-label |
| `ReportsPage.tsx` | 双线迷你折线 `DualSparkline` | **Y 域只取实际值 → 估算线超出区间时被裁到图外（真缺陷）** | Y 域并入估算值；同上三项；末点圆 |
| `ReportsPage.tsx` | 盈亏日历热力图 | 无月份/星期刻度，一片格子看不出时间位置 | 加月份刻度行（月份变化处标注）+ 星期刻度列（仅标一/三/五） |
| `LookthroughPage.tsx` | 行业穿透横向条 | 无满格轨道 → 短条无从判断基准 | 加满格轨道（`bg-border/30`）+ 内缩 0.5 填充条 |
| `LookthroughPage.tsx` | 行业穿透横向条 · 虚拟桶 | 虚拟桶**不设宽度** → 渲染成 0 宽虚线残条 | 虚拟桶也按真实占比给宽度，仅用虚线描边表达「组成未知」 |
| `LookthroughPage.tsx` | 风格箱九宫格 | 3×3 全部同底色，重心格与空格只能靠数字区分 | 按占比着色（单色 ramp，6%~40% 混合 primary） |

## 验收

| 项 | 结果 |
|---|---|
| `npx tsc -b` | ✅ 通过 |
| `npx vitest run` | ✅ **13 文件 / 96 passed**（新增 18 个图表测试） |
| `cargo test --lib --no-default-features` | ✅ 257 passed / 0 failed / 6 ignored |
| 新增 `fundDetailChart.test.ts`（12） | 时间键 UTC 无跨日偏移；标记值与所在行 `nav` 恒等（全量校验）；周末归位；占位流水不打点；早于首日归首行；刻度等距且与数据疏密无关 |
| 新增 `navChartGeometry.test.tsx`（6） | **渲染真实组件断言 SVG 坐标**：每个买/卖/分红圆点都能在净值线的数据点上找到 `(cx,cy)` 完全相同的点（误差 <0.01px）；实心/空心/外环可区分；同日买卖同心；X/Y 刻度各自等距；成本线 + Y 轴单位存在 |
| **真实浏览器实测**（dev server + Chromium，mock 数据） | 圆点离线偏差 **0.004px**；X 刻度 03-19/05-03/06-17/08-01/09-15 间距**恒为 262.5px**；Y 刻度 3.900→4.400 间距**恒为 52px**；同日买卖渲染为「红实心点 r=4 + 绿外环 r=6.8 fill=none」同心 |
| 旧配置反证 | 用旧分类轴跑同一几何断言会失败（刻度退化为交易日期）——已实测记录于上文「根因」 |

## 遗留 / 未做

- **`package-lock.json` 仍锁定 `@tauri-apps/api: ^1.6.0`**（`package.json` 已是 `^2.1.1`）——与 v2.6.10 的 `Cargo.lock` 污染同源（麒麟分支合并残留）。当前 `node_modules` 实为 2.11.1，不影响本机构建，但**跑 `npm ci` 会装回 v1 API 直接断构建**。本轮只把版本号 2.6.13→2.6.15（发版时漏改），依赖段未动，待单独处理。
- 净值走势图未加「区间缩放/框选」交互；`reinvest_dividend`（红利再投）仍不打点（沿用旧口径，未纳入本次范围）。
