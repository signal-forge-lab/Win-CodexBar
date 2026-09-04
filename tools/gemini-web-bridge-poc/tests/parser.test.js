const test = require('node:test');
const assert = require('node:assert/strict');
const parser = require('../extension/gemini-parser.js');
const spendParser = require('../extension/aistudio-parser.js');
const aiHubMixParser = require('../extension/aihubmix-parser.js');

test('parses AI Quota Deck style jSf9Qc current and weekly windows', () => {
  const inner = [
    2,
    [
      [1200, 0.25, 1, [[1788000000, 0]]],
      [24000, 0.10, 2, [[1788500000, 0]]]
    ]
  ];
  const response = JSON.stringify([['jSf9Qc', null, JSON.stringify(inner)]]);
  const result = parser.parseJSf9Qc(response);
  assert.equal(result.source, 'jSf9Qc');
  assert.equal(result.plan, 'Pro');
  assert.equal(result.current.used_percent, 25);
  assert.equal(result.weekly.used_percent, 10);
  assert.match(result.current.resets_at, /^2026-/);
});

test('parses Riah style VxUbXb remaining fractions into used percentages', () => {
  const payload = [
    null,
    null,
    [
      [5, [1788000000, 0], null, [0.70]],
      [27, [1788500000, 0], null, [0.90]]
    ],
    2
  ];
  const response = JSON.stringify([['wrb.fr', 'VxUbXb', JSON.stringify(payload)]]);
  const result = parser.parseVxUbXb(response);
  assert.equal(result.source, 'VxUbXb');
  assert.equal(result.plan, 'Pro');
  assert.ok(Math.abs(result.current.used_percent - 30) < 1e-9);
  assert.ok(Math.abs(result.weekly.used_percent - 10) < 1e-9);
});

test('DOM fallback requires both named meters and does not invent reset timestamps', () => {
  const result = parser.parseDomText('Current usage\n42% used\nWeekly limit\n17% used');
  assert.equal(result.source, 'dom');
  assert.equal(result.current.used_percent, 42);
  assert.equal(result.weekly.used_percent, 17);
  assert.equal(result.current.resets_at, null);
  assert.equal(result.weekly.resets_at, null);
});

test('partial or malformed payloads fail closed', () => {
  assert.equal(parser.parseJSf9Qc('not json jSf9Qc'), null);
  assert.equal(parser.parseVxUbXb('[]'), null);
  assert.equal(parser.parseDomText('Current usage 10% used'), null);
});

test('parses sanitized AI Studio spend without inventing quota', () => {
  const result = spendParser.parseSpendText([
    'Current period spend: $12.34 USD',
    'Monthly spend cap',
    '$20.00 USD',
    '/',
    '$50.00 USD',
    'Billing period: Current month',
    'Project name: Demo Project'
  ].join('\n'));
  assert.deepEqual(result, {
    used: 12.34,
    cap_used: 20,
    limit: 50,
    currency: 'USD',
    period: 'Current month',
    resets_at: null,
    scope: 'Project Demo Project',
    source: 'dom'
  });
});

test('parses current AI Studio Monthly spend cap wording', () => {
  const result = spendParser.parseSpendText([
    'Current spend',
    '$12.34 USD',
    'Monthly spend cap',
    '$20.00 USD',
    '/',
    '$50.00 USD'
  ].join('\n'));
  assert.equal(result.used, 12.34);
  assert.equal(result.cap_used, 20);
  assert.equal(result.limit, 50);
  assert.equal(result.currency, 'USD');
});

test('parses current localized AI Studio net spend layout', () => {
  const result = spendParser.parseSpendText([
    'Project',
    'Gemini Project',
    '1 か月の費用の上限試験運用版',
    '費用の上限を設定',
    '合計費用',
    'August 7 - September 3, 2026',
    '料金',
    '¥739.13',
    '-',
    'コスト削減',
    '¥694.39',
    '=',
    '総費用',
    '¥44.75'
  ].join('\n'));
  assert.deepEqual(result, {
    used: 44.75,
    cap_used: null,
    limit: null,
    currency: 'JPY',
    period: 'August 7 - September 3, 2026',
    resets_at: null,
    scope: 'Project Gemini Project',
    source: 'dom'
  });
});

test('parses localized monthly spend cap when configured', () => {
  const result = spendParser.parseSpendText([
    '総費用',
    '¥44.75',
    '1 か月の費用の上限試験運用版',
    '費用の上限を編集',
    '￥1,054',
    '/',
    '￥2,000'
  ].join('\n'));
  assert.equal(result.used, 44.75);
  assert.equal(result.cap_used, 1054);
  assert.equal(result.limit, 2000);
  assert.equal(result.currency, 'JPY');
});

