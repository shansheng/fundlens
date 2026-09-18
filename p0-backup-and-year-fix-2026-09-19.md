# 第 1 批修复：import_db 恢复前自动备份 + OCR 年份动态化

> 日期：2026-09-19
> 基准：main @ `5be3fc3`（工作区干净）
> 依据：`FundLens-业务审查报告评估-2026-09-19.md` §6 的「第 1 批（数据安全 + 一行修复）」
> 范围：只做这 2 项 + 1 处连带测试修复（见 §3），其余批次未动

---

## 1. 修复 ①：`import_db` / `import_db_b64` 恢复前自动备份

### 问题

`auto_backup_before_write` 全库仅 2 处调用（快照导入 `commands.rs:2179`、云拉取 `:2687`），
**整库恢复路径完全没有备份**。而 `db::import_db_backup` 是裸覆盖、内部也不备份：

```rust
pub fn import_db_backup(src: &std::path::Path) -> SqlResult<()> {
    let mut guard = lock_db();
    let live = ...;
    live.restore(DatabaseName::Main, src, None)   // ← 直接 restore，无 pre-backup
}
```

`import_db` 的文档注释写着「调用方（前端）必须先经用户二次确认」——**注释级约束，代码零强制**。
用户选错备份文件或选到旧版本 → 当前数据**不可逆销毁**。

### 修法

在**覆盖之前**插入一次 best-effort 备份，与快照导入 / 云拉取同一口径（tag 用 `pre-restore`）：

```rust
let pre_restore_backup = crate::backup::auto_backup_before_write("pre-restore").map(|b| b.file);
db::import_db_backup(src).map_err(|e| format!("导入恢复失败: {e}"))?;
```

两个命令（`import_db` 路径版 / `import_db_b64` 内容版）都加，位置**必须在 `import_db_backup` 之前**。

> ⚠️ 为什么不能放进 `with_conn` 闭包：`auto_backup_before_write` 自己会取全局连接，
> 闭包内嵌套加锁 = **死锁**。这与云同步那轮的结论同源（见 skill `fundlens-sync-kernel` §锁作用域纪律）。

### 安全副本不能"备了但找不到"

只备份不回传 = 安全网形同虚设。故 `BackupInfo` 增一字段并透传到前端：

| 层 | 改动 |
|----|------|
| `commands.rs` `BackupInfo` | 新增 `pre_restore_backup: Option<String>`（serde camelCase → `preRestoreBackup`） |
| `export_db` | 填 `None`（导出不是覆盖操作） |
| `src/api.ts` | `BackupInfo` 加 `preRestoreBackup?: string | null` |
| `src/pages/AboutPage.tsx` | 新增 `restoreMsg(info)`，恢复成功后报出副本文件名；**未生成时显式告警**而非静默 |

前端两处恢复入口（桌面 `importDb` / 移动端 `importDbB64`）共用同一文案函数。

---

## 2. 修复 ②：OCR 无年份日期补全年份不再写死 2026

### 问题

```rust
/// 当前年份（用于无年份日期补足）。无 chrono 依赖时回退 2026。
fn chrono_year() -> i32 {
    // 使用 time::OffsetDateTime 若可用；否则固定 2026。
    // 为保持零依赖，这里用简单回退（交易记录多为当年，足够预览提示）。
    2026
}
```

注释理由「为保持零依赖」**不成立** —— `chrono` 在 `src-tauri/Cargo.toml:28` 是**非可选依赖**，
项目各处（`commands.rs` 等）本就在用 `chrono::Local::now()`。

失效场景：2027 年 1 月导入 2026 年 12 月的截图（京东金融的 `08-13 22:15:26` 这类无年份日期）
→ 被补成 **2027-12-xx**。此时 `has_year=false` 让用户核对也无济于事 —— 要核对的正是错的年份。

### 修法

```rust
fn chrono_year() -> i32 {
    use chrono::Datelike;
    chrono::Local::now().year()
}
```

函数体在 `ocr.rs` 内，未受 `ocr` feature 门控 → 两档构建都会编译到。

---

## 3. 连带修复：一条"时间炸弹"测试

改 ② 时发现 `extract_first_date_chinese_format` 里有一条**依赖硬编码年份**的断言：

```rust
assert_eq!(extract_first_date("8月11日"), Some(("2026-08-11".to_string(), false)));
```

