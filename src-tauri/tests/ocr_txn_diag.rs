// =====================================================================================
// FundLens 记账截图导入 —— 真机诊断工具集（5 个诊断，全部按环境变量开关）
//
// 用途：把「OCR 抽错」定位到**具体哪一层**（OCR 没读到 / 行聚类错 / 块切分错 / 字段分类错），
// 而不是只看最终解析结果猜原因。改 `ocr.rs` 抽取逻辑、排查「日期为空/串值」「类型不对」
// 「名称乱序」这类问题时先用它，比读代码猜快得多。
//
// ┌─────────────────────┬──────────────────────────────────────────────────────────────┐
// │ diag_txn_code_resolution │ 逐笔打印「OCR 名称 → 本地优先 → 最终解析代码」（看命中率/错类） │
// │ diag_platform_probe      │ 逐笔打印 raw 标签 / has_year / date / time / amount / name（定平台）│
// │ diag_dump_lines          │ 原样倾倒 OCR 行（文本 + x/y）→ 区分「OCR 没读到」与「逻辑丢了」 │
// │ diag_block_split         │ 复刻「行聚类 → 块切分」，打印每行 y/hmax/bottom 与逐块成员（切分错位）│
// │ diag_platform_names      │ 京东/腾讯口径名称 → 代码解析的确定性体检（含 ⛔错类探针）        │
// └─────────────────────┴──────────────────────────────────────────────────────────────┘
//
// 运行方式（**必须带 --features ocr**，否则文件整体被 cfg 掉、0 用例）：
//   cd src-tauri
//   # ⚠️ 本机 CLT 的 usr/include/c++/v1 是空壳目录，带 ocr 的 cargo 必带 CXXFLAGS，否则
//   #    clipper-sys 报 `'vector' file not found`（详见 DETAILS-口径-OCR-发版.md §OCR）
//   export CXXFLAGS="-isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"
//   TEST_TXN_IMGS="a.jpg:b.jpg" cargo test --test ocr_txn_diag diag_platform_probe --features ocr -- --nocapture
//
// 安全约定：
// - **无 `TEST_TXN_IMGS`（及 `diag_platform_names` 所需的 `FUNDLENS_DATA_DIR`）时全部自动跳过**，
//   0 用例、0 耗时 → 可安全留在仓库里，不会影响 CI 与门禁（与 `ocr_e2e.rs` 同约定）。
// - `FUNDLENS_DATA_DIR` **必须指向真实库的副本**（`sqlite3 <真库> ".backup <副本>"`），
//   因为 `init_db` 会跑迁移 —— 绝不指向真库。
// - 截图路径只从环境变量传入，**不在源码里硬编码任何本地绝对路径**。
//
// ⛔ 修复前/后对照实验（证明某改动有净收益的硬办法）：
//   git stash push -- src-tauri/src/ocr.rs   # 回到修复前
//   TEST_TXN_IMGS="a.jpg:b.jpg" cargo test --test ocr_txn_diag diag_platform_probe --features ocr -- --nocapture
//   git stash pop                             # 恢复（先 diff 确认再 pop）
// =====================================================================================
#[cfg(feature = "ocr")]
use fundlens_lib::ocr::{extract_txn_rows, recognize_image};

