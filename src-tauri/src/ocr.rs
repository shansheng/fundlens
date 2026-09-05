// 本地 OCR 模块（v1.1 实装：PaddleOCR / PP-OCRv4，纯 Rust 引擎）
//
// 引擎：rusto-rs（RapidOCR 的纯 Rust 实现，使用 PaddleOCR 的 PP-OCRv4 模型，
//       经 MNN 推理，无 OpenCV / PaddlePaddle C++ 运行时依赖）。
// 模型权重：det.mnn / rec.mnn / cls.mnn / dict.txt（PP-OCRv4 mobile），
//       由 src-tauri/download_ocr_models.sh 下载到 resources/ocr/。
//
// 识别流程：截图 -> 文本行(含包围盒) -> 按 y 聚类成表格行、行内按 x 排序
//           -> 平台模板抽取「基金代码 / 名称 / 持有份额 / 单位净值」。
// 截图与识别结果均不离开本机。
//
// OCR 引擎代码用 `ocr` 特性门控：未开启时 recognize_image 返回明确错误，
// 不破坏默认构建。

use std::collections::HashSet;

/// 单行 OCR 识别结果（已转为轴对齐包围盒，单位像素）
#[derive(Debug, Clone)]
pub struct OcrLine {
    pub text: String,
    pub score: f64,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 一张图中重建出的一行表格：cells 已按 x 从左到右排序
#[derive(Debug, Clone)]
pub struct OcrRow {
    pub y: i32,
    pub cells: Vec<OcrLine>,
}

/// 从持仓截图中抽取的「基金持仓」条目
#[derive(Debug, Clone)]
pub struct OcrFund {
    pub code: String,
    /// 基金名称（支付宝「我的持有」布局通常不含 6 位代码，此时以名称作库内主键代理）
    pub name: String,
    /// 持有份额（京东金融 / 理财通 风格）
    pub shares: f64,
    /// 单位净值（京东金融 / 理财通 风格）
    pub nav: f64,
    /// 持仓金额（支付宝「金额」列，元）
    pub holding_amount: f64,
    /// 持有收益（支付宝「持有收益」列，元，带正负）
    pub holding_profit: f64,
    /// 昨日收益（支付宝「昨日收益」列，元，带正负）
    pub yesterday_profit: f64,
    /// 收益率（支付宝「收益率」列，百分数，如 -36.53 表示 -36.53%）
    pub profit_rate: f64,
    pub confidence: f64,
}

/// 从**交易记录**截图中抽取的「交易流水」条目
#[derive(Debug, Clone)]
pub struct OcrTxn {
    /// 归一化交易类型：buy / sell / dividend
    pub txn_type: String,
    /// 原始类型标签（如「买入」「赎回」「现金分红」），供预览展示与核对
    pub txn_type_raw: String,
    /// 交易日期（ISO：YYYY-MM-DD；截图仅有月日时补当前年）
    pub date: String,
    /// 日期是否含年份（false 表示由当前年补足，预览时需提醒核对）
    pub has_year: bool,
    /// 交易时间（HH:MM；截图含时间时抓取，否则空串）。
    /// 用途：判断 15:00 前后——15:00 前按当日净值结算、15:00 后按下一交易日净值结算。
    pub time: String,
    /// 基金代码（6 位）；无则空串（命令阶段按名称补真实代码）
    pub code: String,
    /// 基金名称（已剥离类型关键词，如「易方达蓝筹买入」→「易方达蓝筹」）
    pub name: String,
    /// 交易份额（买/卖；分红为 0）
    pub shares: f64,
    /// 成交金额（元，必填）
    pub amount: f64,
    /// 单位净值 / 成交价格（元）
    pub price: f64,
    /// 置信度 0~1（类型 + 日期 + 名称/代码 + 金额 越全越高）
    pub confidence: f64,
}

// ============ 特性门控的 OCR 引擎 ============

#[cfg(feature = "ocr")]
mod engine {
    use crate::ocr::OcrLine;
    use once_cell::sync::OnceCell;
    use std::sync::Mutex;

    static ENGINE: OnceCell<Mutex<rusto::RustO>> = OnceCell::new();

    /// 解析模型目录：环境变量 > 打包资源目录 > 用户数据目录 ocr 子目录 > 开发期 resources/ocr。
    /// **每个候选都校验 `det.mnn / rec.mnn / dict.txt` 三件套是否都存在**——只看目录存在不够，
    /// Windows 安装包未正确打包模型时 `resources/ocr/` 是空目录（旧实现会返回此空目录后，
    /// 在 `RustOConfig::ppv4` 阶段才报 "Failed to open file: \\?\D:\FundLens\ocr\det.mnn" 之类的误导性错误）。
    fn model_dir(app: Option<&tauri::AppHandle>) -> Option<String> {
        fn valid(p: &std::path::Path) -> bool {
            p.join("det.mnn").is_file()
                && p.join("rec.mnn").is_file()
                && p.join("dict.txt").is_file()
        }
        let report = |p: &std::path::Path| p.to_string_lossy().into_owned();

        // 1) 环境变量（FUNDLENS_OCR_DIR）：调试/手动放置模型时常用
        if let Ok(d) = std::env::var("FUNDLENS_OCR_DIR") {
            let p = std::path::PathBuf::from(&d);
            if valid(&p) {
                return Some(report(&p));
            }
        }
        // 2) 打包资源目录（Tauri installer 默认；CI 缺失模型会落到用户数据目录兜底）
        if let Some(a) = app {
            if let Some(rd) = a.path_resolver().resource_dir() {
                let p = rd.join("ocr");
                if valid(&p) {
                    return Some(report(&p));
                }
            }
            // 3) 用户数据目录 ocr 子目录兜底：用户可从开源仓库下载三个 .mnn + dict.txt
            //    手动放到 %APPDATA%/com.fundlens.app/ocr/（Windows）/ ~/Library/Application Support/com.fundlens.app/ocr/（macOS）
            if let Some(ad) = a.path_resolver().app_data_dir() {
                let p = ad.join("ocr");
                if valid(&p) {
                    return Some(report(&p));
                }
            }
        }
        // 4) 开发期：tauri dev 的工作目录为 src-tauri
        if let Ok(cwd) = std::env::current_dir() {
            let p = cwd.join("resources").join("ocr");
            if valid(&p) {
                return Some(report(&p));
            }
        }
        None
    }

    pub fn recognize(path: &str, app: Option<&tauri::AppHandle>) -> Result<Vec<OcrLine>, String> {
        let dir = model_dir(app).ok_or_else(|| {
            "OCR 模型未找到：det.mnn / rec.mnn / dict.txt 三件套缺失。".to_string()
                + " 已尝试：环境变量 FUNDLENS_OCR_DIR、app.path().resource_dir()/ocr、"
                + "app_data_dir/ocr、cwd/resources/ocr（每个候选都校验文件齐备才接受）。"
                + " 修复方式：(1) 开发期运行 src-tauri/download_ocr_models.sh；"
                + "(2) 安装包缺失时从开源仓库下载 det.mnn/rec.mnn/dict.txt/cls.mnn 到"
                + "%APPDATA%/com.fundlens.app/ocr/（Windows）或"
                + "~/Library/Application Support/com.fundlens.app/ocr/（macOS）;"
                + "(3) 或设置环境变量 FUNDLENS_OCR_DIR 指向含三件套的目录。"
        })?;

        let det = format!("{dir}/det.mnn");
        let rec = format!("{dir}/rec.mnn");
        let dict = format!("{dir}/dict.txt");

        // 惰性初始化 PP-OCRv4 引擎（MNN 推理）。模型在 new() 时按绝对路径加载，
        // 故需保证 det/rec/dict 路径存在且可读。
        let eng = ENGINE.get_or_try_init(|| -> Result<Mutex<rusto::RustO>, String> {
            let cfg = rusto::RustOConfig::ppv4(det, rec, dict);
            rusto::RustO::new(cfg)
                .map(Mutex::new)
                .map_err(|e| format!("OCR 引擎初始化失败: {e}"))
        })?;

        let mut g = eng.lock().unwrap();
        let out = g
            .run(path)
            .map_err(|e| format!("OCR 识别失败: {e}"))?;

        // RustOOutput 的 frame 已是轴对齐包围盒（top/left/width/height），直接取用。
        let mut lines = Vec::new();
        for r in out.to_text_results() {
            lines.push(OcrLine {
                text: r.text,
                score: r.score as f64,
                x: r.frame.left.round() as i32,
                y: r.frame.top.round() as i32,
                w: r.frame.width.round() as i32,
                h: r.frame.height.round() as i32,
            });
        }
        Ok(lines)
    }
}

/// 对单张图片执行 OCR。返回文本行（含包围盒）。
/// 未开启 `ocr` 特性或模型缺失时返回 Err，调用方据此提示用户。
pub fn recognize_image(path: &str, app: Option<&tauri::AppHandle>) -> Result<Vec<OcrLine>, String> {
    #[cfg(feature = "ocr")]
    {
        engine::recognize(path, app)
    }
    #[cfg(not(feature = "ocr"))]
    {
        let _ = (path, app);
        Err(
            "OCR 未启用：本构建未开启 `ocr` 特性。请用 `npm run tauri build --features ocr` 构建，并先运行 src-tauri/download_ocr_models.sh 下载模型。"
                .into(),
        )
    }
}

// ============ 纯逻辑：表格重建 + 字段抽取（可单测，无需模型） ============

/// 按垂直重叠把文本行聚类成表格行；行内按 x 从左到右排序。
pub fn reconstruct_rows(lines: &[OcrLine]) -> Vec<OcrRow> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&OcrLine> = lines.iter().collect();
    sorted.sort_by(|a, b| a.y.cmp(&b.y).then(a.x.cmp(&b.x)));

    let mut rows: Vec<OcrRow> = Vec::new();
    for line in sorted {
        let top = line.y;
        let bottom = line.y + line.h;
        let mut placed = false;
        for row in rows.iter_mut() {
            let rtop = row.y;
            let rbottom = row.y
                + row
                    .cells
                    .iter()
                    .map(|c| c.h)
                    .max()
                    .unwrap_or(0);
            // 行重叠（含少量容差）则归入同一行
            if top <= rbottom + 4 && bottom >= rtop - 4 {
                row.cells.push(line.clone());
                if top < row.y {
                    row.y = top;
                }
                placed = true;
                break;
            }
        }
        if !placed {
            rows.push(OcrRow {
                y: top,
                cells: vec![line.clone()],
            });
        }
    }
    for row in rows.iter_mut() {
        row.cells.sort_by_key(|c| c.x);
    }
    rows.sort_by_key(|r| r.y);
    rows
}

// 名称折行两行的最大 y 间隔（同一基金名称折成两行时，两行距离较小）
// 真实手机截图（支付宝/京东金融）OCR 输出中，折行行距通常在 25~50px 之间。
// 此值必须**小于**相邻两只基金之间的 y 间距（通常 ≥100px），
// 否则会把不同基金的名称错误合并。
const NAME_MERGE_GAP: i32 = 80;
// 孤立碎片回收的最大 y 间距：仅用于「短 CJK 名称后缀」回收到前一组。
// 京东金融的长名称（如「南方中证A500ETF」+「联接C」）折行后间距可能较大，
// 故放宽到 170px 以覆盖极端场景——但仅限短后缀，不会误合并不同基金。
const ORPHAN_RECOVER_GAP: i32 = 170;
// 数值行关联到「上方名称卡片」的最大纵向距离。
// 真实手机高清截图中，卡片名称与下方数值行垂直间距可能 >120px（卡片较高），
// 故放宽到 220 以覆盖高分辨率截图；配合「最近上方名称」关联，不会跨卡片误关联。
const NAME_Y_BAND: i32 = 220;
// 允许数值行轻微上探到名称（OCR 行高抖动时数值可能略高于名称基线）
const NAME_OVERLAP_TOL: i32 = 30;
// 名称左缘最多可位于数值左缘右侧该像素数（防止数值被右侧远处名称抢走）
const NAME_X_TOL: i32 = 80;

/// 单个数值识别行（已解析为数值与元信息）
struct NumTok {
    raw: String,
    x: i32,
    y: i32,
    value: f64,
    has_percent: bool,
    signed: bool,
}

/// 由相邻名称行合并出的「基金卡片」
struct NameGroup {
    cx: i32,
    cy: i32,
    texts: Vec<String>,
}

fn is_signed(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('+') || t.starts_with('-')
}