它过去能过，只是因为 `chrono_year()` 恰好也写死 2026。若只改实现不改测试，
该用例会**在 2026 年之后必然变红**（表现为"跨年后测试突然失败"）。

已改为断言**行为**（"无年份 → 补当前年"）而非具体年份：

```rust
assert_eq!(extract_first_date("8月11日"), Some((format!("{}-08-11", chrono_year()), false)));
```

并另加一条显式契约测试 `chrono_year_tracks_system_clock_not_constant`，
同时在注释里**如实记录其局限**：当前年恰好等于被写死的那个年时无法区分（2026 年写死 2026 时测不出），
但从次年起必然失败 —— 仍优于无断言。

---

## 4. 门禁

| 门禁 | 结果 |
|------|------|
| `npx tsc -b` | ✅ exit 0 |
| `npx vitest run` | ✅ 15 files / 104 passed（与改前一致） |
| `cargo test --lib --no-default-features` | ✅ **258 passed / 0 failed / 6 ignored**（改前 257，+1 为新增契约测试） |
| `cargo check --lib`（**default features = ocr 开**） | ✅ 编译通过（0 error / 0 warning） |

> `cargo check --lib` 在本机首次失败于 `clipper-sys` 的 `'vector' file not found` ——
> 这是**既有的本机 C++ 工具链问题，与本次改动无关**：`/Library/Developer/CommandLineTools/usr/include/c++/v1`
> 是只剩 `__cxx_version` 的**空壳目录**，libc++ 头实际在 SDK 下。加
> `CXXFLAGS="-isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"` 后通过。

---

## 5. 验证：做了**变异测试**（不是"测试绿了就算数"）

修复 ① 的断言已扩展进既有的 `export_import_db_roundtrip_preserves_positions`，
使其成为**永久回归覆盖**。断言四层：

1. 返回值 `pre_restore_backup` 必须为 `Some`（`.expect` → 缺失即 panic）
2. 文件名带 `pre-restore` tag
3. 副本文件**真实存在且非空**
4. **副本内容必须是「覆盖之前」的那一版** —— 单独打开副本查 `positions`，
   断言此前被 `delete_fund` 删掉的 `000777` 在副本里**不存在**

第 4 条是关键：它把「覆盖前备份」与「覆盖后备份」（副本里会重新出现持仓、安全网形同虚设）区分开。

**变异验证**（证明该测试真的能发现缺失的修复，而非恒绿）：

```
把 import_db 里的 auto_backup_before_write 调用临时替换为  let pre_restore_backup: Option<String> = None;
→ 测试失败：panicked at src/commands.rs:4696: "import_db 必须在覆盖前自动备份，并回传安全副本文件名"
→ 恢复真实实现后重新通过
```

同批次的 `export_db` 新增断言：`pre_restore_backup.is_none()`（导出不是覆盖操作，不应产生副本）。

---

## 6. 未做 / 未验证（诚实标注）

| 项 | 说明 |
|----|------|
| **GUI 端未跑** | 本轮未出包，未在真实 App 里点过「从备份恢复」。新增的文案展示（`restoreMsg`）只有 `tsc` + 类型检查，**没有渲染级验证** |
| **移动端路径未单独验** | `import_db_b64` 与 `import_db` 逻辑对称且共用同一备份调用，但移动端（Android SAF）未实机走通 |
| 修复 ② 的断言局限 | 见 §3 —— 在"当前年 == 被写死的年"时无法证伪，这是 `chrono::Local::now()` 不可注入导致的 |
| 恢复前备份可被剪枝 | `create_backup` 末尾会 `prune(keep_count())`（默认保留 7 份）→ 若用户随后连续多次导入，安全副本可能被更晚的备份挤出保留窗口。**已随返回值告知文件名，但未做「pin 住安全副本」的保护** |

---

## 7. 出包后建议复验

1. 桌面端：导出备份 → 改动数据 → 从备份恢复 → 确认提示里出现 `fundlens-<stamp>-pre-restore.db`，
   且该文件确实出现在数据目录 `backups/` 下
2. 故意选一个**旧的**备份文件恢复 → 确认安全副本里是"恢复前"的数据（可再选它恢复回来）
3. OCR：导入一张年份在截图里缺失的交易记录截图 → 预览里补出的年份应等于本机当前年
