# FundLens v2.2.0 发版记录 —— 基金穿透 P0

> 发版日期：2026-09-08 · 基线 main@5d668de（基于 2c97901 图表重构之后）
> 规划文档：`FUNDLENS-基金穿透功能规划-2026-09-08.md`（v1.1，三项决策已裁定）

## 一、本版新增：基金穿透（Look-through）P0

组合层面的**虚拟底层资产表**——把 N 只基金的黑盒拆开，回答三问：
我实际持有什么 / 暴露是否集中重复 / 隐性重仓是谁。

### 功能清单
- **两级行业穿透**：9 大类 L1（Rust 映射常量 ~90 条精确表 + 包含规则容错）
  + L2 细分（东财行业名直出，零维护成本），UI「大类/细分」切换，两级分母一致
- **个股虚拟重仓表**：穿透权重/市值/持有基金数 + CR5/CR10 集中度徽标
  + 行展开贡献基金明细 + **隐性重仓预警**（≥3 只基金且合计穿透占比 >5%，已裁定阈值）
- **当日行业贡献**（P0 已裁定）：交易时段 Σ(穿透市值×当日涨跌幅)，
  复用既有估算行情链路（fetch_quotes 节流 + quotes_cache 回写），不新增出站压力
- **口径红线**：不放大原则（披露权重直用）、未穿透桶显式单列（虚线样式）、
  组合覆盖率常驻口径条、货基/理财/金额兜底不产生个股行、境外股票整桶（P1 细分）

### 工程落点
| 层 | 文件 | 内容 |
|---|---|---|
| 表 | `stock_profile`（新） | 东财行业画像，90 天新鲜度，IF NOT EXISTS 幂等 |
| 数据 | `data.rs::fetch_stock_industry` | push2 单股详情 f127，节流复用，失败兜底未分类 |
| 引擎 | `lookthrough.rs`（新 ~850 行） | 聚合纯函数 + 10 项不变量单测 |
| 命令 | `lookthrough_overview` / `fetch_stock_profiles` | ACL 双处注册 |
| 页面 | `LookthroughPage.tsx`（新）+ `/lookthrough` 路由 | 入口插在策略信号上方 |

### 质量验证
- `npx tsc -b`：0 错误；`npx vitest run`：**44 passed**（39+5）
- `cargo test --lib --no-default-features`：**124 passed**（含穿透不变量 10 项）
- 真实 DB 复现式对账：198 只基金、Σ(行业桶+未穿透)/总市值 = 1.000000 ✓
- 不变量：L2 合计=L1、ΣL1=100%、货基不出现在个股表、覆盖率截断不为负、
  当日贡献与手算一致、隐性重仓阈值双分支、CR5/CR10、映射表抽查

## 二、构建环境备注（重要）

CLT 21（Apple clang 21）更新后 `/Library/Developer/CommandLineTools/usr/include/c++/v1`
只剩 `__cxx_version` 空壳，clipper-sys（OCR 链路）编译报 `'vector' file not found`。
**修复**：构建时注入
`CXXFLAGS="-isystem /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/c++/v1"`
（已固化在 `fl-build-desktop.sh`）。本机 cargo 为 x86_64（Rosetta），产物与 2.1.1 一致为 x86_64。

## 三、交付物

| 产物 | 状态 |
|---|---|
| 桌面版 /Applications/FundLens.app | ✅ v2.2.0 已部署（mtime 02:07 晚于构建产物，CFBundleShortVersionString=2.2.0） |
| Android APK（arm64） | ✅ `FundLens-2.2.0-arm64-lookthrough.apk`（29.3M，zipalign+apksigner 签名，证书 SHA-256 `787bd931...` 与项目 keystore 一致实测核验） |
| 麒麟分支 | ✅ **仅同步代码不打包**（f168771，Tauri1 适配保留，cargo 124 passed）；fl-build 重打包待另行安排 |

> Android 构建备注：vite 清空 dist 时被 WorkBuddy 安全垫片拦截（>50 文件批量删除保护），
> 先清理 dist 再构建即可；构建+签名一体脚本 `fl-build-android.sh`（含 keystore 密码，**不进 git**）。

## 四、git 状态

- main：`5d668de`（穿透 P0 `d8072cb` + 死代码清理 `0d32a47` + 版本 2.2.0）
- feat/kylin-v10-aarch64：`f168771`（main 23 提交合入，含 v1 invoke/path_resolver/
  无 dialog plugin/tauri.conf v1 schema/ACL 删除五处适配；cargo test 124 passed）
- `.gitignore` 新增 `*.apk`/`*.idsig`/`*.app.tar.gz`（发版二进制不进 git）

## 五、已知边界（下轮 P1）

- 基金两两重合矩阵（伪分散识别）未实现（页面留有占位提示）
- 行业钻取、FundDetailPage 单基金穿透卡片、境外股票行业细分
- jjcc 接口 `topline=10` 是否限制中报/年报全量持仓需实测

> **免责声明**：基金穿透为持仓结构分析工具，输出基于公开披露数据的近似画像，
> 不构成投资建议。市场有风险，投资需谨慎。
