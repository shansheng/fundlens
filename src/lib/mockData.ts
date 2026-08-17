// FundLens 演示数据（浏览器环境下替代后端，保证 UI 可独立预览/评审）
// 真实数据来源见 SPEC.md 第 4 节：天天基金/东方财富 F10（披露持仓）+ 新浪/腾讯 push2（实时行情）

export interface PlatformMeta {
  code: string;
  name: string;
  /** 用于在 Lucide 图标旁展示的主题色（仅装饰，非涨跌色） */
  accent: string;
}

export const PLATFORMS: Record<string, PlatformMeta> = {
  alipay: { code: 'alipay', name: '支付宝', accent: '#1677FF' },
  jd_finance: { code: 'jd_finance', name: '京东金融', accent: '#E1251B' },
  tencent_licai: { code: 'tencent_licai', name: '腾讯理财通', accent: '#07C160' },
};

export interface MockStockQuote {
  stockCode: string;
  stockName: string;
  price: number;
  prevClose: number;
}

export interface MockFund {
  code: string;
  name: string;
  platform: string;
  shares: number; // 持有份额
  costAmount: number; // 累计成本
  officialNav: number; // 上一交易日官方单位净值
  reportPeriod: string; // '2026Q2'
  disclosureType: 'top10' | 'full';
  fundType: string; // 基金类型码：006=指数型 007=混合型 008=股票指数 ...
  fundTypeLabel: string;
  holdings: { stockCode: string; stockName: string; weight: number }[];
  quotes: MockStockQuote[];
  /** 跟踪指数行情（指数/ETF 类）。提供时估值按该指数涨跌近似未披露部分 */
  trackedIndex?: { indexCode: string; indexName: string; price: number; prevClose: number };
}

