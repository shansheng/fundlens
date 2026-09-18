import { describe, expect, test } from 'vitest';
import { localDateKey, localFileStamp, localStamp, todayStr } from './date';

describe('lib/date 本地时区口径', () => {
  // 回归护栏：`toISOString().slice(0,10)` 在 GMT+8 的 00:00–08:00 会跨日偏差一天，
  // 曾导致记账表单默认交易日、导入预览默认日期写入「昨天」。
  test('localDateKey 取本地日历日，含当日首尾时刻', () => {
    expect(localDateKey(new Date(2026, 0, 1, 0, 30))).toBe('2026-01-01');
    expect(localDateKey(new Date(2026, 0, 1, 23, 30))).toBe('2026-01-01');
    expect(localDateKey(new Date(2026, 11, 31, 0, 0, 0))).toBe('2026-12-31');
  });

  test('localDateKey 补零到两位', () => {
    expect(localDateKey(new Date(2026, 8, 7, 12, 0))).toBe('2026-09-07');
  });

  test('todayStr 等于当前的本地日', () => {
    expect(todayStr()).toBe(localDateKey(new Date()));
  });

  test('localStamp 输出后端同格式 YYYY-MM-DD HH:MM:SS', () => {
    expect(localStamp(new Date(2026, 0, 1, 9, 5, 3))).toBe('2026-01-01 09:05:03');
  });

  test('localFileStamp 无冒号，可直接入文件名', () => {
    expect(localFileStamp(new Date(2026, 0, 1, 9, 5, 3))).toBe('2026-01-01-09-05-03');
    expect(localFileStamp(new Date(2026, 0, 1, 9, 5, 3))).not.toMatch(/:/);
  });
});
