(function installCodexBarAiStudioParser(root, factory) {
  const parser = factory();
  root.CodexBarAiStudioSpendParser = parser;
  if (typeof module !== 'undefined' && module.exports) module.exports = parser;
})(typeof globalThis !== 'undefined' ? globalThis : this, function buildParser() {
  const SYMBOL_TO_CODE = Object.freeze({ '$': 'USD', '€': 'EUR', '£': 'GBP', '¥': 'JPY', '￥': 'JPY' });

  function parseMoney(raw) {
    if (typeof raw !== 'string') return null;
    const text = raw.replace(/\u00a0/g, ' ').trim();
    const match = text.match(/^([$€£¥￥])?\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*(USD|EUR|GBP|JPY)?$/i);
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
    const match = text.match(new RegExp(`(?:${labels.join('|')})\\s*[:\\-]?\\s*([$€£¥￥]?\\s*[0-9][0-9,]*(?:\\.[0-9]+)?\\s*(?:USD|EUR|GBP|JPY)?)`, 'i'));
    return match ? parseMoney(match[1]) : null;
  }

  function parseSpendCap(text, currency) {
    const lines = text.split(/\n+/).map((line) => line.trim()).filter(Boolean);
    const capIndex = lines.findIndex((line) =>
      (line.includes('\u8cbb\u7528') && line.includes('\u4e0a\u9650')) ||
      /monthly spend cap|spending limit|spend limit|monthly limit|budget/i.test(line)
    );
    if (capIndex < 0) return null;

    const panel = lines.slice(capIndex, capIndex + 8);
    const inlinePair = panel.join(' ').match(/([$€£¥￥]\s*[0-9][0-9,]*(?:\.[0-9]+)?\s*(?:USD|EUR|GBP|JPY)?)\s*\/\s*([$€£¥￥]\s*[0-9][0-9,]*(?:\.[0-9]+)?\s*(?:USD|EUR|GBP|JPY)?)/i);
    if (inlinePair) {
      const capUsed = parseMoney(inlinePair[1]);
      const limit = parseMoney(inlinePair[2]);
      if (!capUsed || !limit || capUsed.currency !== currency || limit.currency !== currency) return null;
      return { cap_used: capUsed.amount, limit: limit.amount };
    }

    const slashIndex = panel.findIndex((line) => line === '/' || line.includes('/'));
    if (slashIndex >= 0) {
      let capUsed = null;
      for (let index = slashIndex - 1; index >= 0 && !capUsed; index -= 1) {
        capUsed = parseMoney(panel[index]);
      }
      let limit = null;
      for (let index = slashIndex + 1; index < panel.length && !limit; index += 1) {
        limit = parseMoney(panel[index]);
      }
      if (!capUsed || capUsed.currency !== currency) return null;
      if (limit) {
        if (limit.currency !== currency) return null;
        return { cap_used: capUsed.amount, limit: limit.amount };
      }
      const afterSlash = panel.slice(slashIndex + 1, slashIndex + 4).join(' ');
      if (/^(?:\s*[-–—]\s*|.*(?:not set|no limit|unlimited).*)$/i.test(afterSlash)) {
        return { cap_used: capUsed.amount, limit: null };
      }
      return null;
    }

    const nearby = panel.slice(1, 5).join(' ');
    const japaneseUnset = nearby.includes('費用の上限を設定') && !nearby.includes('編集');
    const englishUnset = /set\s+(?:a\s+)?(?:monthly\s+)?(?:spend\s+)?(?:cap|limit)/i.test(nearby)
      && !/edit/i.test(nearby);
    if (japaneseUnset || englishUnset) {
      return { cap_used: null, limit: null };
    }
    return null;
  }

  function safeScope(text) {
    const match = text.match(/(?:selected\s+project|project\s+name)\s*[:\-]\s*([^\n]{1,64})/i);
    const lines = text.split(/\n+/).map((line) => line.trim()).filter(Boolean);
    const splitIndex = lines.findIndex((line) => /^project$/i.test(line));
    const value = (match?.[1] || (splitIndex >= 0 ? lines[splitIndex + 1] : '') || '').trim();
    if (!value || value.includes('@') || /\b(?:billing|account)\b/i.test(value)) return null;
    if (!/^[\p{L}\p{N} _.-]{1,64}$/u.test(value)) return null;
    return `Project ${value}`;
  }

  function safePeriod(value) {
    if (typeof value !== 'string') return null;
    const period = value.trim();
    if (!period || period.length > 64 || period.includes('@')) return null;
    if (/\b(?:billing\s+account|account\s+id)\b/i.test(period)) return null;
    if (!/^[\p{L}\p{N} _.,/–—-]{1,64}$/u.test(period)) return null;
    return period;
  }

  function parseSpendText(raw) {
    if (typeof raw !== 'string') return null;
    const text = raw.replace(/\u00a0/g, ' ');
    const spend = captureMoney(text, [
      '総費用',
      'total cost',
      'current(?: billing)? period spend',
      'current spend',
      'spend this month',
      "this month(?:'s)? spend",
      'total spend'
    ]);
    if (!spend) return null;
    const cap = parseSpendCap(text, spend.currency);
    if (!cap) return null;
    const periodMatch = text.match(/(?:billing period|period)\s*[:\-]\s*([^\n]{1,64})/i);
    const visibleRange = text.match(/\b([A-Z][a-z]+\s+\d{1,2}\s*[-–—]\s*[A-Z][a-z]+\s+\d{1,2},\s+\d{4})\b/);
    const period = periodMatch
      ? safePeriod(periodMatch[1])
      : visibleRange
        ? safePeriod(visibleRange[1])
        : 'Current period';
    if (!period) return null;
    return {
      used: spend.amount,
      cap_used: cap.cap_used,
      limit: cap.limit,
      currency: spend.currency,
      period,
      resets_at: null,
      scope: safeScope(text),
      source: 'dom'
    };
  }

  return Object.freeze({ parseMoney, parseSpendText, safePeriod });
});
