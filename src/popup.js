chrome.runtime.getURL = path => new URL(String(path).replace(/^\/+/, ""), nbResourceBase).href;
const post = message => window.webkit.messageHandlers.extension.postMessage(JSON.stringify(message));
chrome.tabs = {
  async query(filter = {}, callback) {
    const tabs = nbTabs.filter(tab => filter.active === undefined || filter.active === tab.active);
    callback?.(tabs);
    return tabs;
  },
  sendMessage(tab, message, options, callback) {
    if (typeof options === "function") [callback, options] = [options, undefined];
    return callbackResult(request(id => {
      if (options?.documentId !== undefined || (options?.frameId !== undefined && options.frameId !== 0))
        throw new Error("Only the top frame supports tab messages");
      return {tabMessage: {id, tab, message}};
    }), callback);
  }
};

chrome.extension = {getBackgroundPage: () => null};
const pendingCalls = new Map();
let nextCall = 0;
globalThis.nbApiResult = (id, result, error) => {
  const pending = pendingCalls.get(id);
  if (!pending) return;
  pendingCalls.delete(id);
  if (error) pending.reject(new Error(error));
  else pending.resolve(result);
};
function request(message) {
  return new Promise((resolve, reject) => {
    const id = ++nextCall;
    try {
      post(message(id));
      pendingCalls.set(id, {resolve, reject});
    } catch (error) { reject(error); }
  });
}

function callbackResult(promise, callback) {
  if (!callback) return promise;
  promise.then(callback, error => {
    chrome.runtime.lastError = {message: error.message};
    try { callback(); } finally { delete chrome.runtime.lastError; }
  });
}

chrome.scripting = {
  executeScript({target, func, args = []}, callback) {
    return callbackResult(request(id => {
      if (typeof func !== "function")
        throw new Error("Nothing currently supports function injection only");
      return {execute: {id, target, source: String(func), args}};
    }), callback);
  }
};