test('waits for configured AI Studio cap amount instead of caching a partial page', () => {
  assert.equal(spendParser.parseSpendText([
    '\u7dcf\u8cbb\u7528',
    '\u00a544.75',
    '1 \u304b\u6708\u306e\u8cbb\u7528\u306e\u4e0a\u9650\u8a66\u9a13\u904b\u7528\u7248',
    '\u8cbb\u7528\u306e\u4e0a\u9650\u3092\u8a2d\u5b9a\u307e\u305f\u306f\u7de8\u96c6'
  ].join('\n')), null);
});

test('accepts an explicitly unset AI Studio spend cap as cost-only', () => {
  const result = spendParser.parseSpendText([
    '\u7dcf\u8cbb\u7528',
    '\u00a544.75',
    '1 \u304b\u6708\u306e\u8cbb\u7528\u306e\u4e0a\u9650\u8a66\u9a13\u904b\u7528\u7248',
    '\u8cbb\u7528\u306e\u4e0a\u9650\u3092\u8a2d\u5b9a'
  ].join('\n'));
  assert.equal(result.used, 44.75);
  assert.equal(result.cap_used, null);
  assert.equal(result.limit, null);
});

test('parses current spend with an explicitly unconfigured cap', () => {
  const result = spendParser.parseSpendText([
    'Total cost',
    '€44.75',
    'Monthly spend cap',
    '€413.47',
    '/',
    '–'
  ].join('\n'));
  assert.equal(result.used, 44.75);
  assert.equal(result.cap_used, 413.47);
  assert.equal(result.limit, null);
  assert.equal(result.currency, 'EUR');
});

test('parses both yen glyph variants as JPY', () => {
  assert.deepEqual(spendParser.parseMoney('¥44.75'), { amount: 44.75, currency: 'JPY' });
  assert.deepEqual(spendParser.parseMoney('￥706'), { amount: 706, currency: 'JPY' });
});

test('AI Studio spend parser fails closed on partial or conflicting money', () => {
  assert.equal(spendParser.parseSpendText('Spending limit: $50 USD'), null);
  assert.equal(
    spendParser.parseSpendText('Current period spend: $12 USD\nMonthly spend cap\n$20 USD\n/\n€50 EUR'),
    null
  );
  assert.equal(spendParser.parseMoney('$12 EUR'), null);
  assert.equal(spendParser.safePeriod('person@example.com'), null);
  assert.equal(spendParser.safePeriod('Billing account 123456'), null);
});

test('AIHubMix parser picks the newest active positive funding balance', () => {
  const result = aiHubMixParser.latestFundingFromResponse({
    success: true,
    data: [
      { grant_type: 4, status: 1, quota: -500000, balance_after: 3500000, created_time: 40 },
      { grant_type: 2, status: 2, quota: 2000000, balance_after: 5500000, created_time: 30 },
      { grant_type: 1, status: 1, quota: 2000000, balance_after: 5000000, created_time: 20 },
      { grant_type: 3, status: 1, quota: 500000, balance_after: 3000000, created_time: 10 }
    ]
  });
  assert.deepEqual(result, {
    funded_balance_usd: 10,
    funding_created_at: 20,
    source: 'network'
  });
});

test('AIHubMix DOM fallback parses positive quota and balance-after cells', () => {
  const result = aiHubMixParser.latestFundingFromRows([
    ['Alipay', 'Available', '+$2.00', '$10.00', '2026-09-04'],
    ['Deduct', 'Available', '-$1.00', '$9.00', '2026-09-04']
  ]);
  assert.deepEqual(result, {
    funded_balance_usd: 10,
    funding_created_at: null,
    source: 'dom'
  });
  assert.equal(aiHubMixParser.parseDisplayAmount('+$2.00'), 2);
});

test('AIHubMix parser fails closed on malformed or non-funding history', () => {
  assert.equal(aiHubMixParser.latestFundingFromResponse({ success: false, data: [] }), null);
  assert.equal(aiHubMixParser.latestFundingFromResponse({ success: true, data: 'not-an-array' }), null);
  assert.equal(aiHubMixParser.latestFundingFromResponse({
    success: true,
    data: [{ grant_type: 4, status: 1, quota: -500000, balance_after: 1000000, created_time: 1 }]
  }), null);
  assert.equal(aiHubMixParser.latestFundingFromRows([['Deduct', 'Available', '-$1', '$9']]), null);
});
