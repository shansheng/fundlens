// FundLens · Phase-2 CloudBase 同步 M2-a：传输抽象 + 云端同步编排内核（无云可测）
//
// 设计（详见仓库根 perf-cloudbase-v2.6.0-design-2026-09-09.md §4.3 M2）：
// - 云通道 = 「可列清单 / 可上传 / 可下载」的对象容器。本模块只定义这个抽象（SyncTransport）
//   与基于它的编排（push/pull），**不绑定具体后端**；CloudBase 云函数、自建 HTTP relay、
//   本地目录（共享盘 / 网盘同步盘）各自实现 trait 即可，编排与 UI 零改动。
// - 载荷复用 M3 的设备快照 JSONL（sync::snapshot_to_jsonl / parse_snapshot），不引入新格式。
// - 拉取编排：list → 排除本设备与已应用过的 → 每设备只取最新一份 → get → parse → LWW 回放
//   → 记 seen 水位。**关键语义**：设备快照是该设备「全部存活行 + 删除墓碑」的全量状态，
//   因此新版本必然覆盖旧版本 → 每个远端设备只需应用最新一份，传输量最小且天然幂等。
// - seen 水位存 sync_meta(key = `cloud_seen:<设备>`)，值是已应用的最新快照 key。
//   与「按增量水位」不同：这里按**整份快照**去重，所以重复拉取不会重复回放。
// - 配置存 sync_meta（**不是** settings）→ 同步令牌不会随快照上传到云端，各设备独立配置。
use rusqlite::{Connection, Result as SqlResult};
use std::collections::BTreeMap;

/// 条目类型：设备快照（本阶段唯一实现）与整库备份（M4 云上传，后续阶段复用同一 trait）。
pub const KIND_SNAPSHOT: &str = "snapshot";
pub const KIND_BACKUP: &str = "backup";

/// sync_meta 键前缀：某远端设备已应用的最新快照 key。
pub const SEEN_PREFIX: &str = "cloud_seen:";
/// sync_meta 键：最近一次云端推送/拉取时间（供 UI 状态展示）。
pub const META_LAST_PUSH: &str = "cloud_last_push";
pub const META_LAST_PULL: &str = "cloud_last_pull";
/// sync_meta 键：云通道配置（mode / endpoint / token / dir）。
pub const META_MODE: &str = "cloud_mode";
pub const META_ENDPOINT: &str = "cloud_endpoint";
pub const META_TOKEN: &str = "cloud_token";
pub const META_DIR: &str = "cloud_dir";

/// 通道模式：关闭 / 本地目录 / HTTP relay / CloudBase PostgreSQL 直连。
pub const MODE_OFF: &str = "off";
pub const MODE_DIR: &str = "dir";
/// HTTP relay（自建 relay/server.js 或 CloudBase 云函数）。
pub const MODE_CLOUD: &str = "cloud";
/// CloudBase 环境 PG 的 postgREST 直连：无需任何自建服务或云函数，
/// 客户端持环境 API Key 直接读写台账表。当前主力云通道。
pub const MODE_PG: &str = "pg";

/// 时间戳长度：`YYYYMMDD-HHMMSSmmm`（毫秒精度，保证同秒内多次推送不撞名）。
pub const STAMP_LEN: usize = 18;

/// 本地目录通道下，快照存放的子目录名。
pub const DIR_SNAPSHOT_SUBDIR: &str = "snapshots";

// ============================================================
// 条目与传输抽象
// ============================================================

/// 远端一份快照条目的元信息（不含正文）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSnapshot {
    /// 远端唯一键：设备快照为 `{device}/{stamp}.jsonl`
    pub key: String,
    /// 源设备标识（清单未提供时为空串，由 key 解析兜底）
    pub device: String,
    /// 导出时间（`YYYY-MM-DD HH:MM:SS`，由时间戳还原）
    pub at: String,
    /// 字节数
    pub size: i64,
    /// 类型：snapshot / backup
    pub kind: String,
}

/// 一次上传请求。
pub struct PutRequest<'a> {
    /// 远端唯一键
    pub key: &'a str,
    /// 源设备标识
    pub device: &'a str,
    /// 导出时间
    pub at: &'a str,
    /// 条目类型（snapshot / backup）
    pub kind: &'a str,
    /// 正文（快照为 JSONL 文本字节；备份为 .db 二进制）
    pub body: &'a [u8],
}

/// 云通道传输抽象。所有实现都必须满足：
/// - `list` 只返回元信息，不得下载正文（拉取决策依赖它，做到 O(1) 网络往返）；
/// - `put` 幂等：同 key 重复上传等价于覆盖，不产生重复条目；
/// - `get` 按 key 取回**原始字节**（文本/二进制通吃，为 M4 备份上传留出空间）。
pub trait SyncTransport {
    /// 列出远端全部条目。
    fn list(&self) -> Result<Vec<RemoteSnapshot>, String>;
    /// 上传一份正文，返回落位后的条目元信息。
    fn put(&self, req: &PutRequest) -> Result<RemoteSnapshot, String>;
    /// 按 key 取回正文原始字节；key 不存在时报错。
    fn get(&self, key: &str) -> Result<Vec<u8>, String>;
}

// ============================================================
// 命名与时间戳
// ============================================================

/// 当前时刻 → (毫秒精度时间戳, 展示时间)。
///
/// 毫秒精度是必需的：同一设备在同一秒内二次推送若撞名，接收方的
/// 「新版本必然更新」判定会误把第二次当成重复而跳过，导致丢变更。
pub fn now_pair() -> (String, String) {
    let n = chrono::Local::now();
    (
        format!("{}{:03}", n.format("%Y%m%d-%H%M%S"), n.timestamp_subsec_millis()),
        n.format("%Y-%m-%d %H:%M:%S").to_string(),
    )
}

/// 时间戳合法性：长度 18、第 9 位为 `-`、其余全为数字。
pub fn is_valid_stamp(s: &str) -> bool {
    s.len() == STAMP_LEN
        && s.as_bytes()[8] == b'-'
        && s.bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || b.is_ascii_digit())
}

/// 时间戳 → 展示时间（`20260910-221500123` → `2026-09-10 22:15:00`）。
pub fn stamp_to_at(stamp: &str) -> String {
    if stamp.len() < 15 || stamp.as_bytes()[8] != b'-' {
        return stamp.to_string();
    }
    let (d, t) = stamp.split_at(8);
    let t = &t[1..7.min(t.len())];
    if d.len() != 8 || t.len() != 6 {
        return stamp.to_string();
    }
    format!(
        "{}-{}-{} {}:{}:{}",
        &d[0..4],
        &d[4..6],
        &d[6..8],
        &t[0..2],
        &t[2..4],
        &t[4..6]
    )
}

/// 生成设备快照的远端键：`{device}/{stamp}.jsonl`。
///
/// 设备标识用 `dev-<hex>-<hex>`（sync::device_id），不含 `/`，因此键恰好两段。
/// 若外部传入含 `/` 的设备名，键会变成三段 → parse_snapshot_key 拒绝 → 编排侧跳过，
/// 不会静默转换成另一个设备。
pub fn snapshot_key(device: &str, stamp: &str) -> String {
    format!("{device}/{stamp}.jsonl")
}

/// 解析远端键 → (设备, 时间戳)；不合规返回 None。
pub fn parse_snapshot_key(key: &str) -> Option<(String, String)> {
    let (dev, file) = key.rsplit_once('/')?;
    if dev.is_empty() || dev.contains('/') {
        return None;
    }
    let stamp = file.strip_suffix(".jsonl")?;
    if !is_valid_stamp(stamp) {
        return None;
    }
    Some((dev.to_string(), stamp.to_string()))
}

/// 取条目的时间戳（key 优先，回落到 at）。
fn entry_stamp(e: &RemoteSnapshot) -> String {
    parse_snapshot_key(&e.key)
        .map(|(_, s)| s)
        .unwrap_or_default()
}

// ============================================================
// 拉取计划（纯函数，无 IO）
// ============================================================

