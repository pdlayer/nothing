// Runs in a private WebKit script world in the top document.
return await new Promise(resolve => {
  globalThis.cancelNothingPicker?.();
  const events = new AbortController();
  const capture = {capture: true, signal: events.signal};
  const overlay = document.createElement("div");
  overlay.style.cssText = "all:initial!important;position:fixed!important;inset:auto!important;pointer-events:none!important;z-index:2147483647!important;background:rgba(190,76,60,.17)!important;outline:2px solid #dc7464!important;";
  document.documentElement.appendChild(overlay);
  function finish(selector = "") {
    overlay.remove();
    events.abort();
    delete globalThis.cancelNothingPicker;
    resolve(selector);
  }
  globalThis.cancelNothingPicker = () => finish();
  function element(event) {
    const el = document.elementFromPoint(event.clientX, event.clientY);
    return el && el !== document.body && el !== document.documentElement ? el : null;
  }
  function selector(el) {
    const path = [];
    for (; el && el !== document.documentElement; el = el.parentElement) {
      if (el.id && document.querySelectorAll("#" + CSS.escape(el.id)).length === 1) {
        path.unshift("#" + CSS.escape(el.id));
        break;
      }
      const name = CSS.escape(el.localName);
      const classes = Array.from(el.classList).slice(0, 3).map(c => "." + CSS.escape(c)).join("");
      if (classes && document.querySelectorAll(name + classes).length === 1) {
        path.unshift(name + classes);
        break;
      }
      const siblings = Array.from(el.parentElement.children).filter(n => n.localName === el.localName);
      path.unshift(name + (siblings.length > 1 ? ":nth-of-type(" + (siblings.indexOf(el) + 1) + ")" : ""));
    }
    return path.join(" > ");
  }
  window.addEventListener("pointermove", e => {
    const rect = element(e)?.getBoundingClientRect();
    for (const key of ["left","top","width","height"])
      overlay.style.setProperty(key, (rect?.[key] || 0) + "px", "important");
  }, capture);
  function select(el, event) {
    const rule = selector(el);
    const rect = el.getBoundingClientRect();
    const clip = {left: Math.max(0, rect.left)/innerWidth, top: Math.max(0, rect.top)/innerHeight,
      right: Math.min(innerWidth,rect.right)/innerWidth, bottom: Math.min(innerHeight,rect.bottom)/innerHeight};
    finish(JSON.stringify({rule, clip, documentTime:performance.timeOrigin, fromLeft:event.clientX < rect.left+rect.width/2,
      reduced:matchMedia("(prefers-reduced-motion: reduce)").matches}));
  }
  for (const type of ["pointerdown","pointerup","mousedown","mouseup","click"]) {
    window.addEventListener(type, e => {
      if (!e.isTrusted) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (type === "click") {
        const el = element(e);
        if (el) select(el, e);
      }
    }, capture);
  }
  window.addEventListener("keydown", e => {
    if (e.key === "Escape") { e.preventDefault(); e.stopImmediatePropagation(); finish(); }
  }, capture);
  for (const type of ["pagehide","blur"]) window.addEventListener(type, () => finish(), capture);
});