#[cfg(feature = "ocr")]
#[test]
fn diag_txn_code_resolution() {
    let spec = match std::env::var("TEST_TXN_IMGS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            println!("SKIP diag_txn_code_resolution: 未设置 TEST_TXN_IMGS");
            return;
        }
    };

    // 本地优先解析需要 DB：由调用方设置 FUNDLENS_DATA_DIR（指向**真实库的副本**，勿用真库）
    // 后在此 init_db；未设置则该路径不生效，只能验证联网分支。
    if std::env::var("FUNDLENS_DATA_DIR").is_ok() {
        match fundlens_lib::db::init_db(None) {
            Ok(()) => println!("DB 已就绪（本地优先路径生效）"),
            Err(e) => println!("⚠️ init_db 失败（本地优先路径不可用）: {e}"),
        }
    } else {
        println!("未设置 FUNDLENS_DATA_DIR → 本地优先路径不生效（仅联网）");
    }

    let known = fundlens_lib::db::known_fund_names().unwrap_or_default();
    println!("known_fund_names: {} 条", known.len());

    let mut all = Vec::new();
    for p in spec.split(':') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let lines = recognize_image(p, None).expect("OCR 引擎应成功识别");
        let txns = extract_txn_rows("alipay", &lines);
        println!("---- {} → {} 条交易 ----", p, txns.len());
        all.extend(txns);
    }

    let (mut ok, mut fail, mut local_hits) = (0usize, 0usize, 0usize);
    println!(
        "\n{:<3} {:<5} {:<34} {:<10} {}",
        "idx", "type", "OCR 名称", "本地优先", "最终解析"
    );
    for (i, t) in all.iter().enumerate() {
        let is_real = t.code.len() == 6 && t.code.chars().all(|c| c.is_ascii_digit());
        let corrected = fundlens_lib::ocr::correct_name_against_known(&t.name, &known);
        let probe = corrected.clone().unwrap_or_else(|| t.name.clone());
        let local = if is_real {
            None
        } else {
            fundlens_lib::data::resolve_fund_code_local(&probe)
        };
        if local.is_some() {
            local_hits += 1;
        }
        let resolved = if is_real {
            Some(t.code.clone())
        } else {
            fundlens_lib::data::resolve_fund_code(&probe)
        };
        if resolved.is_some() {
            ok += 1;
        } else {
            fail += 1;
        }
        println!(
            "{:<3} {:<5} {:<34} {:<10} {:?}{}",
            i,
            t.txn_type,
            truncate(&t.name, 32),
            local.clone().unwrap_or_else(|| "-".into()),
            resolved,
            match &corrected {
                Some(c) => format!("  [已纠正→{}]", c),
                None => String::new(),
            }
        );
    }
    println!(
        "\n解析成功 {} / 失败 {}（共 {}）；其中本地优先命中 {}",
        ok,
        fail,
        all.len(),
        local_hits
    );
}

// =====================================================================================
// 按图逐个打印「原始类型标签 + 日期/时间 + 年份标记」——用于判定一张交易截图属于哪个平台。
//
// 判据（三平台原始标签 differs）：
//   京东金融   → txn_type_raw ∈ {转入, 转出, 分红}，且 has_year=false（截图只印 08-13）
//   腾讯理财通 → txn_type_raw ∈ {买入, 取出, 分红}，has_year=true
//   支付宝     → txn_type_raw ∈ {买入, 卖出, 分红}，has_year=true
//
// 运行：TEST_TXN_IMGS="a.jpg:b.jpg" cargo test --test ocr_txn_diag diag_platform_probe --features ocr -- --nocapture
// =====================================================================================
#[cfg(feature = "ocr")]
#[test]
fn diag_platform_probe() {
    let spec = match std::env::var("TEST_TXN_IMGS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            println!("SKIP diag_platform_probe: 未设置 TEST_TXN_IMGS");
            return;
        }
    };
    if std::env::var("FUNDLENS_DATA_DIR").is_ok() {
        let _ = fundlens_lib::db::init_db(None);
    }
    for p in spec.split(':') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let lines = match recognize_image(p, None) {
            Ok(l) => l,
            Err(e) => {
                println!("---- {} → OCR 失败: {} ----", p, e);
                continue;
            }
        };
        let txns = extract_txn_rows("alipay", &lines);
        println!("\n==== {} → {} 条 ====", p, txns.len());
        for (i, t) in txns.iter().enumerate() {
            println!(
                "  {:<2} raw={:<4} year={:<5} date={:<12} time={:<6} amount={:<10.2} name={}",
                i, t.txn_type_raw, t.has_year, t.date, t.time, t.amount, t.name
            );
        }
    }
}

// =====================================================================================
// 原样倾倒一张图的 OCR 行（文本 + 坐标），用于判断某个字段是「OCR 没识别到」还是
// 「识别到了但抽取逻辑丢掉了」。排查「首条交易日期为空」这类问题时用它最快。
//
// 运行：TEST_TXN_IMGS="a.jpg" cargo test --test ocr_txn_diag diag_dump_lines --features ocr -- --nocapture
// =====================================================================================
#[cfg(feature = "ocr")]
#[test]
fn diag_dump_lines() {
    let spec = match std::env::var("TEST_TXN_IMGS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            println!("SKIP diag_dump_lines: 未设置 TEST_TXN_IMGS");
            return;
        }
    };
    let first = spec.split(':').next().unwrap_or("").trim();
    if first.is_empty() {
        return;
    }
    let lines = match recognize_image(first, None) {
        Ok(l) => l,
        Err(e) => {
            println!("OCR 失败: {}", e);
            return;
        }
    };
    println!("\n==== {} 共 {} 行 ====", first, lines.len());
    for (i, l) in lines.iter().enumerate() {
        // 标注该行是否被判为 chrome（会被抽取层过滤掉）——chrome 行是「字段消失」的常见原因
        println!("{:>3} x={:<5} y={:<5} {:?}", i, l.x, l.y, l.text);
    }
}