/// 计算本次要拉取的条目：**每个远端设备最多一份（最新）**，按导出时间升序排列。
///
/// 过滤规则（逐条都是「必须」而非优化）：
/// 1. 只要设备快照（备份不参与合并）；
/// 2. 排除本设备 → 防止把自己的快照套回自己（回环）；
/// 3. key 必须可解析且与声明的设备一致 → 清单被污染时不误判来源；
/// 4. 时间戳 ≤ 已应用水位 → 跳过（重复拉取不重复回放，幂等）。
///
/// 之所以每设备只取最新一份：设备快照是全量状态（存活行 + 墓碑），新版本必然覆盖旧版本，
/// 应用最新一份即可收敛，无需按序回放历史快照。
pub fn plan_pull(
    remote: &[RemoteSnapshot],
    own_device: &str,
    seen: &BTreeMap<String, String>,
) -> Vec<RemoteSnapshot> {
    let mut best: BTreeMap<String, RemoteSnapshot> = BTreeMap::new();
    for r in remote {
        if r.kind != KIND_SNAPSHOT {
            continue;
        }
        let (key_dev, stamp) = match parse_snapshot_key(&r.key) {
            Some(v) => v,
            None => continue,
        };
        // 清单声明的设备与 key 前缀不一致 → 不可信，跳过（不猜）
        if !r.device.is_empty() && r.device != key_dev {
            continue;
        }
        let dev = key_dev;
        if dev == own_device {
            continue;
        }
        if let Some(prev_key) = seen.get(&dev) {
            let prev_stamp = parse_snapshot_key(prev_key)
                .map(|(_, s)| s)
                .unwrap_or_default();
            if !prev_stamp.is_empty() && stamp <= prev_stamp {
                continue;
            }
        }
        match best.get(&dev) {
            Some(cur) if entry_stamp(cur) >= stamp => {}
            _ => {
                best.insert(dev, r.clone());
            }
        }
    }
    let mut out: Vec<RemoteSnapshot> = best.into_values().collect();
    // 升序：多设备汇合时按快照时间推进，结果与顺序无关（LWW 逐行判定），但可复现便于排障
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.key.cmp(&b.key)));
    out
}

// ============================================================
// 已应用水位（sync_meta）
// ============================================================

/// 读取全部 `cloud_seen:<设备>` 水位 → 设备名 → 已应用的最新快照 key。
pub fn seen_keys(conn: &Connection) -> SqlResult<BTreeMap<String, String>> {
    let mut stmt =
        conn.prepare("SELECT key, value FROM sync_meta WHERE key LIKE ?1")?;
    let pattern = format!("{SEEN_PREFIX}%");
    let mut rows = stmt.query([pattern])?;
    let mut out = BTreeMap::new();
    while let Some(r) = rows.next()? {
        let k: String = r.get(0)?;
        let v: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
        if let Some(dev) = k.strip_prefix(SEEN_PREFIX) {
            if !dev.is_empty() {
                out.insert(dev.to_string(), v);
            }
        }
    }
    Ok(out)
}

/// 记录「已应用某设备的最新快照」。
pub fn mark_seen(conn: &Connection, device: &str, key: &str) -> SqlResult<()> {
    crate::sync::write_meta(conn, &format!("{SEEN_PREFIX}{device}"), key)
}

// ============================================================
// 云通道配置（存 sync_meta → 令牌不随快照外传）
// ============================================================

/// 云通道配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudConfig {
    /// off / dir / cloud / pg
    pub mode: String,
    /// cloud：relay 服务地址；pg：CloudBase REST 基址
    /// （形如 `https://<envId>.api.tcloudbasegateway.com/v1/rdb/rest`）
    pub endpoint: String,
    /// cloud：同步令牌；pg：CloudBase 环境 API Key（service_role）
    pub token: String,
    /// 本地目录模式：快照根目录
    pub dir: String,
}

impl CloudConfig {
    /// 是否配置完整、可发起同步。
    pub fn is_ready(&self) -> bool {
        match self.mode.as_str() {
            MODE_DIR => !self.dir.trim().is_empty(),
            // 两种 HTTP 通道的完备条件相同：地址 + 密钥
            MODE_CLOUD | MODE_PG => {
                !self.endpoint.trim().is_empty() && !self.token.trim().is_empty()
            }
            _ => false,
        }
    }

    /// 规范化：未知模式回落 off；各字段去首尾空白。
    pub fn normalized(mut self) -> Self {
        self.mode = match self.mode.trim() {
            MODE_DIR => MODE_DIR.to_string(),
            MODE_CLOUD => MODE_CLOUD.to_string(),
            MODE_PG => MODE_PG.to_string(),
            _ => MODE_OFF.to_string(),
        };
        self.endpoint = self.endpoint.trim().to_string();
        self.token = self.token.trim().to_string();
        self.dir = self.dir.trim().to_string();
        self
    }
}

/// 读取配置（缺失字段回落默认值；容错，不报错）。
pub fn load_config(conn: &Connection) -> CloudConfig {
    let get = |k: &str| -> String {
        crate::sync::read_meta(conn, k)
            .ok()
            .flatten()
            .unwrap_or_default()
    };
    CloudConfig {
        mode: get(META_MODE),
        endpoint: get(META_ENDPOINT),
        token: get(META_TOKEN),
        dir: get(META_DIR),
    }
    .normalized()
}

/// 保存配置（规范化后写入 sync_meta）。
pub fn save_config(conn: &Connection, cfg: &CloudConfig) -> SqlResult<CloudConfig> {
    let c = cfg.clone().normalized();
    crate::sync::write_meta(conn, META_MODE, &c.mode)?;
    crate::sync::write_meta(conn, META_ENDPOINT, &c.endpoint)?;
    crate::sync::write_meta(conn, META_TOKEN, &c.token)?;
    crate::sync::write_meta(conn, META_DIR, &c.dir)?;
    Ok(c)
}

// ============================================================
// 编排：推送 / 拉取
// ============================================================

/// 推送结果。
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushOutcome {
    /// 远端键
    pub key: String,
    /// 快照内变更条数
    pub count: usize,
    /// 快照字节数
    pub size: i64,
    /// 完成时间
    pub at: String,
}

/// 单个远端设备的拉取明细。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullDetail {
    pub device: String,
    pub key: String,
    /// 成功应用条数
    pub applied: usize,
    /// 因本地更新更晚而落冲突表的条数
    pub conflicts: usize,
    /// 该快照的导出时间
    pub at: String,
}

/// 拉取结果。
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullOutcome {
    /// 拉取计划内的快照份数
    pub planned: usize,
    /// 实际下载并回放的份数
    pub pulled: usize,
    /// 因来源是本设备而跳过的份数（回环保护）
    pub skipped_own: usize,
    /// 累计应用条数
    pub applied: usize,
    /// 累计冲突条数
    pub conflicts: usize,
    /// 逐设备明细
    pub details: Vec<PullDetail>,
    /// 完成时间
    pub at: String,
}

/// 推送本设备快照到远端（全量快照，等价于 M3 的「导出」但目标换成云通道）。
pub fn push(conn: &Connection, transport: &dyn SyncTransport) -> Result<PushOutcome, String> {
    let changes = crate::sync::full_device_snapshot(conn).map_err(|e| format!("生成快照失败: {e}"))?;
    let device = crate::sync::device_id(conn).map_err(|e| format!("读取设备标识失败: {e}"))?;
    let (stamp, at) = now_pair();
    let text = crate::sync::snapshot_to_jsonl(&changes, &device, &at);
    let key = snapshot_key(&device, &stamp);
    let entry = transport.put(&PutRequest {
        key: &key,
        device: &device,
        at: &at,
        kind: KIND_SNAPSHOT,
        body: text.as_bytes(),
    })?;
    crate::sync::write_meta(conn, META_LAST_PUSH, &at).map_err(|e| format!("记录推送时间失败: {e}"))?;
    Ok(PushOutcome {
        key: entry.key,
        count: changes.len(),
        size: text.len() as i64,
        at,
    })
}

/// 计算本次拉取计划（**只读**：列远端清单 + 反查水位，不写库、不改远端）。
///
/// 与 `pull_apply` 拆开的唯一原因：命令层需要在「真正写库之前」插入整库自动备份，
/// 而备份会自行获取全局连接 → 不能在已持有连接的闭包里调用（`db::with_conn` 的锁不可重入）。
pub fn pull_plan(
    conn: &Connection,
    transport: &dyn SyncTransport,
) -> Result<Vec<RemoteSnapshot>, String> {
    let own = crate::sync::device_id(conn).map_err(|e| format!("读取设备标识失败: {e}"))?;
    let remote = transport.list()?;
    let seen = seen_keys(conn).map_err(|e| format!("读取同步水位失败: {e}"))?;
    Ok(plan_pull(&remote, &own, &seen))
}