export const MOCK_FUNDS: MockFund[] = [
  {
    code: '005827',
    name: '易方达蓝筹精选混合',
    platform: 'alipay',
    shares: 12345.67,
    costAmount: 22000,
    officialNav: 2.5123,
    reportPeriod: '2026Q2',
    disclosureType: 'full',
    fundType: '007',
    fundTypeLabel: '混合型',
    holdings: [
      { stockCode: '600519', stockName: '贵州茅台', weight: 0.0945 },
      { stockCode: '000858', stockName: '五粮液', weight: 0.0821 },
      { stockCode: '000568', stockName: '泸州老窖', weight: 0.0612 },
      { stockCode: '600036', stockName: '招商银行', weight: 0.0734 },
      { stockCode: '601318', stockName: '中国平安', weight: 0.0589 },
      { stockCode: '000333', stockName: '美的集团', weight: 0.0517 },
      { stockCode: '002594', stockName: '比亚迪', weight: 0.0488 },
      { stockCode: '600276', stockName: '恒瑞医药', weight: 0.0392 },
      { stockCode: '601012', stockName: '隆基绿能', weight: 0.0331 },
      { stockCode: '300750', stockName: '宁德时代', weight: 0.0715 },
    ],
    quotes: [
      { stockCode: '600519', stockName: '贵州茅台', price: 1685.0, prevClose: 1650.0 },
      { stockCode: '000858', stockName: '五粮液', price: 142.3, prevClose: 145.1 },
      { stockCode: '000568', stockName: '泸州老窖', price: 168.5, prevClose: 170.2 },
      { stockCode: '600036', stockName: '招商银行', price: 38.9, prevClose: 38.2 },
      { stockCode: '601318', stockName: '中国平安', price: 52.4, prevClose: 53.0 },
      { stockCode: '000333', stockName: '美的集团', price: 71.2, prevClose: 70.5 },
      { stockCode: '002594', stockName: '比亚迪', price: 256.8, prevClose: 248.0 },
      { stockCode: '600276', stockName: '恒瑞医药', price: 45.1, prevClose: 46.0 },
      { stockCode: '601012', stockName: '隆基绿能', price: 18.7, prevClose: 19.4 },
      { stockCode: '300750', stockName: '宁德时代', price: 198.3, prevClose: 190.5 },
    ],
  },
  {
    code: '003095',
    name: '中欧医疗健康混合C',
    platform: 'jd_finance',
    shares: 8890.12,
    costAmount: 15000,
    officialNav: 1.9234,
    reportPeriod: '2026Q2',
    disclosureType: 'top10',
    fundType: '007',
    fundTypeLabel: '混合型',
    holdings: [
      { stockCode: '300760', stockName: '迈瑞医疗', weight: 0.0931 },
      { stockCode: '600276', stockName: '恒瑞医药', weight: 0.0812 },
      { stockCode: '002821', stockName: '凯莱英', weight: 0.0523 },
      { stockCode: '300347', stockName: '泰格医药', weight: 0.0478 },
      { stockCode: '000661', stockName: '长春高新', weight: 0.0419 },
      { stockCode: '688180', stockName: '君实生物', weight: 0.0385 },
      { stockCode: '603259', stockName: '药明康德', weight: 0.0567 },
      { stockCode: '300015', stockName: '爱尔眼科', weight: 0.0492 },
      { stockCode: '600763', stockName: '通策医疗', weight: 0.0334 },
      { stockCode: '002044', stockName: '美年健康', weight: 0.0271 },
    ],
    quotes: [
      { stockCode: '300760', stockName: '迈瑞医疗', price: 268.4, prevClose: 262.0 },
      { stockCode: '600276', stockName: '恒瑞医药', price: 45.1, prevClose: 46.0 },
      { stockCode: '002821', stockName: '凯莱英', price: 92.3, prevClose: 95.1 },
      { stockCode: '300347', stockName: '泰格医药', price: 58.6, prevClose: 57.2 },
      { stockCode: '000661', stockName: '长春高新', price: 118.9, prevClose: 121.4 },
      { stockCode: '688180', stockName: '君实生物', price: 33.2, prevClose: 32.8 },
      { stockCode: '603259', stockName: '药明康德', price: 71.5, prevClose: 69.0 },
      { stockCode: '300015', stockName: '爱尔眼科', price: 13.4, prevClose: 13.9 },
      { stockCode: '600763', stockName: '通策医疗', price: 47.8, prevClose: 48.6 },
      { stockCode: '002044', stockName: '美年健康', price: 5.21, prevClose: 5.18 },
    ],
  },
  {
    code: '161725',
    name: '招商中证白酒指数',
    platform: 'tencent_licai',
    shares: 20000.0,
    costAmount: 18500,
    officialNav: 0.9845,
    reportPeriod: '2026Q2',
    disclosureType: 'top10',
    fundType: '006',
    fundTypeLabel: '指数型',
    trackedIndex: { indexCode: '399997', indexName: '中证白酒', price: 11016, prevClose: 10800 },
    holdings: [
      { stockCode: '600519', stockName: '贵州茅台', weight: 0.1512 },
      { stockCode: '000858', stockName: '五粮液', weight: 0.1421 },
      { stockCode: '000568', stockName: '泸州老窖', weight: 0.1134 },
      { stockCode: '002304', stockName: '洋河股份', weight: 0.0812 },
      { stockCode: '600809', stockName: '山西汾酒', weight: 0.0923 },
      { stockCode: '000596', stockName: '古井贡酒', weight: 0.0431 },
      { stockCode: '603369', stockName: '今世缘', weight: 0.0389 },
      { stockCode: '000799', stockName: '酒鬼酒', weight: 0.0212 },
      { stockCode: '600779', stockName: '水井坊', weight: 0.0198 },
      { stockCode: '000860', stockName: '顺鑫农业', weight: 0.0156 },
    ],
    quotes: [
      { stockCode: '600519', stockName: '贵州茅台', price: 1685.0, prevClose: 1650.0 },
      { stockCode: '000858', stockName: '五粮液', price: 142.3, prevClose: 145.1 },
      { stockCode: '000568', stockName: '泸州老窖', price: 168.5, prevClose: 170.2 },
      { stockCode: '002304', stockName: '洋河股份', price: 88.4, prevClose: 90.1 },
      { stockCode: '600809', stockName: '山西汾酒', price: 198.7, prevClose: 201.3 },
      { stockCode: '000596', stockName: '古井贡酒', price: 172.3, prevClose: 170.0 },
      { stockCode: '603369', stockName: '今世缘', price: 46.2, prevClose: 45.5 },
      { stockCode: '000799', stockName: '酒鬼酒', price: 52.1, prevClose: 53.0 },
      { stockCode: '600779', stockName: '水井坊', price: 48.9, prevClose: 49.6 },
      { stockCode: '000860', stockName: '顺鑫农业', price: 17.3, prevClose: 17.0 },
    ],
  },
  {
    code: '001632',
    name: '天弘中证食品饮料ETF联接',
    platform: 'alipay',
    shares: 15600.0,
    costAmount: 12000,
    officialNav: 1.4567,
    reportPeriod: '2026Q2',
    disclosureType: 'top10',
    fundType: '006',
    fundTypeLabel: '指数型',
    trackedIndex: { indexCode: '399396', indexName: '中证食品饮料', price: 18180, prevClose: 18000 },
    holdings: [
      { stockCode: '600519', stockName: '贵州茅台', weight: 0.1212 },
      { stockCode: '000858', stockName: '五粮液', weight: 0.1023 },
      { stockCode: '000568', stockName: '泸州老窖', weight: 0.0811 },
      { stockCode: '600887', stockName: '伊利股份', weight: 0.0724 },
      { stockCode: '603288', stockName: '海天味业', weight: 0.0635 },
      { stockCode: '002304', stockName: '洋河股份', weight: 0.0521 },
      { stockCode: '600809', stockName: '山西汾酒', weight: 0.0489 },
      { stockCode: '000895', stockName: '双汇发展', weight: 0.0332 },
      { stockCode: '600298', stockName: '安琪酵母', weight: 0.0214 },
      { stockCode: '002507', stockName: '涪陵榨菜', weight: 0.0145 },
    ],
    quotes: [
      { stockCode: '600519', stockName: '贵州茅台', price: 1685.0, prevClose: 1650.0 },
      { stockCode: '000858', stockName: '五粮液', price: 142.3, prevClose: 145.1 },
      { stockCode: '000568', stockName: '泸州老窖', price: 168.5, prevClose: 170.2 },
      { stockCode: '600887', stockName: '伊利股份', price: 27.8, prevClose: 27.2 },
      { stockCode: '603288', stockName: '海天味业', price: 41.2, prevClose: 42.0 },
      { stockCode: '002304', stockName: '洋河股份', price: 88.4, prevClose: 90.1 },
      { stockCode: '600809', stockName: '山西汾酒', price: 198.7, prevClose: 201.3 },
      { stockCode: '000895', stockName: '双汇发展', price: 25.1, prevClose: 24.8 },
      { stockCode: '600298', stockName: '安琪酵母', price: 33.4, prevClose: 33.0 },
      { stockCode: '002507', stockName: '涪陵榨菜', price: 14.2, prevClose: 14.0 },
    ],
  },
];

