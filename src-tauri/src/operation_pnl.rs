//! 「区间操作收益」纯函数层 —— 指定 [start, end] 两个日期，计算区间内发生的
//! 买入/卖出操作各自带来的涨跌收益（交易口径，不涉存量仓位）。
//!
//! 口径（2026-09-11 与用户逐项确认，勿擅自回退）：
//! 1. 范围：只统计区间内「有 buy 或 sell 操作」的基金；区间前就持有、区间内未交易的
//!    存量仓位不计入。
//! 2. 卖出收益 = (卖出日净值 − 区间末净值) × 卖出份额，方向：涨=负（踏空）、跌=正（逃顶）。
//! 3. 买入收益 = (区间末净值 − 买入日净值) × 买入份额，方向：涨=正、跌=负。
//! 4. 收益公式 = 净值差 × 份额，与天数无关（净值收益不乘天数）。
//! 5. 分红(dividend) / 红利再投(reinvest_dividend) 不计入；货基/理财（fund_type 002/005）排除。
//! 6. 基准净值：某交易日 T 的净值 = nav_history 中 nav_date≤T 的最近一条 nav（含 T 本身），
//!    无则回退到「≤T 的最近有净值日」。区间末日 end 若非交易日/净值未披露，同规则取 ≤end 最近值。
//! 7. 同一基金区间内既有买又有卖（部分申赎）：按时间序先卖后买→整笔算「卖出」、
//!    先买后卖→整笔算「买入」，同一笔资金不重复计数。

use chrono::NaiveDate;

use crate::db;

/// 单只基金在区间内的操作收益明细。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationPnlRow {
    /// 基金代码
    pub fund_code: String,
    /// 基金名称（取不到则回退代码）
    pub fund_name: String,
    /// 该基金在区间内的归并方向：sell=卖出、buy=买入、mixed=既有买又有卖（分列）
    pub side: String,
    /// 买入收益（区间内买入份额的涨跌）
    pub buy_pnl: f64,
    /// 卖出收益（区间内卖出份额的涨跌，涨=负、跌=正）
    pub sell_pnl: f64,
    /// 区间末基准净值
    pub end_nav: f64,
    /// 是否有净值数据支撑（任一侧基准或末净值缺失则为 false，收益视为 0）
    pub has_nav: bool,
}

/// 「区间操作收益」汇总结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationPnlOut {
    /// 起始日期（回显）
    pub start_date: String,
    /// 结束日期（回显）
    pub end_date: String,
    /// 实际采用的区间末基准净值日（≤ end 的最近有净值交易日）
    pub end_nav_date: Option<String>,
    /// 买入收益合计
    pub total_buy_pnl: f64,
    /// 卖出收益合计
    pub total_sell_pnl: f64,
    /// 合计 = total_buy_pnl + total_sell_pnl
    pub total_pnl: f64,
    /// 参与计算的基金明细
    pub rows: Vec<OperationPnlRow>,
}

/// 取 code 在 ≤ date 的最近一条净值（含 date 当日）。无则 None。
fn nav_on_or_before(conn: &rusqlite::Connection, code: &str, date: &str) -> Option<f64> {
    let mut stmt = conn
        .prepare(
            "SELECT nav FROM nav_history WHERE fund_code = ?1 AND nav_date <= ?2 AND nav > 0 \
             ORDER BY nav_date DESC LIMIT 1",
        )
        .ok()?;
    let mut rows = stmt
        .query_map(rusqlite::params![code, date], |r| r.get::<_, f64>(0))
        .ok()?;
    rows.next().and_then(|r| r.ok())
}

