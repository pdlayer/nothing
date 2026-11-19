(() => {
  const listeners = new Set();
  const clicks = new Set();
  const event = set => ({
    addListener: fn => set.add(fn),
    removeListener: fn => set.delete(fn),
    hasListener: fn => set.has(fn)
  });

  let registrations;
  chrome.runtime.onInstalled = event(listeners);
  globalThis.nbExtensionInstalled = async () => {
    registrations = [];
    try {
      await Promise.all([...listeners].map(listener => listener({reason: "install"})));
      await Promise.all(registrations);
    } finally {
      registrations = undefined;
    }
  };
  globalThis.nbContextMenuClick = (info, tab) =>
    Promise.all([...clicks].map(listener => Promise.resolve().then(() => listener(info, tab))));
  if (!NB_CAN_USE_MENUS) return;
  let nextId = 0;
  chrome.contextMenus = {
    create(properties, callback) {
      const item = {...properties, id: properties.id ?? ++nextId};
      const promise = nbRuntimeCall({createMenu: item});
      registrations?.push(promise);
      promise.then(() => callback?.(), error => {
        if (!callback) { console.error(error); return; }
        chrome.runtime.lastError = {message: error.message};
        try { callback(); } finally { delete chrome.runtime.lastError; }
      });
      return item.id;
    },
    onClicked: event(clicks)
  };
})();
