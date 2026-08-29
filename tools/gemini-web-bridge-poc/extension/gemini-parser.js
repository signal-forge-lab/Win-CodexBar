(function installCodexBarGeminiParser(root, factory) {
  const parser = factory();
  root.CodexBarGeminiAppsPocParser = parser;
  if (typeof module !== 'undefined' && module.exports) module.exports = parser;
})(typeof globalThis !== 'undefined' ? globalThis : this, function buildParser() {
  function clampPercent(value) {
    const number = Number(value);
    if (!Number.isFinite(number)) return null;
    return Math.max(0, Math.min(100, number));
  }

  function epochSecondsToIso(value) {
    const seconds = Number(value);
    if (!Number.isFinite(seconds) || seconds <= 0) return null;
    const date = new Date(seconds * 1000);
    return Number.isNaN(date.getTime()) ? null : date.toISOString().replace(/\.\d{3}Z$/, 'Z');
  }

  function firstJsonArray(text, start) {
    let depth = 0;
    let inString = false;
    let escaped = false;
    for (let index = start; index < text.length; index += 1) {
      const char = text[index];
      if (escaped) {
        escaped = false;
        continue;
      }
      if (inString && char === '\\') {
        escaped = true;
        continue;
      }
      if (char === '"') {
        inString = !inString;
        continue;
      }
      if (inString) continue;
      if (char === '[') depth += 1;
      if (char === ']' && --depth === 0) return text.substring(start, index + 1);
    }
    return null;
  }

  function planFromJsfTier(tier) {
    if (tier === 1) return 'Free';
    if (tier === 2) return 'Pro';
    return null;
  }

  function parseJSf9Qc(text) {
    if (typeof text !== 'string' || !text.includes('jSf9Qc')) return null;
    const start = text.indexOf('[');
    if (start < 0) return null;
    try {
      const block = firstJsonArray(text, start);
      if (!block) return null;
      const outer = JSON.parse(block);
      const inner = JSON.parse(outer?.[0]?.[2]);
      const limits = inner?.[1];
      if (!Array.isArray(limits)) return null;

      let current = null;
      let weekly = null;
      for (const limit of limits) {
        if (!Array.isArray(limit)) continue;
        const usedPercent = clampPercent(Number(limit[1]) * 100);
        if (usedPercent == null) continue;
        const resetsAt = epochSecondsToIso(limit?.[3]?.[0]?.[0]);
        if (limit[2] === 1) {
          current = { label: 'Current usage', used_percent: usedPercent, resets_at: resetsAt };
        } else if (limit[2] === 2) {
          weekly = { label: 'Weekly limit', used_percent: usedPercent, resets_at: resetsAt };
        }
      }
      if (!current || !weekly) return null;
      return { plan: planFromJsfTier(inner?.[0]), source: 'jSf9Qc', current, weekly };
    } catch (_) {
      return null;
    }
  }

  function parseVxPayload(text) {
    if (typeof text !== 'string') return null;
    const match = text.match(/\[\["wrb\.fr","VxUbXb","((?:\\.|[^"\\])*)"/);
    if (!match) return null;
    try {
      return JSON.parse(JSON.parse('"' + match[1] + '"'));
    } catch (_) {
      return null;
    }
  }

  function planFromVxCode(code) {
    if (code === 0) return 'Free';
    if (code === 1) return 'Plus';
    if (code === 2) return 'Pro';
    if (code === 3 || code === 4) return 'Ultra';
    return null;
  }

  function parseVxUbXb(text) {
    const payload = parseVxPayload(text);
    if (!payload || !Array.isArray(payload[2])) return null;
    let current = null;
    let weekly = null;

    for (const bucket of payload[2]) {
      if (!Array.isArray(bucket)) continue;
      let remaining = null;
      for (let index = bucket.length - 1; index >= 0; index -= 1) {
        if (Array.isArray(bucket[index]) && typeof bucket[index][0] === 'number') {
          remaining = bucket[index][0];
          break;
        }
      }
      if (!Number.isFinite(remaining)) continue;
      const usedPercent = clampPercent((1 - remaining) * 100);
      if (usedPercent == null) continue;
      const timestamp = Array.isArray(bucket[1]) ? bucket[1][0] : null;
      const resetsAt = epochSecondsToIso(timestamp);
      if (bucket[0] === 5) {
        current = { label: 'Current usage', used_percent: usedPercent, resets_at: resetsAt };
      } else if (bucket[0] === 27) {
        weekly = { label: 'Weekly limit', used_percent: usedPercent, resets_at: resetsAt };
      }
    }

    if (!current || !weekly) return null;
    return { plan: planFromVxCode(payload[3]), source: 'VxUbXb', current, weekly };
  }

  function parseDomText(text) {
    if (typeof text !== 'string') return null;
    const currentMatch = text.match(/Current usage[\s\S]*?(\d+(?:\.\d+)?)% used/i);
    const weeklyMatch = text.match(/Weekly limit[\s\S]*?(\d+(?:\.\d+)?)% used/i);
    if (!currentMatch || !weeklyMatch) return null;
    const currentPercent = clampPercent(currentMatch[1]);
    const weeklyPercent = clampPercent(weeklyMatch[1]);
    if (currentPercent == null || weeklyPercent == null) return null;
    return {
      plan: null,
      source: 'dom',
      current: { label: 'Current usage', used_percent: currentPercent, resets_at: null },
      weekly: { label: 'Weekly limit', used_percent: weeklyPercent, resets_at: null }
    };
  }

  return Object.freeze({ firstJsonArray, parseJSf9Qc, parseVxUbXb, parseDomText });
});
