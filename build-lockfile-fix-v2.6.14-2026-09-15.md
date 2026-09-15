# 构建修复记录：main 分支 Cargo.lock 被 Tauri1 锁文件污染（v2.6.14 起构建失败）

- 日期：2026-09-15
- 影响版本：**v2.6.10 ~ v2.6.14**（main 分支）
- 影响面：macOS / Android 全部无法出包
- 修复提交：见本文件所在 commit

---

## 1. 症状

在 main 分支执行 `zsh fl-build-desktop.sh`，tauri CLI 在构建前直接报错退出：

```
Found version mismatched Tauri packages. Make sure the NPM package and Rust crate
versions are on the same major/minor releases:
tauri (v1.8.3) : @tauri-apps/api (v2.11.1)
```

`src-tauri/Cargo.toml` 明确要求 `tauri = "2"`、`tauri-build = "2"`、`tauri-plugin-dialog = "2"`，
而 `src-tauri/Cargo.lock` 里锁的是 **tauri 1.8.3** 全家桶。

## 2. 根因定位

逐 commit 检查 `src-tauri/Cargo.lock` 中 `tauri` 包的版本：

| commit | tauri 版本 | 说明 |
|--------|-----------|------|
| `e0838e7`（v2.6.9） | **2.11.5** | 最后可正常构建的版本 |
| `a1f3f7b` | 2.11.5 | 同步内核改动，正常 |
| `0d50979` | 2.11.5 | 正常 |
| `4d47db8`（v2.6.10） | 2.11.5 | 正常 |
| **`6927683`（"chore(release): lockfile 同步到 v2.6.10"）** | **1.8.3** | ⚠️ **污染引入点** |
| `d6b3ae0` … `68ca5af`（v2.6.11~v2.6.14） | 1.8.3 | 全部继承污染 |

`6927683` 的提交标题是「lockfile 同步到 v2.6.10」，但把 **麒麟分支 `feat/kylin-v10-aarch64`
的 Tauri 1 锁文件**（tauri 1.8.3 / tauri-build 1.5.6 / tauri-utils 1.6.2）提交到了 main。
`Cargo.lock` 与 `Cargo.toml` 的 `tauri = "2"` 直接冲突，tauri CLI 在自检阶段即失败。

> 注：`Cargo.toml` 的依赖集合在 v2.6.9 → v2.6.14 之间**没有任何变化**（仅 `[package] version` 一行），
> 所以污染的锁文件没有任何正当来源。

## 3. 修复方式（最小改动）

不做全量重解析，而是把锁文件恢复到已验证可构建的状态：

```bash
git checkout e0838e7 -- src-tauri/Cargo.lock   # 取回 tauri 2.11.5 的锁文件
# 仅把 [[package]] name = "fundlens" 的 version 改为 2.6.14
cargo metadata --manifest-path src-tauri/Cargo.toml --format-version 1 > /dev/null
```

`cargo metadata` 退出码 0，且相对 `e0838e7` 的锁文件差异**仅 1 行**（fundlens 版本号），
证明依赖图与 v2.6.9 完全一致，修复无副作用。

## 4. 连带修复：Android assets 模型仍是 PP-OCRv4

`src-tauri/gen/android/app/src/main/assets/ocr/` 是**被 git 跟踪**的 Android 资源副本，
v2.6.9 升级 PP-OCRv5 时只更新了 `src-tauri/resources/ocr/`，该副本仍是 v4 权重。
本次执行 `fl-build-android.sh` 时由构建流程同步为 v5，一并提交：

| 文件 | 修复前 | 修复后 |
|------|--------|--------|
| `gen/android/app/src/main/assets/ocr/det.mnn` | v4 | v5（sha256 `945745e4…`，与 `resources/ocr/` 一致） |
| `gen/android/app/src/main/assets/ocr/rec.mnn` | v4 | v5（sha256 `9416adfd…`，与 `resources/ocr/` 一致） |
| `gen/android/app/src/main/assets/ocr/MODEL_VERSION` | 缺失 | `ppocrv5-mobile` |

> 该副本与 `resources/ocr/` 哈希一致，是 Android 侧 OCR 模型的实际来源，此前极易漏提交。

## 5. 验证结果

| 项 | 结果 |
|----|------|
| `zsh fl-build-desktop.sh` | ✅ 成功，产出 `FundLens.app`（3m35s） |
| macOS 部署核验 | ✅ `/Applications` 版本 2.6.14，`Resources/ocr` 含 v5 权重 |
| macOS 启动冒烟 | ✅ 进程正常拉起后正常退出 |
| `zsh fl-build-android.sh` | ✅ 成功，`FundLens-2.6.14-arm64.apk` 34MB |
| APK 签名 | ✅ SHA-256 `787bd931…`（与历史版本同一签名） |
| APK 内 OCR 资源 | ✅ `assets/ocr/{det,rec}.mnn` 为 v5，`MODEL_VERSION=ppocrv5-mobile` |

## 6. 防复发措施

1. **禁止在 main 分支提交麒麟（Tauri 1）的 `Cargo.lock`**。跨分支同步时，
   `tauri.conf.json` 是已知需人工处理的冲突文件，`Cargo.lock` **必须同样列入人工检查清单**。
2. 构建前的快速自检（任一出包流程前执行）：
   ```bash
   # 断言 main 的锁文件是 Tauri 2
   grep -A1 '^name = "tauri"$' src-tauri/Cargo.lock | grep -q '2\.' || echo "LOCKFILE 被 Tauri1 污染"
   ```
3. 发版（版本升位）时，`Cargo.lock` 只允许改动 `[[package]] name = "fundlens"` 的 version 一行；
   出现其它差异必须查明来源。
