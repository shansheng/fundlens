// 免费公开数据源（v0.1）
// - 个股实时行情：腾讯财经 qt.gtimg.cn / 东方财富 push2（A 股，红涨绿跌）
// - 基金披露持仓：天天基金/东方财富 F10（按当前可获取的最新报告期）
// 详见 SPEC.md 第 4 节。本模块只做最小可用 HTTP 拉取 + 解析，失败安全返回 None。

use std::collections::HashMap;

use chrono::Datelike;
use chrono::Timelike;

use crate::valuation::{DisclosedHolding, StockQuote};

/// 东财接口返回 GBK 编码，reqwest 默认按 UTF-8 解码会乱码；统一用 encoding_rs 解码。
fn decode_gbk(bytes: &[u8]) -> String {
    let (cow, _, _) = encoding_rs::GBK.decode(bytes);
    cow.into_owned()
}

/// 智能解码：优先按 UTF-8，失败（如腾讯行情的 GBK）再回退 GBK。
/// 适用「内容编码不确定」的接口（如东财 F10 披露持仓接口当前返回 UTF-8）。
fn decode_body(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => decode_gbk(bytes),
    }
}

/// 当前是否为 A 股交易时段（9:30-11:30, 13:00-15:00，周一至周五）
pub fn is_trading_now() -> bool {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let weekday = now.weekday().num_days_from_monday(); // 0=Mon
    if weekday >= 5 {
        return false;
    }
    let secs = now.num_seconds_from_midnight();
    let morning = secs >= 9 * 3600 + 30 * 60 && secs <= 11 * 3600 + 30 * 60;
    let afternoon = secs >= 13 * 3600 && secs <= 15 * 3600;
    morning || afternoon
}

/// 今天是否为工作日（周一至周五）。盘后（收盘后、盘前）仍属工作日，
/// 此时平台会更新当日 gsz/dwjz，可用于计算「当日实际收益」；周末/节假日则无更新。
pub fn is_weekday_now() -> bool {
    use chrono::Datelike;
    let weekday = chrono::Local::now().weekday().num_days_from_monday(); // 0=Mon
    weekday < 5
}

/// 穿透估值用的基准指数：按「基金类型 + 基金名称」识别标的指数，
/// 替换原先「所有权益类统一用沪深300」的粗放做法。
///
/// 为什么更准：指数/ETF联接基金的未披露部分（现金+其余成分）本就是其跟踪指数的成分，
/// 用真实标的指数近似在数学上远比沪深300贴近；债券基金未披露部分多为债券，用国债指数比沪深300准。
///
/// 返回 (gtimg_symbol, digit_code, 中文名)。失败安全：网络异常时调用方退化为零波动近似。
pub fn pick_benchmark(fund_type: &str, fund_name: &str) -> (String, String, String) {
    let name = fund_name.to_lowercase();
    // 1) 名称关键字优先匹配具体标的指数（覆盖绝大多数宽基/热门指数基金、ETF 与行业/主题基金）
    //    (关键字, gtimg符号, 数字代码, 中文名)
    let name_rules: &[(&str, &str, &str, &str)] = &[
        // —— 宽基 / 热门指数（既有）——
        ("沪深300", "sh000300", "000300", "沪深300"),
        ("中证500", "sh000905", "000905", "中证500"),
        ("中证1000", "sh000852", "000852", "中证1000"),
        ("创业板", "sz399006", "399006", "创业板指"),
        ("科创50", "sh000688", "000688", "科创50"),
        ("上证50", "sh000016", "000016", "上证50"),
        ("中证红利", "sh000922", "000922", "中证红利"),
        ("红利低波", "sh000922", "000922", "中证红利"),
        ("深证成指", "sz399001", "399001", "深证成指"),
        // —— 行业 / 主题指数（未披露部分用对应行业指数近似，而非笼统沪深300）——
        // 新能源车 / 光伏 / 新能源（顺序：新能源车 必须在 新能源 之前）
        ("新能源车", "sz399976", "399976", "CS新能车"),
        ("光伏", "sz399808", "399808", "中证新能"),
        ("新能源", "sz399808", "399808", "中证新能"),
        // 白酒 / 酒（顺序：必须在 消费 之前）
        ("白酒", "sz399997", "399997", "中证白酒"),
        ("酒", "sz399997", "399997", "中证白酒"),
        // 医疗 / 医药（顺序：医疗 必须在 医药 之前）
        ("医疗", "sz399989", "399989", "中证医疗"),
        ("医药", "sh000933", "000933", "中证医药"),
        // 金融地产
        ("券商", "sz399975", "399975", "证券公司"),
        ("证券", "sz399975", "399975", "证券公司"),
        ("银行", "sz399986", "399986", "中证银行"),
        ("地产", "sz399393", "399393", "国证地产"),
        // 科技制造
        ("军工", "sz399967", "399967", "中证军工"),
        ("芯片", "sz980017", "980017", "国证芯片"),
        ("半导体", "sz980017", "980017", "国证芯片"),
        ("电子", "sz399811", "399811", "CSSW电子"),
        ("传媒", "sz399971", "399971", "中证传媒"),
        // 周期与资源
        ("煤炭", "sz399998", "399998", "中证煤炭"),
        ("钢铁", "sz399440", "399440", "国证钢铁"),
        ("有色", "sz399395", "399395", "国证有色"),
        ("基建", "sz399995", "399995", "基建工程"),
        // 其他主题
        ("消费", "sh000932", "000932", "中证消费"),
        ("农业", "sh000949", "000949", "中证农业"),
        ("环保", "sh000827", "000827", "中证环保"),
    ];
    for (kw, sym, code, cn) in name_rules {
        if name.contains(&kw.to_lowercase()) {
            return (sym.to_string(), code.to_string(), cn.to_string());
        }
    }
    // 2) 按类型兜底：债券→国债指数；其余权益类（含指数/ETF联接/QDII/货币）→沪深300 宽基代理
    match fund_type {
        "004" => ("sh000012".to_string(), "000012".to_string(), "上证国债".to_string()),
        _ => ("sh000300".to_string(), "000300".to_string(), "沪深300".to_string()),
    }
}