/// 按给定计划下载并回放他设备快照。
///
/// 事务粒度 = 每份快照一个事务：任一设备回放失败只回滚该设备，不影响已应用的其它设备；
/// 同时在事务内开启 `defer_foreign_keys`（与 M3 文件导入同一口径），
/// 避免「被 LWW 跳过的父行」导致子表先落库时误报外键失败。
pub fn pull_apply(
    conn: &Connection,
    transport: &dyn SyncTransport,
    plan: &[RemoteSnapshot],
) -> Result<PullOutcome, String> {
    let own = crate::sync::device_id(conn).map_err(|e| format!("读取设备标识失败: {e}"))?;

    let mut out = PullOutcome {
        planned: plan.len(),
        ..Default::default()
    };

    for item in plan {
        let bytes = transport.get(&item.key)?;
        let text = String::from_utf8(bytes)
            .map_err(|e| format!("远端快照 {} 不是合法 UTF-8: {e}", item.key))?;
        let (header, changes) = crate::sync::parse_snapshot(&text)
            .map_err(|e| format!("远端快照 {} 解析失败: {e}", item.key))?;
        // 来源以**快照头**为准（命名可被外部工具改写，头是我们自己写的权威声明）
        let from = header
            .map(|h| h.device)
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| item.device.clone());
        if from == own {
            out.skipped_own += 1;
            continue;
        }

        conn.execute_batch("BEGIN; PRAGMA defer_foreign_keys = ON;")
            .map_err(|e| format!("开启事务失败: {e}"))?;
        match crate::sync::apply_changeset_lww(conn, &changes, &from) {
            Ok((applied, conflicts)) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| format!("提交事务失败: {e}"))?;
                mark_seen(conn, &from, &item.key)
                    .map_err(|e| format!("记录同步水位失败: {e}"))?;
                out.pulled += 1;
                out.applied += applied;
                out.conflicts += conflicts;
                out.details.push(PullDetail {
                    device: from,
                    key: item.key.clone(),
                    applied,
                    conflicts,
                    at: item.at.clone(),
                });
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(format!("回放远端快照 {} 失败: {e}", item.key));
            }
        }
    }

    let at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if out.pulled > 0 {
        crate::sync::write_meta(conn, META_LAST_PULL, &at)
            .map_err(|e| format!("记录拉取时间失败: {e}"))?;
    }
    out.at = at;
    Ok(out)
}

/// 一步拉取 = `pull_plan` + `pull_apply`（内核可独立测试；命令层需要插入备份时请分别调用）。
pub fn pull(conn: &Connection, transport: &dyn SyncTransport) -> Result<PullOutcome, String> {
    let plan = pull_plan(conn, transport)?;
    pull_apply(conn, transport, &plan)
}

// ============================================================
// 内置实现 1：本地目录（共享盘 / 网盘同步盘 / U 盘）
// ============================================================
//
// 零云依赖的可用通道：把「远端」落到一个目录，多设备各自读写同一份目录即可同步。
// 目录布局：`{root}/snapshots/{device}/{stamp}.jsonl`，与 snapshot_key 的两段结构一一对应。

/// 以本地目录作为「远端」的传输实现。
pub struct DirTransport {
    root: std::path::PathBuf,
}

impl DirTransport {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn snap_root(&self) -> std::path::PathBuf {
        self.root.join(DIR_SNAPSHOT_SUBDIR)
    }

    /// 键 → 磁盘路径；拒绝路径穿越（`..`、绝对路径、多段/反斜杠）。
    fn resolve(&self, key: &str) -> Result<std::path::PathBuf, String> {
        if key.contains('\\') || key.starts_with('/') || key.contains("..") {
            return Err(format!("非法远端键: {key}"));
        }
        let (dev, _) = parse_snapshot_key(key).ok_or_else(|| format!("非法远端键: {key}"))?;
        if dev.contains('.') {
            return Err(format!("非法远端键: {key}"));
        }
        Ok(self.snap_root().join(key))
    }
}

impl SyncTransport for DirTransport {
    fn list(&self) -> Result<Vec<RemoteSnapshot>, String> {
        let root = self.snap_root();
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&root) {
            Ok(rd) => rd,
            Err(_) => return Ok(out), // 目录不存在 = 远端还没有任何快照
        };
        for dev_entry in rd.flatten() {
            if !dev_entry.path().is_dir() {
                continue;
            }
            let dev = dev_entry.file_name().to_string_lossy().to_string();
            let inner = match std::fs::read_dir(dev_entry.path()) {
                Ok(i) => i,
                Err(_) => continue,
            };
            for f in inner.flatten() {
                let name = f.file_name().to_string_lossy().to_string();
                let key = format!("{dev}/{name}");
                let (d, stamp) = match parse_snapshot_key(&key) {
                    Some(v) => v,
                    None => continue,
                };
                out.push(RemoteSnapshot {
                    key,
                    device: d,
                    at: stamp_to_at(&stamp),
                    size: f.metadata().map(|m| m.len() as i64).unwrap_or(0),
                    kind: KIND_SNAPSHOT.to_string(),
                });
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn put(&self, req: &PutRequest) -> Result<RemoteSnapshot, String> {
        if req.kind != KIND_SNAPSHOT {
            return Err(format!("目录通道当前仅支持设备快照，收到类型: {}", req.kind));
        }
        let path = self.resolve(req.key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
        std::fs::write(&path, req.body).map_err(|e| format!("写入快照失败: {e}"))?;
        let stamp = parse_snapshot_key(req.key)
            .map(|(_, s)| s)
            .unwrap_or_default();
        Ok(RemoteSnapshot {
            key: req.key.to_string(),
            device: req.device.to_string(),
            at: stamp_to_at(&stamp),
            size: req.body.len() as i64,
            kind: KIND_SNAPSHOT.to_string(),
        })
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        let path = self.resolve(key)?;
        std::fs::read(&path).map_err(|e| format!("读取远端快照 {key} 失败: {e}"))
    }
}

// ============================================================
// 内置实现 2：HTTP 通道（CloudBase 云函数 / 自建 relay 共用同一协议）
// ============================================================
//
// 协议（服务端实现见仓库根 `cloudbase/functions/sync-relay/`）：
//   所有请求 `POST {endpoint}?action=<list|put|get>`，头 `x-sync-token: <令牌>`。
//   - list：无正文；响应 JSON `{"ok":true,"items":[RemoteSnapshot...]}`
//   - put ：查询串带 key/device/at/kind，**正文即原始字节**；响应 JSON `{"ok":true,"item":{...}}`
//   - get ：查询串带 key；响应正文即原始字节
//   失败：HTTP 4xx/5xx + JSON `{"ok":false,"error":"..."}`
//
// 为什么用「查询串带元信息 + 正文原始字节」而不是把正文塞进 JSON：
// 快照实测 3.8MB、M4 整库备份 11MB，JSON+base64 会再多 33% 且要两次内存拷贝；
// 原始字节直接走 HTTP body，文本与二进制通吃，M4 备份上传无需改协议。

/// HTTP 通道请求超时（秒）。快照数 MB 级，给足余量。
const HTTP_TIMEOUT_SECS: u64 = 120;

/// 以 HTTP relay 为后端的传输实现。
pub struct HttpTransport {
    endpoint: String,
    token: String,
    client: reqwest::blocking::Client,
}

impl HttpTransport {
    /// 构造；endpoint 不得自带查询串（会与 `?action=` 冲突）。
    pub fn new(endpoint: &str, token: &str) -> Result<Self, String> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        if endpoint.is_empty() {
            return Err("云通道地址为空".to_string());
        }
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Err(format!("云通道地址必须以 http(s):// 开头: {endpoint}"));
        }
        if endpoint.contains('?') {
            return Err(format!("云通道地址不应包含查询串: {endpoint}"));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))?;
        Ok(Self {
            endpoint,
            token: token.to_string(),
            client,
        })
    }

    fn url_with(&self, action: &str, extra: &[(&str, &str)]) -> String {
        let mut u = format!("{}?action={}", self.endpoint, encode_component(action));
        for (k, v) in extra {
            u.push('&');
            u.push_str(k);
            u.push('=');
            u.push_str(&encode_component(v));
        }
        u
    }

    fn post(&self, url: &str, body: Vec<u8>) -> Result<reqwest::blocking::Response, String> {
        self.client
            .post(url)
            .header("x-sync-token", &self.token)
            .header("content-type", "application/octet-stream")
            .body(body)
            .send()
            .map_err(|e| format!("请求云通道失败: {e}"))
    }

    /// 解析统一响应信封；`ok=false` 或非 2xx 一律转成可读错误。
    fn envelope(&self, status: reqwest::StatusCode, text: &str) -> Result<Envelope, String> {
        let env: Envelope = serde_json::from_str(text).map_err(|e| {
            format!("云通道响应不是合法 JSON (HTTP {status}): {e}")
        })?;
        if !env.ok {
            let msg = if env.error.is_empty() {
                format!("HTTP {status}")
            } else {
                env.error
            };
            return Err(format!("云通道返回错误: {msg}"));
        }
        if !status.is_success() {
            return Err(format!("云通道返回 HTTP {status}"));
        }
        Ok(env)
    }
}

/// 统一响应信封。
#[derive(Debug, serde::Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    error: String,
    #[serde(default)]
    items: Vec<RemoteSnapshot>,
    #[serde(default)]
    item: Option<RemoteSnapshot>,
}

