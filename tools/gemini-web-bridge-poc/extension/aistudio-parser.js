(function installCodexBarAiStudioParser(root, factory) {
  const parser = factory();
  root.CodexBarAiStudioSpendParser = parser;
  if (typeof module !== 'undefined' && module.exports) module.exports = parser;
})(typeof globalThis !== 'undefined' ? globalThis : this, function buildParser() {
  const SYMBOL_TO_CODE = Object.freeze({ '$': 'USD', '€': 'EUR', '£': 'GBP', '¥': 'JPY' });

  function parseMoney(raw) {
    if (typeof raw !== 'string') return null;
    const text = raw.replace(/\u00a0/g, ' ').trim();
    const match = text.match(/^([$€£¥])?\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*(USD|EUR|GBP|JPY)?$/i);
    if (!match) return null;
    const amount = Number(match[2].replace(/,/g, ''));
    if (!Number.isFinite(amount) || amount < 0) return null;
    const explicit = match[3]?.toUpperCase() || null;
    const inferred = match[1] ? SYMBOL_TO_CODE[match[1]] : null;
    const currency = explicit || inferred;
    if (!currency || (explicit && inferred && explicit !== inferred)) return null;
    return { amount, currency };
  }

  function captureMoney(text, labels) {
    const match = text.match(new RegExp(`(?:${labels.join('|')})\\s*[:\\-]?\\s*([$€£¥]?\\s*[0-9][0-9,]*(?:\\.[0-9]+)?\\s*(?:USD|EUR|GBP|JPY)?)`, 'i'));
    return match ? parseMoney(match[1]) : null;
  }

  function safeScope(text) {
    const match = text.match(/(?:selected\s+project|project\s+name)\s*[:\-]\s*([^\n]{1,64})/i);
    if (!match) return null;
    const value = match[1].trim();
    if (!value || value.includes('@') || /\b(?:billing|account)\b/i.test(value)) return null;
    if (!/^[\p{L}\p{N} _.-]{1,64}$/u.test(value)) return null;
    return `Project ${value}`;
  }

  function parseSpendText(raw) {
    if (typeof raw !== 'string') return null;
    const text = raw.replace(/\u00a0/g, ' ');
    const spend = captureMoney(text, [
      'current(?: billing)? period spend',
      'current spend',
      'spend this month',
      "this month(?:'s)? spend",
      'total spend'
    ]);
    if (!spend) return null;
    const limit = captureMoney(text, ['spending limit', 'spend limit', 'monthly limit', 'budget']);
    if (limit && limit.currency !== spend.currency) return null;
    const periodMatch = text.match(/(?:billing period|period)\s*[:\-]\s*([^\n]{1,64})/i);
    const period = periodMatch?.[1]?.trim() || 'Current period';
    if (!period || period.length > 64) return null;
    return {
      used: spend.amount,
      limit: limit?.amount ?? null,
      currency: spend.currency,
      period,
      resets_at: null,
      scope: safeScope(text),
      source: 'dom'
    };
  }

  return Object.freeze({ parseMoney, parseSpendText });
});
