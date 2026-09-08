# FundLens v2.3.0 发版记录 — 基金穿透 P1（重合矩阵 / 行业钻取 / 境外细分 / 单基金穿透）

- 日期：2026-09-08
- 版本：2.3.0（package.json / src-tauri/tauri.conf.json 均由 2.2.0 升位）
- 分支：main `e97be1e`；麒麟 `a1f7ff1`（仅同步代码，未重打包）
- 前置：v2.2.0 穿透 P0（行业两级穿透 + 个股虚拟重仓 + 当日行业贡献）
- 规模：`e97be1e` 变更 18 个文件、+910 / -25 行

## 一、P1 交付内容

### 1. 基金两两重合矩阵（识别「伪分散」）
- 新命令 `lookthrough_overlap`（纯 DB 聚合，无网络请求，毫秒级）
- **双口径**：
  - 权重重合度 `weightOverlap = Σ min(wᵢₛ, wⱼₛ)`（共同持仓逐股取小求和，100% = 完全复制）
  - top10 Jaccard = |∩| / |∪|（集合口径，只看是否都持有，不看权重）
- UI：高重合对榜单（权重重合 >40% 走预警样式）+ 宽屏对称矩阵表（基金数 ≤12 时展示）
- 货基 / 兜底 / 无披露基金不参与（无持仓向量，两两恒 0，避免虚假全重合）
- 上三角存储，读取时镜像补齐；懒加载——首次切到「基金重合」Tab 才查询

### 2. 行业钻取
- 穿透页行业条（大类 / 细分两级均可）点击展开该行业成分股
- 前端过滤 `stocks`（与行业条完全同口径），保留隐性重仓预警标记
- 「未穿透」桶点击显示「无成分股」——不放大原则在 UI 上的诚实表达

### 3. 境外股行业细分
- `fetch_stock_industry` 扩展港美股：东财 secid 市场前缀 116（港股）/ 105（美股），行业字段沿用同一套 f127
- 有画像的境外股：L2 = 东财行业名（如腾讯 → 互联网服务），L1 恒为「境外资产」（市场风险视角稳定，不因行业名漂移）
- 无画像回退「港股」「美股」桶；**需点「补行业画像」拉取港美股行业**（90 天新鲜度）
- 不变量「L2 合计 = L1」在引入境外细分后仍成立（单测覆盖）

### 4. FundDetailPage 单基金穿透卡
- 新命令 `lookthrough_fund(code)`：复用同一套 aggregate，单基金输入，分母 = 该基金市值
- 卡片：行业分布条形图（同口径样式）+ 穿透前十大重仓 + 覆盖率 / 报告期徽标
- 有披露且覆盖率 >0 才渲染；失败静默，不阻塞详情页其余内容

### 5. jjcc 全量持仓核对结论（实测，结论性）
- **topline 参数无效**：`topline=10` 与 `topline=200` 响应逐字节相同，接口恒返回「请求期 + 前一期」各前十大
- 全量持仓链路（`FundArchivesDatas.aspx?type=ccmx`）为 JS 渲染，非浏览器环境返回空，不可用
- **结论**：维持「前十大」口径（披露类型仍按 top10 / full 区分标注），覆盖率受此上限约束；实测结论已注释在 `data.rs`，避免后续重复踩坑

## 二、变更清单（`e97be1e`）

| 层 | 文件 | 变更 |
|---|---|---|
| 后端 | `src-tauri/src/lookthrough.rs` | +306：重合矩阵纯函数（weightOverlap / Jaccard，上三角）、境外细分分支、5 项不变量单测 |
| 后端 | `src-tauri/src/data.rs` | +26：港美股 secid 映射与代码清洗（美股仅纯字母代码大写）；jjcc topline 结论注释 |
| 后端 | `src-tauri/src/commands.rs` | +101：`lookthrough_overlap` / `lookthrough_fund` 命令；`fetch_stock_profiles` 扩展收集 A/HK/US 代码 |
| 后端 | `src-tauri/src/lib.rs` | +2：两个新命令注册进 invoke handler |
| 后端 | `src-tauri/permissions/fundlens.toml` | +10：`fl-lookthrough-overlap`、`fl-lookthrough-fund` 权限项 |
| 后端 | `src-tauri/capabilities/default.json` | +2：两个权限项挂进默认 capability（与 gen/schemas 五处同步） |
| 前端 | `src/api.ts` | +93：`OverlapResult` / `OverlapCell` / `FundLookthroughResult` 类型 + `lookthroughOverlap` / `lookthroughFund`（含 mock） |
| 前端 | `src/pages/LookthroughPage.tsx` | +218：第三个 Tab「基金重合」、行业钻取、移除 P1 占位文案、修复按钮 `role="cell"` 覆盖隐式 button |
| 前端 | `src/pages/FundDetailPage.tsx` | +68：单基金穿透卡（懒加载 `lookthroughFund` → `lt` state） |
| 前端 | `src/pages/LookthroughPage.test.tsx` | +53：钻取交互 / 重合 Tab / 不足 2 只空态 |
| 版本 | `package.json`、`src-tauri/tauri.conf.json` | 2.2.0 → 2.3.0 |