/// 拉取一批实时行情（腾讯 qt.gtimg.cn 格式，GBK 编码）
/// codes: 如 ["sh600519", "sz000858", "hk00700"]
/// 返回 key = 纯数字代码（如 "600519"/"00700"），与披露持仓 stock_code 对齐。
pub fn fetch_quotes(codes: &[String]) -> Option<HashMap<String, StockQuote>> {
    if codes.is_empty() {
        return Some(HashMap::new());
    }
    let joined = codes.join(",");
    let url = format!("https://qt.gtimg.cn/q={joined}");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client.get(&url).send().ok()?;
    let bytes = resp.bytes().ok()?;
    let body = decode_gbk(&bytes);

    let mut map = HashMap::new();
    for line in body.lines() {
        let line = line.trim();
        // 仅处理 v_xxx="..." 形式；v_xxx 即变量名，含交易所前缀与代码
        if !line.starts_with("v_") {
            continue;
        }
        // 格式：v_hk00700="100~腾讯控股~00700~441.000~461.600~..."
        let eq = match line.find('=') {
            Some(i) => i,
            None => continue,
        };
        // 变量名中的数字即为真实代码（去掉 v_ 前缀后取数字）
        let stock_code: String = line[..eq]
            .trim_start_matches("v_")
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        if stock_code.is_empty() {
            continue;
        }
        // 取值部分（两引号之间）
        let rest = &line[eq + 1..];
        let vstart = match rest.find('"') {
            Some(i) => i + 1,
            None => continue,
        };
        let vend = match rest[vstart..].find('"') {
            Some(i) => vstart + i,
            None => continue,
        };
        let payload = &rest[vstart..vend];
        let parts: Vec<&str> = payload.split('~').collect();
        // parts: [状态, 名称, 代码, 当前价, 昨收, ...]
        if parts.len() < 5 {
            continue;
        }
        let price: f64 = parts[3].parse().unwrap_or(0.0);
        let prev_close: f64 = parts[4].parse().unwrap_or(0.0);
        let name = if parts.len() > 1 {
            parts[1].to_string()
        } else {
            String::new()
        };
        map.insert(
            stock_code.clone(),
            StockQuote {
                stock_code,
                name,
                price,
                prev_close,
            },
        );
    }
    Some(map)
}

/// 拉取基金披露持仓（东方财富 F10，按最新报告期）
/// 返回 (report_period, disclosure_type, holdings)
/// 注：真实环境需按"财务数据时效性原则"动态选择最新披露期（季报/中报/年报）。
/// 报告期结束日（用于时效排序与「是否已发生」判断）。
/// 季末：一季报 3/31、中报 6/30、三季报 9/30、年报 12/31。
fn period_end(year: i32, season: u32) -> chrono::NaiveDate {
    match season {
        1 => chrono::NaiveDate::from_ymd_opt(year, 3, 31).unwrap_or(chrono::NaiveDate::MAX),
        2 => chrono::NaiveDate::from_ymd_opt(year, 6, 30).unwrap_or(chrono::NaiveDate::MAX),
        3 => chrono::NaiveDate::from_ymd_opt(year, 9, 30).unwrap_or(chrono::NaiveDate::MAX),
        4 => chrono::NaiveDate::from_ymd_opt(year, 12, 31).unwrap_or(chrono::NaiveDate::MAX),
        _ => chrono::NaiveDate::MAX,
    }
}

