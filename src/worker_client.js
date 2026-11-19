(() => {
  const script = NB_WORKER_URL;
  navigator.serviceWorker.addEventListener("message", async event => {
    const reply = event.ports[0];
    if (!reply || event.source?.scriptURL !== script) return;
    try {
      reply.postMessage({value: await globalThis.nbRuntimeCall(event.data)});
    } catch (error) {
      reply.postMessage({error: String(error.message || error)});
    } finally { reply.close(); }
  });
  const ready = navigator.serviceWorker.register(script, {type: NB_WORKER_TYPE, scope: NB_WORKER_SCOPE})
    .then(() => navigator.serviceWorker.ready)
    .then(registration => registration.active);
  ready.catch(error => console.error("Extension worker:", error));
  function invoke(method, ...args) {
    return ready.then(worker => new Promise((resolve, reject) => {
      const {port1, port2} = new MessageChannel();
      const finish = (value, error) => {
        clearTimeout(timer);
        port1.close();
        if (error) reject(new Error(error)); else resolve(value);
      };
      const timer = setTimeout(() => finish(null, "Extension worker timed out"), 15000);
      port1.onmessage = event => finish(event.data.value, event.data.error);
      try { worker.postMessage({method, args}, [port2]); }
      catch (error) { port2.close(); finish(null, String(error)); }
    }));
  }
  for (const method of ["nbReceiveMessage", "nbPortCommand", "nbApply", "nbExtensionInstalled", "nbContextMenuClick"])
    globalThis[method] = (...args) => invoke(method, ...args);
})();
