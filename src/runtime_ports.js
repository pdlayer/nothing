(() => {
  const connectListeners = new Set();
  const incoming = new Map();
  const event = listeners => ({
    addListener: listener => listeners.add(listener),
    removeListener: listener => listeners.delete(listener),
    hasListener: listener => listeners.has(listener)
  });
  const dispatch = (listeners, ...args) => {
    for (const listener of listeners) {
      try { listener(...args); } catch (error) { console.error(error); }
    }
  };
  function port(name, send, close) {
    const messages = new Set();
    const disconnects = new Set();
    let connected = true;
    const api = {
      name,
      onMessage: event(messages),
      onDisconnect: event(disconnects),
      postMessage(message) {
        if (!connected) throw new Error("Port is disconnected");
        send(message);
      },
      disconnect() {
        if (!connected) return;
        connected = false;
        close();
      }
    };
    return {
      api,
      receive: message => { if (connected) dispatch(messages, message, api); },
      closed(error) {
        if (!connected) return;
        connected = false;
        if (error) chrome.runtime.lastError = {message: error.message};
        try { dispatch(disconnects, api); }
        finally { if (error) delete chrome.runtime.lastError; }
      }
    };
  }

  chrome.runtime.onConnect = event(connectListeners);
  chrome.runtime.connect = (options = {}) => {
    if (typeof options !== "object" || options === null)
      throw new Error("Only connections within this extension are supported");
    const key = Array.from(crypto.getRandomValues(new Uint32Array(4))).join("-");
    const name = String(options.name ?? "");
    const call = (op, message) => globalThis.nbRuntimeCall({port: {key, op, name, message}});
    let closed = false;
    let requests = call("open");
    const local = port(name, message => {
      const snapshot = JSON.parse(JSON.stringify(message));
      requests = requests.then(() => call("send", snapshot));
      requests.catch(failed);
    }, () => {
      closed = true;
      requests.then(() => call("close")).catch(() => {});
    });
    function failed(error) {
      if (closed) return;
      closed = true;
      local.closed(error);
      call("close").catch(() => {});
    }
    requests.then(async () => {
      while (!closed) {
        const update = await call("pull");
        if (closed) break;
        for (const message of update.messages) local.receive(message);
        if (update.closed) {
          closed = true;
          local.closed();
        }
      }
    }).catch(failed);
    return local.api;
  };

  function flush(state) {
    if (!state.pending) return;
    const resolve = state.pending;
    state.pending = null;
    clearTimeout(state.wait);
    resolve({messages: state.messages.splice(0), closed: state.closed});
  }

  globalThis.nbPortCommand = async ({key, op, name, message}) => {
    if (op === "open") {
      if (!connectListeners.size) throw new Error("Receiving end does not exist");
      if (incoming.has(key) || incoming.size >= 128) throw new Error("Cannot open extension port");
      const state = {messages: [], closed: false, pending: null};
      function retire() {
        clearTimeout(state.expiry);
        incoming.delete(key);
        state.closed = true;
        flush(state);
      }
      function renew() {
        clearTimeout(state.expiry);
        state.expiry = setTimeout(() => { retire(); state.local.closed(); }, 15000);
      }
      state.renew = renew;
      state.retire = retire;
      state.local = port(name, data => {
        if (state.messages.length >= 128) throw new Error("Extension port queue is full");
        state.messages.push(JSON.parse(JSON.stringify(data)));
        flush(state);
      }, () => { state.closed = true; flush(state); });
      incoming.set(key, state);
      renew();
      dispatch(connectListeners, state.local.api);
      return null;
    }
    const state = incoming.get(key);
    if (!state) {
      if (op === "close") return null;
      throw new Error("Port is disconnected");
    }
    state.renew();
    switch (op) {
      case "close":
        state.retire();
        state.local.closed();
        return null;
      case "send":
        if (state.closed) throw new Error("Port is disconnected");
        state.local.receive(message);
        return null;
      case "pull":
        if (state.pending) throw new Error("A port read is already pending");
        return new Promise(resolve => {
          state.pending = packet => {
            if (packet.closed) state.retire();
            resolve(packet);
          };
          if (state.closed || state.messages.length) flush(state);
          else state.wait = setTimeout(() => flush(state), 3000);
        });
      default: throw new Error("Unknown extension port operation");
    }
  };
})();
