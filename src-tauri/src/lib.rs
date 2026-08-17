// FundLens 库入口：注册 Tauri 命令与插件，初始化本地 SQLite。
pub mod commands;
pub mod db;
pub mod valuation;
pub mod ocr;
pub mod data;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 应用启动时确保本地数据库与迁移就绪
    if let Err(e) = crate::db::init_db() {
        eprintln!("FundLens 数据库初始化失败: {e}");
    }

    // 开发模式下：空库时种子演示基金，并实测三个免费数据源（A1/A2/A3）
    #[cfg(debug_assertions)]
    seed_demo_data();

    // 非阻塞刷新 A 股交易日历（远程拉取 + 内置兜底），避免阻塞启动；失败自动降级。
    std::thread::spawn(|| {
        crate::data::refresh_calendar();
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            crate::commands::get_overview,
            crate::commands::get_fund_detail,
            crate::commands::get_stats,
            crate::commands::export_db,
            crate::commands::import_db,
            crate::commands::import_screenshots,
            crate::commands::import_txn_screenshots,
            crate::commands::refresh_quotes,
            crate::commands::add_fund,
            crate::commands::update_position,
            crate::commands::delete_fund,
            crate::commands::list_disclosures,
            crate::commands::fetch_disclosure,
            crate::commands::fetch_all_disclosures,
            crate::commands::fetch_quotes,
            crate::commands::refresh_nav_history,
            crate::commands::get_fund_series,
            crate::commands::read_image_data_url,
            crate::commands::list_transactions,
            crate::commands::add_transaction,
            crate::commands::delete_transaction,
            crate::commands::import_transactions,
            crate::commands::get_weekly_report,
            crate::commands::get_monthly_report,
            crate::commands::get_pnl_calendar,
            crate::commands::write_text_file,
        ])
        .run(tauri::generate_context!())
        .expect("FundLens 启动失败");
}

/// 开发模式：空库时插入演示基金（易方达中小盘混合 110011）并实测三个免费数据源。
/// 通过 stderr 打印各源拉取结果，便于在 `tauri dev` 控制台直接验证联网可用性。
#[cfg(debug_assertions)]
fn seed_demo_data() {
    use crate::db;
    use crate::data;

    // 仅在空库时种子，避免污染已有数据
    if db::list_funds().map(|v| !v.is_empty()).unwrap_or(false) {
        return;
    }
    eprintln!("[FundLens][dev] 检测到空库，开始种子演示数据并实测三个数据源…");

    let code = "110011";
    let _ = db::insert_fund(&db::FundRow {
        code: code.into(),
        name: "易方达中小盘混合".into(),
        platform: "alipay".into(),
        official_nav: 1.0,
        report_period: None,
        disclosure_type: None,
        fund_type: String::new(),
        track_index: String::new(),
        valuation_applicable: true,
    });
    // 演示持仓基线（账户 1 = 默认账户），随后重算 positions 缓存
    let _ = db::set_baseline(1, code, 1000.0, 4196.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set");

    // A3 官方净值基线（lsjz 提供净值；类型用 fundsuggest 的 FTYPE，更可靠）
    match data::fetch_official_nav(code) {
        Some(nav) => {
            let ftype = data::fetch_fund_type(code).unwrap_or_default();
            eprintln!(
                "[FundLens][dev] A3 官方净值: nav={} date={} type={}({})",
                nav.nav,
                nav.nav_date,
                ftype,
                data::fund_type_label(&ftype)
            );
            let _ = db::update_fund_nav(
                code,
                nav.nav,
                &ftype,
                data::is_estimable_fund(&ftype),
                &nav.nav_date,
            );
        }
        None => eprintln!("[FundLens][dev] A3 官方净值拉取失败（fundgz 已失效，已切换 lsjz）"),
    }

    // A1 披露持仓（东财 F10 jjcc）
    match data::fetch_disclosure(code) {
        Some((period, dtype, holdings)) => {
            eprintln!(
                "[FundLens][dev] A1 披露持仓: {} 条 ({} · {})",
                holdings.len(),
                period,
                dtype
            );
            for h in &holdings {
                let _ = db::upsert_disclosure(code, &h.stock_code, &h.stock_name, h.weight, &period, &dtype);
            }
        }
        None => eprintln!("[FundLens][dev] A1 披露持仓拉取失败"),
    }

    // A2 实时行情（腾讯 qt.gtimg.cn 主源）
    match crate::commands::refresh_quotes() {
        Ok(out) => eprintln!("[FundLens][dev] A2 实时行情: 抓取 {} 条 @ {}", out.count, out.at),
        Err(e) => eprintln!("[FundLens][dev] A2 实时行情拉取失败: {e}"),
    }

    eprintln!("[FundLens][dev] 种子完成，可在 UI 查看本地自算实时估值。");
}
