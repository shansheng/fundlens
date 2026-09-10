# FundLens M2 云同步 — 后端选型与落地（CloudBase PG 直连）

- 日期：2026-09-10
- 分支：`main`
- 状态：✅ M2 云通道后端**已打通并实测通过**（Rust 单测 187 passed / 0 failed / 6 ignored；前端 54 passed）
- 关联：`perf-cloudbase-v2.6.0-design-2026-09-09.md`（v2.6.0 性能与缓存）、`sync.rs`（M1 同步内核）

---

## 1. 背景与选型结论

M1 内核（`sync.rs`）已产出「设备快照 = 全量 JSONL + LWW 回放 + `seen` 水位」，但它依赖一个抽象层 `SyncTransport`（`list` / `put` / `get`）来落远端。M2 需要给这个抽象层接上真实后端。

候选与最终裁定：

| 候选 | 结论 | 原因 |
|---|---|---|
| 自建轻量服务器（Lighthouse） | ❌ 废弃 | 用户账号下**没有实例** |
| 自建 relay（`relay/server.js`）+ `MODE_CLOUD` | ⚠️ 保留备用 | 零依赖 Node 4 端点服务，适合**需要二进制载荷**（M4 备份包）或内网/自管场景 |
| CloudBase 云函数 relay | ❌ 不可行 | 试用环境（`baas_trial`）**无函数命名空间**，`createFunction` 报「未找到指定的Namespace」 |
| **CloudBase PostgreSQL 直连（`MODE_PG`）** | ✅ **采用** | 无需任何服务端代码（无函数、无 relay），直接用 PG REST + API Key |

**核心设计**：`PgRestTransport` 直连 CloudBase 的 postgREST 端点，实现零服务端架构。快照本身是 JSONL 文本，天然适配 REST。

---

## 2. 传输层架构

```
sync.rs（M1 内核：快照生成 / LWW 回放 / 水位）
        │  仅依赖 trait
        ▼
  SyncTransport { list() / put() / get() }
        │
        ├── DirTransport      目录（本地/NAS/云盘同步盘）      MODE_DIR
        ├── PgRestTransport   CloudBase PG REST 直连          MODE_PG   ← 本次新增
        ├── HttpTransport     自定 relay 协议（?action=...）   MODE_CLOUD
        └── MemTransport      测试内存实现                     (test only)
```

`transport_from_config(cfg)`（`cloud.rs:996`）按 `cfg.mode` 路由：

- `MODE_OFF`="off" / `MODE_DIR`="dir" / `MODE_CLOUD`="cloud" / **`MODE_PG`="pg"** ← 新增
- `is_ready()`：`MODE_CLOUD | MODE_PG` 都要求 `endpoint + token` 非空
- `normalized()`：把 `"  pg  "` 这类带空白值规范化成 `"pg"`，**未知值一律回落 `off`**（有单测守住 `cloud.rs:1732`，防止手误输入静默变 off）

---

## 3. CloudBase PG REST 协议要点

- REST 基址：`https://{envId}.api.tcloudbasegateway.com/v1/rdb/rest`
- 鉴权：`Authorization: Bearer <API Key>`（**API Key 是唯一密钥**）
- 表：`fl_sync`
- 过滤算子式：`kind=eq.snapshot`、`order=stamp.asc`、`limit=1`
- **幂等 upsert**：`Prefer: resolution=merge-duplicates,return=representation`
- 删除返回行数：`Prefer: return=representation`（DELETE 后解析返回体计行）

### 3.1 表结构

```sql
CREATE TABLE IF NOT EXISTS public.fl_sync (
  device text NOT NULL,
  stamp  text NOT NULL,
  kind   text NOT NULL DEFAULT 'snapshot',
  size   integer NOT NULL DEFAULT 0,
  body   text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (device, stamp)
);
CREATE INDEX IF NOT EXISTS fl_sync_kind_stamp_idx ON public.fl_sync (kind, stamp);
ALTER TABLE public.fl_sync ENABLE ROW LEVEL SECURITY;
GRANT SELECT, INSERT, UPDATE, DELETE ON public.fl_sync TO service_role;
```

迁移文件已归档进仓库：`cloudbase/migrations/20260910131035_fl_sync_store.sql`

### 3.2 ⚠️ RLS 语义（关键）

`fl_sync` 开启了 RLS 且**没有任何 policy** → 对 anon / authenticated 角色是 **deny-all**。
只有 `service_role` 的 API Key 能读写。这意味着：

