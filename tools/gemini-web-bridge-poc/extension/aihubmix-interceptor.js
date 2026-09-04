(function installCodexBarAiHubMixRechargeInterceptor() {
  if (globalThis.__codexbarAiHubMixRechargeInterceptorInstalled) return;
  globalThis.__codexbarAiHubMixRechargeInterceptorInstalled = true;

  const WINDOW_MESSAGE = 'CODEXBAR_AIHUBMIX_RECHARGE_SNAPSHOT';

  function isRechargeUrl(raw) {
    try {
      const url = new URL(raw, location.href);
      return url.origin === 'https://console.aihubmix.com'
        && url.pathname === '/call/usr/quota_rec';
    } catch (_) {
      return false;
    }
  }

  function publish(value) {
    const payload = globalThis.CodexBarAiHubMixRechargeParser?.latestFundingFromResponse(value);
    if (!payload) return;
    window.postMessage({ type: WINDOW_MESSAGE, payload }, location.origin);
  }

  const originalFetch = window.fetch;
  if (typeof originalFetch === 'function') {
    window.fetch = async function codexBarAiHubMixFetch(...args) {
      const response = await originalFetch.apply(this, args);
      try {
        const requestUrl = typeof args[0] === 'string' ? args[0] : args[0]?.url;
        if (response.ok && isRechargeUrl(requestUrl || response.url)) {
          response.clone().json().then(publish).catch(() => {});
        }
      } catch (_) {}
      return response;
    };
  }

  const originalOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function codexBarAiHubMixOpen(method, url, ...rest) {
    this.__codexbarAiHubMixRechargeUrl = url;
    return originalOpen.call(this, method, url, ...rest);
  };

  const originalSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.send = function codexBarAiHubMixSend(...args) {
    if (isRechargeUrl(this.__codexbarAiHubMixRechargeUrl || '')) {
      this.addEventListener('load', () => {
        if (this.status < 200 || this.status >= 300) return;
        try {
          const value = this.responseType === 'json'
            ? this.response
            : JSON.parse(this.responseText || 'null');
          publish(value);
        } catch (_) {}
      }, { once: true });
    }
    return originalSend.apply(this, args);
  };
})();
