// 端到端验证：真实 MNN 引擎对持仓截图做 OCR，并抽取基金字段。
// 运行：cargo test --test ocr_e2e（ocr 已默认开启，无需再传 --features ocr）
// 依赖 resources/ocr/{det.mnn,rec.mnn,dict.txt}（由 download_ocr_models.sh 下载）。
// 测试图默认 /tmp/test_fund.png，可用 TEST_IMG 环境变量覆盖。
// 注意：测试图是私有固件，不在仓库内。缺失时本测试自动跳过（不影响 `cargo test` 全绿）。

#[cfg(feature = "ocr")]
use fundlens_lib::ocr::{extract_fund_rows, recognize_image};
#[cfg(feature = "ocr")]
use std::path::Path;

#[cfg(feature = "ocr")]
#[test]
fn e2e_recognize_fund_image() {
    let path = std::env::var("TEST_IMG").unwrap_or_else(|_| "/tmp/test_fund.png".into());

    // 测试图缺失：跳过（CI / 无固件环境不应因此让 cargo test 失败）
    if !Path::new(&path).exists() {
        println!(
            "SKIP e2e_recognize_fund_image: 测试图 {path} 不存在；放置该图或设置 TEST_IMG 后重试。"
        );
        return;
    }
    println!("OCR target image: {path}");

    let lines = recognize_image(&path, None).expect("OCR 引擎应成功识别");
    assert!(!lines.is_empty(), "OCR 未产生任何文本行（检查模型文件与图片）");

    println!("---- 原始 OCR 文本行 ({}) ----", lines.len());
    for l in &lines {
        println!(
            "  [x={:4} y={:4} w={:4} h={:3} score={:.2}] {}",
            l.x, l.y, l.w, l.h, l.score, l.text
        );
    }

    let funds = extract_fund_rows("alipay", &lines);
    println!("---- 抽取基金持仓 ({}) ----", funds.len());
    for f in &funds {
        println!(
            "  code={} name='{}' shares={} nav={} amount={} profit={} yest={} rate={} conf={:.2}",
            f.code,
            f.name,
            f.shares,
            f.nav,
            f.holding_amount,
            f.holding_profit,
            f.yesterday_profit,
            f.profit_rate,
            f.confidence
        );
    }

    // 引擎应成功加载模型并至少抽取到一支基金（按名称验证，避免依赖代码列是否存在）
    assert!(!funds.is_empty(), "OCR 引擎识别到文本但未抽取到基金持仓");
    let names: Vec<&str> = funds.iter().map(|f| f.name.as_str()).collect();
    assert!(
        names.iter().any(|n| n.contains("易方达") || n.contains("华夏")),
        "未从测试图中识别到预期基金名称；实际: {names:?}"
    );
}