/// 根据当前日期动态选取「最新可获取报告期」的抓取候选列表。
///
/// 财务数据时效性原则（A 股）：一季报(季末 3/31，4 月底前披露)、中报(6/30，8 月底前)、
/// 三季报(9/30，10 月底前)、年报(12/31，次年 4 月底前)。故「此刻能拉到的最新一期」
/// = 报告期结束日（period_end）已过、处于披露窗口内的那一期；该期若尚未披露则逐期回退。
///
/// 返回按报告期结束日【从新到旧】排序的候选 (year, season, disclosure_type)：
/// - 中报(2)/年报(4) 披露全持仓 → "full"
/// - 一季报(1)/三季报(3) 仅披露前十大 → "top10"
/// 调用方按顺序尝试，首个返回非空持仓的即为准——天然优先最新期次，且对「该期尚未披露」
/// 的基金自动回退到上一期（如 8 月初部分基金中报未出 → 回退到当年一季报）。
fn candidate_periods(now: chrono::NaiveDate) -> Vec<(i32, u32, &'static str)> {
    let mut all: Vec<(i32, u32, &'static str)> = Vec::new();
    // 覆盖当前年及往前 3 年（足够回退），不前瞻未发生的财年
    for year in (now.year() - 3)..=now.year() {
        all.push((year, 1, "top10")); // 一季报
        all.push((year, 2, "full")); // 中报（全持仓）
        all.push((year, 3, "top10")); // 三季报
        all.push((year, 4, "full")); // 年报（全持仓）
    }
    // 仅保留「报告期已结束」的期次（period_end <= now），避免抓取尚未发生的季度
    all.retain(|(y, s, _)| period_end(*y, *s) <= now);
    // 按报告期结束日从新到旧排序
    all.sort_by(|a, b| period_end(b.0, b.1).cmp(&period_end(a.0, a.1)));
    // 限制最多尝试 12 期，避免极端情况下过多网络请求
    all.truncate(12);
    all
}

pub fn fetch_disclosure(fund_code: &str) -> Option<(String, String, Vec<DisclosedHolding>)> {
    // 动态选取最新可获取报告期候选（按当前日期），优先尝试最新期次，逐期回退。
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let now = chrono::Local::now().naive_local().date();
    for (year, season, dtype) in candidate_periods(now) {
        let url = format!(
            "https://fundf10.eastmoney.com/FundArchivesDatas.aspx?type=jjcc&code={}&topline=10&year={}&season={}",
            fund_code, year, season
        );
        let resp = match client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .header("Referer", "https://fundf10.eastmoney.com/")
            .send()
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        let status = resp.status();
        let bytes = match resp.bytes() {
            Ok(b) => b,
            Err(_) => continue,
        };
        let body = decode_body(&bytes);
        let mut rows = parse_holding_table(&body);
        // 用当前报告期/口径覆盖（解析器只抽字段，不判断口径）
        let report_period =
            extract_curyear(&body).unwrap_or_else(|| format!("{}Q{}", year, season));
        for h in &mut rows {
            h.disclosure_type = dtype.to_string();
            h.report_period = report_period.clone();
        }
        #[cfg(debug_assertions)]
        eprintln!(
            "[FundLens][dev] disclosure year={year} season={season} status={} rows={}",
            status,
            rows.len()
        );
        if !rows.is_empty() {
            return Some((report_period, dtype.to_string(), rows));
        }
    }
    None
}

fn extract_curyear(body: &str) -> Option<String> {
    // 报告期标题形如「2026年2季度」「2026年第2季度」「2026年第二季度」。
    // 从「YYYY年N季」提取（N∈{1,2,3,4}，支持数字或中文数字一二三四，容忍「第」字），
    // 避免误命中正文日期（如「2026年08月13日」不含『季』字）。
    // 东财会把同一基金多季度各渲染一张表，只取第一张 <tbody>（最新报告期），故首个匹配即最新期。
    let quarter_of = |token: char| -> Option<u32> {
        match token {
            '1' | '一' => Some(1),
            '2' | '二' => Some(2),
            '3' | '三' => Some(3),
            '4' | '四' => Some(4),
            _ => None,
        }
    };
    // 关键修正：原始实现用字节偏移 body[abs-4..abs] 回退取年份，当中文字符（3 字节）出现在
    // 「年」前 4 字节内时，abs-4 会落在某个汉字的中间，触发
    // "start byte index is not a char boundary" panic 并令整个 app 崩溃。
    // 这里改用字符向量索引，保证 year 始终由完整字符组成，绝不会越字符边界。
    let chars: Vec<char> = body.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    while i < n {
        if chars[i] != '年' {
            i += 1;
            continue;
        }
        // 「年」前须有 4 位年份数字（字符维度，天然对齐边界）
        if i < 4 {
            i += 1;
            continue;
        }
        let year: String = chars[i - 4..i].iter().collect();
        if !year.chars().all(|c| c.is_ascii_digit()) {
            i += 1;
            continue;
        }
        // 取「年」之后最多 16 个字符作为报告期标题窗口（按字符截取，避免字节越界）
        let end = std::cmp::min(n, i + 1 + 16);
        let rest: String = chars[i + 1..end].iter().collect();
        // 窗口内须含「季」字（日期如「08月」不含『季』，从而排除）；取其前最近的有效季度标记
        if let Some(p) = rest.find('季') {
            let before: String = rest[..p].chars().collect();
            if let Some(last) = before.chars().last() {
                if let Some(qn) = quarter_of(last) {
                    return Some(format!("{}Q{}", year, qn));
                }
            }
        }
        i += 1;
    }
    // 回退：curyear: 标记（部分接口直接给出年份）
    if let Some(idx) = body.find("curyear:") {
        let rest = &body[idx + "curyear:".len()..];
        let year: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if year.len() == 4 {
            return Some(format!("{}Q?", year));
        }
    }
    None
}

// 从东财 jjcc 的 HTML 表格行提取「股票代码 / 股票名称 / 占净值比例」。
// 注意：表格在 名称 与 占净值 之间有一列「股吧/行情」链接列，列序不固定，
// 因此不依赖固定下标，而是按「5~6 位纯数字=代码」「以 % 结尾的数字=占比」定位。
// 解析策略：先以 </td> 作为单元格分隔符（替换为 |），再整体剥离 HTML 标签，
// 这样单元格边界保留、标签残留被清除——避免「取最后一个 > 之后」在
// 「>00700</a>」此类结构下取到空串的问题。
fn parse_holding_table(body: &str) -> Vec<DisclosedHolding> {
    // 东财会把同一基金的多个季度快照（如 2025Q1~Q4）各渲染一张表。
    // 只取第一张 <tbody>（最新报告期）的行，避免跨季度持仓被合并成「曾持有」集合。
    let region = if let Some(s) = body.find("<tbody") {
        let after = &body[s..];
        if let Some(e) = after.find("</tbody>") {
            &after[..e]
        } else {
            body
        }
    } else {
        body
    };
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for tr in region.split("</tr>") {
        let delimited = tr.replace("</td>", "|").replace("</th>", "|");
        let plain = strip_tags(&delimited);
        let cells: Vec<String> = plain
            .split('|')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();

        // 股票代码：5~6 位纯数字（A 股 6 位，如 600519；港股 5 位带前导零，如 00700）
        let code_idx = cells.iter().position(|c| {
            (5..=6).contains(&c.len()) && c.chars().all(|ch| ch.is_ascii_digit())
        });
        let code_idx = match code_idx {
            Some(i) => i,
            None => continue,
        };
        let code = cells[code_idx].clone();

        // 同接口常把同一张持仓表重复返回多次，按代码去重，只保留首次出现
        if !seen.insert(code.clone()) {
            continue;
        }

        // 占净值比例：以 '%' 结尾的正数（如 9.96%）
        let weight = cells.iter().find_map(|c| {
            let trimmed = c.trim();
            if trimmed.ends_with('%') {
                let v = trimmed.trim_end_matches('%').replace(',', "").parse::<f64>().ok()?;
                if v > 0.0 {
                    return Some(v);
                }
            }
            None
        });
        let weight = match weight {
            Some(w) => w,
            None => continue,
        };

        // 名称：代码后一列
        let name = cells.get(code_idx + 1).cloned().unwrap_or_default();

        out.push(DisclosedHolding {
            stock_code: code,
            stock_name: name,
            weight: weight / 100.0,
            report_period: "latest".into(),
            disclosure_type: "top10".into(),
        });
    }
    out
}

/// 剥离 HTML 标签，保留标签之间的纯文本
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {
                if !in_tag {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// 官方净值基线（替代已失效的 fundgz.1234567.com.cn）
/// 端点：东财 F10 历史净值 lsjz API（JSON，需 UA + Referer）
/// 返回 DWJZ(单位净值) / FSRQ(净值日期) / FundType(类型码)
#[derive(Debug, Clone)]
pub struct OfficialNav {
    pub nav: f64,
    pub nav_date: String,
    pub fund_type: String,
}

pub fn fetch_official_nav(fund_code: &str) -> Option<OfficialNav> {
    let url = format!(
        "https://api.fund.eastmoney.com/f10/lsjz?fundCode={}&pageIndex=1&pageSize=1",
        fund_code
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fundf10.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let data = v.get("Data")?;
    let list = data.get("LSJZList")?.as_array()?;
    let first = list.first()?;
    let nav: f64 = first.get("DWJZ")?.as_str()?.parse().ok()?;
    let nav_date = first.get("FSRQ").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let fund_type = data.get("FundType").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(OfficialNav { nav, nav_date, fund_type })
}

// ============ 历史净值（东财 F10 lsjz 历史净值接口）============
// 端点：api.fund.eastmoney.com/f10/lsjz（JSON，需 UA + Referer）
// 返回 LSJZList：FSRQ=净值日期(YYYY-MM-DD) / DWJZ=单位净值 / LJJZ=累计净值。
// 用于「基金净值走势图」：自动拉取 + 本地 nav_history 缓存（见 db.rs）。

/// 单个历史净值点（按日期升序排列后供图表使用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NavPoint {
    pub date: String, // FSRQ，YYYY-MM-DD
    pub nav: f64,     // DWJZ 单位净值
    pub acc_nav: f64, // LJJZ 累计净值（缺失记为 0）
}

/// 拉取基金历史净值（失败安全：超时/解析失败返回 None）。
/// `months` = 期望覆盖的月数（用于估算 pageSize）；传 0 表示全量（约 10000 条，覆盖数十年）。
/// 接口默认按日期降序返回，返回前会在 parse 内排序为升序。
pub fn fetch_nav_history(code: &str, months: u32) -> Option<Vec<NavPoint>> {
    let page_size = if months == 0 {
        10000
    } else {
        (months.saturating_mul(22).saturating_add(10)).min(10000)
    };
    let url = format!(
        "https://api.fund.eastmoney.com/f10/lsjz?fundCode={}&pageIndex=1&pageSize={}",
        code, page_size
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fundf10.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    parse_nav_history(&body)
}

/// 将东财 lsjz 返回的 JSON 解析为升序历史净值序列（纯函数，便于离线单测）。
/// 跳过 DWJZ 为空（分红除息日等无净值）的记录；LJJZ 缺失记为 0。
pub fn parse_nav_history(body: &str) -> Option<Vec<NavPoint>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = v.get("Data")?;
    let list = data.get("LSJZList")?.as_array()?;
    let mut out: Vec<NavPoint> = Vec::new();
    for item in list {
        // FSRQ 可能为 "2026-08-13" 形式；不足 10 位视为无效
        let date = item.get("FSRQ").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if date.len() < 10 {
            continue;
        }
        // DWJZ 可能以字符串或数字形式出现；统一转字符串再解析
        let raw = match item.get("DWJZ") {
            Some(x) => x
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| x.as_f64().map(|n| n.to_string()))
                .unwrap_or_default(),
            None => String::new(),
        };
        let nav: f64 = match raw.trim().parse::<f64>() {
            Ok(n) if n > 0.0 => n,
            _ => continue, // 无净值（如分红除息日）跳过
        };
        let acc_raw = match item.get("LJJZ") {
            Some(x) => x
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| x.as_f64().map(|n| n.to_string()))
                .unwrap_or_default(),
            None => String::new(),
        };
        let acc_nav: f64 = acc_raw.trim().parse::<f64>().unwrap_or(0.0);
        out.push(NavPoint { date, nav, acc_nav });
    }
    if out.is_empty() {
        return None;
    }
    // FSRQ 为 ISO 格式，字典序即时间序 → 升序排列便于图表从左到右时间递增
    out.sort_by(|a, b| a.date.cmp(&b.date));
    Some(out)
}

// ============ 盘中实时估值（天天基金 gsz 估算净值）============
// 端点：fundgz.tenorfun.com/js/{code}.js（JSONP，返回 jsonpgz({...})）
// 提供交易时段内每只开放式基金的「估算净值 gsz」与「单位净值 dwjz」，
// 由此可得盘中实时涨跌幅 = gsz/dwjz - 1。该值由平台直接计算，
// 覆盖面广（股票/混合/指数/ETF联接/债券等多数开放式基金），
// 优于仅依赖披露持仓的本地自算估值；故作为「优先来源」。

/// 盘中实时估值结果
#[derive(Debug, Clone)]
pub struct FundEstimate {
    pub est_nav: f64,        // 估算净值 gsz
    pub est_change_pct: f64, // 估算涨跌幅 = gsz/dwjz - 1
    pub prev_nav: f64,       // 单位净值 dwjz（上一交易日），用于计算当日实际收益 = gsz - dwjz
    pub gztime: String,      // 估值时间，如 "2026-08-15 14:30"
}

/// 解析天天基金 JSONP：jsonpgz({...}) → FundEstimate。纯函数，便于离线单测。
pub fn parse_fund_estimate(body: &str) -> Option<FundEstimate> {
    let s = body.trim();
    let s = s.strip_prefix("jsonpgz(")?;
    let s = s.strip_suffix(';').unwrap_or(s);
    let s = s.strip_suffix(')').unwrap_or(s);
    let s = s.trim();
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let gsz: f64 = v.get("gsz")?.as_str()?.parse().ok()?;
    let dwjz: f64 = v.get("dwjz")?.as_str()?.parse().ok()?;
    if dwjz <= 0.0 {
        return None;
    }
    let est_change_pct = gsz / dwjz - 1.0;
    let gztime = v.get("gztime").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(FundEstimate {
        est_nav: gsz,
        est_change_pct,
        prev_nav: dwjz,
        gztime,
    })
}

/// 联网拉取单只基金盘中实时估值（失败安全：超时/解析失败返回 None）。
pub fn fetch_fund_estimate(code: &str) -> Option<FundEstimate> {
    let url = format!("https://fundgz.tenorfun.com/js/{code}.js");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fundf10.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    parse_fund_estimate(&body)
}

/// 并发拉取多只基金的盘中实时估值（线程池并行，单只 4s 超时），
/// 返回成功解析的子集。用于总览页一次性获取全部基金估值，避免串行阻塞。
pub fn fetch_fund_estimates(codes: &[String]) -> HashMap<String, FundEstimate> {
    use std::thread;
    let mut out = HashMap::new();
    if codes.is_empty() {
        return out;
    }
    let results: Vec<(String, Option<FundEstimate>)> = thread::scope(|s| {
        let handles: Vec<_> = codes
            .iter()
            .map(|c| {
                let code = c.clone();
                s.spawn(move || (code.clone(), fetch_fund_estimate(&code)))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (c, est) in results {
        if let Some(e) = est {
            out.insert(c, e);
        }
    }
    out
}

/// 估值是否为「当日新鲜」：gztime 以今日日期开头，说明平台当前仍在更新。
/// 非交易时段 gztime 停留在上一交易日，应判定为不新鲜、不采用。
pub fn estimate_is_fresh(gztime: &str) -> bool {
    if gztime.len() < 10 {
        return false;
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    gztime.starts_with(&today)
}

/// 是否适用本地自算估值（基于东财 FundType 码）
/// 适用：股票型(001)/混合型(007)/指数型(008)/ETF联接(009)
/// 不适用：货币型(002)/QDII(003)/债券型(004)/理财型(005)/分级(006)
pub fn is_equity_fund(fund_type: &str) -> bool {
    matches!(fund_type, "001" | "007" | "008" | "009")
}

/// 是否 QDII 基金（海外投资，净值 T+1/T+2 确认；境外未收盘时不应把平台 gsz 当作「当日收益」）。
pub fn is_qdii_fund(fund_type: &str) -> bool {
    fund_type == "003"
}

/// QDII 跟踪的海外市场区域（用于判定境外交易时段）：us=美股 / hk=港股 / other=其他（默认按美股时区处理）。
fn qdii_overseas_region(fund_name: &str) -> &'static str {
    if fund_name.contains("香港")
        || fund_name.contains("恒生")
        || fund_name.contains("港股")
        || fund_name.contains("H股")
        || fund_name.contains("沪港")
    {
        "hk"
    } else if fund_name.contains("美国")
        || fund_name.contains("纳斯达克")
        || fund_name.contains("标普")
        || fund_name.contains("道琼")
        || fund_name.contains("环球")
        || fund_name.contains("全球")
        || fund_name.contains("海外")
        || fund_name.contains("美元")
        || fund_name.contains("纳指")
    {
        "us"
    } else {
        // 未明确标识的 QDII 默认按美股时区处理（多数 QDII 主投美股）
        "other"
    }
}

/// 近似判断当前是否处于美股夏令时：3 月第 2 个周日 ~ 11 月第 1 个周日。
fn is_us_daylight(now: &chrono::DateTime<chrono::Local>) -> bool {
    let m = now.month();
    let d = now.day();
    (m > 3 && m < 11) || (m == 3 && d >= 8) || (m == 11 && d < 1)
}

/// 当前北京时间下，该 QDII 基金所跟踪的海外市场是否处于交易时段。
/// - 港股：09:30–16:00（北京，无时令）
/// - 美股：夏令时 21:30–04:00、冬令时 22:30–05:00（北京，跨午夜）
/// 用于「境外未收盘时不报当日」的 T+1 建模：海外交易中则平台 gsz 仍在形成、非终值，应抑制「当日收益」。
pub fn qdii_overseas_open(fund_name: &str) -> bool {
    let now = chrono::Local::now();
    let t = now.hour() as i32 * 60 + now.minute() as i32; // 北京分钟数（0~1439）
    match qdii_overseas_region(fund_name) {
        "hk" => t >= 9 * 60 + 30 && t < 16 * 60,
        _ => {
            let edt = is_us_daylight(&now);
            let (open, close) = if edt { (21 * 60 + 30, 4 * 60) } else { (22 * 60 + 30, 5 * 60) };
            if open > close {
                // 跨午夜：如 21:30–04:00
                t >= open || t < close
            } else {
                t >= open && t < close
            }
        }
    }
}

pub fn fund_type_label(fund_type: &str) -> &'static str {
    match fund_type {
        "001" => "股票型",
        "002" => "货币型",
        "003" => "QDII",
        "004" => "债券型",
        "005" => "理财型",
        "006" => "分级",
        "007" => "混合型",
        "008" => "指数型",
        "009" => "ETF联接",
        _ => "未知",
    }
}

/// 资产大类映射（用于「资产配置全景」聚合）。
/// 权益=股票/混合/指数/ETF联接/分级；固收=债券/理财；货币；QDII；未知归 other。
pub fn asset_category(fund_type: &str) -> &'static str {
    match fund_type {
        "001" | "007" | "008" | "009" | "006" => "equity",
        "004" | "005" => "fixed",
        "002" => "money",
        "003" => "qdii",
        _ => "other",
    }
}

pub fn asset_category_label(category: &str) -> &'static str {
    match category {
        "equity" => "权益类",
        "fixed" => "固收类",
        "money" => "货币类",
        "qdii" => "QDII",
        _ => "其他",
    }
}

// ============ 基金名称 → 代码 解析（供 OCR 截图导入补全真实代码）============

/// 内置「基金名称 → 代码」别名表（常见基金，可按需扩展）。
/// 作为联网搜索的兜底：当联网搜索不可用/超时，仍能解析已知基金的真实代码，
/// 避免把基金名称当作代码入库。代码均经公开资料核实。
const FUND_NAME_ALIASES: &[(&str, &str)] = &[
    ("鹏华酒指数C", "012043"),
    ("鹏华酒C", "012043"),
    ("中欧医疗健康混合A", "003095"),
    ("博时恒生医疗保健ETF联接", "014424"),
    ("建信高端装备股票A", "011506"),
    ("天弘恒生科技ETF联接(QDII)C", "012349"),
    ("天弘恒生科技ETF联接C", "012349"),
    ("富国高质量混合", "012255"),
    ("易方达中小盘混合", "110011"),
    // —— 京东持仓（用户 2026-08-15 提供，已核实代码）——
    ("中欧红利优享灵活配置混合C", "004815"),
    ("华富永鑫灵活配置混合C", "001467"),
    ("银华中证全指证券公司ETF发起式", "025193"),
    ("建信上海金ETF联接C", "009034"),
    ("大成高鑫股票A", "000628"),
    ("南方中证A500ETF联接C", "022435"),
];

/// 清洗用于匹配的字符串：仅保留中日韩汉字与 ASCII 字母数字，去除标点/空格/括号。
/// 例如「天弘恒生科技 ETF联接（QDII)C」→「天弘恒生科技ETF联接QDIIC」
fn clean_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| (0x4E00..=0x9FFF).contains(&(*c as u32)) || c.is_ascii_alphanumeric())
        .collect()
}