## 三、真实数据对账（2026-09-08，113 只持仓基金）

- 两两对数 **6,328**；**最大权重重合 59.5%**（财通集成电路产业C × 财通价值动量混合A，共同 8 只）
- 典型伪分散：景顺长城价值发现 A1 × A2 Jaccard **100%**（同一基金不同份额，分散是假的）
- 不变量 `weight_overlap ≤ min(Σwᵢ, Σwⱼ)` 在全部 6,328 对上成立；矩阵对称性成立
- 境外股画像当前 0 条（仓库 DB 尚未拉港美股行业），点「补行业画像」后按境外细分生效

## 四、质量验证

| 项 | 结果 |
|---|---|
| `cargo test --lib --no-default-features` | **129 passed**（+5 P1 不变量：对称 / 上界 / 完全复制 / 货基剔除 / 单基金分母） |
| `npx vitest run` | **47 passed**（+3：钻取交互 / 重合 Tab / 不足 2 只空态） |
| `npx tsc -b` | 0 错误 |
| 真实 DB 复现对账 | 重合矩阵公式一致、不变量成立 |
| 麒麟分支 `cargo test` | **129 passed**（与 main 一致；Tauri 1 适配保留：v1 invoke / `path_resolver` / 无 dialog plugin / v1 conf schema / 删除 ACL） |

## 五、交付物

| 产物 | 状态 |
|---|---|
| 桌面版 `/Applications/FundLens.app` | ✅ v2.3.0 已部署（26 MB，`CFBundleShortVersionString` = `CFBundleVersion` = **2.3.0**，核验通过） |
| Android APK（aarch64） | ✅ `FundLens-2.3.0-arm64-lookthrough-p1.apk`（30,790,246 B ≈ 29.4 MB），apksigner 签名证书 SHA-256 `787bd931…ee37c`，与 2.1.1/2.2.0 同一签名，可覆盖安装 |
| 麒麟分支 | ✅ 仅同步代码不打包（`9357040`，Tauri 1 五处适配保留，cargo 129 passed） |

**构建与签名细节**

- 桌面：`zsh fl-build-desktop.sh`（含 CLT 21 的 `CXXFLAGS` C++ 头修复），release 全量 6m54s → bundle → `rm -rf /Applications/FundLens.app && cp -R …/bundle/macos/FundLens.app /Applications/`
- Android：`zsh fl-build-android.sh`（`npx tauri android build -t aarch64 --apk` → zipalign → apksigner），9m30s
- 版本一致性：本次发现 `src-tauri/Cargo.toml` / `Cargo.lock` 仍停留在 2.1.1（对外版本此前只维护在 `tauri.conf.json`），已对齐到 **2.3.0**（`a7f615d`），后续以三处一致为准

## 六、使用说明（升级后如何验证）

1. **穿透页**：侧边栏「策略信号」上方新增「基金穿透」（路由 `/lookthrough`，图标 Layers）
2. **行业钻取**：大类 / 细分两个 Tab 下的行业条均可点击展开成分股
3. **基金重合**：切到第三个 Tab「基金重合」，首次进入触发查询；>40% 的对标红
4. **单基金穿透**：任意基金详情页新增「穿透」卡片（有披露且覆盖率 >0 才出现）
5. **境外细分**：点「补行业画像」拉取港美股行业后，境外股 L2 由「港股/美股」桶细化为东财行业名

## 七、已知限制与风险

- **前十大上限**：jjcc topline 实测无效，穿透覆盖率上限 = 前十大占股票市值比重，已在 UI 常驻展示覆盖率
- **重合矩阵规模**：113 只基金 = 6,328 对，纯内存聚合；基金数翻倍到 300+ 时需评估渲染（当前矩阵表限 ≤12 只，榜单 Top N 不受影响）
- **境外画像**：依赖「补行业画像」手动触发，未拉取前境外股走港股/美股兜底桶
- **同份额重复**：A1/A2 类份额会被判为 100% 重合，属真实结论（伪分散），非 bug，但榜单会占位

## 八、回滚方案

- 代码：`git revert e97be1e` 后重打包；前端 Tab 与详情页卡片均为新增渲染分支，回退 P0 行为不受影响
- 数据：`stock_profile` 表为 P0 引入的纯缓存表，删除后自动重建；无破坏性迁移
- 版本：回退 `package.json` 与 `tauri.conf.json` 至 2.2.0 即可

## 九、下一步（P2 备选池）

- 风格箱（大/小盘 × 价值/成长九宫格，需市值与估值数据源）
- 口径开关（中报全量 vs 前十大切换——待东财全量链路可用）
- 重合矩阵钻取（点击单元格展开两基金共同持仓明细）

> 免责声明：本页所有输出为持仓结构分析，基于公开数据和量化方法，仅供参考，不构成投资建议。市场有风险，投资需谨慎。