// =====================================================================================
// 复刻 extract_txn_rows 的「行聚类 → 交易块切分」两步，逐行/逐块打印中间结构。
//
// 用途：判定「某字段为空」是 OCR 缺失、reconstruct_rows 分组错误，还是 TXN_GAP 块切分
// 把字段切到了相邻块（块切分只按「纵向间隙 > 45px」或「出现类型标签」断开，切错后
// ctx_date 会串到前/后一笔）。
//
// 注意：本测试**复刻**块切分规则（TXN_GAP=45），与 ocr.rs 的常量必须同步修改。
//
// 运行：TEST_TXN_IMGS="a.jpg" cargo test --test ocr_txn_diag diag_block_split --features ocr -- --nocapture
// =====================================================================================
#[cfg(feature = "ocr")]
#[test]
fn diag_block_split() {
    use fundlens_lib::ocr::reconstruct_rows;
    const TXN_GAP: i32 = 45;

    let spec = match std::env::var("TEST_TXN_IMGS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            println!("SKIP diag_block_split: 未设置 TEST_TXN_IMGS");
            return;
        }
    };
    let first = spec.split(':').next().unwrap_or("").trim();
    if first.is_empty() {
        return;
    }
    let lines = match recognize_image(first, None) {
        Ok(l) => l,
        Err(e) => {
            println!("OCR 失败: {}", e);
            return;
        }
    };
    let rows = reconstruct_rows(&lines);
    let has_type_cell = |r: &fundlens_lib::ocr::OcrRow| {
        r.cells.iter().any(|c| {
            let t = &c.text;
            t.contains("转入")
                || t.contains("买入")
                || t.contains("申购")
                || t.contains("买进")
                || t.contains("卖出")
                || t.contains("赎回")
                || t.contains("转出")
                || t.contains("取出")
                || t.contains("分红")
        })
    };

    println!("\n==== {} —— {} 个 OCR 行 → {} 个重建行 ====", first, lines.len(), rows.len());
    for (i, r) in rows.iter().enumerate() {
        let h = r.cells.iter().map(|c| c.h).max().unwrap_or(0);
        let cells: Vec<String> = r
            .cells
            .iter()
            .map(|c| format!("{:?}@x{}w{}h{}", c.text, c.x, c.w, c.h))
            .collect();
        println!(
            "行{:>2}  y={:<5} hmax={:<4} bottom={:<5} 类型={:<5} {:?}",
            i,
            r.y,
            h,
            r.y + h,
            if has_type_cell(r) { "有" } else { "无" },
            cells
        );
    }

    // 复刻块切分
    let mut blocks: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut last_bottom: Option<i32> = None;
    for (i, r) in rows.iter().enumerate() {
        let row_bottom = r.y + r.cells.iter().map(|c| c.h).max().unwrap_or(0);
        let mut new_block = false;
        if !cur.is_empty() {
            if has_type_cell(r) {
                new_block = true;
            } else if let Some(lb) = last_bottom {
                if r.y - lb > TXN_GAP {
                    new_block = true;
                }
            }
        }
        if new_block {
            blocks.push(std::mem::take(&mut cur));
        }
        cur.push(i);
        last_bottom = Some(row_bottom);
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }

    println!("\n---- 块切分（TXN_GAP={}）→ {} 块 ----", TXN_GAP, blocks.len());
    for (bi, b) in blocks.iter().enumerate() {
        let texts: Vec<String> = b
            .iter()
            .flat_map(|&i| rows[i].cells.iter().map(|c| c.text.clone()))
            .collect();
        println!("块{:<2} 行{:?} → {:?}", bi, b, texts);
    }
}

#[cfg(feature = "ocr")]
fn truncate(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        format!("{}…({}字)", chars[..n].iter().collect::<String>(), chars.len())
    }
}