/// RFC 3986 未保留字符集之外的字节做百分号编码（不引第三方 url 依赖）。
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl SyncTransport for HttpTransport {
    fn list(&self) -> Result<Vec<RemoteSnapshot>, String> {
        let url = self.url_with("list", &[]);
        let resp = self.post(&url, Vec::new())?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| format!("读取云通道响应失败: {e}"))?;
        Ok(self.envelope(status, &text)?.items)
    }

    fn put(&self, req: &PutRequest) -> Result<RemoteSnapshot, String> {
        let url = self.url_with(
            "put",
            &[
                ("key", req.key),
                ("device", req.device),
                ("at", req.at),
                ("kind", req.kind),
            ],
        );
        let resp = self.post(&url, req.body.to_vec())?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| format!("读取云通道响应失败: {e}"))?;
        let env = self.envelope(status, &text)?;
        // 服务端未回条目元信息时按请求本身合成（不影响后续判定：判定以 key 为准）
        Ok(env.item.unwrap_or_else(|| RemoteSnapshot {
            key: req.key.to_string(),
            device: req.device.to_string(),
            at: req.at.to_string(),
            size: req.body.len() as i64,
            kind: req.kind.to_string(),
        }))
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        let url = self.url_with("get", &[("key", key)]);
        let resp = self.post(&url, Vec::new())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            // 服务端出错时会回 JSON 信封，尽量把 error 字段挖出来
            let msg = serde_json::from_str::<Envelope>(&text)
                .ok()
                .filter(|e| !e.error.is_empty())
                .map(|e| e.error)
                .unwrap_or(text);
            return Err(format!("下载远端条目 {key} 失败 (HTTP {status}): {msg}"));
        }
        let bytes = resp
            .bytes()
            .map_err(|e| format!("读取远端条目 {key} 正文失败: {e}"))?;
        Ok(bytes.to_vec())
    }
}

// ============================================================
// 内置实现 3：CloudBase PostgreSQL（postgREST 直连）
// ============================================================

/// 同步表名。表结构见 `cloudbase/migrations/*_fl_sync_store.sql`：
/// `(device, stamp)` 为主键，`kind` 区分快照/备份，`body` 存正文文本。
/// 该表 RLS 已开启且**不放通任何策略** → anon/authenticated 一律拒绝，
/// 只有 service_role（环境 API Key）可读写，即「API Key 本身是唯一密钥」。
pub const TABLE_PG: &str = "fl_sync";

/// CloudBase PostgreSQL 通道：客户端直连环境 PG 的 REST 接口。
///
/// 与 `HttpTransport`（relay）的分工：
/// - relay 需要一个常驻服务/云函数，协议自定义、可承载二进制；
/// - 本通道**零服务端部署**，直接用环境 API Key 读写 PG，代价是鉴权边界下沉到
///   数据库（API Key 泄露即数据泄露），且当前只承载文本载荷（快照为 JSONL 文本）。
pub struct PgRestTransport {
    base: String,
    token: String,
    client: reqwest::blocking::Client,
}