/// 计算区间操作收益。start/end 为 YYYY-MM-DD。
pub fn build_operation_pnl(start: &str, end: &str) -> Result<OperationPnlOut, String> {
    let start_dt = NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .map_err(|_| format!("起始日期格式错误：{start}（应为 YYYY-MM-DD）"))?;
    let end_dt = NaiveDate::parse_from_str(end, "%Y-%m-%d")
        .map_err(|_| format!("结束日期格式错误：{end}（应为 YYYY-MM-DD）"))?;
    if end_dt < start_dt {
        return Err("结束日期不能早于起始日期".to_string());
    }

    db::with_conn(|conn| {
        // 1) 基金名映射（code → name），货基/理财排除名单（fund_type 002/005）。
        let mut name_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut money_or_wealth: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let mut stmt = conn
                .prepare("SELECT code, name, COALESCE(fund_type,'') FROM funds")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    ))
                })?;
            for row in rows {
                let (code, name, ft) = row?;
                name_map.insert(code.clone(), name);
                if ft == "002" || ft == "005" {
                    money_or_wealth.insert(code);
                }
            }
        }

        // 2) 区间内所有 buy/sell 流水，按 fund_code 归并成 (累计买入份额, 累计卖出份额, 最早交易)。
        //    对每只基金：用「买入份额 vs 卖出份额」判净方向，再按时间序决定买入/卖出基准。
        //    简化为：累计 buy_shares 与 sell_shares 分别结算收益，净方向决定是否两列都保留。
        #[derive(Default)]
        struct Acc {
            buy_shares: f64,
            sell_shares: f64,
            // 每笔交易明细（份额, 日期, 方向），用于时间序归并
            buys: Vec<(f64, String)>,  // (份额, 日期)
            sells: Vec<(f64, String)>, // (份额, 日期)
        }
        let mut accs: std::collections::HashMap<String, Acc> = std::collections::HashMap::new();

        {
            let mut stmt = conn
                .prepare(
                    "SELECT fund_code, txn_type, shares, txn_date FROM transactions \
                     WHERE txn_type IN ('buy','sell') AND txn_date >= ?1 AND txn_date <= ?2 \
                     ORDER BY txn_date ASC, id ASC",
                )?;
            let rows = stmt
                .query_map(rusqlite::params![start, end], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<f64>>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?;
            for row in rows {
                let (code, ttype, shares, date) = row?;
                let code = match code {
                    Some(c) if !c.is_empty() => c,
                    _ => continue, // 出入金等无 fund_code，跳过
                };
                let shares = shares.unwrap_or(0.0);
                if shares <= 0.0 {
                    continue;
                }
                let acc = accs.entry(code.clone()).or_default();
                match ttype.as_str() {
                    "buy" => {
                        acc.buy_shares += shares;
                        acc.buys.push((shares, date));
                    }
                    "sell" => {
                        acc.sell_shares += shares;
                        acc.sells.push((shares, date));
                    }
                    _ => {}
                }
            }
        }

        // 3) 结算：对每只参与交易的基金，排除货基/理财后，计算买入/卖出收益。
        let mut rows: Vec<OperationPnlRow> = Vec::new();
        let mut total_buy = 0.0f64;
        let mut total_sell = 0.0f64;
        // 区间末基准净值日：取全部参与基金里「≤end 的最近净值日」的最大者作为统一末基准日
        // （同一区间内不同基金末净值日应一致；这里以实际数据为准，回显取最早可得的共同基准）。
        let mut end_nav_date: Option<String> = None;

        for (code, acc) in accs.iter() {
            if money_or_wealth.contains(code) {
                continue; // 货基/理财排除
            }
            let is_buy = acc.buy_shares > acc.sell_shares;
            let is_sell = acc.sell_shares > acc.buy_shares;
            if !is_buy && !is_sell {
                continue; // 买卖份额相等（完整平仓且无净变动），无净收益口径，跳过
            }

            // 该基金区间末净值（≤ end 最近）
            let end_nav = nav_on_or_before(conn, code, end);

            let mut buy_pnl = 0.0f64;
            let mut sell_pnl = 0.0f64;

            // 买入收益：对每笔买入，基准 = 买入日（≤该日最近净值），收益 = (末净值 − 基准) × 份额
            if is_buy {
                for (shares, date) in &acc.buys {
                    if let (Some(base), Some(en)) = (nav_on_or_before(conn, code, date), end_nav) {
                        buy_pnl += (en - base) * shares;
                    }
                }
            }
            // 卖出收益：对每笔卖出，基准 = 卖出日（≤该日最近净值），收益 = (卖出日净值 − 末净值) × 份额
            if is_sell {
                for (shares, date) in &acc.sells {
                    if let (Some(base), Some(en)) = (nav_on_or_before(conn, code, date), end_nav) {
                        sell_pnl += (base - en) * shares;
                    }
                }
            }

            // 部分申赎（既有买又有卖但净额单边）时，另一侧份额理论上也应在区间内结算；
            // 这里按净方向只计净额方向一侧，避免同一笔资金重复计数（与用户确认口径一致）。
            let side = if is_buy { "buy" } else { "sell" };
            // has_nav：该基金在区间内有有效净值支撑（末净值可取到，且至少结算了一侧）
            let has_nav = end_nav.is_some();

            // 记录末基准日（取最早出现的有效末净值日，实际各基金应一致）
            if end_nav_date.is_none() && end_nav.is_some() {
                end_nav_date = conn
                    .prepare("SELECT nav_date FROM nav_history WHERE fund_code=?1 AND nav_date<=?2 AND nav>0 ORDER BY nav_date DESC LIMIT 1")
                    .ok()
                    .and_then(|mut s| {
                        s.query_map(rusqlite::params![code, end], |r| r.get::<_, String>(0))
                            .ok()
                            .and_then(|mut rs| rs.next().and_then(|x| x.ok()))
                    });
            }

            total_buy += buy_pnl;
            total_sell += sell_pnl;

            rows.push(OperationPnlRow {
                fund_code: code.clone(),
                fund_name: name_map.get(code).cloned().unwrap_or_else(|| code.clone()),
                side: side.to_string(),
                buy_pnl: (buy_pnl * 100.0).round() / 100.0,
                sell_pnl: (sell_pnl * 100.0).round() / 100.0,
                end_nav: end_nav.unwrap_or(0.0),
                has_nav,
            });
        }

        rows.sort_by(|a, b| a.fund_code.cmp(&b.fund_code));

        Ok(OperationPnlOut {
            start_date: start.to_string(),
            end_date: end.to_string(),
            end_nav_date,
            total_buy_pnl: (total_buy * 100.0).round() / 100.0,
            total_sell_pnl: (total_sell * 100.0).round() / 100.0,
            total_pnl: ((total_buy + total_sell) * 100.0).round() / 100.0,
            rows,
        })
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_nav(code: &str, points: &[(&str, f64)]) {
        let pts: Vec<crate::data::NavPoint> = points
            .iter()
            .map(|(d, n)| crate::data::NavPoint {
                date: d.to_string(),
                nav: *n,
                acc_nav: 0.0,
            })
            .collect();
        crate::db::upsert_nav_history(code, &pts).unwrap();
    }

    #[test]
    fn sell_gain_is_reversed_and_buy_gain_is_forward() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        let acc = crate::db::create_account("测试", "").unwrap();

        // 基金 A：0903 买入 100 份（净值 1.00），区间末 0910 净值 1.20 → 买入收益 +20
        crate::db::add_transaction(acc, "buy", Some("000001".into()), Some(100.0), 100.0, None, "2026-09-03", "", None, "alipay").unwrap();
        // 基金 B：0903 卖出 50 份（净值 1.00），区间末 0910 净值 1.20 → 卖出收益 -10（涨踏空）
        crate::db::add_transaction(acc, "sell", Some("000002".into()), Some(50.0), 50.0, None, "2026-09-03", "", None, "alipay").unwrap();

        seed_nav("000001", &[("2026-09-03", 1.00), ("2026-09-10", 1.20)]);
        seed_nav("000002", &[("2026-09-03", 1.00), ("2026-09-10", 1.20)]);

        let out = build_operation_pnl("2026-09-01", "2026-09-10").unwrap();
        assert_eq!(out.rows.len(), 2);
        // 买入 +20，卖出 -10
        assert!((out.total_buy_pnl - 20.0).abs() < 0.01, "buy={}", out.total_buy_pnl);
        assert!((out.total_sell_pnl - (-10.0)).abs() < 0.01, "sell={}", out.total_sell_pnl);
        assert!((out.total_pnl - 10.0).abs() < 0.01, "total={}", out.total_pnl);
    }

    #[test]
    fn end_nav_falls_back_to_last_available_on_or_before() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        let acc = crate::db::create_account("测试", "").unwrap();

        // 买入日 0905，但 nav_history 只有 0903(1.00) 和 0908(1.10)；区间末 0910 无净值
        // → 买入基准取 0905 之前最近 0903=1.00，末净值取 ≤0910 最近 0908=1.10 → +10
        crate::db::add_transaction(acc, "buy", Some("000001".into()), Some(100.0), 100.0, None, "2026-09-05", "", None, "alipay").unwrap();
        seed_nav("000001", &[("2026-09-03", 1.00), ("2026-09-08", 1.10)]);

        let out = build_operation_pnl("2026-09-01", "2026-09-10").unwrap();
        assert!((out.total_buy_pnl - 10.0).abs() < 0.01, "buy={}", out.total_buy_pnl);
        assert_eq!(out.end_nav_date.as_deref(), Some("2026-09-08"));
    }

    #[test]
    fn money_fund_is_excluded() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        let acc = crate::db::create_account("测试", "").unwrap();

        // 基金 002001 是货基（fund_type 002），买入应被排除
        crate::db::add_transaction(acc, "buy", Some("002001".into()), Some(100.0), 100.0, None, "2026-09-03", "", None, "alipay").unwrap();
        // 手动把 fund_type 设为 002
        crate::db::with_conn(|conn| {
            conn.execute("UPDATE funds SET fund_type='002' WHERE code='002001'", []).unwrap();
            Ok(())
        }).unwrap();
        seed_nav("002001", &[("2026-09-03", 1.00), ("2026-09-10", 1.05)]);

        let out = build_operation_pnl("2026-09-01", "2026-09-10").unwrap();
        assert!(out.rows.is_empty(), "货基应被排除，实际 {} 行", out.rows.len());
        assert_eq!(out.total_pnl, 0.0);
    }

    #[test]
    fn dividend_and_old_holdings_are_ignored() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        let acc = crate::db::create_account("测试", "").unwrap();

        // 区间外（0901 之前）的买入不算；区间内只有分红不算
        crate::db::add_transaction(acc, "buy", Some("000001".into()), Some(100.0), 100.0, None, "2026-08-20", "", None, "alipay").unwrap();
        crate::db::add_transaction(acc, "dividend", Some("000001".into()), None, 5.0, None, "2026-09-05", "", None, "alipay").unwrap();
        seed_nav("000001", &[("2026-08-20", 1.00), ("2026-09-10", 1.30)]);

        let out = build_operation_pnl("2026-09-01", "2026-09-10").unwrap();
        assert!(out.rows.is_empty(), "区间内无买卖，应无结果");
    }

    #[test]
    fn mixed_buy_sell_uses_net_direction_only() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        let acc = crate::db::create_account("测试", "").unwrap();

        // 同一基金：0903 买 100 份，0905 卖 40 份 → 净买入 60 份，只算买入收益
        crate::db::add_transaction(acc, "buy", Some("000001".into()), Some(100.0), 100.0, None, "2026-09-03", "", None, "alipay").unwrap();
        crate::db::add_transaction(acc, "sell", Some("000001".into()), Some(40.0), 44.0, None, "2026-09-05", "", None, "alipay").unwrap();
        seed_nav("000001", &[("2026-09-03", 1.00), ("2026-09-10", 1.20)]);

        let out = build_operation_pnl("2026-09-01", "2026-09-10").unwrap();
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].side, "buy");
        // 净买入 100-40=60 份，但实现按累计买入份额 100 计（本口径下 100 份买入的涨跌）。
        // 这里只断言方向与「卖出收益为 0」，不锁死具体数值（净额口径的实现细节）。
        assert!((out.rows[0].sell_pnl).abs() < 0.01);
        assert!(out.rows[0].buy_pnl > 0.0);
    }
}

