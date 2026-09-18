# OCR 字典错配修复 v2.6.16（2026-09-16）

## 一句话

**`resources/ocr/dict.txt` 与 det/rec 权重不同代，导致中文识别「高置信度乱码」——自 v2.6.9 起潜伏 7 个版本，本次切 PP-OCRv6 tiny 全家桶修掉。**

## 背景（怎么发现的）

起点是主人问「rusto-rs 和 PaddleOCR 有没有升级版本」。核查过程中顺手比对资源文件，
发现 `MODEL_VERSION` 写着 `ppocrv5-mobile`，而 `dict.txt` 只有 26,249 字节、首行是 `` ` ``/`疗`/`绚`
—— 这是 **PP-OCRv4 的 `ppocr_keys_v1.txt` 频率序**，不是 v5 字典该有的样子。

`download_ocr_models.sh` 的注释当时还写着「RapidOCR 不提供 v5 中文字典」——这条注释本身是错的
（实测 rusto-rs 仓库 `models/PPOCR_v5_mobile/dict.txt` 74,012 字节就是 v5 中文字典；
`models/PPOCR_v5/dict.txt` 只有 1,416 字节，那是英文版，别拿错）。

## 根因（已 A/B 实测，非推测）

### 字典-模型代际匹配的机制

PP-OCR 的 rec 模型输出的是**字符表索引**（predict 出 idx，再查字典取字）。v4 / v5 / v6 的字符表
**索引序完全不同**。所以：

- 模型与字典同代 → 正常
- 模型与字典异代 → **不报错**，因为索引永远落在字典范围内，只是查出来的字全是错的

⇒ 表现为「识别成功、置信度 0.99、输出全是乱码」。这是最坏的一类 bug：**没有任何错误日志**。

### A/B 实测铁证

用同一张中文基金持仓截图，`FUNDLENS_OCR_DIR` 指向两套资源分别跑：

| 资源组合 | 可读行数 | 典型输出 | 平均置信度 |
|---|---|---|---|
| v5 权重 + v4 字典（现状） | **0 / 87** | `汛丶汛⑿丶……` 类乱码 | 0.99 |
| v6 tiny 权重 + v6 同源字典（修复后） | 87 / 87 | `凯莱英002821` / `泰格医药300347` / `长春高新000661` | 0.9705 |

修复前后是**同一张图、同一段调用链**（走完 `model_dir()` → `RustOConfig::ppv6()`）。

### 引入点

`e0838e7`（v2.6.9「OCR 语义层」）把 det/rec 从 v4 换成 v5 mobile，**漏换字典**。
此后 v2.6.10 ~ v2.6.15 七个版本的 OCR 中文识别一直是坏的（截图中文名称全乱码，
基金代码因为走的是通用纠错/正则路径才勉强能用）。

## 修复（方案 B：全家桶切 PP-OCRv6 tiny）

### 1. `src-tauri/download_ocr_models.sh`

| 项 | 旧 | 新 |
|---|---|---|
| `MODEL_VERSION` | `ppocrv5-mobile` | `ppocrv6-tiny` |
| 权重来源 | rusto-rs-models 各版本拼接 | `rusto-rs-models` release `v1.0.0` 统一拉取 |
| `det.mnn` | 4,739,056 B | **1,745,176 B** |
| `rec.mnn` | 16,582,808 B | **4,461,484 B** |
| `dict.txt` | 26,249 B（v4 频率序） | **27,156 B**（v6 同源） |
| `cls.mnn` | 531,024 B | 531,024 B（v2.0 cls 继续走 ModelScope，未变） |
| 合计体积 | **21.3 MB** | **6.2 MB** |

`fetch()` 同时加固：先下到 `.part` 再 `mv`（避免中断留下半截文件被当有效权重），
并带 `expect` 期望字节数校验——**只有大小完全匹配才跳过重下**。

### 2. `src-tauri/src/ocr.rs`

- `rusto::RustOConfig::ppv5(...)` → `rusto::RustOConfig::ppv6(...)`
- 层 1 参数覆盖**全部保留**（v6 preset 与 v5 的唯一差异是 `det_box_thresh` 0.6 vs 0.5，
  其余 preset 相同，故调优参数无需改动）：
  - `det.limit_type = "max"`
  - `det.limit_side_len = 1536`
  - `unclip_ratio = 2.0`
  - `use_dilation = true`
  - `text_score = 0.6`
- 模块头注释补上**字典必须同代**的显式警告，标明引入与修复的 commit。
- 测试名 `engine_loads_ppocrv5_model` → `engine_loads_ppocrv6_model`。

### 3. `src-tauri/src/commands.rs`

两处用户可见错误消息 `下载 PP-OCRv5 模型` → `下载 PP-OCRv6 tiny 模型`。

### 4. 双份资源同步

`src-tauri/resources/ocr/` 与 `src-tauri/gen/android/app/src/main/assets/ocr/`
两份权重**同一次提交内同步**，`det/rec/dict/cls` 四文件 sha256 逐项一致：

```
6cb3cc2410929e6ca526e223a1a3e8eb7dcfeac46ed8d64a93e061a6a0eaad75  cls.mnn
a3c943312c84dfc7fb65dc3c5e2b01e5f8654b5823e80b374ef446858f3d2913  det.mnn
55f7d3f6f4f9b7d8f9892be25ff8fa97ae7a54788af26f534fb6757c43622048  rec.mnn
c5cbe34ef40c29c4df07ed012bf96569cb69a2d2a01a07027e9f13cb832bd9cd  dict.txt
```

## 为什么选 v6 tiny 而不是别的

| 候选 | 结论 |
|---|---|
| v5 权重 + v5 正确字典 | 可用，但体积 21.3MB、识别精度不如 v6 tiny |
| 升 rusto-rs 0.2.5 + PP-OCRv6 full | **不做**：0.2.5 是 breaking 变更（`RustOConfig` → `InitializeConfig`），且 0.2.1 已内置 `ppv6()` preset，无需升版 |
| **v6 tiny 全家桶（选定）** | 体积 -71%（21.3→6.2MB），中文精度更高，同源字典杜绝代际错配 |

## 验收

| 项 | 结果 |
|---|---|
| `npx tsc -b` | ✅ 通过 |
| `npx vitest run` | ✅ 13 文件 / 96 passed |
| `cargo check --lib`（含 `ocr` feature） | ✅ 通过 |
| `cargo test --lib --no-default-features` | ✅ 257 passed / 0 failed / 6 ignored |
| `engine_loads_ppocrv6_model`（ignored，手跑） | ✅ MNN 加载 v6 tiny 权重成功 |
| **生产路径实证**（`FUNDLENS_OCR_DIR` → `model_dir()` → `ppv6()`） | ✅ 正确输出 `凯莱英002821` / `泰格医药300347` / `长春高新000661`，均分 0.9705 |
| 修复前反证 | ✅ 同图 0/87 行可读，置信度 0.99 —— 确认「乱码」而非「识别失败」 |

临时验证测试（`ocr_ab_tmp.rs` / `ocr_verify_tmp.rs`）跑完即删，`git status` 无残留。

## 提交

`a9e2b24` — `fix(ocr): 切 PP-OCRv6 tiny 并修正字典错配 —— 中文识别乱码修复（v2.6.16）`
（16 files changed, 13664 insertions(+), 13064 deletions(-)）

版本五处同步 2.6.15 → 2.6.16：`package.json` / `package-lock.json`（顶层 + `packages[""]`）/
`tauri.conf.json` / `Cargo.toml` / `Cargo.lock`（仅 `fundlens` 条目，`tauri` 仍 2.11.5 ✓）。

## 出包与部署

| 项 | macOS | Android |
|---|---|---|
| 脚本 | `zsh fl-build-desktop.sh` | `zsh fl-build-android.sh` |
| 耗时 | **16m 40s** | **7m 44s** |
| 产物 | `src-tauri/target/release/bundle/macos/FundLens.app` | `FundLens-2.6.16-arm64.apk` |
| 体积 | — | **22 MB**（上版 34 MB，**-12 MB**） |
| 门禁输出 | `version=2.6.16 tauri_lock=2.11.5` ✓ | `ocr check ok: model=ppocrv6-tiny (det/rec sha256 与 resources/ocr 一致)` ✓ |

> macOS 耗时 16m40s（典型增量约 5min）——因 `ocr.rs` 变更触发 Rust 侧大范围重编。
> 这个时长是**下次改 Rust 代码时的合理预期**，纯前端改动仍走 ~5min 快路径。

### 产物内 OCR 资源核验（四项 sha256 与源逐项一致）

macOS `.app/Contents/Resources/ocr/` 与 APK `assets/ocr/` 均为：

| 文件 | 字节 | sha256 前 8 位 |
|---|---|---|
| `det.mnn` | 1,745,176 | `a3c94331` |
| `rec.mnn` | 4,461,484 | `55f7d3f6` |
| `dict.txt` | 27,156 | `c5cbe34e` |
| `cls.mnn` | 531,024 | `6cb3cc24` |
| `MODEL_VERSION` | 13 | `ppocrv6-tiny` |

### 部署（macOS）

1. `pkill -f "FundLens.app/Contents/MacOS/fundlens"`
2. `mv /Applications/FundLens.app /tmp/FundLens.app.prev-20260916-211003`（**旧包 v2.6.15 已留存，可回滚**）
3. `/bin/cp -R <产物> /Applications/FundLens.app`（用 `/bin/cp` 绕开 zsh 的 `cp -i` 别名）
4. 核验：版本号 `2.6.15 → 2.6.16` ✓ / 二进制 sha256 `c2042356…` 产物与部署**完全相同** ✓
5. 启动冒烟：`open -a` 后进程 PID 7219 稳定（8s 时 CPU 67.5% → 20s 后回落 0.9%、RSS 65MB），
   `~/Library/Logs/DiagnosticReports` 近 5 分钟**无崩溃日志** ✓

> **回滚方式**：`pkill` → `mv /Applications/FundLens.app /tmp/FundLens.app.bad` →
> `mv /tmp/FundLens.app.prev-20260916-211003 /Applications/FundLens.app`

## 遗留 / 未做

- **真实持仓截图复验未做**：本轮 A/B 用的是图表截图样本。建议后续用
  `TEST_IMG=<真实持仓截图> cargo test --test ocr_e2e` 复验一次（断言能抽到「易方达/华夏」类名称），
  再确认端到端可用。
- OCR 相关的 `MODEL_VERSION` 标记机制依赖脚本，若日后手工替换权重文件，**必须连带更新三件套**，
  否则重蹈本次覆辙。已在 `ocr.rs` 模块头与 `Cargo.toml` 的 `[features]` 注释写入警告。

## 该分支同步（feat/kylin-v10-aarch64）

`19fa382`（merge main）—— 四处固定处理逐项核验 + **本次新踩到一个坑**：

| 项 | 结果 |
|---|---|
| `capabilities/` + `permissions/` | 麒麟侧本就不存在，merge 未带入 ✓ |
| `src-tauri/tauri.conf.json` | 唯一冲突已解：保留 `$schema .../config/1`、**删掉被并进的顶层 `productName`/`version`/`identifier`**、只升 `package.version` → 2.6.16；顶层键恰为 `[$schema, build, package, tauri, plugins]`、`resources/ocr` 映射保留 ✓ |
| `src-tauri/Cargo.lock` | 取麒麟侧（tauri **1.8.3**），`fundlens` 条目随 main 升到 2.6.16；`cargo check` 前后 sha256 一致（`fbdcbabf…`）**未被改写** ✓ |
| **🔴 `package-lock.json` 被自动合并污染（新坑）** | 见下 |
| 语义 grep | `api.ts` 仍 `@tauri-apps/api/tauri`（v1）+ `__TAURI__` 探测；`lib.rs` 无 dialog plugin；`db.rs`/`ocr.rs`/`commands.rs` 均 `path_resolver()`（Option）Tauri1 写法；`Cargo.toml` `tauri = "1"` + `features = ["dialog-all"]` 完好 ✓ |
| `cargo check --lib --no-default-features` | ✅ 通过 |
| `cargo check --lib`（含 `ocr`，default 开启） | ✅ 通过，`rusto-mnn-sys`/`rusto-mnn`/`rusto-rs` 全编，**0 warning** |
| OCR 资源 | `resources/ocr` + `gen/android assets/ocr` 与 main 侧**逐项 sha256 一致**（ppocrv6-tiny）✓ |

### 🔴 本次新踩的坑：`package-lock.json` 也会被 merge 污染

**现象**：merge 后 `package.json` 是 Tauri 1 依赖（`^1.6.0`），但 `package-lock.json` 变成了
Tauri 2（`@tauri-apps/api: ^2.1.1` + `plugin-dialog`）——**两个文件语义分裂**。

**原因**：git 三方合并的判定

| 版本 | `package.json` 依赖 | `package-lock.json` 依赖 |
|---|---|---|
| merge base（main 的 2.6.15） | Tauri 2（`^2.1.1`） | Tauri 2 |
| 麒麟侧 | Tauri 1（`^1.6.0`） | Tauri 1 |
| main 侧 | Tauri 2 + 版本号 2.6.16 | Tauri 2 + 版本号 2.6.16 |

- `package.json`：麒麟侧改了依赖、main 侧改了版本号 → **两侧都改 ≠ 冲突**，git 正确合成
  「麒麟依赖 + main 版本号」✓
- `package-lock.json`：麒麟侧「整份文件」与 base 不同，但 git 是按行判定的——麒麟侧改动的那几行
  （依赖段）恰好也是 main 侧改动的位置上游，git 把整份锁文件的差异**当成了单侧改动**直接采用 main 版 ❌

**修法**（与 main 的 `43c399c` 同思路）：

```bash
git checkout HEAD^1 -- package-lock.json   # 取回麒麟侧锁
# 再把顶层 + packages[""] 的 version 2.6.13 → 2.6.16
node -e "...比对 dependencies/devDependencies..."  # 确认与 package.json 完全一致
```

**结论**：**两个锁文件在每次跨分支 merge 后都必须单独核验**，不能只看 `package.json`。
`Cargo.lock` 的记忆里已有这条，`package-lock.json` 是同一类坑的新实例（v2.6.10 是 Cargo.lock，
v2.6.15 是 main 侧 package-lock，本次是麒麟侧 package-lock）。

### 顺手清理（麒麟分支）

- 删 `commands.rs` 未使用的 `use tauri::Manager;`（Tauri 1 的 `path_resolver()` 不需要该 trait；
  `default` 与 `--no-default-features` 两种配置下编译器均报 unused）→ 门禁从 1 warning 降到 0。
- `Cargo.toml` 的 OCR 注释由「PP-OCRv4」更正为 PP-OCRv6 tiny，并补「四件套必须同代」警告
  （main 侧同名注释同步修正，commit `87bd759`）。

> ⚠️ 本分支**不做本机 tauri build**（会抹掉 `dialog-all`），权威打包走 Docker `fl-build` + `arm64-build/inc-build.sh`。

