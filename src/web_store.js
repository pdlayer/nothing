(() => {
  let button;
  let replaced;
  let installed = new Set();
  let listing;
  const labels = document.documentElement.lang.startsWith("ru")
    ? ["Добавить в Nothing", "Добавлено в Nothing"]
    : ["Add to Nothing", "Added to Nothing"];

  function render() {
    const id = location.pathname.match(/^\/detail\/(?:[^/]+\/)?([a-p]{32})\/?$/)?.[1];
    if (listing !== id || !button?.isConnected) {
      button?.remove();
      if (replaced) replaced.style.removeProperty("display");
      button = replaced = null;
      listing = id;
      const heading = document.querySelector("h1");
      if (!id || !heading) return;
      replaced = [...document.querySelectorAll("button:disabled")].find(candidate =>
        candidate.getClientRects().length &&
        /^(add to chrome|install|установить|добавить в chrome)$/i.test(candidate.textContent.trim()));
      button = document.createElement("button");
      button.type = "button";
      button.style.cssText = "border:0;border-radius:20px;padding:10px 20px;background:#234f91;color:white;font:500 14px system-ui;white-space:nowrap;vertical-align:middle;cursor:pointer;margin-inline-start:12px";
      button.addEventListener("click", event => {
        if (!event.isTrusted) return;
        event.stopPropagation();
        window.webkit.messageHandlers.nothingStore.postMessage(null);
      });
      button.textContent = labels[0];
      if (replaced) {
        replaced.before(button);
        replaced.style.display = "none";
      }
      if (!replaced || button.getBoundingClientRect().right > innerWidth) {
        heading.append(button);
      }
    }
    if (!button) return;
    const done = installed.has(id);
    if (button.textContent !== labels[Number(done)]) button.textContent = labels[Number(done)];
    button.disabled = done;
  }
  globalThis.nbStoreInstalled = ids => { installed = new Set(ids); render(); };
  new MutationObserver(render).observe(document.documentElement, { childList: true, subtree: true });
  render();
})();