/// 名称相似度评分（越高越匹配，0 表示不匹配）
fn match_name_score(q: &str, cand: &str) -> usize {
    if q == cand {
        return 1000 + q.len();
    }
    // 子串包含（要求较长一侧 ≥6 字符，避免短名误命中）
    if q.len() >= 6 && cand.contains(q) {
        return 500 + q.len();
    }
    if cand.len() >= 6 && q.contains(cand) {
        return 400 + cand.len();
    }
    0
}

/// 简易 URL 编码（按字节 percent-encode），用于拼接搜索关键词。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

/// 解析东方财富基金搜索接口返回的 JSON，提取与查询最匹配的 6 位基金代码。
/// 失败安全：解析异常或无人选返回 None。可被离线单测直接调用。
fn parse_fund_search(q: &str, body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let datas = v.get("Datas")?.as_array()?;
    let mut best: Option<(String, usize)> = None;
    for d in datas {
        let cand_name = d.get("NAME").and_then(|x| x.as_str()).unwrap_or("");
        let code = d.get("CODE").and_then(|x| x.as_str()).unwrap_or("");
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let score = match_name_score(q, &clean_for_match(cand_name));
        if score > 0
            && best
                .as_ref()
                .map_or(true, |(_, bs)| score > *bs)
        {
            best = Some((code.to_string(), score));
        }
    }
    // 兜底：评分无人选命中，但接口本身已按相关度排序，取首条（仍需是 6 位纯数字）。
    // 避免 OCR 名称轻微噪声时「宁可错填成名称也不联网」的情况。
    if best.is_none() {
        if let Some(d) = datas.first() {
            let code = d.get("CODE").and_then(|x| x.as_str()).unwrap_or("");
            if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) {
                best = Some((code.to_string(), 0));
            }
        }
    }
    best.map(|(code, _)| code)
}

