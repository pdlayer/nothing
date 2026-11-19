(() => {
  const call = globalThis.nbNativeCall ?? (packet => window.webkit.messageHandlers[NB_RUNTIME_HANDLER]
    .postMessage(packet));
  globalThis.nbRuntimeCall = call;
  function complete(promise, callback) {
    if (!callback) return promise;
    promise.then(callback, error => {
      chrome.runtime.lastError = {message: error.message};
      try { callback(); } finally { delete chrome.runtime.lastError; }
    });
  }
  chrome.runtime.getURL = path => new URL(String(path).replace(/^\/+/, ""), (globalThis.nbWorkerResources ?? NB_RUNTIME_BASE)).href;
  chrome.runtime.sendMessage = (message, callback) =>
    complete(call({runtimeMessage: message}), callback);
  if (globalThis.nbBackgroundHost) {
    chrome.tabs = {create: (options, callback) => complete(call({createTab: options}), callback)};
    chrome.offscreen = {
      createDocument: (options, callback) => complete(call({offscreen: "create", options}), callback),
      hasDocument: (callback) => complete(call({offscreen: "has"}), callback),
      closeDocument: (callback) => complete(call({offscreen: "close"}), callback)
    };
  }
  chrome.storage.local.set = (patch, callback) =>
    complete(call({set: patch}).then(() => { nbApply(patch); }), callback);
})();
