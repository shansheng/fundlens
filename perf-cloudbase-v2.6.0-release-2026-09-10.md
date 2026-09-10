# FundLens v2.6.0 发版记录（性能缓存 + CloudBase 同步内核）

- 日期：2026-09-10
- 版本：v2.6.0（桌面已部署 /Applications；麒麟代码已同步 2a6e2cf，不打壳）
- main 提交线：e7aa001(perf) → c158480(加固) → 5929ab7+da44bc0(sync M1) → ab0989e(merge) → d4eed72(升位)

## 背景与动因
用户反馈「总览/持仓页刷新转圈」「周报/月报卡」，并启用 CloudBase 提出多设备同步诉求。
取证（真实库副本实测）：SQLite 查询非瓶颈（positions 198 / disclosures 2622 / nav_history 68k，
全表 <100ms）；真凶 = get_overview 每次调用全量重做（~839 符号行情网络批抓取 + ~200 持仓逐只
重算），且 ReportsPage 挂载并发 6 命令、每个报表内部各自整跑一遍 get_overview = 同刻 5~6 份行情风暴。
设计文档：`perf-cloudbase-v2.6.0-design-2026-09-09.md`。

## 一、性能优化（总览/明细/报表）
- e7aa001：进程内短 TTL + single-flight 快照缓存（`cached_with` seam；交易时段 10s / 盘前盘后休市 60s），
  get_overview / get_fund_detail 命中即 clone；**口径零变化**（compute_* 与原函数体逐段 diff byte-identical，
  独立验证工人取证）；14 个写命令成功路径调 invalidate_caches 全失效；补 nav_history(fund_code,nav_date)
  复合索引 + cache_size=-65536/mmap_size/busy_timeout PRAGMA（execute_batch 下发——PRAGMA 赋值返回行
  会使 conn.execute 报 ExecuteReturnedResults，该坑曾致 27 测试失败）。
- build_period_report 复用缓存入口 → 报表 4+1 并发命令共享一次行情快照，消除周报/月报卡顿。
- c158480：P2/P3 加固——8 处 Mutex 锁中毒容错（unwrap_or_else into_inner）、午休(11:30-13:00) TTL 60s、
  失效函数去重去 dead_code。测试 146→158 全绿。

## 二、CloudBase 多设备同步 M1 内核（纯本地，无云/无 UI/无新命令）
- 5929ab7 + da44bc0：SQLite 触发器在 DB 层自动变更跟踪——13 张参与表加 updated_at + ai/au/ad 触发器
  记 sync_log（row_key = 业务主键 JSON 数组，跨设备稳定身份）；sync.rs 纯函数内核
  collect_changeset(复合水位 ts,id)/apply_changeset(_lww)/baseline_export/水位读写；sync_conflicts 表。
- **两轮 QA 修出的关键缺陷**：
  - D1(P0) 回放回环：初版用 `PRAGMA recursive_triggers=OFF` 且想用 `PRAGMA triggers=OFF`，均错——后者
    是**不存在于 SQLite 的 pragma（静默忽略，CLI 实证）**，前者不拦顶层语句点燃触发器 → 回放会写
    sync_log + 覆盖源端 updated_at → 双向无限同步。终版 = **sync_meta.sync_pause 暂停标记守卫**（触发器
    每处副作用带 NOT EXISTS 条件，回放 SyncPauseGuard RAII 置/清标记），测试断言回放零 sync_log 且
    源端 updated_at 保留、守卫释放后恢复记录。
  - D2(P1) rowid≠业务主键（funds/settings/grid_funds/grid_settings/position_daily 5 表）→ row_key 方案；
  - D3(P1) 同毫秒水位丢变更 → (ts,id) 复合水位；
  - D4(P1) 列名注入面 → apply 按 PRAGMA table_info 白名单校验、未知列丢弃。
- 派生/缓存表（nav_history/disclosures/quotes_cache/est_cache/stock_*/index_constituent 等）不参与同步，
  各设备自官方源重拉，避免同步风暴。

## 三、验证与部署
- Rust 全量：158 passed / 0 failed / 3 ignored（macOS --no-default-features）。
- 真实库副本迁移：8.7MB 真库（198 持仓/4746 流水/6.8万 nav_history）.backup 后 init_sync_schema 两遍
  幂等通过；桌面首次启动真实库迁移实机验证 ✅（sync_log/sync_meta/sync_conflicts 建齐、positions
  updated_at 就位、funds 3 触发器、sync_pause 无残留）。
- 桌面部署：`src-tauri/target/release/bundle/macos/FundLens.app` → /Applications，PlistBuddy=2.6.0，
  dst mtime 02:22:36 ≥ src 02:22:25；启动冒烟 pgrep OK。
- 麒麟分支 feat/kylin-v10-aarch64：merge main v2.6.0 → 2a6e2cf（唯一冲突 tauri.conf.json：保留
  $schema config/1、package.version=2.6.0）；cargo check --lib ✅；已推送。**未打麒麟壳**（Docker fl-build
  由用户侧流程做）。

## 四、遗留与后续
- P2 潜伏：cached_with/invalidate 的 8 处 Mutex unwrap 锁中毒防护已随 c158480 加固；sync 同毫秒
  tie-break（D6）未实现，注释文档化（同毫秒同行双写概率极低）。
- CloudBase M1 后续：M2 CloudBase 私有桶通道（上传/下载/watermark 推进）、M3 UI（立即同步/冲突提示）、
  M4 自动备份——已取消客户端加密（用户裁定数据无保密必要，Tencent 私有权限即边界）。
- 报表/总览提速体感请真机验证；若仍有卡顿，下一步查行情 host RTT（代理镜像环境）。