// =====================================================================================
// 京东 / 腾讯口径名称 → 解析 6 位代码 的确定性体检
//
// 起因：支付宝侧的「需补代码」修复（本地表优先 + 类份额守卫）改的是 data.rs 的共用函数，
// 三平台共享同一链路。本测试用**真库观察到的实际名称**验证京东/腾讯走这条链路的表现，
// 重点看两件事：① 命中率 ② 是否错类（返回代码的类份额与查询名是否一致）。
//
// 用例来源（均已从真库副本核对）：
//   京东 013403 华夏恒生科技ETF发起式联接(QDII)C ／ 022435 南方中证A500ETF联接C
//   京东 025518 「东方红中证东方红红利低波动指数证券投资基金A类」（funds 表原文，官方实为 D 类）
//   腾讯 011393 中欧融益稳健一年持有期混（尾部截断）／ 001877 宝盈国家安全战略沪港深股（尾部截断）
//   腾讯 023081 南方致远混合E ／ 260112 景顺长城能源基建混合A
//
// 运行：FUNDLENS_DATA_DIR=/tmp/fl-diag cargo test --test ocr_txn_diag diag_platform_names --features ocr -- --nocapture
// =====================================================================================
#[cfg(feature = "ocr")]
#[test]
fn diag_platform_names() {
    if std::env::var("FUNDLENS_DATA_DIR").is_err() {
        println!("SKIP diag_platform_names: 未设置 FUNDLENS_DATA_DIR（需指向真实库副本）");
        return;
    }
    match fundlens_lib::db::init_db(None) {
        Ok(()) => println!("DB 已就绪（本地优先路径生效）"),
        Err(e) => {
            println!("⚠️ init_db 失败: {e}");
            return;
        }
    }

    // (平台, 名称口径说明, 名称, 真库已知的正确代码；None = 只观察不错类)
    let cases: Vec<(&str, &str, &str, Option<&str>)> = vec![
        // ---- 京东：名称与动作同行（此处已剥离「转入-」前缀，模拟抽取层输出）----
        ("jd", "官方口径", "华夏恒生科技ETF发起式联接(QDII)C", Some("013403")),
        ("jd", "官方口径", "南方中证A500ETF联接C", Some("022435")),
        ("jd", "全称+A类（funds 原文）", "东方红中证东方红红利低波动指数证券投资基金A类", None),
        // ---- 京东：类份额错配探针（本地只有 C 类，问 A 类，绝不能返回 C 类）----
        ("jd", "⛔错类探针", "华夏恒生科技ETF发起式联接(QDII)A", None),
        // ---- 腾讯：尾部截断名 ----
        ("tx", "尾部截断", "中欧融益稳健一年持有期混", Some("011393")),
        ("tx", "尾部截断", "宝盈国家安全战略沪港深股", Some("001877")),
        ("tx", "官方口径", "南方致远混合E", Some("023081")),
        ("tx", "官方口径", "景顺长城能源基建混合A", Some("260112")),
        // ---- 腾讯：截断候选 + 带类份额查询 → 绝不能落到 C 类 ----
        ("tx", "⛔错类探针", "中欧融益稳健一年持有期混合A", None),
        ("tx", "⛔错类探针", "宝盈国家安全战略沪港深股票A", None),
    ];

    println!(
        "\n{:<4} {:<22} {:<38} {:<10} {}",
        "平台", "口径", "名称", "本地优先", "最终解析（代码）"
    );
    let mut hit_local = 0usize;
    let mut hit_final = 0usize;
    let mut mismatch: Vec<String> = Vec::new();
    for (pf, kind, name, expect) in &cases {
        let local = fundlens_lib::data::resolve_fund_code_local(name);
        if local.is_some() {
            hit_local += 1;
        }
        let resolved = fundlens_lib::data::resolve_fund_code(name);
        if resolved.is_some() {
            hit_final += 1;
        }
        let verdict = match (&resolved, expect) {
            (Some(got), Some(exp)) if got == exp => "✅".to_string(),
            (Some(got), Some(exp)) => {
                let v = format!("❌ 期望 {} 实得 {}", exp, got);
                mismatch.push(format!("{} → {}", name, v));
                v
            }
            (None, Some(exp)) => {
                let v = format!("⚠️ 未解析（期望 {}）", exp);
                mismatch.push(format!("{} → {}", name, v));
                v
            }
            (Some(got), None) => {
                // 无人为期望：把命中代码的官方名打出来供人工判断是否错类
                let canon = fundlens_lib::data::fetch_fund_name(got)
                    .unwrap_or_else(|| "(规范名取不到)".into());
                format!("🔍 {} = {}", got, canon)
            }
            (None, None) => "✅ 未解析（符合预期：宁缺勿错）".to_string(),
        };
        println!(
            "{:<4} {:<22} {:<38} {:<10} {:?}  {}",
            pf,
            kind,
            truncate(name, 36),
            local.clone().unwrap_or_else(|| "-".into()),
            resolved,
            verdict
        );
    }
    println!(
        "\n共 {} 例：本地优先命中 {}，最终解析 {}",
        cases.len(),
        hit_local,
        hit_final
    );
    if mismatch.is_empty() {
        println!("无错配/漏配告警 ✅");
    } else {
        println!("告警 {} 条：", mismatch.len());
        for m in &mismatch {
            println!("  - {}", m);
        }
    }
}
