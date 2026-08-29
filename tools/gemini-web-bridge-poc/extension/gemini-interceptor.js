// MAIN world only. Gemini's WIZ page state is not visible from an isolated
// extension world. These values remain inside this tab and are used only for
// same-origin Gemini requests; they are never sent to the extension background
// worker or the CodexBar native host.
(function installCodexBarGeminiStateRelay() {
  const STATE_MESSAGE = 'CODEXBAR_GEMINI_APPS_POC_STATE';
  const REFRESH_MESSAGE = 'CODEXBAR_GEMINI_APPS_POC_REFRESH_STATE';
  const RETRY_DELAYS_MS = [0, 1000, 3000, 7000, 15000, 30000];

  function postState() {
    const wiz = window.WIZ_global_data || {};
    if (!wiz.SNlM0e) return;
    window.postMessage({
      type: STATE_MESSAGE,
      state: {
        at: wiz.SNlM0e,
        sid: wiz.FdrFJe || null,
        bl: wiz.cfb2h || null
      }
    }, '*');
  }

  RETRY_DELAYS_MS.forEach((delay) => setTimeout(postState, delay));
  setInterval(postState, 10 * 60 * 1000);

  window.addEventListener('message', (event) => {
    if (event.source === window && event.data?.type === REFRESH_MESSAGE) postState();
  });
})();
