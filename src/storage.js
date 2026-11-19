(() => {
  const listeners = new Set();
  globalThis.nbApply = (patch) => {
    const changes = Object.create(null);
    for (const [key, newValue] of Object.entries(patch)) {
      if (!Object.hasOwn(nbState, key) || JSON.stringify(nbState[key]) !== JSON.stringify(newValue))
        changes[key] = {oldValue: nbState[key], newValue};
    }
    nbState = {...nbState, ...patch};
    if (Object.keys(changes).length)
      for (const listener of listeners) listener(changes, "local");
  };
  const messageListeners = new Set();
  globalThis.nbReceiveMessage = (message, sender) => new Promise((resolve, reject) => {
    if (!messageListeners.size) {
      reject(new Error("Receiving end does not exist"));
      return;
    }
    let waiting = false;
    const timer = setTimeout(() => reject(new Error("Extension message timed out")), 10000);
    const respond = value => {
      clearTimeout(timer);
      resolve({value: value ?? null});
    };
    const fail = error => {
      clearTimeout(timer);
      reject(error);
    };
    for (const listener of messageListeners) {
      try {
        const result = listener(message, sender, respond);
        if (result === true) waiting = true;
        else if (result && typeof result.then === "function") {
          waiting = true;
          Promise.resolve(result).then(respond, fail);
        }
      } catch (error) {
        fail(error);
      }
    }
    if (!waiting) respond(null);
  });
  globalThis.chrome = {
    storage: {
      local: {
        async get(keys, callback) {
          let result = {};
          if (keys == null) result = {...nbState};
          else if (typeof keys === "object" && !Array.isArray(keys)) {
            result = Object.fromEntries(Object.entries(keys).map(([k,v]) =>
              [k, Object.hasOwn(nbState,k) ? nbState[k] : v]));
          } else {
            result = Object.fromEntries([].concat(keys).filter(k => Object.hasOwn(nbState,k)).map(k => [k,nbState[k]]));
          }
          callback?.(result);
          return result;
        }
      },
      onChanged: {addListener: fn => listeners.add(fn), removeListener: fn => listeners.delete(fn)}
    },
    runtime: {onMessage: {
      addListener: fn => messageListeners.add(fn),
      removeListener: fn => messageListeners.delete(fn),
      hasListener: fn => messageListeners.has(fn)
    }}
  };
})();