impl PgRestTransport {
    /// 构造；base 形如 `https://<envId>.api.tcloudbasegateway.com/v1/rdb/rest`。
    pub fn new(base: &str, token: &str) -> Result<Self, String> {
        let base = base.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err("CloudBase REST 基址为空".to_string());
        }
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(format!("CloudBase REST 基址必须以 http(s):// 开头: {base}"));
        }
        if token.trim().is_empty() {
            return Err("CloudBase API Key 为空".to_string());
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))?;
        Ok(Self {
            base,
            token: token.trim().to_string(),
            client,
        })
    }

    /// 拼 `{base}/{table}?k=v&...`；值统一百分号编码（`eq.` 中的点属未保留字符，不受影响）。
    fn table_url(&self, query: &[(&str, String)]) -> String {
        let mut u = format!("{}/{}", self.base, TABLE_PG);
        for (i, (k, v)) in query.iter().enumerate() {
            u.push(if i == 0 { '?' } else { '&' });
            u.push_str(k);
            u.push('=');
            u.push_str(&encode_component(v));
        }
        u
    }

    /// 带鉴权的 GET，返回响应体文本；非 2xx 转可读错误。
    fn get_json(&self, url: &str) -> Result<String, String> {
        let resp = self
            .client
            .get(url)
            .header("authorization", format!("Bearer {}", self.token))
            .header("accept", "application/json")
            .send()
            .map_err(|e| format!("请求 CloudBase 失败: {e}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| format!("读取 CloudBase 响应失败: {e}"))?;
        if !status.is_success() {
            return Err(format!("CloudBase 返回 HTTP {status}: {}", pg_error(&text)));
        }
        Ok(text)
    }

    /// 删除某设备在远端的全部条目，返回删除行数。
    ///
    /// 不在 `SyncTransport` 契约内（快照是追加式的，正常同步不删远端），
    /// 仅用于维护与端到端测试善后，避免测试数据污染真实同步清单。
    pub fn delete_device(&self, device: &str) -> Result<u64, String> {
        let url = self.table_url(&[
            ("device", format!("eq.{device}")),
            ("select", "device".to_string()),
        ]);
        let resp = self
            .client
            .delete(&url)
            .header("authorization", format!("Bearer {}", self.token))
            .header("prefer", "return=representation")
            .send()
            .map_err(|e| format!("请求 CloudBase 失败: {e}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| format!("读取 CloudBase 响应失败: {e}"))?;
        if !status.is_success() {
            return Err(format!("CloudBase 返回 HTTP {status}: {}", pg_error(&text)));
        }
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&text).map_err(|e| format!("删除响应不是合法 JSON: {e}"))?;
        Ok(rows.len() as u64)
    }
}

/// CloudBase / postgREST 错误体 → 可读文案（优先 message(+code)，兜底截断原文）。
fn pg_error(text: &str) -> String {
    #[derive(serde::Deserialize)]
    struct ErrBody {
        #[serde(default)]
        code: String,
        #[serde(default)]
        message: String,
    }
    if let Ok(e) = serde_json::from_str::<ErrBody>(text) {
        if !e.message.is_empty() {
            return if e.code.is_empty() {
                e.message
            } else {
                format!("{} ({})", e.message, e.code)
            };
        }
        if !e.code.is_empty() {
            return e.code;
        }
    }
    let t = text.trim();
    if t.is_empty() {
        "（无响应体）".to_string()
    } else {
        t.chars().take(300).collect()
    }
}

impl SyncTransport for PgRestTransport {
    fn list(&self) -> Result<Vec<RemoteSnapshot>, String> {
        let url = self.table_url(&[
            ("select", "device,stamp,kind,size".to_string()),
            ("kind", format!("eq.{KIND_SNAPSHOT}")),
            ("order", "stamp.asc".to_string()),
        ]);
        let text = self.get_json(&url)?;
        #[derive(serde::Deserialize)]
        struct Row {
            device: String,
            stamp: String,
            kind: String,
            #[serde(default)]
            size: i64,
        }
        let rows: Vec<Row> = serde_json::from_str(&text)
            .map_err(|e| format!("CloudBase 清单响应不是合法 JSON: {e}"))?;
        Ok(rows
            .into_iter()
            .map(|r| RemoteSnapshot {
                key: snapshot_key(&r.device, &r.stamp),
                at: stamp_to_at(&r.stamp),
                device: r.device,
                size: r.size,
                kind: r.kind,
            })
            .collect())
    }

    fn put(&self, req: &PutRequest) -> Result<RemoteSnapshot, String> {
        // 键是权威：device/stamp 一律从 key 解析，避免与调用方字段不一致
        let (device, stamp) = parse_snapshot_key(req.key)
            .ok_or_else(|| format!("非法远端键（应为 设备/时间戳.jsonl）: {}", req.key))?;
        let body = std::str::from_utf8(req.body).map_err(|_| {
            "CloudBase PostgreSQL 通道当前仅支持文本载荷（快照为 JSONL 文本）；\
             二进制备份请改用本地目录或 relay 通道"
                .to_string()
        })?;
        let payload = serde_json::json!({
            "device": device,
            "stamp": stamp,
            "kind": req.kind,
            "size": req.body.len() as i64,
            "body": body,
        });
        let url = self.table_url(&[("select", "device,stamp,kind,size".to_string())]);
        let resp = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.token))
            .header("content-type", "application/json")
            // 主键冲突即覆盖 → put 满足「同 key 重复上传等价于覆盖」的幂等契约
            .header("prefer", "resolution=merge-duplicates,return=representation")
            .body(serde_json::to_vec(&payload).map_err(|e| format!("序列化上传载荷失败: {e}"))?)
            .send()
            .map_err(|e| format!("请求 CloudBase 失败: {e}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| format!("读取 CloudBase 响应失败: {e}"))?;
        if !status.is_success() {
            return Err(format!("CloudBase 返回 HTTP {status}: {}", pg_error(&text)));
        }
        // 服务端未回条目时按请求合成（后续判定以 key 为准，不影响正确性）
        Ok(RemoteSnapshot {
            key: req.key.to_string(),
            device,
            at: req.at.to_string(),
            size: req.body.len() as i64,
            kind: req.kind.to_string(),
        })
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        let (device, stamp) = parse_snapshot_key(key)
            .ok_or_else(|| format!("非法远端键（应为 设备/时间戳.jsonl）: {key}"))?;
        let url = self.table_url(&[
            ("select", "body".to_string()),
            ("device", format!("eq.{device}")),
            ("stamp", format!("eq.{stamp}")),
            ("limit", "1".to_string()),
        ]);
        let text = self.get_json(&url)?;
        #[derive(serde::Deserialize)]
        struct Row {
            body: String,
        }
        let rows: Vec<Row> = serde_json::from_str(&text)
            .map_err(|e| format!("CloudBase 条目响应不是合法 JSON: {e}"))?;
        rows.into_iter()
            .next()
            .map(|r| r.body.into_bytes())
            .ok_or_else(|| format!("远端条目不存在: {key}"))
    }
}

/// 按配置构造传输实现；配置不完整/模式未知时报错（调用方据此提示用户去配置）。
pub fn transport_from_config(cfg: &CloudConfig) -> Result<Box<dyn SyncTransport>, String> {
    let c = cfg.clone().normalized();
    match c.mode.as_str() {
        MODE_DIR => {
            if c.dir.is_empty() {
                return Err("尚未设置本地目录通道的目录".to_string());
            }
            Ok(Box::new(DirTransport::new(&c.dir)))
        }
        MODE_CLOUD => {
            if c.endpoint.is_empty() {
                return Err("尚未设置云通道地址".to_string());
            }
            if c.token.is_empty() {
                return Err("尚未设置云通道令牌".to_string());
            }
            Ok(Box::new(HttpTransport::new(&c.endpoint, &c.token)?))
        }
        MODE_PG => {
            if c.endpoint.is_empty() {
                return Err("尚未设置 CloudBase REST 基址".to_string());
            }
            if c.token.is_empty() {
                return Err("尚未设置 CloudBase API Key".to_string());
            }
            Ok(Box::new(PgRestTransport::new(&c.endpoint, &c.token)?))
        }
        _ => Err("云通道未启用".to_string()),
    }
}

// ============================================================
// 内置实现 3：内存通道（单测用，等价于一个空云端）
// ============================================================

#[cfg(test)]
#[derive(Default)]
pub struct MemTransport {
    items: std::cell::RefCell<BTreeMap<String, (RemoteSnapshot, Vec<u8>)>>,
}

#[cfg(test)]
impl MemTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// 远端现有条目数（断言用）。
    pub fn len(&self) -> usize {
        self.items.borrow().len()
    }

    /// 删除某条目，模拟远端被清理。
    pub fn drop_key(&self, key: &str) {
        self.items.borrow_mut().remove(key);
    }
}

#[cfg(test)]
impl SyncTransport for MemTransport {
    fn list(&self) -> Result<Vec<RemoteSnapshot>, String> {
        Ok(self
            .items
            .borrow()
            .values()
            .map(|(m, _)| m.clone())
            .collect())
    }

    fn put(&self, req: &PutRequest) -> Result<RemoteSnapshot, String> {
        let stamp = parse_snapshot_key(req.key)
            .map(|(_, s)| s)
            .unwrap_or_default();
        let entry = RemoteSnapshot {
            key: req.key.to_string(),
            device: req.device.to_string(),
            at: stamp_to_at(&stamp),
            size: req.body.len() as i64,
            kind: req.kind.to_string(),
        };
        self.items
            .borrow_mut()
            .insert(req.key.to_string(), (entry.clone(), req.body.to_vec()));
        Ok(entry)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        self.items
            .borrow()
            .get(key)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| format!("远端条目不存在: {key}"))
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync;

    /// 建一个带同步 schema 的内存库（复用 sync.rs 的最小表集合 + 生产迁移）。
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        sync::tests::setup(&conn);
        conn
    }

    fn entry(key: &str, device: &str, at: &str) -> RemoteSnapshot {
        RemoteSnapshot {
            key: key.to_string(),
            device: device.to_string(),
            at: at.to_string(),
            size: 1,
            kind: KIND_SNAPSHOT.to_string(),
        }
    }

    // ① 时间戳：毫秒精度、长度固定、还原展示时间正确；非法戳被拒。
    #[test]
    fn stamp_format_and_roundtrip() {
        let (stamp, at) = now_pair();
        assert_eq!(stamp.len(), STAMP_LEN, "时间戳长度应为 18: {stamp}");
        assert!(is_valid_stamp(&stamp), "自产时间戳必须合法: {stamp}");
        assert_eq!(stamp_to_at(&stamp), at, "stamp 还原应等于展示时间");
        assert_eq!(stamp_to_at("20260910-221500123"), "2026-09-10 22:15:00");

        assert!(!is_valid_stamp("20260910-221500"), "缺毫秒 → 拒");
        assert!(!is_valid_stamp("2026091a-221500123"), "非数字 → 拒");
        assert!(!is_valid_stamp(""), "空 → 拒");
    }

    // ② 键命名：两段结构、可往返；含 `/` 的设备名/路径穿越键一律解析失败。
    #[test]
    fn snapshot_key_roundtrip_and_rejects_unsafe() {
        let key = snapshot_key("dev-abc-1", "20260910-221500123");
        assert_eq!(key, "dev-abc-1/20260910-221500123.jsonl");
        assert_eq!(
            parse_snapshot_key(&key),
            Some(("dev-abc-1".to_string(), "20260910-221500123".to_string()))
        );

        assert_eq!(parse_snapshot_key("a/b/c.jsonl"), None, "三段 → 拒");
        assert_eq!(parse_snapshot_key("dev/../etc.jsonl"), None, "穿越 → 拒");
        assert_eq!(parse_snapshot_key("dev/20260910-221500123.txt"), None, "后缀 → 拒");
        assert_eq!(parse_snapshot_key("dev/bad.jsonl"), None, "坏戳 → 拒");
    }

    // ③ 拉取计划：排除本设备、排除已应用水位、每设备只留最新一份、升序输出。
    #[test]
    fn plan_pull_picks_newest_per_device_and_honors_seen() {
        let remote = vec![
            entry("dev-a/20260910-100000000.jsonl", "dev-a", "2026-09-10 10:00:00"),
            entry("dev-a/20260910-120000000.jsonl", "dev-a", "2026-09-10 12:00:00"),
            entry("dev-a/20260910-110000000.jsonl", "dev-a", "2026-09-10 11:00:00"),
            entry("dev-b/20260910-090000000.jsonl", "dev-b", "2026-09-10 09:00:00"),
            entry("dev-me/20260910-130000000.jsonl", "dev-me", "2026-09-10 13:00:00"),
        ];
        let plan = plan_pull(&remote, "dev-me", &BTreeMap::new());
        assert_eq!(plan.len(), 2, "本设备应被排除，另两设备各取一份");
        assert_eq!(plan[0].key, "dev-b/20260910-090000000.jsonl", "升序：dev-b 更早");
        assert_eq!(plan[1].key, "dev-a/20260910-120000000.jsonl", "dev-a 只留最新一份");

        // 已应用 dev-a 的最新一份 → 只剩 dev-b
        let mut seen = BTreeMap::new();
        seen.insert("dev-a".to_string(), "dev-a/20260910-120000000.jsonl".to_string());
        let plan2 = plan_pull(&remote, "dev-me", &seen);
        assert_eq!(plan2.len(), 1);
        assert_eq!(plan2[0].device, "dev-b");

        // 清单里声明设备与键前缀不符 → 不可信，跳过
        let poisoned = vec![entry("dev-x/20260910-100000000.jsonl", "dev-y", "2026-09-10 10:00:00")];
        assert!(plan_pull(&poisoned, "dev-me", &BTreeMap::new()).is_empty());

        // 备份条目不参与合并
        let mut backup = entry("dev-c/20260910-100000000.jsonl", "dev-c", "2026-09-10 10:00:00");
        backup.kind = KIND_BACKUP.to_string();
        assert!(plan_pull(&[backup], "dev-me", &BTreeMap::new()).is_empty());
    }

    // ④ 配置：规范化 + 可完整可用判定；令牌存 sync_meta 而非 settings。
    #[test]
    fn config_normalizes_and_stays_out_of_synced_tables() {
        let conn = db();

        let saved = save_config(
            &conn,
            &CloudConfig {
                mode: "  cloud ".into(),
                endpoint: " https://x.example/relay ".into(),
                token: " secret ".into(),
                dir: "  ".into(),
            },
        )
        .unwrap();
        assert_eq!(saved.mode, MODE_CLOUD);
        assert_eq!(saved.endpoint, "https://x.example/relay");
        assert_eq!(saved.token, "secret");
        assert!(saved.is_ready(), "endpoint + token 齐备即可用");

        let loaded = load_config(&conn);
        assert_eq!(loaded, saved, "往返一致");

        // 未知模式回落 off
        save_config(
            &conn,
            &CloudConfig {
                mode: "weird".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(load_config(&conn).mode, MODE_OFF);
        assert!(!load_config(&conn).is_ready(), "off 一律不可用");

        // 令牌必须落在 sync_meta（不参与同步的表），绝不能进 settings（参与同步 → 会被上传）
        let in_settings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM settings WHERE key LIKE 'cloud%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(in_settings, 0, "云通道凭据不得写入 settings（settings 参与同步）");
        let in_meta: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_meta WHERE key = ?1",
                [META_TOKEN],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(in_meta, 1);
    }

    // ⑤ 目录通道：上传 → 列表 → 取回，字节一致；缺键报错；非法键拒绝。
    #[test]
    fn dir_transport_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("fundlens-cloud-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let t = DirTransport::new(&tmp);
        assert!(t.list().unwrap().is_empty(), "空目录 → 无条目");

        let req = PutRequest {
            key: "dev-a/20260910-221500123.jsonl",
            device: "dev-a",
            at: "2026-09-10 22:15:00",
            kind: KIND_SNAPSHOT,
            body: b"{\"fl_sync\":1}\n",
        };
        let e = t.put(&req).unwrap();
        assert_eq!(e.key, req.key);
        assert_eq!(e.at, "2026-09-10 22:15:00", "展示时间由时间戳还原");
        assert_eq!(e.size, 14);
        assert_eq!(e.size, req.body.len() as i64, "条目大小应等于正文字节数");

        let listed = t.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].device, "dev-a");
        assert_eq!(t.get(req.key).unwrap(), req.body, "取回字节一致");

        assert!(t.resolve("dev-a/../../etc/passwd").is_err(), "路径穿越必须拒绝");
        assert!(t.get("dev-a/20260910-000000000.jsonl").is_err(), "缺键应报错");

        // 非快照类型在目录通道被明确拒绝（而不是静默写坏布局）
        let bak = PutRequest {
            kind: KIND_BACKUP,
            ..req
        };
        assert!(t.put(&bak).is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ⑥ 端到端：A 推 → B 拉，B 拿到 A 的全部行；再次拉取不重复回放（幂等）。
    #[test]
    fn push_then_pull_end_to_end_and_idempotent() {
        let cloud = MemTransport::new();
        let a = db();
        let b = db();

        a.execute(
            "INSERT INTO funds(code,name,platform) VALUES('000001','华夏成长','alipay')",
            [],
        )
        .unwrap();
        a.execute("INSERT INTO positions(fund_code,shares) VALUES('000001',100.0)", [])
            .unwrap();

        let pushed = push(&a, &cloud).unwrap();
        assert!(pushed.count >= 2, "快照应含 funds + positions: {}", pushed.count);
        assert_eq!(cloud.len(), 1, "云端落一份");

        let pulled = pull(&b, &cloud).unwrap();
        assert_eq!(pulled.pulled, 1);
        assert_eq!(pulled.applied, pushed.count, "B 应逐条应用 A 的全部变更");
        let name: String = b
            .query_row("SELECT name FROM funds WHERE code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "华夏成长", "A 的数据应落到 B");

        // 幂等：水位已推进 → 计划为空，不再回放
        let again = pull(&b, &cloud).unwrap();
        assert_eq!(again.planned, 0, "已应用过的快照不再进入计划");
        assert_eq!(again.applied, 0, "重复拉取不重复回放");

        // A 自己拉：本设备被排除，绝不回环
        let self_pull = pull(&a, &cloud).unwrap();
        assert_eq!(self_pull.planned, 0);
        assert_eq!(self_pull.applied, 0);
    }

    // ⑦ 增删都被同步：A 改一行 + 删一行 → B 拉到后状态与 A 一致。
    #[test]
    fn pull_propagates_update_and_delete() {
        let cloud = MemTransport::new();
        let a = db();
        let b = db();

        a.execute(
            "INSERT INTO funds(code,name,platform) VALUES('000001','旧名','alipay')",
            [],
        )
        .unwrap();
        a.execute(
            "INSERT INTO funds(code,name,platform) VALUES('000002','要删的','alipay')",
            [],
        )
        .unwrap();
        push(&a, &cloud).unwrap();
        pull(&b, &cloud).unwrap();

        // 第二轮：改 000001 的名字、删掉 000002
        std::thread::sleep(std::time::Duration::from_millis(5));
        a.execute("UPDATE funds SET name='新名' WHERE code='000001'", [])
            .unwrap();
        a.execute("DELETE FROM funds WHERE code='000002'", []).unwrap();
        push(&a, &cloud).unwrap();
        let out = pull(&b, &cloud).unwrap();
        assert_eq!(out.pulled, 1, "应拉到第二轮新快照");

        let name: String = b
            .query_row("SELECT name FROM funds WHERE code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "新名", "改名应同步");
        let gone: i64 = b
            .query_row("SELECT COUNT(*) FROM funds WHERE code='000002'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(gone, 0, "删除墓碑应生效");
        assert_eq!(cloud.len(), 2, "两轮各一份快照");
        assert!(push(&a, &cloud).unwrap().key > String::new());
    }

    // ⑧ 多设备汇合：C 同时拉到 A 与 B 的快照，两份都被应用（LWW 逐行判定，互不阻塞）。
    #[test]
    fn pull_merges_multiple_devices() {
        let cloud = MemTransport::new();
        let a = db();
        let b = db();
        let c = db();

        a.execute("INSERT INTO funds(code,name,platform) VALUES('A001','A基金','alipay')", [])
            .unwrap();
        b.execute("INSERT INTO funds(code,name,platform) VALUES('B001','B基金','alipay')", [])
            .unwrap();
        push(&a, &cloud).unwrap();
        push(&b, &cloud).unwrap();

        let out = pull(&c, &cloud).unwrap();
        assert_eq!(out.planned, 2, "两个他设备各一份");
        assert_eq!(out.pulled, 2);
        assert_eq!(out.details.len(), 2, "逐设备明细两条");
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM funds", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "A、B 的数据都应落地");
        // 水位按设备分别记录
        assert_eq!(seen_keys(&c).unwrap().len(), 2);
    }

    // ⑨ 编排放进真实事务：回放失败必须整体回滚，不留半截状态。
    #[test]
    fn pull_rolls_back_on_failure() {
        let cloud = MemTransport::new();
        let a = db();
        let b = db();
        a.execute("INSERT INTO funds(code,name,platform) VALUES('000001','x','alipay')", [])
            .unwrap();
        push(&a, &cloud).unwrap();

        // 篡改快照：在合法行之后追加一条坏行 → parse_snapshot 整体拒绝（不做部分解析）
        let key = cloud.list().unwrap()[0].key.clone();
        let dev_a = sync::device_id(&a).unwrap();
        let mut body = cloud.get(&key).unwrap();
        body.extend_from_slice(b"{not json}\n");
        cloud
            .put(&PutRequest {
                key: &key,
                device: &dev_a,
                at: "2026-09-10 22:15:00",
                kind: KIND_SNAPSHOT,
                body: &body,
            })
            .unwrap();

        let err = pull(&b, &cloud).unwrap_err();
        assert!(err.contains("解析失败"), "应报解析失败: {err}");
        let n: i64 = b
            .query_row("SELECT COUNT(*) FROM funds", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "坏快照不得写入任何行");
        assert!(seen_keys(&b).unwrap().is_empty(), "失败不应推进水位");
    }

    // ---- HTTP 通道：本地 mock relay（真实走 TCP，验证协议客户端）----

    /// 极简 relay 服务端：只实现 cloud.rs 定义的协议，用于验证客户端而不依赖任何云环境。
    fn spawn_relay(token: &str) -> (String, std::sync::Arc<std::sync::Mutex<BTreeMap<String, (RemoteSnapshot, Vec<u8>)>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let addr = listener.local_addr().unwrap();
        let store: std::sync::Arc<std::sync::Mutex<BTreeMap<String, (RemoteSnapshot, Vec<u8>)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let st = store.clone();
        let tk = token.to_string();

        fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
            hay.windows(needle.len()).position(|w| w == needle)
        }
        fn read_req(stream: &mut std::net::TcpStream) -> Option<(String, Vec<u8>)> {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            let head_end = loop {
                let n = stream.read(&mut tmp).ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = find(&buf, b"\r\n\r\n") {
                    break p;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let len = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.eq_ignore_ascii_case("content-length") {
                        v.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let mut body = buf[head_end + 4..].to_vec();
            while body.len() < len {
                let n = stream.read(&mut tmp).ok()?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&tmp[..n]);
            }
            body.truncate(len);
            Some((head, body))
        }
        fn write_resp(stream: &mut std::net::TcpStream, status: u16, reason: &str, ct: &str, body: &[u8]) {
            let head = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
        fn param(target: &str, name: &str) -> Option<String> {
            let q = target.split_once('?')?.1;
            for pair in q.split('&') {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                if k == name {
                    let mut out = String::new();
                    let bytes = v.as_bytes();
                    let mut i = 0;
                    while i < bytes.len() {
                        if bytes[i] == b'%' && i + 2 < bytes.len() {
                            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                            out.push(u8::from_str_radix(hex, 16).ok()? as char);
                            i += 3;
                        } else {
                            out.push(bytes[i] as char);
                            i += 1;
                        }
                    }
                    return Some(out);
                }
            }
            None
        }

        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                let Some((head, body)) = read_req(&mut stream) else {
                    continue;
                };
                let target = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let got = head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.eq_ignore_ascii_case("x-sync-token") {
                        Some(v.trim().to_string())
                    } else {
                        None
                    }
                });
                if got.as_deref() != Some(tk.as_str()) {
                    write_resp(&mut stream, 401, "Unauthorized", "application/json", br#"{"ok":false,"error":"bad token"}"#);
                    continue;
                }
                match param(&target, "action").unwrap_or_default().as_str() {
                    "list" => {
                        let items: Vec<serde_json::Value> = st
                            .lock()
                            .unwrap()
                            .values()
                            .map(|(m, _)| serde_json::to_value(m).unwrap())
                            .collect();
                        let s = serde_json::json!({ "ok": true, "items": items }).to_string();
                        write_resp(&mut stream, 200, "OK", "application/json", s.as_bytes());
                    }
                    "put" => {
                        let key = param(&target, "key").unwrap_or_default();
                        let stamp = parse_snapshot_key(&key)
                            .map(|(_, s)| s)
                            .unwrap_or_default();
                        let e = RemoteSnapshot {
                            key: key.clone(),
                            device: param(&target, "device").unwrap_or_default(),
                            at: stamp_to_at(&stamp),
                            size: body.len() as i64,
                            kind: param(&target, "kind").unwrap_or_default(),
                        };
                        st.lock().unwrap().insert(key, (e.clone(), body));
                        let s = serde_json::json!({ "ok": true, "item": e }).to_string();
                        write_resp(&mut stream, 200, "OK", "application/json", s.as_bytes());
                    }
                    "get" => {
                        let key = param(&target, "key").unwrap_or_default();
                        let found = st.lock().unwrap().get(&key).cloned();
                        match found {
                            Some((_, bytes)) => {
                                write_resp(&mut stream, 200, "OK", "application/octet-stream", &bytes)
                            }
                            None => write_resp(
                                &mut stream,
                                404,
                                "Not Found",
                                "application/json",
                                br#"{"ok":false,"error":"not found"}"#,
                            ),
                        }
                    }
                    other => {
                        let s = format!(r#"{{"ok":false,"error":"unknown action {other}"}}"#);
                        write_resp(&mut stream, 400, "Bad Request", "application/json", s.as_bytes());
                    }
                }
            }
        });
        (format!("http://{addr}/relay"), store)
    }

    // ⑩ HTTP 通道：list/put/get 往返、令牌校验、缺键报错；地址不合法在构造期即拒。
    #[test]
    fn http_transport_roundtrip_against_mock_relay() {
        let (endpoint, _store) = spawn_relay("tok-123");
        let t = HttpTransport::new(&endpoint, "tok-123").unwrap();

        assert!(t.list().unwrap().is_empty(), "空远端 → 无条目");

        let req = PutRequest {
            key: "dev-a/20260910-221500123.jsonl",
            device: "dev-a",
            at: "2026-09-10 22:15:00",
            kind: KIND_SNAPSHOT,
            body: b"{\"fl_sync\":1}\n",
        };
        let e = t.put(&req).unwrap();
        assert_eq!(e.key, req.key);
        assert_eq!(e.size, 14);
        assert_eq!(t.get(req.key).unwrap(), req.body, "取回字节一致");

        let listed = t.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].device, "dev-a");
        assert_eq!(listed[0].kind, KIND_SNAPSHOT);

        // 令牌错误 → 明确报错（不能静默当成空远端）
        let bad = HttpTransport::new(&endpoint, "wrong").unwrap();
        assert!(bad.list().unwrap_err().contains("bad token"));

        // 缺键 → 报错并在消息里带上 HTTP 状态
        let err = t.get("dev-a/20260910-000000000.jsonl").unwrap_err();
        assert!(err.contains("404"), "应带上状态码: {err}");

        // 构造期校验
        assert!(HttpTransport::new("", "t").is_err());
        assert!(HttpTransport::new("ftp://x/y", "t").is_err());
        assert!(HttpTransport::new("https://x/y?a=1", "t").is_err());
        assert!(HttpTransport::new("http://127.0.0.1:1/relay", "t").is_ok());
    }

    // ⑪ 端到端走 HTTP：A 推 → B 拉，数据落地；二次拉取幂等。
    #[test]
    fn http_end_to_end_push_pull() {
        let (endpoint, _store) = spawn_relay("tok-e2e");
        let net = HttpTransport::new(&endpoint, "tok-e2e").unwrap();

        let a = db();
        let b = db();
        a.execute(
            "INSERT INTO funds(code,name,platform) VALUES('000001','华夏成长','alipay')",
            [],
        )
        .unwrap();
        a.execute("INSERT INTO positions(fund_code,shares) VALUES('000001',100.0)", [])
            .unwrap();

        let pushed = push(&a, &net).unwrap();
        assert!(pushed.key.ends_with(".jsonl"));

        let pulled = pull(&b, &net).unwrap();
        assert_eq!(pulled.pulled, 1);
        assert_eq!(pulled.applied, pushed.count);
        let shares: f64 = b
            .query_row("SELECT shares FROM positions WHERE fund_code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(shares, 100.0, "A 的持仓应经 HTTP 落到 B");

        let again = pull(&b, &net).unwrap();
        assert_eq!(again.planned, 0, "水位已推进 → 幂等");
    }

    // ⑫ 配置 → 传输实现的路由：dir/cloud 各自可用，off 与缺令牌明确报错。
    #[test]
    fn transport_from_config_routes_by_mode() {
        let tmp = std::env::temp_dir().join(format!("fundlens-cfg-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        let dir_cfg = CloudConfig {
            mode: MODE_DIR.into(),
            dir: tmp.to_string_lossy().to_string(),
            ..Default::default()
        };
        assert!(dir_cfg.is_ready());
        let t = transport_from_config(&dir_cfg).unwrap();
        assert!(t.list().unwrap().is_empty(), "空目录可用");

        let (endpoint, _s) = spawn_relay("tok-r");
        let cloud_cfg = CloudConfig {
            mode: MODE_CLOUD.into(),
            endpoint: endpoint.clone(),
            token: "tok-r".into(),
            ..Default::default()
        };
        assert!(cloud_cfg.is_ready());
        assert!(transport_from_config(&cloud_cfg).unwrap().list().unwrap().is_empty());

        assert!(transport_from_config(&CloudConfig::default()).is_err(), "off → 拒");
        assert!(
            transport_from_config(&CloudConfig {
                mode: MODE_CLOUD.into(),
                endpoint,
                token: String::new(),
                ..Default::default()
            })
            .is_err(),
            "缺令牌 → 拒"
        );
        assert!(
            transport_from_config(&CloudConfig {
                mode: MODE_DIR.into(),
                dir: String::new(),
                ..Default::default()
            })
            .is_err(),
            "缺目录 → 拒"
        );
        // pg 通道：路由正确 + 缺密钥/缺地址明确报错（构造期不发网络请求）
        let pg_cfg = CloudConfig {
            mode: MODE_PG.into(),
            endpoint: "https://e.api.tcloudbasegateway.com/v1/rdb/rest".into(),
            token: "k".into(),
            ..Default::default()
        };
        assert!(pg_cfg.is_ready());
        assert!(transport_from_config(&pg_cfg).is_ok(), "pg → 构造成功");
        assert!(
            transport_from_config(&CloudConfig {
                mode: MODE_PG.into(),
                endpoint: "https://e.api.tcloudbasegateway.com/v1/rdb/rest".into(),
                token: String::new(),
                ..Default::default()
            })
            .is_err(),
            "pg 缺 API Key → 拒"
        );
        assert!(
            transport_from_config(&CloudConfig {
                mode: MODE_PG.into(),
                endpoint: String::new(),
                token: "k".into(),
                ..Default::default()
            })
            .is_err(),
            "pg 缺基址 → 拒"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ⑭ CloudBase PG 通道：模式规范化 / 就绪判定 / postgREST URL 拼装与入参校验。
    #[test]
    fn pg_config_and_url_shape() {
        let c = CloudConfig {
            mode: " pg ".into(),
            endpoint: " https://x.api.tcloudbasegateway.com/v1/rdb/rest/ ".into(),
            token: " key ".into(),
            dir: String::new(),
        }
        .normalized();
        assert_eq!(c.mode, MODE_PG, "带空白的 pg 必须规范化成 pg（不得回落 off）");
        // 规范化只去首尾空白；尾斜杠由传输层吃掉（见下方 table_url 断言）
        assert_eq!(c.endpoint, "https://x.api.tcloudbasegateway.com/v1/rdb/rest/");
        assert_eq!(c.token, "key");
        assert!(c.is_ready());

        let t =
            PgRestTransport::new("https://e.api.tcloudbasegateway.com/v1/rdb/rest/", " k ").unwrap();
        // 尾斜杠被吃掉；select/order 拼装正确；逗号做百分号编码，点属未保留字符故保留
        assert_eq!(
            t.table_url(&[
                ("select", "device,stamp".to_string()),
                ("order", "stamp.asc".to_string())
            ]),
            "https://e.api.tcloudbasegateway.com/v1/rdb/rest/fl_sync?select=device%2Cstamp&order=stamp.asc"
        );
        assert_eq!(
            t.table_url(&[("device", "eq.dev/we ird".to_string())]),
            "https://e.api.tcloudbasegateway.com/v1/rdb/rest/fl_sync?device=eq.dev%2Fwe%20ird"
        );

        assert!(PgRestTransport::new("", "k").is_err(), "空基址 → 拒");
        assert!(PgRestTransport::new("ftp://x", "k").is_err(), "非 http(s) → 拒");
        assert!(PgRestTransport::new("https://x", "   ").is_err(), "空密钥 → 拒");
    }

    // ⑮ CloudBase PG 通道：put 拒绝非法远端键；非文本载荷给出明确指引而非静默损坏。
    #[test]
    fn pg_put_rejects_bad_key_and_binary() {
        let t =
            PgRestTransport::new("https://e.api.tcloudbasegateway.com/v1/rdb/rest", "k").unwrap();

        // 非法键（缺时间戳段）→ 构造期即拒，不发请求
        let err = t
            .put(&PutRequest {
                key: "no-stamp.jsonl",
                device: "d",
                at: "x",
                kind: KIND_SNAPSHOT,
                body: b"{}",
            })
            .unwrap_err();
        assert!(err.contains("非法远端键"), "实际: {err}");

        // 非 UTF-8 正文（M4 备份 .db）→ 明确提示换通道
        let err = t
            .put(&PutRequest {
                key: "d/20260910-221500123.jsonl",
                device: "d",
                at: "x",
                kind: KIND_BACKUP,
                body: &[0xff, 0xfe, 0x00],
            })
            .unwrap_err();
        assert!(err.contains("仅支持文本载荷"), "实际: {err}");
    }

    // ⑯ CloudBase PG 通道端到端（默认跳过）：对着**真实环境**跑 put → list → get → 善后。
    //
    // 为什么必须有：本通道的单测只覆盖 URL 拼装与入参校验；postgREST 的真实语义
    // （Prefer 冲突合并、eq. 过滤、空表返回 []、正文原样回传）只有打真服务才能证明。
    //
    // 跑法（密钥从凭据文件读入环境变量，不要写进命令行）：
    //   FUNDLENS_PG_TEST_URL=https://<envId>.api.tcloudbasegateway.com/v1/rdb/rest \
    //   FUNDLENS_PG_TEST_KEY=$(grep -m1 '^eyJ' ~/.workbuddy/fundlens-cloudbase-apikey.txt) \
    //   cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
    //     -- --ignored pg_rest_interop --nocapture
    #[test]
    #[ignore]
    fn pg_rest_interop() {
        let base = match std::env::var("FUNDLENS_PG_TEST_URL") {
            Ok(v) if !v.is_empty() => v,
            _ => {
                eprintln!("跳过：未设置 FUNDLENS_PG_TEST_URL");
                return;
            }
        };
        let key = std::env::var("FUNDLENS_PG_TEST_KEY").unwrap_or_default();
        assert!(!key.is_empty(), "需同时设置 FUNDLENS_PG_TEST_KEY");

        let t = PgRestTransport::new(&base, &key).unwrap();
        // 专测设备名 + 远期时间戳：既不影响真实设备，也不会挤进「每设备取最新一份」的挑选
        let dev = "zz-itest";
        let k = snapshot_key(dev, "20991231-235959999");
        let payload = b"{\"fl_sync\":1,\"device\":\"zz-itest\"}\n";

        // ① 上行幂等：同 key 连推两次，远端仍只有一条
        for _ in 0..2 {
            let e = t
                .put(&PutRequest {
                    key: &k,
                    device: dev,
                    at: "2099-12-31 23:59:59",
                    kind: KIND_SNAPSHOT,
                    body: payload,
                })
                .unwrap();
            assert_eq!(e.size, payload.len() as i64);
        }
        let listed = t.list().unwrap();
        assert_eq!(
            listed.iter().filter(|i| i.device == dev).count(),
            1,
            "同 key 重复上传必须只留一条（postgREST upsert 语义）"
        );

        // ② 下行：正文按原始字节取回
        assert_eq!(t.get(&k).unwrap(), payload, "正文必须字节一致");

        // ③ 不存在的键 → 报错而非静默返回空
        assert!(
            t.get(&snapshot_key(dev, "20991231-235958000")).is_err(),
            "不存在的键必须报错"
        );

        // ④ 善后：清掉本设备，避免污染真实同步清单
        assert!(t.delete_device(dev).unwrap() >= 1, "善后应至少删除 1 行");
        assert!(
            t.list().unwrap().iter().all(|i| i.device != dev),
            "善后必须干净"
        );
    }

    // ⑬ 跨实现协议互操作（默认跳过）：对着**真实 relay 进程**跑一遍完整推送/拉取。
    //
    // 为什么这条不可省：Rust 侧 mock relay 与 relay/server.js 是两个独立实现，
    // 各自自测只能证明「自己和自己一致」，只有这条能证明两边讲的是同一个协议
    // （查询串键名、正文是否原始字节、错误信封、时间戳格式、键命名规则）。
    //
    // 跑法：
    //   cd relay && SYNC_TOKEN=fl-interop-token-123456 PORT=18787 DATA_DIR=/tmp/fl-relay-interop node server.js &
    //   FUNDLENS_RELAY_TEST_URL=http://127.0.0.1:18787 \
    //   FUNDLENS_RELAY_TEST_TOKEN=fl-interop-token-123456 \
    //   cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features -- --ignored relay_protocol_interop --nocapture
    #[test]
    #[ignore]
    fn relay_protocol_interop() {
        let endpoint = match std::env::var("FUNDLENS_RELAY_TEST_URL") {
            Ok(v) if !v.is_empty() => v,
            _ => {
                eprintln!("跳过：未设置 FUNDLENS_RELAY_TEST_URL（需要先起 relay/server.js）");
                return;
            }
        };
        let token = std::env::var("FUNDLENS_RELAY_TEST_TOKEN").unwrap_or_default();
        assert!(!token.is_empty(), "需同时设置 FUNDLENS_RELAY_TEST_TOKEN");

        let t = HttpTransport::new(&endpoint, &token).unwrap();
        let before = t.list().unwrap().len();

        // ① 上传一份快照，确认条目元信息由服务端回填（size/at 必须服务端算得出）
        let key = "dev-interop/20260910-221500123.jsonl";
        let payload = b"{\"fl_sync\":1,\"device\":\"dev-interop\",\"exported_at\":\"x\",\"count\":0}\n";
        let e = t.put(&PutRequest {
            key,
            device: "dev-interop",
            at: "2026-09-10 22:15:00",
            kind: KIND_SNAPSHOT,
            body: payload,
        })
        .unwrap();
        assert_eq!(e.key, key);
        assert_eq!(e.size, payload.len() as i64, "服务端应回报真实字节数");
        assert_eq!(e.at, "2026-09-10 22:15:00", "服务端应能由键还原展示时间");

        // ② 列表能看到它，且正文可原样取回
        let listed = t.list().unwrap();
        assert_eq!(listed.len(), before + 1, "列表应新增一条");
        let mine = listed.iter().find(|i| i.key == key).expect("列表应含刚上传的键");
        assert_eq!(mine.device, "dev-interop");
        assert_eq!(mine.kind, KIND_SNAPSHOT);
        assert_eq!(t.get(key).unwrap(), payload, "正文必须字节一致（原始字节协议）");

        // ③ 端到端：A 推真实库快照 → B 拉取合并
        let a = db();
        let b = db();
        a.execute("INSERT INTO funds(code,name,platform) VALUES('000001','互操作','alipay')", [])
            .unwrap();
        a.execute("INSERT INTO positions(fund_code,shares) VALUES('000001',123.5)", [])
            .unwrap();
        let pushed = push(&a, &t).unwrap();
        let pulled = pull(&b, &t).unwrap();
        assert!(pulled.pulled >= 1, "B 应至少拉到 A 的快照");
        assert_eq!(pulled.applied, pushed.count, "A 的变更应逐条落到 B");
        let shares: f64 = b
            .query_row("SELECT shares FROM positions WHERE fund_code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(shares, 123.5);
        let again = pull(&b, &t).unwrap();
        assert_eq!(again.planned, 0, "再拉一次应幂等");

        // ④ 令牌错误必须被拒（服务端是唯一的访问边界）
        let bad = HttpTransport::new(&endpoint, "definitely-wrong-token").unwrap();
        let err = bad.list().unwrap_err();
        assert!(err.contains("token"), "错误应指出令牌问题: {err}");

        // ⑤ 键校验：非法键不得被接受
        assert!(
            t.get("dev-interop/../secret.jsonl").is_err(),
            "路径穿越键必须被服务端拒绝"
        );
    }
}