- ✅ **安全**：即使环境 ID 泄漏，无 Key 也读不到任何数据
- ⚠️ **风险集中**：Key = 全量数据权限，等同服务端密钥，**不得外传 / 不得入 git**
- 凭据存放在仓库外：`~/.workbuddy/fundlens-cloudbase-apikey.txt`（明文，供本机配置使用）

---

## 4. ⚠️ 踩坑记录：MCP 会话地域绑定

CloudBase 环境在 **ap-singapore**，而 MCP/CLI 默认会话绑定 **ap-shanghai**。表现为：

- 写操作（`createFunction`、`createApiKey`）报**误导性错误**（如 `[CreateApiKey] administrator not found`）
- 读操作（按显式 envId 查）却正常 → 极易误判为权限问题

**判定实验**：
```
envQuery(list, region=ap-singapore) → AUTH_REQUIRED
envQuery(list, region=ap-shanghai) → 空列表
```

**修复**：先执行 `auth(action=set_env, envId=sss-d3ggl1sft593f46c7)` 绑定到 ap-singapore，之后 `createApiKey(keyType=api_key)` 即成功。

> 另注：publishable key（anon 角色）会被 REST 拒绝并报 401 `ACCESS_TOKEN_KID_INVALID`；**必须用 `service_role` 的 API Key**。

---

## 5. FundLens 端配置方式

「同步」页 → 云端同步区块：

| 字段 | 填入 |
|---|---|
| 模式 | **CloudBase（PostgreSQL 直连）** |
| CloudBase REST 基址 | `https://sss-d3ggl1sft593f46c7.api.tcloudbasegateway.com/v1/rdb/rest` |
| CloudBase API Key | `~/.workbuddy/fundlens-cloudbase-apikey.txt` 中 `eyJ...` 开头的整串 |

- 环境：`sss-d3ggl1sft593f46c7`（ap-singapore），PG 实例 `pgdb-a29fpyny`，表 `fl_sync`
- API Key：名称 `fundlens-sync`，角色 `service_role`，**无过期**
- 轮换/吊销：CloudBase 控制台 → 身份认证 → API Key

---

## 6. 能力边界（重要）

| 载荷类型 | `MODE_PG` | `MODE_DIR` | `MODE_CLOUD`(relay) |
|---|---|---|---|
| 设备快照（JSONL 文本） | ✅ | ✅ | ✅ |
| 二进制载荷（M4 备份包） | ❌（`put` 明确报「仅支持文本载荷」） | ✅ | ✅ |

`PgRestTransport::put` 对非 UTF-8 body 直接返回清晰错误，引导用户改用 dir/relay，而不是静默写坏数据。

---

## 7. 验证证据

### 7.1 单元测试（`cloud.rs` 新增）

| 测试 | 覆盖 |
|---|---|
| `pg_config_and_url_shape` | 模式规范化（含空白）、基址尾斜杠处理、URL 拼装与编码、空/非法基址与空 Key 拒绝 |
| `pg_put_rejects_bad_key_and_binary` | 非法远端键 → 「非法远端键」；二进制 → 「仅支持文本载荷」 |
| `pg_rest_interop`（`#[ignore]`） | 真实环境端到端：upsert 幂等（同键→1 行）、body 逐字节往返、缺键报错、`delete_device` 清理 |

### 7.2 全量套件

- Rust：`cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features` → **187 passed / 0 failed / 6 ignored**
- 真实环境互操作：`pg_rest_interop` → **1 passed**（跑完 `fl_sync` 复检为 `[]`，验证自清理）
- 前端：`npx tsc -b` 干净；`npx vitest run` → **54 passed**（含新增 PG 模式 UI 测试）

### 7.3 curl 手工端到端（早期验证）

list `[]`(200) → upsert 行(201) → list 见行(200) → body 逐字节一致(200) → 错误 Key → 401 → DELETE 探针行(204) → 再 list `[]`(200)

---

## 8. 后续（M2 剩余 / M3 / M4）

- **M2 剩余**：备份（`sync_create_backup` 已备命令面）与 UI 细节打磨
- **M3**：多设备冲突 UI（`sync_conflicts` / `sync_meta` 已就绪）
- **M4**：二进制备份包 + 云通道选择（二进制走 dir/relay）
- 若后续需要跨 PG / relay 双通道，抽象层已就位，只需再加一个 `impl SyncTransport`
