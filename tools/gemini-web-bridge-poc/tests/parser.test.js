const test = require('node:test');
const assert = require('node:assert/strict');
const parser = require('../extension/gemini-parser.js');

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
