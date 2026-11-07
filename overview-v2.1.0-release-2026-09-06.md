# FundLens v2.1.0 交付说明（P1-P3 策略引擎 + 麒麟同步）

> 交付时间：2026-09-06 ｜ main: `06f0191` ｜ kylin: `a70bf1f` ｜ tag: `v2.1.0`

## 一、本版完成内容

### P1 —— 参数体系完整 + 回测对拍（commit `fcf446a` 后端 / `1700716` 前端）
- **逐基金可调参数**：投入上限、波动敏感度（0.3–3.0）、卖出费率（0–2%）、卖出冷却日期，整行保存（`grid_save_config`，费率落 `fee_schedule` JSON）
- **货基白名单**：fund_type 含 货币/理财/money 或 code 以 002/005 开头 → 拒启用策略（中文错误提示）
- **事件研究复盘**：信号 T+3/5/10 方向收益回填（buy 后涨 / sell 后跌为「对」，方向反转），按动作聚合 样本数/胜率/平均收益

### P2 —— 延迟回补挂单 + 执行闭环
- **挂单状态机**：pending → triggered/cancelled/expired；28 自然日窗口；软上限 3 单/基金
- **引擎触发检查**：当前净值 ≤ 触发净值 → 产出 priority=5 买入建议（isRebuy 标记 + 挂单转 triggered）
- **止盈自动建单**：8 个止盈族信号卖出时按趋势生成回补计划（连涨/偏强/走强回补 70%、震荡且跌<0.5% 回补 40%、弱趋势不回补），≥100 元才建单
- 前端：卡片参数行、挂单面板（待触发/已触发/取消）、回补计划展示

### P3 —— 组合级风控
- **现金闸门**：可投现金预算 × 20% = 日买入上限；按 priority 排序、超限缩额或暂缓（注 reason）；前端设置弹层输入预算、顶部显示用量
- **启动补算**：应用启动 15s 后静默执行当日信号计算 + outcome 回填（幂等）

### 发版与麒麟同步
- 版本 2.0.0 → 2.1.0（package.json / package-lock.json / tauri.conf.json / Cargo.toml + Cargo.lock）
- GitHub：`main` 推送 `06f0191` + tag `v2.1.0`
- 麒麟分支 `feat/kylin-v10-aarch64`：合并 main 并人工裁决 2 处冲突（tauri.conf.json 保留 Tauri1 结构仅升版本、permissions 文件保留 kylin 删除）→ `a70bf1f` 已推送

## 二、验证结果

| 门禁 | main | kylin |
|---|---|---|
| Rust 单测 | 114 passed | cargo check 0 err |
| tsc | 干净 | 干净（npm install 对齐 @tauri-apps/api v1 后） |
| vite build | ✓ | ✓ |
| vitest | 39 passed | 39 passed |

麒麟产物指纹（容器内 objdump/readelf）：
- ELF aarch64 stripped（GNU/Linux 3.7.0）
- GLIBC max **2.30**（麒麟 2.31 OK）
- NEEDED `libwebkit2gtk-4.0.so.37`（Tauri1 正确）
- deb Depends `libwebkit2gtk-4.0-37`
- 二进制 mtime 晚于源码 → 落地确证

## 三、产物

- `dist-arm64/deb/fund-lens_2.1.0_arm64.deb`（19M）
- `dist-arm64/appimage/FundLens_2.1.0_aarch64.AppImage`（84M，手工修复版；CLI 坏包已剔除）
- macOS 侧：需另行 `npm run tauri build` 部署 /Applications（本轮回合未做 macOS 打包）

## 四、遗留/注意

- 启动静默补算依赖 setup 15s sleep，首次启动后约 15s 才落当日信号
- P1 复盘需「刷新复盘」触发回填（LIMIT 300/批）；历史表现按 T3/T5/T10 展示
- kylin 切分支后必须 `npm install`（main 遗留 @tauri-apps/api v2 与 kylin v1 冲突）