/// 联网按名称搜索基金代码（东方财富基金搜索接口）。失败安全：任何异常返回 None。
pub fn search_fund_code(name: &str) -> Option<String> {
    let q = clean_for_match(name);
    if q.is_empty() {
        return None;
    }
    // 注意：旧接口 fundapi.eastmoney.com/fundtradenew/search 已失效（返回 404），
    // 改用 fundsuggest.eastmoney.com 的 FundSearchAPI，返回结构一致（Datas/CODE/NAME）。
    let url = format!(
        "https://fundsuggest.eastmoney.com/FundSearch/api/FundSearchAPI.ashx?m=1&key={}",
        urlencode(name)
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fund.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    parse_fund_search(&q, &body)
}

/// 按基金名称解析真实 6 位代码：先查本地别名表（精确），再联网搜索兜底。
/// 解析不到返回 None（调用方保留原名作为主键）。
pub fn resolve_fund_code(name: &str) -> Option<String> {
    let q = clean_for_match(name);
    if q.is_empty() {
        return None;
    }
    for (alias, code) in FUND_NAME_ALIASES {
        if clean_for_match(alias) == q {
            return Some(code.to_string());
        }
    }
    search_fund_code(name)
}

/// 取基金类型（可靠来源：东方财富基金搜索接口的 `FundBaseInfo.FTYPE` 描述字段，
/// 例如「混合型-偏股」「指数型-海外股票」「股票型」）。
/// 重要：该接口的 `FUNDTYPE`/`RSFUNDTYPE` 数字码实为「销售适用类型」，
/// 对多数权益基金误报为 002（货币型），不可用于估值口径判定；
/// 故以描述性的 `FTYPE` 映射回我们的类型码。
/// 失败安全：任何异常或解析不到返回 None。
pub fn fetch_fund_type(fund_code: &str) -> Option<String> {
    if fund_code.len() != 6 || !fund_code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let url = format!(
        "https://fundsuggest.eastmoney.com/FundSearch/api/FundSearchAPI.ashx?m=1&key={}",
        urlencode(fund_code)
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fund.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let datas = v.get("Datas")?.as_array()?;
    for d in datas {
        let code = d.get("CODE").and_then(|x| x.as_str()).unwrap_or("");
        if code == fund_code {
            if let Some(ftype) = d
                .get("FundBaseInfo")
                .and_then(|b| b.get("FTYPE"))
                .and_then(|x| x.as_str())
            {
                return Some(fund_type_code_from_ftype(ftype).to_string());
            }
        }
    }
    None
}

/// 将东方财富 FTYPE 描述映射回我们的类型码（供 fund_type_label / asset_category / is_equity_fund 使用）。
/// 顺序很重要：先判「联接/ETF联接」「指数」，再判「股票」，否则「指数型-股票」会被误判为股票型。
fn fund_type_code_from_ftype(ftype: &str) -> &'static str {
    if ftype.contains("ETF联接") || ftype.contains("联接") {
        "009"
    } else if ftype.contains("指数") {
        "008"
    } else if ftype.contains("股票") {
        "001"
    } else if ftype.contains("混合") {
        "007"
    } else if ftype.contains("债券") {
        "004"
    } else if ftype.contains("货币") {
        "002"
    } else if ftype.contains("QDII") {
        "003"
    } else {
        "000"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_for_match_strips_punctuation_and_spaces() {
        assert_eq!(
            clean_for_match("天弘恒生科技 ETF联接（QDII)C"),
            "天弘恒生科技ETF联接QDIIC"
        );
        assert_eq!(clean_for_match("博时恒生医疗 保健ETF联接..."), "博时恒生医疗保健ETF联接");
    }

    #[test]
    fn resolve_known_fund_by_alias() {
        assert_eq!(resolve_fund_code("鹏华酒指数C"), Some("012043".to_string()));
        assert_eq!(resolve_fund_code("中欧医疗健康混合A"), Some("003095".to_string()));
        assert_eq!(resolve_fund_code("博时恒生医疗保健ETF联接"), Some("014424".to_string()));
        assert_eq!(resolve_fund_code("建信高端装备股票A"), Some("011506".to_string()));
        assert_eq!(resolve_fund_code("天弘恒生科技ETF联接(QDII)C"), Some("012349".to_string()));
        assert_eq!(resolve_fund_code("富国高质量混合"), Some("012255".to_string()));
    }

    #[test]
    fn match_name_score_prefers_exact() {
        assert!(match_name_score("中欧医疗健康混合A", "中欧医疗健康混合A") > 0);
        // 含括号差异的清洗后应命中
        assert!(match_name_score("天弘恒生科技ETF联接QDIIC", &clean_for_match("天弘恒生科技ETF联接(QDII)C")) > 0);
        // 短名不应误命中
        assert_eq!(match_name_score("酒", "鹏华酒指数C"), 0);
    }

    #[test]
    fn parse_fund_search_picks_exact_match() {
        // 模拟东方财富 fundsuggest 接口的真实返回结构
        let body = r#"{"ErrCode":0,"ErrMsg":"fromes","Datas":[{"CODE":"000628","NAME":"大成高鑫股票A"},{"CODE":"000001","NAME":"华夏成长混合"}]}"#;
        assert_eq!(parse_fund_search("大成高鑫股票A", body), Some("000628".to_string()));
    }

    #[test]
    fn parse_fund_search_fallback_to_top_when_no_score() {
        // 名称轻微噪声导致评分未命中，仍应取接口相关度排序的首条
        let body = r#"{"ErrCode":0,"Datas":[{"CODE":"022435","NAME":"南方中证A500ETF联接C"}]}"#;
        assert_eq!(parse_fund_search("南方中证A500ETF联C", body), Some("022435".to_string()));
    }

    #[test]
    fn parse_fund_search_rejects_non6_code() {
        let body = r#"{"ErrCode":0,"Datas":[{"CODE":"ABC","NAME":"某基金"},{"CODE":"123","NAME":"短码基金"}]}"#;
        assert_eq!(parse_fund_search("某基金", body), None);
    }

    #[test]
    fn parse_fund_estimate_reads_gsz_and_dwjz() {
        // 模拟天天基金 JSONP 真实返回
        let body = r#"jsonpgz({"fundcode":"000628","name":"大成高鑫股票A","jzrq":"2026-08-14","dwjz":"3.4520","gsz":"3.4680","gztime":"2026-08-15 14:30","sx":"1"})"#;
        let e = parse_fund_estimate(body).unwrap();
        assert_eq!(e.est_nav, 3.4680);
        assert_eq!(e.prev_nav, 3.4520);
        // 估算涨跌幅 = 3.4680/3.4520 - 1 ≈ 0.004636
        assert!((e.est_change_pct - (3.4680 / 3.4520 - 1.0)).abs() < 1e-9);
        assert_eq!(e.gztime, "2026-08-15 14:30");
    }

    #[test]
    fn parse_fund_estimate_handles_trailing_semicolon() {
        let body = "jsonpgz({\"dwjz\":\"1.0000\",\"gsz\":\"1.0200\",\"gztime\":\"2026-08-15 09:35\"});";
        let e = parse_fund_estimate(body).unwrap();
        assert!((e.est_change_pct - 0.02).abs() < 1e-9);
    }

    #[test]
    fn parse_fund_estimate_rejects_bad_input() {
        assert!(parse_fund_estimate("jsonpgz({})").is_none()); // 缺 gsz/dwjz
        assert!(parse_fund_estimate("not json").is_none());
        assert!(parse_fund_estimate("jsonpgz({\"dwjz\":\"0\",\"gsz\":\"1\"})").is_none()); // dwjz<=0
    }

    #[test]
    fn pick_benchmark_detects_index_from_name() {
        // 指数/ETF联接基金：名称含宽基关键字 → 真实跟踪指数，而非笼统沪深300
        let (_, code, name) = pick_benchmark("008", "华夏沪深300ETF联接A");
        assert_eq!(code, "000300");
        assert_eq!(name, "沪深300");

        let (_, code, name) = pick_benchmark("009", "南方中证500ETF联接C");
        assert_eq!(code, "000905");
        assert_eq!(name, "中证500");

        let (_, code, name) = pick_benchmark("008", "易方达创业板ETF");
        assert_eq!(code, "399006");
        assert_eq!(name, "创业板指");

        let (_, code, name) = pick_benchmark("008", "工银科创50ETF联接");
        assert_eq!(code, "000688");
        assert_eq!(name, "科创50");

        let (_, code, name) = pick_benchmark("008", "富国中证红利指数");
        assert_eq!(code, "000922");
        assert_eq!(name, "中证红利");
    }

    #[test]
    fn pick_benchmark_detects_industry_from_name() {
        // 行业/主题基金：名称含行业关键字 → 对应行业指数，而非笼统沪深300
        let (_, code, name) = pick_benchmark("007", "中欧医疗健康混合A");
        assert_eq!(code, "399989");
        assert_eq!(name, "中证医疗");

        let (_, code, name) = pick_benchmark("008", "鹏华酒指数C");
        assert_eq!(code, "399997");
        assert_eq!(name, "中证白酒");

        let (_, code, name) = pick_benchmark("008", "国泰中证证券公司ETF联接");
        assert_eq!(code, "399975");
        assert_eq!(name, "证券公司");

        let (_, code, name) = pick_benchmark("007", "招商中证白酒指数");
        assert_eq!(code, "399997");

        let (_, code, name) = pick_benchmark("008", "国联安中证半导体ETF");
        assert_eq!(code, "980017");
        assert_eq!(name, "国证芯片");

        // 新能源车 优先于 新能源
        let (_, code, name) = pick_benchmark("008", "天弘中证新能源车ETF联接");
        assert_eq!(code, "399976");
        assert_eq!(name, "CS新能车");

        // 光伏 → 中证新能（与新能源车/新能源不冲突）
        let (_, code, name) = pick_benchmark("008", "天弘中证光伏产业ETF");
        assert_eq!(code, "399808");
        assert_eq!(name, "中证新能");
    }

    #[test]
    fn pick_benchmark_falls_back_by_type() {
        // 债券型 → 国债指数
        let (_, code, name) = pick_benchmark("004", "易方达增强回报债券A");
        assert_eq!(code, "000012");
        assert_eq!(name, "上证国债");

        // 名称无关键字 + 股票/混合/货币/QDII 等 → 沪深300 宽基代理
        let (_, code, name) = pick_benchmark("001", "易方达蓝筹精选混合");
        assert_eq!(code, "000300");
        assert_eq!(name, "沪深300");

        // 名称无行业/主题关键字的混合基金 → 仍回落沪深300 宽基代理
        let (_, code, _name) = pick_benchmark("007", "兴全合宜灵活配置混合");
        assert_eq!(code, "000300");

        // 名称无关键字的指数基金（如普通指数A）也落到沪深300 宽基代理，不做错误猜测
        let (_, code, name) = pick_benchmark("008", "某宽基指数A");
        assert_eq!(code, "000300");
        assert_eq!(name, "沪深300");
    }

    #[test]
    fn is_qdii_fund_matches_type_code_003() {
        assert!(is_qdii_fund("003"));
        assert!(!is_qdii_fund("001"));
        assert!(!is_qdii_fund("007"));
    }

    #[test]
    fn qdii_overseas_region_detects_us_and_hk() {
        // 港股区域
        assert_eq!(qdii_overseas_region("华夏恒生科技ETF联接(QDII)"), "hk");
        assert_eq!(qdii_overseas_region("易方达香港恒生H股"), "hk");
        // 美股区域
        assert_eq!(qdii_overseas_region("广发纳斯达克100指数(QDII)"), "us");
        assert_eq!(qdii_overseas_region("博时标普500ETF联接"), "us");
        assert_eq!(qdii_overseas_region("华夏全球科技先锋(QDII)"), "us");
        // 未标识 → other（按美股时区处理）
        assert_eq!(qdii_overseas_region("某QDII基金"), "other");
    }

    #[test]
    fn extract_curyear_reads_quarter_from_title() {
        // 中报（半年报）
        assert_eq!(extract_curyear("2026年2季度投资组合"), Some("2026Q2".to_string()));
        // 年报
        assert_eq!(extract_curyear("2025年4季度投资组合"), Some("2025Q4".to_string()));
        // 带「第」字
        assert_eq!(extract_curyear("2026年第2季度"), Some("2026Q2".to_string()));
        // 中文数字季度
        assert_eq!(extract_curyear("2026年第二季度投资组合"), Some("2026Q2".to_string()));
    }

    #[test]
    fn extract_curyear_avoids_date_collision() {
        // 正文日期「2026年08月」的月=0X 不是季度数字，不应误命中；
        // 真正的报告期（2026年2季度）在更靠后位置，应被正确提取。
        let body = "更新于2026年08月13日 09:30，基金2026年2季度投资组合如下";
        assert_eq!(extract_curyear(body), Some("2026Q2".to_string()));
        // 只有日期、无报告期标题时返回 None（而非错误的「2026Q0」）
        assert_eq!(extract_curyear("数据更新于2026年08月13日"), None);
    }

    #[test]
    fn extract_curyear_no_panic_on_multibyte_before_year() {
        // 回归：当「年」前 4 字节落在某个中文字符中间（如「某某年」）时，
        // 旧实现 body[abs-4..abs] 会触发 "start byte index is not a char boundary" panic
        // 并使整个 app 崩溃（abort）。新实现按字符索引，安全返回 None。
        assert_eq!(extract_curyear("某某年2季度投资组合"), None);
        // 正常报告期不受字符索引改动影响
        assert_eq!(
            extract_curyear("基金2026年2季度投资组合"),
            Some("2026Q2".to_string())
        );
    }

    #[test]
    fn candidate_periods_prefers_latest_ended_as_of_aug() {
        // 模拟「当前日期 = 2026-08-13」：最新已结束报告期 = 2026Q2（中报，6/30 结束）
        let now = chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        let cands = candidate_periods(now);
        // 最新候选应为 2026Q2（full 全持仓）
        assert_eq!(cands.first(), Some(&(2026, 2, "full")));
        // 尚未结束的 2026Q3/Q4 不应出现
        assert!(!cands.contains(&(2026, 3, "top10")));
        assert!(!cands.contains(&(2026, 4, "full")));
        // 上一期 2026Q1 与 2025Q4 应作为回退候选存在
        assert!(cands.contains(&(2026, 1, "top10")));
        assert!(cands.contains(&(2025, 4, "full")));
        // 按从新到旧排序：2026Q2 应在 2026Q1 之前
        let pos_q2 = cands.iter().position(|c| *c == (2026, 2, "full")).unwrap();
        let pos_q1 = cands.iter().position(|c| *c == (2026, 1, "top10")).unwrap();
        assert!(pos_q2 < pos_q1);
    }

    #[test]
    fn parse_nav_history_reads_lsjz_list() {
        // 模拟东财 lsjz 真实返回：含一条 DWJZ 为空（分红除息日）应跳过
        let body = r#"{"Data":{"LSJZList":[{"FSRQ":"2026-08-11","DWJZ":"3.4520","LJJZ":"3.4520"},{"FSRQ":"2026-08-12","DWJZ":"3.4600","LJJZ":"3.4600"},{"FSRQ":"2026-08-13","DWJZ":"","LJJZ":""}],"FundType":"007"}}"#;
        let pts = parse_nav_history(body).unwrap();
        assert_eq!(pts.len(), 2); // 跳过空 DWJZ
        assert_eq!(pts[0].date, "2026-08-11");
        assert_eq!(pts[0].nav, 3.4520);
        assert_eq!(pts[1].date, "2026-08-12");
        assert!(pts[0].date < pts[1].date); // 升序
    }

    #[test]
    fn parse_nav_history_handles_null_and_numeric() {
        // LJJZ 为 null → 0；DWJZ 以数字而非字符串出现也应解析；DWJZ 为 null 的行应跳过
        let body = r#"{"Data":{"LSJZList":[{"FSRQ":"2026-08-10","DWJZ":"1.0000","LJJZ":null},{"FSRQ":"2026-08-09","DWJZ":2.5,"LJJZ":2.6},{"FSRQ":"2026-08-08","DWJZ":null,"LJJZ":"3.0"}]}}"#;
        let pts = parse_nav_history(body).unwrap();
        assert_eq!(pts.len(), 2); // 仅保留有 DWJZ 的两条
        assert_eq!(pts[0].date, "2026-08-09"); // 升序：08-09 在前
        assert_eq!(pts[0].nav, 2.5);
        assert_eq!(pts[0].acc_nav, 2.6);
        assert_eq!(pts[1].date, "2026-08-10");
        assert_eq!(pts[1].acc_nav, 0.0); // LJJZ null → 0
    }

    #[test]
    fn parse_nav_history_returns_none_on_garbage() {
        assert!(parse_nav_history("not json").is_none());
        assert!(parse_nav_history(r#"{"Data":{}}"#).is_none());
        assert!(parse_nav_history(r#"{"Data":{"LSJZList":[]}}"#).is_none());
    }

    #[test]
    fn candidate_periods_early_year_falls_back_to_prior_year_annual() {
        // 模拟「当前日期 = 2026-02-01」：2026Q1 尚未结束（3/31），最新应为 2025Q4（年报）
        let now = chrono::NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        let cands = candidate_periods(now);
        assert_eq!(cands.first(), Some(&(2025, 4, "full")));
        assert!(!cands.contains(&(2026, 1, "top10"))); // 2026Q1 未结束，不应出现
    }
}