/// 是否为 UI chrome 文本（标题/筛选/导航栏/标签/交易记录/推广），应被忽略。
///
/// 覆盖平台：
/// - 支付宝：「我的持有」「金额排序」「偏股/偏债」「金选…」「收益/率」等
/// - 京东金融：「理财师」「大家在关注榜」「交易：X笔买入中合计…」「全部(N)」
///            「股票型(N)」「债券型(N)」「混合」「稳健」「基金圈」「自选」「持仓」
///            底部导航栏文字、头像旁姓名、货币符号等
/// - 腾讯理财通持仓：「腾讯理财通」「资产明细」「筛选」「按持有金额排序」
///            推广行（产品解读/行借解读/专属报告/详情/恭喜收盈/基金经理来信）
///            Tab（定投计划）、汇总块（稳健资产/持仓服务）、底部推荐区
fn is_chrome(s: &str) -> bool {
    const EXACT: &[&str] = &[
        // 通用
        "我的持有", "持有收益", "持仓收益", "持有金额", "金额排序", "偏股", "偏债", "黄金", "全部", "指数", "名称",
        "金额", "昨日收益", "收益率", "排行", "自选", "基金市场", "正确内容", "基金", "基金名称",  // 列头
        // 交易记录页（三平台）
        "交易记录", "交易明细", "流水", "资金流水",
        "全部持有", "收益明细",           // 支付宝 tab
        "账户明细",                       // 京东金融 tab 标题
        "全部交易", "进阶资产", "所有月份", // 腾讯理财通 tab/筛选
        "明细",                           // 支付宝筛选行
        // 京东金融专属
        "理财师", "大家在关注榜", "基金持仓",
        "稳健", "基金圈", "持仓",  // 底部导航
        "混合Q",                   // 筛选标签截断（实际为"混合"）
        "吕",                      // 头像旁噪声文字
        // 腾讯理财通持仓页专属
        "腾讯理财通",              // 标题栏
        "资产明细",                 // 区块标题
        "筛选",                     // 筛选按钮文字
        "按持有金额排序",           // 排序下拉文字
        "产品解读",                 // 推广行前缀（如「产品解读 基金经理来信：... 详情」）
        "行借解读",                 // 推广行前缀（如「行借解读 恭喜收盈！... 详情」）
        "专属报告",                 // 推广行前缀（如「专属报告 【专属服务】... 详情」）
        "详情",                     // 推广行链接文字
        "定投计划",                 // Tab 标签（与「交易明细」并列）
        "稳健资产",                 // 汇总块标题（如「稳健资产(元)」）
        "持仓服务",                 // 底部推荐区标题
        "长期理财",                 // 底部推荐区链接
        "查看更多",                 // 底部推荐区链接
    ];
    if EXACT.contains(&s) {
        return true;
    }
    // 子串匹配：覆盖 OCR 把多个 chrome 词粘连成一行的情况（如「资产明细 筛选」→「资产明细筛选了」），
    // 以及可变内容的推广/状态文本。这些词绝不会出现在基金名称中，子串匹配安全。
    const CHROME_SUBSTR: &[&str] = &[
        "腾讯理财通", "资产明细", "筛选", "按持有金额排序",
        "产品解读", "行借解读", "专属报告", "定投计划", "稳健资产",
        "持仓服务", "长期理财", "查看更多", "交易明细", "全部交易",
        "进阶资产", "所有月份", "银行卡", "活期", "金选",
        "基金经理来信", "关注多元", "跑赢沪深", "投资分析速递",
        "专属服务", "根据你的持仓情况", "预计", "点前", "到账",
        "支付成功", "订单完成", "现金发放", "转出完成", "交易进行中",
        "买入成功", "取出成功", "转换成功", "份额待确认", "恭喜",
    ];
    if CHROME_SUBSTR.iter().any(|c| s.contains(c)) {
        return true;
    }
    // 「稳健资产(元)」等带单位后缀的汇总标题
    if s.starts_with("稳健资产") {
        return true;
    }
    if s.starts_with("金选") {
        return true;
    }
    if s.contains("收益/率") || s.contains("昨日收益") || s.contains("持有收益") {
        return true;
    }
    // 京东交易记录：「交易：X笔买入中合计Y.YY元」
    if s.contains("交易") && (s.contains("笔买入") || s.contains("合计")) {
        return true;
    }
    // 京东金融 / 腾讯理财通 交易状态文本（非交易数据）
    if ["支付成功", "订单完成", "现金发放", "转出完成", "交易进行中",
        "买入成功", "取出成功", "转换成功", "份额待确认"].contains(&s.trim()) {
        return true;
    }
    // 「预计XX-XX XX点前到账」——支付宝卖出待到账提示
    if s.contains("预计") && (s.contains("点前") || s.contains("到账")) {
        return true;
    }
    // 「银行卡买入」「活期」——腾讯理财通支付方式（非交易数据）
    if s.starts_with("银行卡") || (s == "活期") {
        return true;
    }
    // 京东筛选标签：「全部(33)」「股票型(4)」「债券型(3)」
    let trimmed = s.trim();
    if let Some(paren_pos) = trimmed.find('(') {
        if trimmed[paren_pos..].contains(')') {
            let prefix = &trimmed[..paren_pos];
            if ["全部", "股票型", "债券型", "混合", "指数型", "QDII", "FOF"].contains(&prefix) {
                return true;
            }
        }
    }
    // 关注榜标签带序号：「大家在关注榜 No.7>」「No.7>」
    if trimmed.starts_with("No.") || trimmed.contains("No.") {
        return true;
    }
    // 时间（如 14:34 / 15:28）
    if s.contains(':') && s.chars().filter(|c| *c == ':').count() >= 1 {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() >= 2 && parts[0].trim().len() <= 2 && parts[1].trim().len() <= 2 {
            return true;
        }
    }
    // 纯短数字徽标（如 54/59）：1~2 位纯数字
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 2 && s.chars().all(|c| c.is_ascii_digit() || c == ' ') {
        return true;
    }
    // 货币符号 / 纯符号行
    if trimmed == "￥" || trimmed == "¥" || trimmed == "▍" || trimmed == ">" {
        return true;
    }
    // 腾讯理财通持仓页推广文本（非精确匹配，含可变内容）
    // 「恭喜收盈！」「恭喜！」——推广恭喜语
    if trimmed.starts_with("恭喜") {
        return true;
    }
    // 「基金经理来信：限额守初心」等推广子标题
    if trimmed.starts_with("基金经理来信") || trimmed.contains("基金经理来信") {
        return true;
    }
    // 「关注多元资产配置」「你的致远近1月跑赢沪深300」等推荐文案
    if trimmed.starts_with("关注多元") || trimmed.contains("关注多元资产配置")
        || trimmed.contains("跑赢沪深") || trimmed.contains("致远近")
    {
        return true;
    }
    // 「二季度投资分析速递」「一季度...」等报告标题
    if trimmed.contains("季度投资分析") || trimmed.contains("投资分析速递") {
        return true;
    }
    // 「【专属服务】」标签
    if trimmed.contains("【专属服务】") || trimmed.contains("【专属") {
        return true;
    }
    // 「根据你的持仓情况，为你精选以下内容：」底部推荐引导
    if s.starts_with("根据你的持仓情况") || s.contains("根据你的持仓情况") {
        return true;
    }
    // 「(元)」单位后缀独立成行时（如「稳健资产(元)」被 OCR 拆为「稳健资产」+「(元)」）
    if trimmed == "(元)" || trimmed == "(元)" {
        return true;
    }
    false
}

/// 是否为 OCR 噪音/垃圾文本：纯拉丁字母乱码（如 LEXBRERE）、过短无意义片段等。
/// PaddleOCR 偶尔对非文字区域（图标/按钮/水印）产生纯 ASCII 拉丁乱码，需过滤。
///
/// 重要：基金名称折行产生的**短 CJK 后缀**（如「联接C」「混合C」「接C」）
/// 是有效名称碎片，必须保留。它们含 CJK 字符且匹配已知后缀模式。
fn is_garbage(s: &str) -> bool {
    let trimmed = s.trim();

    // === 保护有效的基金名称后缀（折行碎片）===
    // 这些是名称第 2 行的常见结尾，绝不能当垃圾过滤
    const NAME_SUFFIXES: &[&str] = &[
        "联接C", "联接A", "联接QDII", "联接(QDII)C",
        "混合C", "混合A", "混合B",
        "ETF联", "ETF联接C", "ETF联接A",
        "股票A", "股票B",
        "发起式", "发起式C",
        "债券C", "债券A",
        "指数C", "指数A",
        "LOF", "FOF",
    ];
    // 清理后精确匹配或尾部匹配
    let cleaned_for_suffix: String = trimmed
        .chars()
        .filter(|c| (0x4E00..=0x9FFF).contains(&(*c as u32)) || c.is_ascii_alphanumeric())
        .collect();
    for &suffix in NAME_SUFFIXES {
        if cleaned_for_suffix == suffix || cleaned_for_suffix.ends_with(suffix) {
            return false; // 明确是名称后缀 → 不是垃圾
        }
    }

    // 纯拉丁字母（无 CJK、无数字），长度 ≥ 3 → 几乎一定是噪音
    let has_cjk = trimmed.chars().any(|c| (0x4E00..=0x9FFF).contains(&(c as u32)));
    let has_ascii_alpha = trimmed.chars().any(|c| c.is_ascii_alphabetic());
    let has_digit = trimmed.chars().any(|c| c.is_ascii_digit());
    if !has_cjk && has_ascii_alpha && !has_digit && trimmed.len() >= 3 {
        return true;
    }
    // 极短的非 CJK 文本（1~2 个 ASCII 字符且不是已知缩写）
    if !has_cjk && trimmed.len() <= 2 && !(trimmed == "A" || trimmed == "C" || trimmed.starts_with("ETF")) {
        return true;
    }
    false
}

/// OCR 常见字符纠错：PaddleOCR 对某些 CJK 字符的典型误识别。
/// 返回 Some(修正后) 或 None（无需修正）。
///
/// 覆盖范围：
/// - 通用 OCR 误识别（夺→高鑫 等）
/// - 支付宝/京东金融截图中的典型乱码（LEXBRERE 等拉丁噪音）
/// - 常见截断/标点丢失（配置混C→配置混合C、联接QDII→联接(QDII)）
fn correct_ocr_char(s: &str) -> Option<String> {
    // 常见单字/词级替换（按优先级排列，先匹配的先生效）
    const CORRECTIONS: &[(&str, &str)] = &[
        // === CJK 字符误识别 ===
        ("夺", "高鑫"),       // 大成高鑫 → 大成夺（OCR 常误，京东/支付宝均出现）
        ("夺股票", "高鑫股票"), // 同上上下文
        ("高新", "高鑫"),     // 变体：有时被识别为"高新"
        ("高新股票", "高鑫股票"),
        // === 拉丁字母乱码（PaddleOCR 对图标/水印区域的噪音）===
        ("LEXBRERE", ""),     // 华富永鑫灵活配置 的典型噪音（京东）
        ("LEXBRER", ""),
        ("BRE", ""),          // LEXBRERE 的子串残留
        ("LEXBR", ""),
        ("EREBR", ""),        // 反向变体
        ("BRERE", ""),
        // === 截断补全 ===
        ("配置混C", "配置混合C"), // 混合C 尾部 CJK 被截断
        ("配置混合c", "配置混合C"),
        ("联接c", "联接C"),   // 大小写统一
        ("联接QDII", "联接(QDII)"), // 括号丢失
        ("发起式·", "发起式"), // 多余中间点
        ("公司ETF发", "公司ETF发起式"), // 尾部截断
        // 注意：不设 ("ETF联", "ETF联接") —— OCR 可能将名称拆为
        // 「...ETF联」+「接C」两行，合并后自然得到「ETF联接C」；
        // 若在此处预先把「联」→「联接」，合并后会变成「联接接C」（双重 接）。
    ];
    for &(wrong, right) in CORRECTIONS {
        if s.contains(wrong) {
            return Some(s.replace(wrong, right));
        }
    }
    None
}

/// 名称候选：含 CJK、非 chrome（纯数值已被 parse_number 排除在外）、非垃圾拉丁
fn is_name_candidate(s: &str) -> bool {
    if is_chrome(s) {
        return false;
    }
    if is_garbage(s) {
        return false;
    }
    s.chars().any(|c| (0x4E00..=0x9FFF).contains(&(c as u32)))
}

/// 把相邻（纵向接近）的名称行合并为「基金卡片」，处理名称折行。
///
/// 两阶段合并：
/// 1. 主合并：y 间距 ≤ NAME_MERGE_GAP 的相邻名称行合并为一组
/// 2. 孤立碎片回收：首轮未合并的**短 CJK 后缀**（如「联接C」「混合C」「接C」），
///    若其 y 坐标在某个已存在组的 NAME_MERGE_GAP*1.5 范围内且 x 接近，则追加到该组。
///    这修复了京东金融长名称（如「南方中证A500ETF联接C」）折行后第 2 行碎片
///    因 y 间距略大而未能合并的问题。
fn merge_name_groups(name_lines: &[&OcrLine]) -> Vec<NameGroup> {
    if name_lines.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&OcrLine> = name_lines.to_vec();
    sorted.sort_by(|a, b| a.y.cmp(&b.y).then(a.x.cmp(&b.x)));

    // 阶段 1：主合并
    let mut groups: Vec<NameGroup> = Vec::new();
    for l in sorted {
        let text = clean_name(&l.text);
        if let Some(g) = groups.last_mut() {
            if l.y - g.cy <= NAME_MERGE_GAP {
                g.texts.push(text);
                let n = g.texts.len() as i32;
                g.cy = (g.cy * (n - 1) + l.y) / n;
                if l.x < g.cx {
                    g.cx = l.x;
                }
                continue;
            }
        }
        groups.push(NameGroup {
            cx: l.x,
            cy: l.y,
            texts: vec![text],
        });
    }

    // 阶段 2：孤立碎片回收 —— 实际上阶段 1 已用放宽的 NAME_MERGE_GAP(130) 合并，
    // 正常截图不应再有孤立碎片。但作为安全网：如果仍有未合并的组，
    // 检查是否为可附加到前一组的短后缀。
    let mut i = 1;
    while i < groups.len() {
        let frag_text = groups[i].texts.join("");
        let frag_clean: String = frag_text
            .chars()
            .filter(|c| (0x4E00..=0x9FFF).contains(&(*c as u32)) || c.is_ascii_alphanumeric())
            .collect();
        // 短 CJK 碎片（≤6 字符）且是已知名称后缀模式 → 尝试回收到前一组
        let is_short_suffix = frag_clean.len() <= 6
            && (frag_clean.ends_with("C")
                || frag_clean.ends_with("A")
                || frag_clean.ends_with("B")
                || frag_clean.contains("联接")
                || frag_clean.contains("混合")
                || frag_clean.contains("发起式")
                || frag_clean.contains("股票"));

        if is_short_suffix && i > 0 {
            let gap = groups[i].cy - groups[i - 1].cy;
            if gap <= ORPHAN_RECOVER_GAP {
                // 回收：移入前一组
                let texts = std::mem::take(&mut groups[i].texts);
                let texts_len = texts.len() as i32;
                groups[i - 1].texts.extend(texts);
                let n = groups[i - 1].texts.len() as i32;
                groups[i - 1].cy = (groups[i - 1].cy * (n - texts_len) + groups[i].cy) / n;
                groups.remove(i);
                continue; // 不递增 i，重新检查当前位置
            }
        }
        i += 1;
    }

    groups
}

