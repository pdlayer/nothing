globalThis.nbBackgroundHost = true;
globalThis.nbWorkerResources = NB_WORKER_RESOURCES;
globalThis.nbNativeCall = async message => {
  const clients = await self.clients.matchAll({type: "window", includeUncontrolled: true});
  const host = clients.find(client => client.url === NB_WORKER_CLIENT);
  if (!host) throw new Error("Extension worker host closed");
  return new Promise((resolve, reject) => {
    const {port1, port2} = new MessageChannel();
    const timer = setTimeout(() => {
      port1.close();
      reject(new Error("Extension API request timed out"));
    }, 15000);
    port1.onmessage = event => {
      clearTimeout(timer);
      port1.close();
      if (event.data.error) reject(new Error(event.data.error));
      else resolve(event.data.value);
    };
    host.postMessage(message, [port2]);
  });
};
self.addEventListener("message", event => {
  const reply = event.ports[0];
  if (!reply || event.source?.url !== NB_WORKER_CLIENT) return;
  event.waitUntil((async () => {
    try {
      const {method, args} = event.data;
      if (!["nbReceiveMessage", "nbPortCommand", "nbApply", "nbExtensionInstalled", "nbContextMenuClick"].includes(method))
        throw new Error("Unknown worker event");
      reply.postMessage({value: await globalThis[method](...args)});
    } catch (error) {
      reply.postMessage({error: String(error.message || error)});
    } finally { reply.close(); }
  })());
});