/** 当前是否处于交易时段（9:30-11:30, 13:00-15:00，周一至周五） */
export function isTradingNow(date = new Date()): boolean {
  const day = date.getDay();
  if (day === 0 || day === 6) return false;
  const h = date.getHours();
  const m = date.getMinutes();
  const t = h * 60 + m;
  const morning = t >= 9 * 60 + 30 && t <= 11 * 60 + 30;
  const afternoon = t >= 13 * 60 && t <= 15 * 60;
  return morning || afternoon;
}

// —— 盘中演示：浏览器预览的行情随时间小幅摆动，模拟实时跳动 ——
// 真实路径（Tauri）由后端 fetch_quotes 拉取腾讯实时行情，无需此函数。

function seedFromCode(code: string): number {
  let h = 0;
  for (let i = 0; i < code.length; i += 1) h = (h * 31 + code.charCodeAt(i)) % 997;
  return h;
}

/** 纯函数：给定秒级时间 t 与种子，返回围绕 0 的确定性小幅摆动（约 ±1%，多频叠加）。 */
export function mockPriceOscillation(tSeconds: number, seed: number): number {
  return (
    Math.sin(tSeconds / 27 + seed) * 0.006 +
    Math.sin(tSeconds / 11 + seed * 1.9) * 0.004
  );
}

/**
 * 浏览器预览用：在「基准涨跌幅」基础上叠加随时间变化的实时摆动，得到盘中现价。
 * basePrice/prevClose 为演示基准（昨收附近），price 会随当前时间小幅浮动，
 * 使估值在刷新时跳动，从而演示「盘中实时估值」的核心体验。
 */
export function liveMockPrice(basePrice: number, prevClose: number, code: string): number {
  if (!(prevClose > 0) || !(basePrice > 0)) return basePrice;
  const baseRet = basePrice / prevClose - 1;
  const osc = mockPriceOscillation(Date.now() / 1000, seedFromCode(code));
  return +(prevClose * (1 + baseRet + osc)).toFixed(3);
}