/// 把一个数值行归入基金：基于「符号 + 数值大小/列位置」分类，**不依赖表头列序**。
///
/// - 持有金额：本卡片数值中**无符号（正值）者且最大**，即当前市值（恒为正且无 +/- 号）。
/// - 持仓收益 / 昨日收益 的区分按平台分两套启发式：
///   * 京东/支付宝（2 列固定布局）：右列（最大 x）恒为「持仓收益」，另一有符号值 = 昨日收益。
///     该列位置信号可靠，即使某日 |昨日收益| > |持仓收益|（单日大跌）也正确。
///   * 腾讯理财通（3 列布局）：每张卡片三列表头顺序不固定，列位置不可靠，
///     改用「绝对值较大者 = 持仓收益（累计），较小者 = 昨日收益（单日）」。
/// - 百分号 → 收益率（无论出现在哪列都优先提取）。
fn assign_card_numbers(f: &mut OcrFund, card_nums: &[&NumTok], is_tencent: bool) {
    // 1) 百分号 → 收益率（优先提取，最无歧义）
    for nl in card_nums.iter().filter(|n| n.has_percent) {
        f.profit_rate = nl.value;
    }

    // 2) 剩余非百分号数值
    let rest: Vec<&&NumTok> = card_nums.iter().filter(|n| !n.has_percent).collect();
    if rest.is_empty() {
        return;
    }
    let signed_vals: Vec<&&NumTok> = rest.iter().copied().filter(|n| n.signed).collect();
    let unsigned_vals: Vec<&&NumTok> = rest.iter().copied().filter(|n| !n.signed).collect();

    // 3) 持有金额 = 无符号值中最大者（市值恒为正且无符号，通常也是本卡片最大数值）
    if !unsigned_vals.is_empty() {
        f.holding_amount = unsigned_vals.iter().map(|n| n.value).fold(0.0_f64, f64::max);
    } else if rest.len() == 2 {
        // 两个都带符号（OCR 偶发丢失市值的 + 号）：金额取绝对值较大者
        let a = rest[0].value.abs();
        let b = rest[1].value.abs();
        f.holding_amount = a.max(b);
    }

    // 4) 持仓收益 / 昨日收益
    if signed_vals.is_empty() {
        return;
    } else if signed_vals.len() == 1 {
        // 仅一个带符号值：归为持仓收益（主盈亏）
        f.holding_profit = signed_vals[0].value;
    } else if is_tencent {
        // 腾讯理财通：绝对值较大者 = 持仓收益（累计），较小者 = 昨日收益（单日）
        let mut sv: Vec<&&NumTok> = signed_vals;
        sv.sort_by(|x, y| {
            x.value
                .abs()
                .partial_cmp(&y.value.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        f.yesterday_profit = sv[0].value; // 最小绝对值 -> 昨日收益（单日）
        f.holding_profit = sv[sv.len() - 1].value; // 最大绝对值 -> 持仓收益（累计）
    } else {
        // 京东/支付宝 2 列布局：右列（最大 x）= 持仓收益，其余有符号值 = 昨日收益
        let mut sv: Vec<&&NumTok> = signed_vals;
        sv.sort_by_key(|n| n.x); // 按 x 升序，最后一个是右列
        f.holding_profit = sv[sv.len() - 1].value; // 最右 -> 持仓收益
        // 其余（左侧）有符号值归为昨日收益；若多于 2 个有符号值，取除最右外绝对值最大者
        let left: Vec<&&NumTok> = sv[..sv.len() - 1].to_vec();
        f.yesterday_profit = left
            .iter()
            .map(|n| n.value)
            .fold(f64::INFINITY, |acc, v| {
                if v.abs() < acc { v } else { acc }
            });
        if !f.yesterday_profit.is_finite() {
            f.yesterday_profit = 0.0;
        }
    }
}

/// 抽取「基金持仓」条目（几何驱动，基于真实截图验证）。
///
/// 支持多平台布局（通过 `platform` 参数区分）：
///
/// **支付宝**「我的持有」：卡片式布局，左侧基金名称（可能折行成两行），右侧两列数值——
/// 中列「金额 / 昨日收益」、右列「持有收益 / 收益率」。通常不显示 6 位基金代码。
///
/// **京东金融**「基金持仓」：类似卡片式，但有以下差异：
/// - 名称可能更长（含 ETF 联接/发起式等后缀），折行更频繁
/// - 行间可能有交易记录（「交易：1笔买入中合计50.00元」）和关注榜标签
/// - 底部有导航栏（基金/稳健/基金圈/自选/持仓）
/// - 列结构相同：名称(左) | 金额/昨日收益(中) | 持仓收益/率(右)
///
/// OCR 文本行顺序不稳定（数字常出现在名称前后），因此**必须以包围盒几何（x,y）为准**：
/// 按 y 邻近合并名称行成卡片，按 x 聚类数值列，再把每个数值关联到其左侧最近、
/// y 最邻近的名称卡片，最后按「列 + 百分号/符号」分类。
pub fn extract_fund_rows(platform: &str, lines: &[OcrLine]) -> Vec<OcrFund> {
    // 平台特定预处理：未来可在此按 platform 调整阈值/规则
    let _is_jd = platform == "jd" || platform.contains("京东");
    // 当前 JD 与支付宝共用几何驱动抽取；差异通过通用改进（chrome过滤/名称合并/纠错）覆盖。
    // 1) 过滤 UI chrome，分类为数值行 / 名称行
    let mut name_lines: Vec<&OcrLine> = Vec::new();
    let mut num_lines: Vec<NumTok> = Vec::new();
    for l in lines {
        let t = l.text.trim();
        if is_chrome(t) {
            continue;
        }
        if let Some((value, _had_dot)) = parse_number(t) {
            num_lines.push(NumTok {
                raw: l.text.clone(),
                x: l.x,
                y: l.y,
                value,
                has_percent: t.contains('%'),
                signed: is_signed(t),
            });
        } else if is_name_candidate(t) {
            name_lines.push(l);
        }
    }

    // 2) 合并相邻名称行 → 基金卡片（处理名称折行）
    let groups = merge_name_groups(&name_lines);
    if groups.is_empty() {
        return Vec::new();
    }

    // 3) 初始化基金卡片
    let mut funds: Vec<OcrFund> = groups
        .iter()
        .map(|g| OcrFund {
            code: String::new(),
            name: g.texts.join(""),
            shares: 0.0,
            nav: 0.0,
            holding_amount: 0.0,
            holding_profit: 0.0,
            yesterday_profit: 0.0,
            profit_rate: 0.0,
            confidence: 0.0,
        })
        .collect();

    // 4) 每个数值行关联到其「最近的上方名称卡片」（卡片式布局：数值在名称正下方）。
    //    不再要求名称在数值左侧（真实截图中名称行较宽、首列数值可能略偏左），
    //    改为按「数值在名称下方且纵向最近」关联，配合放宽的 NAME_Y_BAND 覆盖高分辨率卡片。
    let mut card_num_map: Vec<Vec<&NumTok>> = vec![Vec::new(); funds.len()];
    for nl in &num_lines {
        let mut best: Option<(usize, i32)> = None;
        for (i, g) in groups.iter().enumerate() {
            let dy = nl.y - g.cy; // 数值相对名称的纵向距离（正=在下方）
            if dy < -NAME_OVERLAP_TOL {
                continue; // 数值在名称上方，不可能是该卡片
            }
            if dy > NAME_Y_BAND {
                continue; // 相距过远
            }
            if g.cx > nl.x + NAME_X_TOL {
                continue; // 名称整体远在数值右侧，跨卡片，跳过
            }
            // 取纵向距离最小者（最近的名称卡片）
            if best.map_or(true, |(_, bd)| dy < bd) {
                best = Some((i, dy));
            }
        }
        match best {
            Some((i, _)) => {
                if is_fund_code(&nl.raw) {
                    funds[i].code = normalize_code(&nl.raw);
                } else {
                    card_num_map[i].push(nl);
                }
            }
            None => continue,
        }
    }

    // 5) 逐卡片分类：每张卡片的数值用 x 聚类分成左右两组，再按符号/% 分字段
    let is_tencent = platform == "tencent_licaitong";
    for (i, f) in funds.iter_mut().enumerate() {
        assign_card_numbers(f, &card_num_map[i], is_tencent);
    }

    // 5) 清理：过滤无效、按名称去重、空 code 以名称作代理主键
    funds.retain(|f| {
        !f.name.is_empty()
            && (f.holding_amount > 0.0 || f.holding_profit != 0.0 || f.shares > 0.0 || f.nav > 0.0)
    });
    let mut seen: HashSet<String> = HashSet::new();
    funds.retain(|f| {
        let key = if f.code.is_empty() { f.name.clone() } else { f.code.clone() };
        seen.insert(key)
    });
    for f in funds.iter_mut() {
        if f.code.is_empty() {
            f.code = f.name.clone();
        }
    }
    funds
}

/// 按平台选择字段定位规则（当前共用几何驱动的 unify 抽取）
pub fn parse_by_platform(platform: &str, lines: &[OcrLine]) -> Vec<OcrFund> {
    extract_fund_rows(platform, lines)
}

// ============ 交易记录抽取（买/卖/分红） ============
//
// 思路（几何驱动，与持仓抽取同源，但面向「每行一笔交易」的列表布局）：
//   1) reconstruct_rows 把文本行按 y 聚类成表格行；
//   2) 把相邻行切成「交易块」：遇到交易类型标签（买入/卖出/分红）起新块；
//      块间纵向大间隙（TXN_GAP）也起新块（兼容无类型标签的截图）；
//   3) 逐块抽取 类型 / 日期 / 代码 / 名称 / 数值，并按「净值=最小小数、
//      金额=其余与净值满足 份额×净值≈金额 者」做字段分类。
//
// 说明：交易记录字段多、三平台布局差异大，首版（无样本）识别率有限；
// 预览可手改，待用户提供样本后按平台微调阈值。

/// 净值候选上限：小于该值且含小数的数值视为单位净值/价格
const PRICE_MAX: f64 = 50.0;
/// 交易块之间起新块的纵向间隙阈值（像素）
const TXN_GAP: i32 = 45;

/// 识别交易类型标签（三平台全覆盖），返回归一化类型。
///
/// | 平台     | buy 标签        | sell 标签       | dividend 标签 |
/// |----------|-----------------|-----------------|---------------|
/// | 支付宝   | 买入 / 申购      | 卖出 / 赎回      | 分红 / 现金分红 |
/// | 京东金融 | 转入             | 转出            | 分红           |
/// | 腾讯理财通| 买入             | 取出            | (无)           |
fn detect_txn_type(s: &str) -> Option<&'static str> {
    let t = s.trim();
    // buy
    if t.contains("买入") || t.contains("申购") || t.contains("买进") || t.contains("转入") {
        return Some("buy");
    }
    // sell
    if t.contains("卖出") || t.contains("赎回") || t.contains("转出") || t.contains("取出") {
        return Some("sell");
    }
    // dividend
    if t.contains("分红") {
        return Some("dividend");
    }
    None
}

/// 从文本中剥离三平台特有的类型/前缀关键词，还原纯净基金名称。
///
/// 覆盖：
/// - 通用：现金分红 / 买入 / 卖出 / 分红 / 申购 / 赎回 / 买进
/// - 支付宝：`基金 | ` 前缀（如 "基金 | 永赢睿信混合A" → "永赢睿信混合A"）
/// - 京东金融：`转入-` / `转出-` / `分红-` 前缀（如 "转入-嘉实港股..." → "嘉实港股..."）
/// - 腾讯理财通：名称本身干净，无需额外剥离
fn strip_type_kw(s: &str) -> String {
    let mut t = s.to_string();
    // 通用类型词
    for kw in ["现金分红", "买入", "卖出", "分红", "申购", "赎回", "买进"] {
        t = t.replace(kw, "");
    }
    // 支付宝：「基金 | XXX」
    t = t.replace("基金|", "").replace("基金 | ", "");
    // 京东金融：「转入-/转出-/分红-XXX」
    for prefix in ["转入-", "转出-", "分红-"] {
        if t.starts_with(prefix) {
            t = t[prefix.len()..].to_string();
        }
    }
    t.trim().to_string()
}

/// 从文本中提取第一个日期，返回 (ISO 字符串, 是否含年份)。
/// 支持：`YYYY-MM-DD` / `YYYY/MM/DD`（含年份）以及 `MM-DD` / `MM/DD`（补当前年）。
/// 不使用正则，手写扫描；仅接受 `-` 与 `/` 作为分隔符，规避把金额(含`.`)或时间(含`:`)误判为日期。
fn extract_first_date(s: &str) -> Option<(String, bool)> {
    let chars: Vec<char> = s.chars().collect();
    let is_sep = |c: char| c == '-' || c == '/';
    let valid_md = |m: u32, d: u32| (1..=12).contains(&m) && (1..=31).contains(&d);

    for i in 0..chars.len() {
        // 1) 四位年份分支：YYYY[-/]MM[-/]DD
        if i + 9 <= chars.len()
            && chars[i..i + 4].iter().all(|c| c.is_ascii_digit())
            && is_sep(chars[i + 4])
        {
            let y: String = chars[i..i + 4].iter().collect();
            let mut p = i + 5;
            let mut mm = String::new();
            while p < chars.len() && chars[p].is_ascii_digit() && mm.len() < 2 {
                mm.push(chars[p]);
                p += 1;
            }
            if !mm.is_empty() && p < chars.len() && is_sep(chars[p]) {
                let mut q = p + 1;
                let mut dd = String::new();
                while q < chars.len() && chars[q].is_ascii_digit() && dd.len() < 2 {
                    dd.push(chars[q]);
                    q += 1;
                }
                if let (Ok(m), Ok(d)) = (mm.parse::<u32>(), dd.parse::<u32>()) {
                    if valid_md(m, d) {
                        return Some((format!("{}-{:02}-{:02}", y, m, d), true));
                    }
                }
            }
        }

        // 2) 月日分支：MM[-/]DD（1~2 位），不接在四位数字之后
        if i + 5 <= chars.len() {
            let mut p = i;
            let mut mm = String::new();
            while p < chars.len() && chars[p].is_ascii_digit() && mm.len() < 2 {
                mm.push(chars[p]);
                p += 1;
            }
            if (1..=2).contains(&mm.len()) && p < chars.len() && is_sep(chars[p]) {
                let mut q = p + 1;
                let mut dd = String::new();
                while q < chars.len() && chars[q].is_ascii_digit() && dd.len() < 2 {
                    dd.push(chars[q]);
                    q += 1;
                }
                if (1..=2).contains(&dd.len()) {
                    if let (Ok(m), Ok(d)) = (mm.parse::<u32>(), dd.parse::<u32>()) {
                        if valid_md(m, d) {
                            // 当前年（识别期）；预览会提示「无年份」
                            let year = chrono_year();
                            return Some((format!("{}-{:02}-{:02}", year, m, d), false));
                        }
                    }
                }
            }
        }

        // 3) 中文日期分支：YYYY年M月D日（含年份）/ M月D日（补当前年）
        //    例如「2026年8月11日」「8月11日」。仅当数字后紧跟 年/月/日 才命中，避免误判金额。
        if i + 2 <= chars.len() && chars[i].is_ascii_digit() {
            let mut num = String::new();
            let mut p = i;
            while p < chars.len() && chars[p].is_ascii_digit() && num.len() < 4 {
                num.push(chars[p]);
                p += 1;
            }
            if p < chars.len() {
                if chars[p] == '年' {
                    // 年 -> 月 -> 日
                    let mut q = p + 1;
                    let mut mm = String::new();
                    while q < chars.len() && chars[q].is_ascii_digit() && mm.len() < 2 {
                        mm.push(chars[q]);
                        q += 1;
                    }
                    if !mm.is_empty() && q < chars.len() && chars[q] == '月' {
                        let mut r2 = q + 1;
                        let mut dd = String::new();
                        while r2 < chars.len() && chars[r2].is_ascii_digit() && dd.len() < 2 {
                            dd.push(chars[r2]);
                            r2 += 1;
                        }
                        if (1..=2).contains(&dd.len()) && r2 < chars.len() && chars[r2] == '日' {
                            if let (Ok(m), Ok(d)) = (mm.parse::<u32>(), dd.parse::<u32>()) {
                                if valid_md(m, d) {
                                    return Some((format!("{}-{:02}-{:02}", num, m, d), true));
                                }
                            }
                        }
                    }
                } else if chars[p] == '月' {
                    // M月D日（无年）
                    let mut q = p + 1;
                    let mut dd = String::new();
                    while q < chars.len() && chars[q].is_ascii_digit() && dd.len() < 2 {
                        dd.push(chars[q]);
                        q += 1;
                    }
                    if (1..=2).contains(&dd.len()) && q < chars.len() && chars[q] == '日' {
                        if let (Ok(m), Ok(d)) = (num.parse::<u32>(), dd.parse::<u32>()) {
                            if valid_md(m, d) {
                                return Some((format!("{}-{:02}-{:02}", chrono_year(), m, d), false));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// 从文本中提取第一个时间（返回 "HH:MM"，小时归一化为两位）。
///
/// 支持：
/// - "2026-07-22 21:16:32" / "2026-07-22 21:16" → "21:16"
/// - "08-13 22:15:26" → "22:15"
/// - 独立时间 "23:04:20" / "21:16" → "23:04" / "21:16"
///
/// 安全约束：仅接受 `H:MM` / `HH:MM` / `HH:MM:SS` 形态（冒号前后为数字），
/// 不会把纯日期（如 "2026-08-11"）或金额（如 "1,000.00"）误判为时间。
fn extract_first_time(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // 收集最多 2 位小时
        let mut hh = String::new();
        let mut p = i;
        while p < chars.len() && chars[p].is_ascii_digit() && hh.len() < 2 {
            hh.push(chars[p]);
            p += 1;
        }
        // 必须紧跟冒号
        if p >= chars.len() || chars[p] != ':' {
            i = p.max(i + 1);
            continue;
        }
        p += 1;
        // 收集 2 位分钟
        let mut mm = String::new();
        while p < chars.len() && chars[p].is_ascii_digit() && mm.len() < 2 {
            mm.push(chars[p]);
            p += 1;
        }
        if mm.len() < 2 {
            i = p.max(i + 1);
            continue;
        }
        let hh2 = if hh.len() == 1 {
            format!("0{hh}")
        } else {
            hh
        };
        return Some(format!("{hh2}:{mm}"));
    }
    None
}

/// 从一个交易块的全部文本行（含被 is_chrome 过滤的）中同时提取
/// (日期 ISO, 是否含年份, 时间 HH:MM)。
///
/// 优先级：
/// 1) 含日期的单元格（如 "2026-07-22 21:16:32" / "08-13 22:15:26"）：同单元格直接取时间；
/// 2) 若日期单元格未带时间，且存在独立的「纯时间」单元格（如腾讯理财通常把 "21:16"
///    单独成行，而该单元格会被 is_chrome 当成页面状态栏时间过滤掉），则从原始单元格补取。
///
/// 页面顶部状态栏时间（如 "21:32"）通常不在交易块内——交易块由类型标签/纵向间隙切分，
/// 状态栏会被切到独立块且该块无类型会被整体跳过，故不会被误取。
fn extract_block_datetime(cells: &[&OcrLine]) -> (String, bool, String) {
    let mut date = String::new();
    let mut has_year = false;
    let mut time = String::new();

    // 第一遍：优先取「日期+时间」同单元格（最可靠）；时间字段优先带年份单元格
    for c in cells {
        if let Some((d, hy)) = extract_first_date(&c.text) {
            if (hy && !has_year) || date.is_empty() {
                date = d;
                has_year = hy;
            }
            if time.is_empty() {
                if let Some(tm) = extract_first_time(&c.text) {
                    time = tm;
                }
            }
        }
    }

    // 第二遍：日期单元格未带时间时，从独立时间单元格补取（跳过已含日期的单元格）
    if time.is_empty() {
        for c in cells {
            if extract_first_date(&c.text).is_some() {
                continue;
            }
            if let Some(tm) = extract_first_time(&c.text) {
                time = tm;
                break;
            }
        }
    }

    (date, has_year, time)
}

/// 当前年份（用于无年份日期补足）。无 chrono 依赖时回退 2026。
fn chrono_year() -> i32 {
    // 使用 time::OffsetDateTime 若可用；否则固定 2026。
    // 为保持零依赖，这里用简单回退（交易记录多为当年，足够预览提示）。
    2026
}

/// 抽取「交易记录」条目（几何驱动）。
///
/// 支持三平台交易列表截图（支付宝/京东金融/腾讯理财通）。首版三平台共用同一几何
/// 抽取逻辑，差异通过通用改进（chrome 过滤 / 类型标签 / 数值分类）覆盖；待用户提供
/// 样本后按平台微调阈值。
pub fn extract_txn_rows(platform: &str, lines: &[OcrLine]) -> Vec<OcrTxn> {
    let _ = platform; // 首版三平台共用；后续可按 platform 调阈值
    let rows = reconstruct_rows(lines);
    if rows.is_empty() {
        return Vec::new();
    }

    // 1) 把行切成「交易块」
    let mut blocks: Vec<Vec<&OcrRow>> = Vec::new();
    let mut cur: Vec<&OcrRow> = Vec::new();
    let mut last_bottom: Option<i32> = None;
    for r in &rows {
        let row_bottom = r.y + r.cells.iter().map(|c| c.h).max().unwrap_or(0);
        let has_type = r.cells.iter().any(|c| detect_txn_type(&c.text).is_some());
        let mut new_block = false;
        if !cur.is_empty() {
            if has_type {
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
        cur.push(r);
        last_bottom = Some(row_bottom);
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }

    // 兜底：整页未被类型/间隙切分（可能整页就是一条或没识别到类型标签），
    // 且行数较多时，按更宽松的间隙(70px)再切一次。
    if blocks.len() <= 1 && rows.len() > 4 {
        let mut b2: Vec<Vec<&OcrRow>> = Vec::new();
        let mut c: Vec<&OcrRow> = Vec::new();
        let mut prev: Option<i32> = None;
        for r in &rows {
            if let Some(p) = prev {
                if r.y - p > 70 {
                    b2.push(std::mem::take(&mut c));
                }
            }
            c.push(r);
            prev = Some(r.y + r.cells.iter().map(|cc| cc.h).max().unwrap_or(0));
        }
        if !c.is_empty() {
            b2.push(c);
        }
        if b2.len() > 1 {
            blocks = b2;
        }
    }

    // 2) 逐块抽取字段（平台感知）
    let mut txns: Vec<OcrTxn> = Vec::new();
    // 日期/时间上下文：腾讯理财通等「按日期分组」布局中，日期常位于分组标题块，
    // 各交易卡自身不含日期/时间。标题块会被下方「无类型即跳过」逻辑丢弃，故需要把
    // 最近一次看到的日期/时间向前传递，供后续交易卡沿用。
    let mut ctx_date = String::new();
    let mut ctx_has_year = false;
    let mut ctx_time = String::new();
    for blk in &blocks {
        let cells: Vec<&OcrLine> = blk
            .iter()
            .flat_map(|r| r.cells.iter())
            .filter(|c| !is_chrome(c.text.trim()))
            .collect();
        if cells.is_empty() {
            continue;
        }

        // --- 类型（三平台全覆盖）---
        let mut txn_type: Option<&'static str> = None;
        let mut txn_type_raw = String::new();
        for c in &cells {
            if let Some(t) = detect_txn_type(&c.text) {
                txn_type = Some(t);
                // 原始标签保留（用于预览展示）
                txn_type_raw = match t {
                    "buy" => {
                        if c.text.contains("转入") { "转入".to_string() }
                        else { "买入".to_string() }
                    }
                    "sell" => {
                        if c.text.contains("转出") || c.text.contains("取出") { 
                            if c.text.contains("取出") { "取出".to_string() } else { "转出".to_string() }
                        } else { "卖出".to_string() }
                    }
                    _ => "分红".to_string(),
                };
                break;
            }
        }

        // --- 日期 + 时间（优先含年份的日期；时间优先同单元格、其次独立时间单元格）---
        // 用原始（未过滤 chrome）单元格，确保腾讯理财通那种被 is_chrome 当成状态栏
        // 而单独成行的时间（如 "21:16"）也能被补取到。
        let raw_block_cells: Vec<&OcrLine> = blk.iter().flat_map(|r| r.cells.iter()).collect();
        let (date, has_year, time) = extract_block_datetime(&raw_block_cells);

        // 更新日期/时间上下文，并取「有效」值：本块自身有则用自身，否则沿用最近一次上下文
        // （覆盖「日期在分组标题块、交易卡自身无日期」的布局）。
        if !date.is_empty() {
            ctx_date = date.clone();
            ctx_has_year = has_year;
        }
        if !time.is_empty() {
            ctx_time = time.clone();
        }
        let eff_date = if date.is_empty() { ctx_date.clone() } else { date.clone() };
        let eff_has_year = if date.is_empty() { ctx_has_year } else { has_year };
        let eff_time = if time.is_empty() { ctx_time.clone() } else { time.clone() };

        // --- 代码（6 位数字）---
        let mut code = String::new();
        for c in &cells {
            if is_fund_code(&c.text) {
                code = normalize_code(&c.text);
                break;
            }
        }

        // --- 名称（CJK 候选，排除 chrome/代码/日期/数值/类型/状态，再按平台剥前缀）---
        let mut name_parts: Vec<String> = Vec::new();
        for c in &cells {
            let t = c.text.trim();
            if t.is_empty() || is_chrome(t) || is_fund_code(t) {
                continue;
            }
            if extract_first_date(t).is_some() || parse_number(t).is_some() {
                continue;
            }
            // 类型 cell → 剥离类型词后若仍是名称候选则收录
            if detect_txn_type(t).is_some() {
                let cleaned = strip_type_kw(t);
                if is_name_candidate(&cleaned) && !cleaned.is_empty() {
                    name_parts.push(clean_name(&cleaned));
                }
                continue;
            }
            // 普通名称候选
            if is_name_candidate(t) {
                // 再做一轮平台前缀剥离（防御性：strip_type_kw 已覆盖大部分场景）
                let cleaned = strip_type_kw(t);
                if is_name_candidate(&cleaned) {
                    name_parts.push(clean_name(&cleaned));
                }
            }
        }
        let name = name_parts.join("");

        // --- 数值分类（金额 / 份额 / 净值）---
        let mut price = 0.0;
        let mut amount = 0.0;
        let mut shares = 0.0;
        let mut nums: Vec<(f64, bool, i32)> = Vec::new(); // (值, 是否有符号, x坐标)
        for c in &cells {
            let t = c.text.trim();
            if is_fund_code(t) || extract_first_date(t).is_some() {
                continue;
            }
            if let Some((v, _)) = parse_number(t) {
                nums.push((v, is_signed(t), c.x));
            }
        }
        if !nums.is_empty() {
            // 1) 净值候选：含小数、正值、< PRICE_MAX 的最小值。
            //    重要：仅当存在 ≥2 个「其余数值」时才启用净值分类；
            //    否则（如分红只有 1 个小数 1.26）该小数就是金额，不是净值。
            let price_cands: Vec<f64> = nums
                .iter()
                .filter(|(v, _, _)| v.fract() != 0.0 && *v > 0.0 && *v < PRICE_MAX)
                .map(|(v, _, _)| *v)
                .collect();
            let others_count = nums.iter().filter(|(v, _, _)| (*v - if !price_cands.is_empty() { price_cands[0] } else { 0.0 }).abs() > 0.001).count();
            if !price_cands.is_empty() && others_count >= 2 {
                price = price_cands.into_iter().fold(f64::INFINITY, f64::min);
            }

            // 2) 其余数值（排除已选净值）：按 x 坐标从大到小排序（右侧优先=金额）
            let mut others: Vec<f64> = nums
                .iter()
                .filter(|(v, _, _)| (*v - price).abs() > 0.001)
                .map(|(v, _, _)| *v)
                .collect();
            others.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

            if price > 0.0 && others.len() >= 2 {
                // 有净值 + ≥2 个其余数值：尝试份额×净值≈金额 配对
                let (x, y) = (others[0], others[1]);
                let prod1 = (x * price).round();
                let prod2 = (y * price).round();
                if (prod1 - y.round()).abs() < 1.0 {
                    shares = x; amount = y;
                } else if (prod2 - x.round()).abs() < 1.0 {
                    shares = y; amount = x;
                } else {
                    // 不满足乘积关系：金额取最大者（通常在最右侧），份额取另一个
                    amount = x.max(y);
                    shares = if (amount - x).abs() < 0.001 { y } else { x };
                }
            } else if price > 0.0 && others.len() == 1 {
                // 有净值 + 1 个其余数值 → 视为金额，反推份额
                amount = others[0].abs();
                if txn_type == Some("buy") || txn_type == Some("sell") {
                    let s = amount / price;
                    if s > 0.0 && s.is_finite() {
                        shares = (s * 100.0).round() / 100.0;
                    }
                }
            } else if price == 0.0 && !others.is_empty() {
                // 无净值：金额取最大正数绝对值（最右侧的大数通常是金额）
                let positive: Vec<f64> = others.iter().copied().filter(|v| *v > 0.0).collect();
                if !positive.is_empty() {
                    amount = positive.into_iter().fold(0.0_f64, f64::max);
                    let rem: Vec<f64> = others.iter().copied()
                        .filter(|v| (*v - amount).abs() > 0.001 && *v > 0.0)
                        .collect();
                    if rem.len() == 1 {
                        shares = rem[0];
                        if txn_type == Some("dividend") {
                            shares = 0.0;
                        }
                    }
                } else if others.len() >= 1 {
                    // 全是负数或零（如腾讯理财通卖出 -163.75）：取绝对值最大者为金额
                    amount = others.iter().map(|v| v.abs()).fold(0.0_f64, f64::max);
                }
            }
        }

        // 卖出时金额取绝对值（腾讯用负数表示流出）
        if amount < 0.0 {
            amount = -amount;
        }

        // --- 置信度 ---
        let mut conf: f64 = 0.0;
        if txn_type.is_some() {
            conf += 0.4;
        }
        conf += if eff_has_year {
            0.3
        } else if !eff_date.is_empty() {
            0.15
        } else {
            0.0
        };
        if !code.is_empty() || !name.is_empty() {
            conf += 0.15;
        }
        if amount > 0.0 {
            conf += 0.15;
        }
        conf = conf.min(1.0);

        // 至少要有类型，或（名称 + 金额）才算一条交易
        if txn_type.is_none() && (name.is_empty() || amount <= 0.0) {
            continue;
        }

        txns.push(OcrTxn {
            txn_type: txn_type.unwrap_or("buy").to_string(),
            txn_type_raw,
            date: eff_date,
            has_year: eff_has_year,
            time: eff_time,
            code,
            name,
            shares,
            amount,
            price,
            confidence: conf,
        });
    }

    txns
}

// ---- 文本工具 ----

/// 6 位连续数字视为基金代码（含以 0 开头的代码，如 000001）。
/// 容错：允许前导中文标签（代码/基金代码）与中间空格（OCR 偶发的 "110 011"），
/// 但仍拒绝含小数点的数值（如 "1685.00" -> 168500 仅 6 位数字，含 '.' 故不匹配）。
fn is_fund_code(s: &str) -> bool {
    let t = s.trim();
    let t = t
        .strip_prefix("基金代码")
        .or_else(|| t.strip_prefix("基金"))
        .or_else(|| t.strip_prefix("代码"))
        .unwrap_or(t)
        .trim_start_matches([':', '：'])
        .trim();
    // 去除前导/尾随非数字字符（如括号、单位），再检查中间是否为 6 位连续数字
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    let only_digit_or_space = t.chars().all(|c| c.is_ascii_digit() || c == ' ');
    digits.len() == 6 && only_digit_or_space
}

fn normalize_code(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// 清理名称：保留 CJK、ASCII 字母与数字，去掉标点/空格/换行，并应用 OCR 纠错
fn clean_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| {
            (0x4E00..=0x9FFF).contains(&(*c as u32))
                || c.is_ascii_alphanumeric()
                || c.is_ascii_whitespace()
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("");
    // 尝试 OCR 字符纠错
    correct_ocr_char(&cleaned).unwrap_or(cleaned)
}

/// 解析数字：去逗号/空格/%，保留小数点与正负号，返回 (值, 是否含小数点)。
///
/// 安全约束：原始字符串必须**以数字或正负号开头**（允许前导空白），
/// 防止含 CJK/字母的基金名称（如「南方中证A500ETF」）被误解析为数字 500。
fn parse_number(s: &str) -> Option<(f64, bool)> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 安全检查：必须以 [+-.0-9] 开头，否则不是纯数值文本
    let first = trimmed.chars().next()?;
    if !first.is_ascii_digit() && first != '+' && first != '-' && first != '.' {
        return None;
    }

    let had_dot = s.contains('.');
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok().map(|v| (v, had_dot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: i32, y: i32) -> OcrLine {
        OcrLine {
            text: text.into(),
            score: 0.95,
            x,
            y,
            w: text.len() as i32 * 10,
            h: 20,
        }
    }

    #[test]
    fn reconstruct_groups_by_row_and_sorts_x() {
        let lines = vec![
            line("110011", 300, 100),
            line("易方达蓝筹", 50, 100),
            line("000001", 300, 200),
            line("华夏成长", 50, 200),
        ];
        let rows = reconstruct_rows(&lines);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells[0].text, "易方达蓝筹");
        assert_eq!(rows[0].cells[1].text, "110011");
    }

    /// 模拟一张卡片：名称在左侧（可折行），右侧两列数值
    /// 真实支付宝布局：中列(x=300)=金额/昨日收益；右列(x=520)=持有收益/收益率
    fn fund_lines(
        idx: i32,
        name1: &str,
        name2: Option<&str>,
        amount: &str,     // 中列，无符号
        profit: &str,     // 右列，有符号
        yest: &str,       // 中列，有符号
        rate: &str,       // 右列，含%
    ) -> Vec<OcrLine> {
        let base = 100 + idx * 140;
        let cy = base + 20;
        let mut v = Vec::new();
        v.push(line(name1, 40, cy - if name2.is_some() { 12 } else { 0 }));
        if let Some(n2) = name2 {
            v.push(line(n2, 40, cy + 12));
        }
        // 中列 (x~300): 金额(上) / 昨日收益(下)
        v.push(line(amount, 300, cy - 15));
        v.push(line(yest, 300, cy + 15));
        // 右列 (x~520): 持有收益(上) / 收益率(下)
        v.push(line(profit, 520, cy - 15));
        v.push(line(rate, 520, cy + 15));
        v
    }

    #[test]
    fn extract_alipay_holdings_real_layout() {
        // 顶部 UI chrome（应被过滤）
        let mut lines = vec![
            line("14:34", 700, 20),
            line("我的持有", 40, 40),
            line("金额排序", 200, 40),
            line("名称", 40, 60),
            line("持有收益/率", 300, 60),
            line("金额/昨日收益", 520, 60),
            line("金选指数基金", 40, 300), // 标签，不应成为名称
        ];
        lines.extend(fund_lines(0, "鹏华酒指数C", None, "16,117.50", "-9,788.47", "+203.00", "-36.53%"));
        lines.extend(fund_lines(1, "中欧医疗健康", Some("混合A"), "11,024.00", "-746.02", "-54.08", "-6.34%"));
        lines.extend(fund_lines(2, "博时恒生医疗", Some("保健ETF联接..."), "9,992.40", "+965.46", "-59.40", "+10.70%"));
        lines.extend(fund_lines(3, "建信高端装备", Some("股票A"), "8,621.03", "+2,966.15", "+137.55", "+52.45%"));
        lines.extend(fund_lines(4, "天弘恒生科技", Some("ETF联接（QDII)C"), "8,168.64", "-292.75", "-80.01", "-3.46%"));
        lines.extend(fund_lines(5, "富国高质量混", Some("合"), "8,163.71", "-2,147.74", "-5.29", "-20.83%"));

        let funds = extract_fund_rows("alipay", &lines);
        assert_eq!(
            funds.len(),
            6,
            "应识别 6 支基金，实际：{:?}",
            funds.iter().map(|f| f.name.clone()).collect::<Vec<_>>()
        );

        let get = |name: &str| funds.iter().find(|f| f.name.contains(name)).expect(name);

        let f1 = get("鹏华酒指数C");
        assert!((f1.holding_amount - 16117.50).abs() < 0.01);
        assert!((f1.holding_profit + 9788.47).abs() < 0.01);
        assert!((f1.yesterday_profit - 203.00).abs() < 0.01);
        assert!((f1.profit_rate + 36.53).abs() < 0.01);

        let f2 = get("中欧医疗健康");
        assert!(f2.name.contains("混合A"));
        assert!((f2.holding_amount - 11024.00).abs() < 0.01);
        assert!((f2.holding_profit + 746.02).abs() < 0.01); // OCR 实际读到 -746.02
        assert!((f2.yesterday_profit + 54.08).abs() < 0.01);
        assert!((f2.profit_rate + 6.34).abs() < 0.01);

        let f3 = get("博时恒生医疗");
        assert!((f3.holding_amount - 9992.40).abs() < 0.01);
        assert!((f3.holding_profit - 965.46).abs() < 0.01);
        assert!((f3.yesterday_profit + 59.40).abs() < 0.01);
        assert!((f3.profit_rate - 10.70).abs() < 0.01);

        let f4 = get("建信高端装备");
        assert!((f4.holding_amount - 8621.03).abs() < 0.01);
        assert!((f4.holding_profit - 2966.15).abs() < 0.01);
        assert!((f4.yesterday_profit - 137.55).abs() < 0.01);
        assert!((f4.profit_rate - 52.45).abs() < 0.01);

        let f5 = get("天弘恒生科技");
        assert!((f5.holding_amount - 8168.64).abs() < 0.01);
        assert!((f5.holding_profit + 292.75).abs() < 0.01);
        assert!((f5.yesterday_profit + 80.01).abs() < 0.01);
        assert!((f5.profit_rate + 3.46).abs() < 0.01);

        let f6 = get("富国高质量");
        assert!(f6.name.contains("混合"));
        assert!((f6.holding_amount - 8163.71).abs() < 0.01);
        assert!((f6.holding_profit + 2147.74).abs() < 0.01);
        assert!((f6.yesterday_profit + 5.29).abs() < 0.01);
        assert!((f6.profit_rate + 20.83).abs() < 0.01);
    }

    /// 多图回归测试：两张**同布局**截图，各含一只基金，且基金在**相同相对 y**（每张图 y 都从 0 起）。
    /// 这正是最易踩坑的场景——旧 `import_screenshots` 把所有图的行拼成一条平铺列表再按 y 聚类，
    /// 图2 的 y=120 名称行会和图1 的 y=120 名称行被 `merge_name_groups` 误并成一只「四合一」基金，
    /// 数值再跨图错配 → 用户看到的「多个截图识别就全乱了」。
    /// 修复后 `import_screenshots` 逐图独立 `extract_fund_rows` 再合并，本测试固化该契约。
    #[test]
    fn extract_holdings_multi_image_isolated() {
        // 截图 1：易方达蓝筹精选混合，cy=120
        let img1 = fund_lines(0, "易方达蓝筹", Some("精选混合"), "16,117.50", "-9,788.47", "+203.00", "-36.53%");
        // 截图 2：华夏成长价值，cy=120（与图1 相对 y 完全相同，模拟另一张同布局截图）
        let img2 = fund_lines(0, "华夏成长", Some("价值"), "5,000.00", "+100.00", "-20.00", "+2.00%");

        // 逐图独立抽取：每张图各自得到自己的 1 只基金
        let f1 = extract_fund_rows("alipay", &img1);
        let f2 = extract_fund_rows("alipay", &img2);
        assert_eq!(f1.len(), 1, "截图1 应识别 1 只基金");
        assert_eq!(f2.len(), 1, "截图2 应识别 1 只基金");
        assert!(f1[0].name.contains("易方达蓝筹"), "截图1 基金名误：{}", f1[0].name);
        assert!(f2[0].name.contains("华夏成长"), "截图2 基金名误：{}", f2[0].name);
        assert!((f1[0].holding_amount - 16117.50).abs() < 0.01);
        assert!((f2[0].holding_amount - 5000.00).abs() < 0.01);

        // 合并（模拟旧的多图拼接写法）：两图 y=120 名称行被误并成一只基金 → 数量 < 2
        let mut combined = img1.clone();
        combined.extend(img2.clone());
        let merged = extract_fund_rows("alipay", &combined);
        assert!(
            merged.len() < 2,
            "回归保护：旧的多图拼接写法会把两张图误并为 {} 只基金（应为 2 只），若此断言失败说明『多图全乱』又回来了",
            merged.len()
        );
    }

    #[test]
    fn extract_ignores_chrome_only() {
        let lines = vec![
            line("我的持有", 40, 40),
            line("金额排序", 200, 40),
            line("排行", 100, 80),
            line("基金市场", 300, 80),
        ];
        assert!(extract_fund_rows("alipay", &lines).is_empty());
    }

    #[test]
    fn dedup_same_fund_across_cards() {
        let mut lines = fund_lines(0, "鹏华酒指数C", None, "16,117.50", "-9,788.47", "+203.00", "-36.53%");
        // 第二张卡片同名（模拟两次识别同一基金）
        lines.extend(fund_lines(1, "鹏华酒指数C", None, "16,117.50", "-9,788.47", "+203.00", "-36.53%"));
        assert_eq!(extract_fund_rows("alipay", &lines).len(), 1);
    }

    // ============ 交易记录抽取测试（基于真实截图样本） ============

    /// 构造一笔支付宝风格交易块：
    /// 布局：类型(左,红/蓝) | "基金 | 名称"(中) | 金额(右) | 日期(下)
    fn alipay_txn_block(
        y: i32,
        txn_type: &str,   // "买入" / "卖出" / "分红"
        fund_name: &str,
        amount: &str,
        date: &str,
    ) -> Vec<OcrLine> {
        let mut v = Vec::new();
        v.push(line(txn_type, 30, y));                          // 类型标签
        v.push(line(&format!("基金 | {}", fund_name), 120, y)); // 名称（含「基金 | 」前缀）
        v.push(line(amount, 520, y));                           // 金额
        v.push(line(date, 200, y + 28));                        // 日期（在名称下方）
        v
    }

    /// 构造一笔京东金融风格交易块：
    /// 布局：[基金](左圆标) | "转入-/转出-/分红-名称"(中) | 金额(右) | 状态(右下) | 月日(中下)
    fn jd_txn_block(
        y: i32,
        action: &str,       // "转入-" / "转出-" / "分红-"
        fund_name: &str,
        amount: &str,
        status: &str,       // "支付成功" / "订单完成" / "现金发放"
        md_date: &str,      // "08-13 22:15:26" (无年份！)
    ) -> Vec<OcrLine> {
        let mut v = Vec::new();
        v.push(line("基金", 30, y));                                  // 圆标文字
        v.push(line(&format!("{}{}", action, fund_name), 100, y));     // 动作+名称
        v.push(line(amount, 520, y));                                 // 金额
        v.push(line(status, 520, y + 24));                            // 状态
        v.push(line(md_date, 100, y + 48));                          // 月日日期
        v
    }

    /// 构造一笔腾讯理财通风格交易块：
    /// 布局：类型(左,蓝) | 名称(中) | ±金额(右) | 支付方式(中下) | 状态(右下) | 日期(下)
    fn tencent_txn_block(
        y: i32,
        txn_type: &str,   // "买入" / "取出" / "转换"
        fund_name: &str,
        amount: &str,      // "+500.00元" / "-163.75元"
        pay_method: &str,  // "银行卡买入" / "取出到\"活期+\"" / "转换成功"
        date: &str,
    ) -> Vec<OcrLine> {
        let mut v = Vec::new();
        v.push(line(txn_type, 30, y));
        v.push(line(fund_name, 100, y));
        v.push(line(amount, 520, y));
        v.push(line(pay_method, 100, y + 24));
        v.push(line(date, 100, y + 48));
        v
    }

    #[test]
    fn extract_alipay_buy_transactions() {
        // 模拟 lz1.jpg 前 3 条：买入永赢睿信 100元、买入易方达北证50 100元、买入方正富邦 1000元
        let mut lines = vec![
            line("全部持有", 60, 35),
            line("收益明细", 180, 35),
            line("交易记录", 300, 35),
            line("明细", 40, 65),
            line("基金", 120, 65),
            line("全部", 400, 65),
        ];
        lines.extend(alipay_txn_block(130, "买入", "永赢睿信混合A", "100.00元", "2026-08-11 23:04:20"));
        lines.extend(alipay_txn_block(230, "买入", "易方达北证50指数A", "100.00元", "2026-08-11 23:03:54"));
        lines.extend(alipay_txn_block(340, "买入", "方正富邦中证保险主题指数(LOF)A", "1,000.00元", "2026-08-11 22:59:54"));
        // 一条卖出
        lines.extend(alipay_txn_block(460, "卖出", "圆信永丰兴源灵活配置混合A", "691.51元", "2026-08-11 22:53:48"));

        let txns = extract_txn_rows("alipay", &lines);
        assert_eq!(txns.len(), 4, "应识别 4 条支付宝交易，实际：{:?}", txns.iter().map(|t| (&*t.txn_type, t.name.clone(), t.amount)).collect::<Vec<_>>());

        // 第一条：买入永赢睿信
        let t0 = &txns[0];
        assert_eq!(t0.txn_type, "buy");
        assert!(t0.name.contains("永赢睿信"), "名称应含'永赢睿信'，实际 '{}'", t0.name);
        assert!((t0.amount - 100.00).abs() < 0.01);
        assert_eq!(t0.date, "2026-08-11");
        assert!(t0.has_year);
        // 交易时间（HH:MM）应被正确抽取
        assert_eq!(t0.time, "23:04", "支付宝交易时间应识别为 23:04，实际 '{}'", t0.time);

        // 卖出条
        let sell = txns.iter().find(|t| t.txn_type == "sell").expect("应有卖出");
        assert!((sell.amount - 691.51).abs() < 0.01);
        assert_eq!(sell.time, "22:53", "卖出交易时间应识别为 22:53，实际 '{}'", sell.time);
    }

    #[test]
    fn extract_txn_rows_multi_image_isolated() {
        // 回归：两张同布局的支付宝交易截图，交易卡在完全相同的相对 y 坐标。
        // 旧实现把多图 OCR 行拼成一条平铺列表再整体抽取，两图同名 y 会被聚成同一行/块，
        // 字段跨图错配、条数变少（即用户反馈的「多图识别全乱」）。修复后必须逐张图独立抽取再合并。
        let img1: Vec<OcrLine> = alipay_txn_block(
            130, "买入", "永赢睿信混合A", "100.00元", "2026-08-11 23:04:20",
        );
        let img2: Vec<OcrLine> = alipay_txn_block(
            130, "买入", "易方达北证50指数A", "100.00元", "2026-08-11 23:03:54",
        );
        // 1) 逐张图独立抽取，各自应得到 1 条
        let t1 = extract_txn_rows("alipay", &img1);
        let t2 = extract_txn_rows("alipay", &img2);
        assert_eq!(t1.len(), 1, "图1 应识别 1 条，实际 {:?}", t1.iter().map(|t| (&*t.txn_type, t.name.clone())).collect::<Vec<_>>());
        assert_eq!(t2.len(), 1, "图2 应识别 1 条，实际 {:?}", t2.iter().map(|t| (&*t.txn_type, t.name.clone())).collect::<Vec<_>>());
        // 2) 合并（修复后做法）应得 2 条，且两只基金都在
        let merged: Vec<_> = t1.into_iter().chain(t2.into_iter()).collect();
        assert_eq!(merged.len(), 2, "合并应得 2 条，实际 {:?}", merged.iter().map(|t| (&*t.txn_type, t.name.clone())).collect::<Vec<_>>());
        assert!(merged.iter().any(|t| t.name.contains("永赢睿信")), "应含永赢睿信");
        assert!(merged.iter().any(|t| t.name.contains("易方达北证50")), "应含易方达北证50");
        // 3) 反向验证：若把两图拼成一条列表再抽（旧实现），只会得到 <2 条（字段错配/合并），
        //    以此证明「逐图独立抽取」是必要契约、不能再回潮为多图拼接。
        let mut combined = img1.clone();
        combined.extend(img2);
        let bad = extract_txn_rows("alipay", &combined);
        assert!(
            bad.len() < 2,
            "旧式拼接应得到 <2 条（证明多图隔离的必要性），实际 {} 条 {:?}",
            bad.len(),
            bad.iter().map(|t| (&*t.txn_type, t.name.clone(), t.amount)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn extract_jd_finance_transfer_in() {
        // 模拟 lj1.jpg 前 3 条：转入嘉实港股 200元、转入华富永鑫 50元、转入华夏恒生科技 100元
        let mut lines = vec![
            line("账户明细", 200, 25),
            line("资产", 80, 70),
            line("收益", 200, 70),
            line("交易", 320, 70),
            line("交易明细", 40, 110),
            line("全部", 380, 110),
        ];
        lines.extend(jd_txn_block(170, "转入-", "嘉实港股互联网产业核心资产混合C", "200.00元", "支付成功", "08-13 22:15:26"));
        lines.extend(jd_txn_block(290, "转入-", "华富永鑫灵活配置混合C", "50.00元", "支付成功", "08-13 22:14:45"));
        lines.extend(jd_txn_block(410, "转入-", "华夏恒生科技ETF发起式联接(QDII)C", "100.00元", "订单完成", "08-13 14:52:25"));
        // 一条分红
        lines.extend(jd_txn_block(540, "分红-", "东方红中证东方红红利低波动指数证券投资基金A类", "9.94元", "现金发放", "08-11 16:28:57"));

        let txns = extract_txn_rows("jd_finance", &lines);
        assert_eq!(txns.len(), 4, "应识别 4 条京东交易，实际：{:?}", txns.iter().map(|t| (&*t.txn_type, t.name.clone())).collect::<Vec<_>>());

        // 转入 → buy
        assert_eq!(txns[0].txn_type, "buy");
        assert!(txns[0].name.contains("嘉实港股"), "名称应含'嘉实港股'，实际 '{}'", txns[0].name);
        assert!((txns[0].amount - 200.00).abs() < 0.01);
        // 京东无年份
        assert!(!txns[0].has_year, "京东日期应标记无年份");
        // 京东交易时间（与日期同单元格 "08-13 22:15:26"）
        assert_eq!(txns[0].time, "22:15", "京东交易时间应识别为 22:15，实际 '{}'", txns[0].time);

        // 分红 → dividend
        let div = txns.iter().find(|t| t.txn_type == "dividend").expect("应有分红");
        assert!((div.amount - 9.94).abs() < 0.01);
        assert_eq!(div.time, "16:28", "京东分红时间应识别为 16:28，实际 '{}'", div.time);
    }

    #[test]
    fn extract_tencent_licaitong_buy_and_sell() {
        // 模拟 lt1.jpg 前 3 条：买入南方致远 500元、取出华夏芯片 -163.75元、买入景顺长城 200元
        let mut lines = vec![
            line("交易明细", 200, 25),
            line("全部交易", 50, 70),
            line("进阶资产", 200, 70),
            line("所有月份", 380, 70),
        ];
        lines.extend(tencent_txn_block(140, "买入", "南方致远混合E", "+500.00元", "银行卡买入", "2026-07-22 21:16:32"));
        lines.extend(tencent_txn_block(260, "取出", "华夏国证半导体芯片ETF联接C", "-163.75元", r#"取出到"活期+""#, "2026-07-21 14:28:13"));
        lines.extend(tencent_txn_block(380, "买入", "景顺长城能源基建混合A", "+200.00元", "银行卡买入", "2026-07-17 14:15:57"));

        let txns = extract_txn_rows("tencent_licai", &lines);
        assert_eq!(txns.len(), 3, "应识别 3 条腾讯理财通交易，实际：{:?}", txns.iter().map(|t| (&*t.txn_type, t.name.clone(), t.amount)).collect::<Vec<_>>());

        // 买入
        assert_eq!(txns[0].txn_type, "buy");
        assert!(txns[0].name.contains("南方致远"));
        assert!((txns[0].amount - 500.00).abs() < 0.01);
        // 腾讯交易时间（与日期同单元格 "2026-07-22 21:16:32"）
        assert_eq!(txns[0].time, "21:16", "腾讯买入时间应识别为 21:16，实际 '{}'", txns[0].time);

        // 取出 → sell（负数金额应取绝对值）
        let sell = &txns[1];
        assert_eq!(sell.txn_type, "sell", "取出应映射为sell，实际 '{}'", sell.txn_type);
        assert!((sell.amount - 163.75).abs() < 0.01, "卖出金额应为正数绝对值，实际 {}", sell.amount);
        assert!(sell.has_year);
        assert_eq!(sell.time, "14:28", "腾讯取出时间应识别为 14:28，实际 '{}'", sell.time);
    }

    /// 真实腾讯理财通布局：日期与时间是**独立单元格**（"2026-07-22" 与 "21:16" 分两行）。
    /// 时间单元格单独成行时会被 is_chrome 当成状态栏时间过滤掉，必须从「原始未过滤」单元格补取。
    /// 这正是用户反馈「腾讯理财通识别不出交易时间」的根因回归测试。
    #[test]
    fn extract_tencent_time_from_separate_cells() {
        let mut lines = vec![
            line("交易明细", 200, 25),
            line("全部交易", 50, 70),
        ];
        // 买入南方致远 500元：日期单元格(无时间) + 独立时间单元格
        lines.push(line("买入", 30, 140));
        lines.push(line("南方致远混合E", 100, 140));
        lines.push(line("+500.00元", 520, 140));
        lines.push(line("银行卡买入", 100, 164));
        lines.push(line("2026-07-22", 100, 188)); // 日期单元格（不含时间）
        lines.push(line("21:16", 260, 188));       // 独立时间单元格（会被 is_chrome 过滤）

        let txns = extract_txn_rows("tencent_licai", &lines);
        assert_eq!(txns.len(), 1, "应识别 1 条腾讯交易，实际：{:?}", txns.iter().map(|t| (&*t.txn_type, t.name.clone(), t.amount, t.time.clone())).collect::<Vec<_>>());

        let t0 = &txns[0];
        assert_eq!(t0.txn_type, "buy");
        assert!(t0.name.contains("南方致远"));
        assert_eq!(t0.date, "2026-07-22");
        assert!(t0.has_year);
        // 关键：独立时间单元格的 "21:16" 必须被补取到（修复点）
        assert_eq!(t0.time, "21:16", "腾讯独立时间单元格应识别为 21:16，实际 '{}'", t0.time);

        // 独立时间单元格不应被误判为数值/名称污染金额
        assert!((t0.amount - 500.00).abs() < 0.01, "金额应仍为 500.00，实际 {}", t0.amount);
    }

    #[test]
    fn extract_alipay_dividend_transaction() {
        // 模拟 lz2.jpg 中的分红条：分红 广发中证红利ETF联接C 1.26元
        let mut lines = vec![
            line("交易记录", 300, 35),
            line("明细", 40, 65),
            line("基金", 120, 65),
        ];
        lines.extend(alipay_txn_block(130, "分红", "广发中证红利ETF联接C", "1.26元", "2026-08-14 03:48:11"));

        let txns = extract_txn_rows("alipay", &lines);
        assert_eq!(txns.len(), 1);
        assert_eq!(txns[0].txn_type, "dividend");
        assert!(txns[0].name.contains("广发中证红利"));
        assert!((txns[0].amount - 1.26).abs() < 0.01);
        // 分红无份额
        assert_eq!(txns[0].shares, 0.0);
        // 支付宝分红时间（凌晨 03:48，验证时段边界也能识别）
        assert_eq!(txns[0].time, "03:48", "支付宝分红时间应识别为 03:48，实际 '{}'", txns[0].time);
    }

    #[test]
    fn extract_jd_finance_dividend_long_name() {
        // 京东长名称分红：国泰海通君增利60天滚动持有债券型发起式证券投资基C类 12.75元
        let mut lines = vec![
            line("账户明细", 200, 25),
            line("交易", 320, 70),
        ];
        lines.extend(jd_txn_block(
            150,
            "分红-",
            "国泰海通君增利60天滚动持有债券型发起式证券投资基C类",
            "12.75元",
            "现金发放",
            "08-11 15:25:52",
        ));

        let txns = extract_txn_rows("jd_finance", &lines);
        assert_eq!(txns.len(), 1);
        assert_eq!(txns[0].txn_type, "dividend");
        assert!(txns[0].name.contains("国泰海通"), "长名称应完整保留，实际 '{}'", txns[0].name);
        assert!((txns[0].amount - 12.75).abs() < 0.01);
    }

    #[test]
    fn extract_txn_rows_ignores_pure_chrome() {
        let lines = vec![
            line("交易记录", 40, 20),
            line("全部", 200, 35),
            line("净值估算", 300, 35),
        ];
        assert!(extract_txn_rows("alipay", &lines).is_empty());
    }

    #[test]
    fn extract_tencent_grouped_by_date_carries_context() {
        // 腾讯理财通「按日期分组」布局：日期在分组标题块（与交易卡之间纵向间隙 > TXN_GAP），
        // 各交易卡自身不含日期。验证：标题块不产出伪交易；交易卡沿用标题日期；卡自身时间仍生效。
        let mut lines = vec![
            line("交易明细", 300, 40),
            line("2026-08-11", 100, 100), // 分组标题：日期（独立块，无类型→跳过，但日期向前传递）
            // 卡1：买入（自带时间）
            line("买入", 30, 200),
            line("南方致远混合E", 100, 200),
            line("+500.00元", 520, 200),
            line("银行卡买入", 100, 224),
            line("21:16", 260, 224),
            // 卡2：卖出（自带时间，验证不依赖上下文时间）
            line("卖出", 30, 320),
            line("南方致远混合E", 100, 320),
            line("-300.00元", 520, 320),
            line("22:05", 260, 344),
        ];
        let txns = extract_txn_rows("tencent_licai", &lines);
        assert_eq!(
            txns.len(),
            2,
            "应识别 2 条真实交易（标题块不产出伪交易），实际 {:?}",
            txns.iter().map(|t| (&*t.txn_type, t.name.clone(), t.date.clone(), t.time.clone())).collect::<Vec<_>>()
        );
        // 两条都沿用分组标题日期
        assert_eq!(txns[0].date, "2026-08-11");
        assert_eq!(txns[1].date, "2026-08-11");
        assert!(txns[0].has_year);
        // 卡自身时间优先
        assert_eq!(txns[0].time, "21:16");
        assert_eq!(txns[1].time, "22:05");
        assert_eq!(txns[0].txn_type, "buy");
        assert_eq!(txns[1].txn_type, "sell");
    }

    #[test]
    fn extract_first_date_chinese_format() {
        assert_eq!(
            extract_first_date("2026年8月11日"),
            Some(("2026-08-11".to_string(), true))
        );
        assert_eq!(
            extract_first_date("2026年08月11日"),
            Some(("2026-08-11".to_string(), true))
        );
        assert_eq!(
            extract_first_date("8月11日"),
            Some(("2026-08-11".to_string(), false))
        );
        // 金额/时间不应被误判为日期
        assert_eq!(extract_first_date("1,000.00"), None);
        assert_eq!(extract_first_date("21:16"), None);
    }

    // ============ 京东金融专属测试 ============

    /// 京东金融布局辅助：名称在左侧（可折行），中列(x~310)=金额/昨日收益，右列(x~530)=持有收益/收益率
    /// 京东与支付宝的差异：名称更长(含ETF联接/发起式)、行间可能有交易记录和标签
    fn jd_fund_lines(
        idx: i32,
        name1: &str,
        name2: Option<&str>,
        amount: &str,
        profit: &str,
        yest: &str,
        rate: &str,
    ) -> Vec<OcrLine> {
        let base = 140 + idx * 130;
        let cy = base + 20;
        let mut v = Vec::new();
        // 名称列 (x=40)
        v.push(line(name1, 40, cy - if name2.is_some() { 14 } else { 0 }));
        if let Some(n2) = name2 {
            v.push(line(n2, 40, cy + 14)); // 折行第 2 行
        }
        // 中列 (x~310): 金额(上) / 昨日收益(下)
        v.push(line(amount, 310, cy - 14));
        v.push(line(yest, 310, cy + 14));
        // 右列 (x~530): 持有收益(上) / 收益率(下)
        v.push(line(profit, 530, cy - 14));
        v.push(line(rate, 530, cy + 14));
        v
    }

    /// 京东金融「基金持仓」页面 6 只基金完整测试。
    ///
    /// 数据来源：用户 2026-08-15 提供的京东持仓截图（j2.jpg），
    /// OCR 原文经人工校验后的正确值。
    ///
    /// 覆盖场景：
    /// - 名称折行合并（#1 中欧红利优享灵活配置混合C、#4 建信上海金ETF联接C、#6 南方中证A500ETF联接C）
    /// - OCR 字符纠错（#5 大成高鑫→大成夺 的反向纠正）
    /// - 垃圾过滤（LEXBRERE 乱码、交易记录、关注榜标签、底部导航）
    /// - 数值列正确关联（金额/昨日收益/持有收益/收益率 四字段全部验证）
    #[test]
    fn extract_jd_finance_6funds_full() {
        let mut lines = vec![
            // === 顶部 UI chrome（必须全部过滤）===
            line("54", 740, 12),           // 电量/时间徽章
            line("15:28", 60, 35),         // 时间
            line("基金持仓", 260, 38),      // 页面标题
            line("理财师", 700, 42),       // 头像标签
            line("我的持有", 40, 75),
            line("金额排序", 280, 75),
            line("全部(33)", 45, 110),     // 筛选标签带计数
            line("股票型(4)", 175, 110),
            line("债券型(3)", 305, 110),
            line("混合Q", 430, 110),        // 截断的"混合"筛选
            line("基金名称", 45, 145),
            line("金额/昨日收益≈", 270, 145),
            line("持有收益/率", 520, 145),
            line("吕", 320, 48),            // 头像旁噪声
        ];

        // === 基金 1: 中欧红利优享灵活配置混合C ===
        lines.extend(jd_fund_lines(
            0,
            "中欧红利优享灵活",
            Some("配置混合C"),
            "6,468.86",
            "+160.08",
            "-91.44",
            "+2.54%",
        ));
        lines.push(line("大家在关注榜No.7>", 45, 218)); // #1 的关注榜标签（应过滤）

        // === 基金 2: 华富永鑫灵活配置混合C ===
        lines.extend(jd_fund_lines(
            1,
            "华富永鑫灵活配置",
            Some("混合C"),
            "5,013.07",
            "-186.93",
            "-236.81",
            "-3.63%",
        ));
        lines.push(line("交易：1笔买入中合计50.00元", 45, 338)); // 交易记录（应过滤）

        // === 基金 3: 银华中证全指证券公司ETF发起式 ===
        lines.extend(jd_fund_lines(
            2,
            "银华中证全指证券",
            Some("公司ETF发起式·"),
            "4,211.72",
            "-188.28",
            "+16.22",
            "-4.28%",
        ));

        // === 基金 4: 建信上海金ETF联接C ===
        lines.extend(jd_fund_lines(
            3,
            "建信上海金ETF联",
            Some("接C"),
            "3,971.49",
            "-305.86",
            "-21.39",
            "-7.15%",
        ));

        // === 基金 5: 大成高鑫股票A（OCR 可能误为"夺"）===
        lines.extend(jd_fund_lines(
            4,
            "大成夺股票A",          // 模拟 OCR 误识别（应被 correct_ocr_char 纠正为 高鑫）
            None,
            "3,938.14",
            "+38.14",
            "-5.42",
            "+0.98%",
        ));

        // === 基金 6: 南方中证A500ETF联接C（关键测试：长名称折行 + 短后缀合并）===
        lines.extend(jd_fund_lines(
            5,
            "南方中证A500ETF",
            Some("联接C"),           // 短后缀！必须与上一行合并
            "3,912.55",
            "-14.38",
            "-28.67",
            "-0.37%",
        ));

        // === 底部导航栏（必须过滤）===
        lines.push(line("￥", 30, 855));
        lines.push(line("稳健", 155, 870));
        lines.push(line("基金圈", 280, 870));
        lines.push(line("自选", 405, 870));
        lines.push(line("持仓", 530, 870));

        // === 执行抽取 ===
        let funds = extract_fund_rows("jd", &lines);

        // 验证：恰好 6 只基金
        assert_eq!(
            funds.len(),
            6,
            "应识别 6 支京东基金，实际 {} 支: {:?}",
            funds.len(),
            funds.iter().map(|f| f.name.as_str()).collect::<Vec<_>>()
        );

        let get = |name: &str| funds.iter().find(|f| f.name.contains(name)).expect(name);

        // --- 基金 1: 中欧红利优享灵活配置混合C ---
        let f1 = get("中欧红利");
        assert!(f1.name.contains("配置混合C"), "name={}", f1.name);
        assert!((f1.holding_amount - 6468.86).abs() < 0.01, "amount={}", f1.holding_amount);
        assert!((f1.holding_profit - 160.08).abs() < 0.01, "profit={}", f1.holding_profit);
        assert!((f1.yesterday_profit + 91.44).abs() < 0.01, "yest={}", f1.yesterday_profit);
        assert!((f1.profit_rate - 2.54).abs() < 0.01, "rate={}", f1.profit_rate);

        // --- 基金 2: 华富永鑫灵活配置混合C ---
        let f2 = get("华富永鑫");
        assert!(f2.name.contains("混合C"), "name={}", f2.name);
        assert!((f2.holding_amount - 5013.07).abs() < 0.01);
        assert!((f2.holding_profit + 186.93).abs() < 0.01);
        assert!((f2.yesterday_profit + 236.81).abs() < 0.01);
        assert!((f2.profit_rate + 3.63).abs() < 0.01);

        // --- 基金 3: 银华中证全指证券公司ETF发起式 ---
        let f3 = get("银华");
        assert!(f3.name.contains("发起式"), "name={}", f3.name);
        assert!((f3.holding_amount - 4211.72).abs() < 0.01);
        assert!((f3.holding_profit + 188.28).abs() < 0.01);
        assert!((f3.yesterday_profit - 16.22).abs() < 0.01);
        assert!((f3.profit_rate + 4.28).abs() < 0.01);

        // --- 基金 4: 建信上海金ETF联接C ---
        let f4 = get("建信上海金");
        assert!(f4.name.contains("联接C"), "name={} — 接C 未合并!", f4.name);
        assert!((f4.holding_amount - 3971.49).abs() < 0.01);
        assert!((f4.holding_profit + 305.86).abs() < 0.01);
        assert!((f4.yesterday_profit + 21.39).abs() < 0.01);
        assert!((f4.profit_rate + 7.15).abs() < 0.01);

        // --- 基金 5: 大成高鑫股票A（OCR 纠错验证）---
        let f5 = get("大成");
        assert!(f5.name.contains("高鑫"), "name={} — '夺'未被纠正为'高鑫'!", f5.name);
        assert!(!f5.name.contains("夺"), "name={} 仍包含误识别字'夺'", f5.name);
        assert!((f5.holding_amount - 3938.14).abs() < 0.01);
        assert!((f5.holding_profit - 38.14).abs() < 0.01);
        assert!((f5.yesterday_profit + 5.42).abs() < 0.01);
        assert!((f5.profit_rate - 0.98).abs() < 0.01);

        // --- 基金 6: 南方中证A500ETF联接C（关键：短后缀合并验证）---
        let f6 = get("南方中证A500");
        assert!(
            f6.name.contains("联接C"),
            "name={} — '联接C'未合并到主名称! 这是核心bug",
            f6.name
        );
        // 确认不存在独立的「联接C」碎片条目（即未被合并的孤立后缀）
        let orphan_suffix_count = funds.iter().filter(|f| f.name == "联接C").count();
        assert_eq!(orphan_suffix_count, 0, "不应有独立的'联接C'碎片条目");
        assert!((f6.holding_amount - 3912.55).abs() < 0.01, "amount={}", f6.holding_amount);
        assert!((f6.holding_profit + 14.38).abs() < 0.01, "profit={}", f6.holding_profit);
        assert!((f6.yesterday_profit + 28.67).abs() < 0.01, "yest={}", f6.yesterday_profit);
        assert!((f6.profit_rate + 0.37).abs() < 0.01, "rate={}", f6.profit_rate);
    }

    // ============ 腾讯理财通持仓抽取测试（基于 t1/t2/t3 真实截图） ============

    /// 腾讯理财通持仓卡片构造器。
    ///
    /// 布局（从真实截图 t1/t2 提取）：
    ///   y-20: 基金名称（左对齐 x=40）
    ///   y+0:  列头「持有金额」(x~80) 「持仓收益」(x~240) 「昨日收益」(x~400) —— chrome，应被过滤
    ///   y+25: 数值行：金额(无符号, x~80) / 持仓收益(有符号±, x~240) / 昨日收益(有符号±, x~400)
    fn tencent_fund_card(
        idx: i32,
        name: &str,
        amount: &str,
        holding_profit: &str,
        yesterday_profit: &str,
    ) -> Vec<OcrLine> {
        tencent_fund_card_offset(idx, name, amount, holding_profit, yesterday_profit, 100)
    }

    /// 带自定义起始 y 的腾讯理财通卡片构造器（用于 t3 等有汇总块的页面）
    fn tencent_fund_card_offset(
        idx: i32,
        name: &str,
        amount: &str,
        holding_profit: &str,
        yesterday_profit: &str,
        base_y: i32,
    ) -> Vec<OcrLine> {
        let cy = base_y + idx * 130;
        let mut v = Vec::new();
        // 基金名称行
        v.push(line(name, 40, cy));
        // 列头（chrome）
        v.push(line("持有金额", 80, cy + 22));
        v.push(line("持仓收益", 240, cy + 22));
        v.push(line("昨日收益", 400, cy + 22));
        // 数值行
        v.push(line(amount, 80, cy + 48));
        v.push(line(holding_profit, 240, cy + 48));
        v.push(line(yesterday_profit, 400, cy + 48));
        v
    }

    /// 腾讯理财通推广行构造器（出现在部分基金卡片下方）
    fn tencent_promo_line(idx: i32, prefix: &str, body: &str) -> OcrLine {
        let base_y = 100 + idx * 130;
        tencent_promo_line_offset(idx, prefix, body, base_y)
    }

    /// 带自定义 base_y 的推广行构造器
    fn tencent_promo_line_offset(_idx: i32, prefix: &str, body: &str, base_y: i32) -> OcrLine {
        line(format!("{} {} 详情", prefix, body).as_str(), 40, base_y + 78)
    }

    #[test]
    fn extract_tencent_licaitong_holdings_t1() {
        // 模拟 t1.jpg 前 6 只基金（含推广行）
        let mut lines = vec![
            // 页头 chrome
            line("21:32", 60, 15),
            line("腾讯理财通", 200, 40),
            line("资产明细", 40, 70),
            line("筛选", 160, 70),
            line("按持有金额排序", 300, 70),
        ];
        // 基金 1: 易方达标普500指数(QDII-LOF)C(人民币份额)
        lines.extend(tencent_fund_card(0, "易方达标普500指数(QDII-LOF)C(人民币份额)", "984.47", "+165.37", "+6.13"));
        // 基金 2: 鹏华弘利混合C + 推广行
        lines.extend(tencent_fund_card(1, "鹏华弘利混合C", "924.86", "+64.00", "+0.09"));
        lines.push(tencent_promo_line(1, "产品解读", "基金经理来信：限额守初心"));
        // 基金 3-6
        lines.extend(tencent_fund_card(2, "东方人工智能主题混合C", "599.39", "-0.61", "0.00"));
        lines.extend(tencent_fund_card(3, "工银瑞信主题策略混合C", "589.29", "-10.71", "-4.16"));
        lines.extend(tencent_fund_card(4, "国投瑞银白银期货(LOF)A", "202.12", "-97.88", "-3.84"));
        lines.extend(tencent_fund_card(5, "华安策略优选混合A", "109.84", "-0.16", "-0.03"));

        let funds = extract_fund_rows("tencent_licaitong", &lines);
        assert_eq!(funds.len(), 6, "应识别 6 支基金，实际：{:?}",
            funds.iter().map(|f| f.name.clone()).collect::<Vec<_>>());

        // 验证第 1 只（含复杂括号名称）
        let f1 = &funds[0];
        assert!(f1.name.contains("易方达标普500"), "name={}", f1.name);
        assert!(f1.name.contains("QDII"), "应保留 QDII 标识");
        assert!((f1.holding_amount - 984.47).abs() < 0.01, "amount={}", f1.holding_amount);
        assert!((f1.holding_profit - 165.37).abs() < 0.01, "profit={}", f1.holding_profit);
        assert!((f1.yesterday_profit - 6.13).abs() < 0.01, "yest={}", f1.yesterday_profit);

        // 验证推广行不产生伪基金
        let promo_count = funds.iter().filter(|f| f.name.contains("产品解读") || f.name.contains("详情")).count();
        assert_eq!(promo_count, 0, "推广行不应产生基金条目");

        // 验证负收益基金
        let f4 = funds.iter().find(|f| f.name.contains("工银瑞信")).expect("应有工银瑞信");
        assert!((f4.holding_profit + 10.71).abs() < 0.01, "负收益 profit={}", f4.holding_profit);
        assert!((f4.yesterday_profit + 4.16).abs() < 0.01, "负昨日 yest={}", f4.yesterday_profit);

        // 验证大额基金（t2 风格的千分位格式也在此验证）
        let f5 = funds.iter().find(|f| f.name.contains("国投瑞银")).expect("应有国投瑞银");
        assert!((f5.holding_amount - 202.12).abs() < 0.01);
    }

    #[test]
    fn extract_tencent_licaitong_holdings_t2_large_amounts() {
        // 模拟 t2.jpg 的 6 只大额基金（含千分位逗号和推广行）
        let mut lines = vec![
            line("资产明细", 40, 70),
            line("筛选", 160, 70),
        ];
        lines.extend(tencent_fund_card(0, "易方达北证50指数C", "5,591.46", "-580.84", "-50.04"));
        lines.extend(tencent_fund_card(1, "宏利消费红利指数C", "5,060.91", "-493.77", "-45.49"));
        lines.extend(tencent_fund_card(2, "华夏国证半导体芯片ETF联接C", "2,819.27", "+112.35", "+22.72"));
        // 推广行（在第 6 只之后，此处先测前 3 只）
        lines.extend(tencent_fund_card(3, "景顺长城能源基建混合A", "2,565.18", "+457.41", "+13.37"));
        lines.extend(tencent_fund_card(4, "东方红穩鑽精选混合C", "2,008.99", "+63.19", "-2.22"));
        lines.extend(tencent_fund_card(5, "南方致远混合E", "1,003.46", "+3.46", "+1.08"));
        lines.push(tencent_promo_line(5, "产品解读", "恭喜！你的致远近1月跑赢沪深300"));

        let funds = extract_fund_rows("tencent_licaitong", &lines);
        assert_eq!(funds.len(), 6, "应识别 6 支，实际：{:?}",
            funds.iter().map(|f| (&f.name, f.holding_amount)).collect::<Vec<_>>());

        // 验证千分位逗号解析正确（parse_number 已支持逗号）
        let f_big = &funds[0]; // 5,591.46
        assert!((f_big.holding_amount - 5591.46).abs() < 0.01, "千分位 amount={}", f_big.holding_amount);
        assert!((f_big.holding_profit + 580.84).abs() < 0.01, "大额亏损 profit={}", f_big.holding_profit);

        // 验证长名称 ETF 联接基金
        let etf = funds.iter().find(|f| f.name.contains("半导体芯片")).expect("应有芯片ETF");
        assert!(etf.name.contains("ETF联接C"), "完整保留 ETF联接C 后缀");
        assert!((etf.holding_amount - 2819.27).abs() < 0.01);
    }

    #[test]
    fn extract_tencent_licaitong_holdings_t3_with_summary_block() {
        // 模拟 t3.jpg：含「稳健资产(元)」汇总块 + Tab 栏 + 底部推荐区 + 2 只基金
        // 注意：汇总块数值必须在 NAME_Y_BAND(120px) 之外，避免泄漏到基金卡片
        let mut lines = vec![
            // 汇总块（页面顶部 y=20~50，远离基金区域）
            line("稳健资产(元)", 40, 20),
            line("2,100.40", 40, 38),
            line("持仓收益", 160, 38),
            line("+21.28", 280, 38),
            line("昨日收益", 400, 38),
            line("+0.57", 520, 38),
            // Tab 栏（y=80，仍在基金区域之外）
            line("交易明细", 200, 80),
            line("定投计划", 360, 80),
            // 区块标题
            line("资产明细", 40, 120),
            line("筛选", 160, 120),
        ];
        // 基金从 y=180 开始（与汇总块 gap > 120 = NAME_Y_BAND）
        // 基金 1: 中欧瑾通C + 推广行
        lines.extend(tencent_fund_card_offset(0, "中欧瑾通C", "1,098.30", "+19.18", "+0.51", 180));
        lines.push(tencent_promo_line_offset(0, "行借解读", "恭喜收盈！关注多元资产配置", 180));
        // 基金 2: 富国天利增长债券A + 推广行
        lines.extend(tencent_fund_card_offset(1, "富国天利增长债券A", "1,002.10", "+2.10", "+0.06", 310));
        lines.push(tencent_promo_line_offset(1, "专属报告", "【专属服务】二季度投资分析速递", 310));
        // 底部推荐区（在所有基金下方）
        lines.push(line("持仓服务", 40, 500));
        lines.push(line("根据你的持仓情况，为你精选以下内容：", 40, 530));
        lines.push(line("长期理财", 40, 560));
        lines.push(line("查看更多", 200, 560));

        let funds = extract_fund_rows("tencent_licaitong", &lines);
        // 仅 2 只真实基金，汇总块/Tab/推广/底部推荐区全部被过滤
        assert_eq!(funds.len(), 2, "应仅识别 2 支基金，实际：{:?}",
            funds.iter().map(|f| f.name.clone()).collect::<Vec<_>>());

        let f1 = funds.iter().find(|f| f.name.contains("中欧瑾通")).expect("应有中欧瑾通C");
        assert!((f1.holding_amount - 1098.30).abs() < 0.01, "amount={} (期望 1098.30)", f1.holding_amount);
        assert!((f1.holding_profit - 19.18).abs() < 0.01, "profit={}", f1.holding_profit);

        let f2 = funds.iter().find(|f| f.name.contains("富国天利")).expect("应有富国天利");
        assert!((f2.holding_amount - 1002.10).abs() < 0.01);
        assert!((f2.holding_profit - 2.10).abs() < 0.01);

        // 「稳健资产」汇总块未被识别为基金
        assert!(funds.iter().all(|f| !f.name.contains("稳健资产")));
        // 底部推荐区未被识别
        assert!(funds.iter().all(|f| !f.name.contains("长期理财") && !f.name.contains("持仓服务")));
    }

    /// 构造一张腾讯理财通真实卡片：名称(y) + 表头行(y+90) + 数值行(y+160)。
    /// 数值行与名称间距 160px（> 旧 NAME_Y_BAND=120，正是真实高清截图「识别 0 只」的根因）。
    /// 表头顺序可任意（模拟真实截图每张卡三列顺序不固定）。
    fn tencent_real_card(
        _idx: i32,
        name: &str,
        headers: [&str; 3],
        vals: [&str; 3],
        base_y: i32,
    ) -> Vec<OcrLine> {
        let mut v = Vec::new();
        v.push(line(name, 40, base_y));
        for (i, h) in headers.iter().enumerate() {
            v.push(line(h, 80 + i as i32 * 180, base_y + 90)); // 表头（chrome，应被过滤）
        }
        for (i, val) in vals.iter().enumerate() {
            v.push(line(val, 80 + i as i32 * 180, base_y + 160)); // 数值（大间距）
        }
        v
    }

    /// 回归：真实腾讯理财通截图样本（用户反馈「识别不出任何基金」）。
    /// 覆盖：① 名称↔数值大间距(160px) ② 粘连 chrome「资产明细筛选了」③ 每张卡三列表头顺序不固定。
    #[test]
    fn extract_tencent_real_sample_no_zero_funds() {
        let mut lines = vec![
            // 顶部 chrome（含粘连行与噪音）
            line("21:32", 300, 20),           // 时间
            line("586乡", 40, 60),             // OCR 噪音（解析为数字但位于顶部，无上方名称→丢弃）
            line("腾讯理财通", 40, 90),        // 标题
            line("资产明细筛选了", 40, 120),   // 粘连 chrome（子串匹配应过滤）
            line("按持有金额排序", 600, 120),  // 排序
        ];
        // 卡片（base_y 间距 220 > 160 数值间距，确保不跨卡片）
        // 1) 持有金额/持仓收益/昨日收益
        lines.extend(tencent_real_card(0, "易方达北证50指数C",
            ["持有金额", "持仓收益", "昨日收益"],
            ["5,591.46", "-580.84", "-50.04"], 300));
        // 2) 顺序同 1，但数值顺序混乱（-45.49 / -493.77 / 5,060.91）
        lines.extend(tencent_real_card(1, "宏利消费红利指数C",
            ["持有金额", "持仓收益", "昨日收益"],
            ["-45.49", "-493.77", "5,060.91"], 520));
        // 3) 表头顺序不同：持仓收益/持有金额/昨日收益
        lines.extend(tencent_real_card(2, "华夏国证半导体芯片ETF联接C",
            ["持仓收益", "持有金额", "昨日收益"],
            ["+22.72", "+112.35", "2,819.27"], 740));
        // 4) 顺序同 1
        lines.extend(tencent_real_card(3, "景顺长城能源基建混合A",
            ["持有金额", "持仓收益", "昨日收益"],
            ["+457.41", "+13.37", "2,565.18"], 960));
        // 5) 表头顺序不同：昨日收益/持有金额/持仓收益
        lines.extend(tencent_real_card(4, "东方红稳健精选混合C",
            ["昨日收益", "持有金额", "持仓收益"],
            ["-2.22", "+63.19", "2,008.99"], 1180));
        // 6) 顺序同 1
        lines.extend(tencent_real_card(5, "南方致远混合E",
            ["持有金额", "持仓收益", "昨日收益"],
            ["+3.46", "+1.08", "1,003.46"], 1400));
        // 底部推广（chrome，应被过滤）
        lines.push(line("详情", 40, 1620));
        lines.push(line("产品解读 恭喜！你的致道远1月跑赢沪深300", 40, 1650));

        let funds = extract_fund_rows("tencent_licaitong", &lines);
        assert_eq!(funds.len(), 6, "应识别 6 支基金，实际：{:?}",
            funds.iter().map(|f| f.name.clone()).collect::<Vec<_>>());

        // 卡 1：金额 5591.46 / 持仓收益 -580.84 / 昨日 -50.04（与列序无关）
        let c1 = funds.iter().find(|f| f.name.contains("易方达北证")).expect("卡1");
        assert!((c1.holding_amount - 5591.46).abs() < 0.01, "amt={}", c1.holding_amount);
        assert!((c1.holding_profit + 580.84).abs() < 0.01, "profit={}", c1.holding_profit);
        assert!((c1.yesterday_profit + 50.04).abs() < 0.01, "yest={}", c1.yesterday_profit);

        // 卡 2：数值乱序，仍应正确（金额=5060.91 无符号；收益 -493.77 大、昨日 -45.49 小）
        let c2 = funds.iter().find(|f| f.name.contains("宏利消费红利")).expect("卡2");
        assert!((c2.holding_amount - 5060.91).abs() < 0.01, "amt={}", c2.holding_amount);
        assert!((c2.holding_profit + 493.77).abs() < 0.01, "profit={}", c2.holding_profit);
        assert!((c2.yesterday_profit + 45.49).abs() < 0.01, "yest={}", c2.yesterday_profit);

        // 卡 3：表头顺序「持仓收益/持有金额/昨日收益」，按符号+数值仍正确
        let c3 = funds.iter().find(|f| f.name.contains("半导体芯片")).expect("卡3");
        assert!((c3.holding_amount - 2819.27).abs() < 0.01, "amt={}", c3.holding_amount);
        assert!((c3.holding_profit - 112.35).abs() < 0.01, "profit={}", c3.holding_profit);
        assert!((c3.yesterday_profit - 22.72).abs() < 0.01, "yest={}", c3.yesterday_profit);

        // 卡 5：表头顺序「昨日收益/持有金额/持仓收益」，仍正确
        let c5 = funds.iter().find(|f| f.name.contains("东方红稳健")).expect("卡5");
        assert!((c5.holding_amount - 2008.99).abs() < 0.01, "amt={}", c5.holding_amount);
        assert!((c5.holding_profit - 63.19).abs() < 0.01, "profit={}", c5.holding_profit);
        assert!((c5.yesterday_profit + 2.22).abs() < 0.01, "yest={}", c5.yesterday_profit);

        // 粘连 chrome「资产明细筛选了」未产生伪基金
        assert!(funds.iter().all(|f| !f.name.contains("资产明细") && !f.name.contains("筛选")));
        // 顶部噪音「586乡」未产生伪基金
        assert!(funds.iter().all(|f| !f.name.contains("586")));
    }

    /// 验证 OCR 字符纠错：夺→高鑫
    #[test]
    fn ocr_correction_dao_to_gaoxin() {
        assert_eq!(correct_ocr_char("大成夺股票A"), Some("大成高鑫股票A".into()));
        assert_eq!(correct_ocr_char("夺股票"), Some("高鑫股票".into()));
        assert_eq!(correct_ocr_char("高新股票A"), Some("高鑫股票A".into()));
        // 无需纠错的输入返回 None
        assert_eq!(correct_ocr_char("大成高鑫股票A"), None);
    }

    /// 验证垃圾过滤：拉丁乱码 LEXBRERE 被过滤，但有效名称后缀不被过滤
    #[test]
    fn garbage_filter_protects_name_suffixes() {
        // 纯拉丁乱码 → 垃圾
        assert!(is_garbage("LEXBRERE"));
        assert!(is_garbage("LEXBRER"));
        // 有效名称后缀 → 不是垃圾
        assert!(!is_garbage("联接C"));
        assert!(!is_garbage("混合C"));
        assert!(!is_garbage("接C"));
        assert!(!is_garbage("ETF联接C"));
        assert!(!is_garbage("股票A"));
        assert!(!is_garbage("发起式"));
    }

    /// 验证京东 chrome 过滤：交易记录、筛选标签、底部导航等
    #[test]
    fn jd_chrome_filtering() {
        // 这些都应被识别为 chrome
        assert!(is_chrome("交易：1笔买入中合计50.00元"));
        assert!(is_chrome("全部(33)"));
        assert!(is_chrome("股票型(4)"));
        assert!(is_chrome("债券型(3)"));
        assert!(is_chrome("混合Q"));
        assert!(is_chrome("大家在关注榜No.7>"));
        assert!(is_chrome("理财师"));
        assert!(is_chrome("基金持仓"));
        assert!(is_chrome("稳健"));
        assert!(is_chrome("￥"));
    }
}
