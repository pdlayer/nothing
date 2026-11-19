(() => {
  const messages = NB_MESSAGES_JSON;
  const language = () => navigator.language || "en";
  chrome.i18n = {
    getUILanguage: language,
    async getAcceptLanguages(callback) {
      const languages = [...navigator.languages];
      callback?.(languages);
      return languages;
    },
    getMessage(name, substitutions = []) {
      const key = String(name).toLowerCase();
      if (key === "@@ui_locale") return language().replaceAll("-", "_");
      const entry = Object.hasOwn(messages, key) ? messages[key] : undefined;
      if (!entry || typeof entry.message !== "string") return "";
      const args = Array.isArray(substitutions) ? substitutions : [substitutions];
      const placeholders = Object.fromEntries(Object.entries(entry.placeholders || {})
        .map(([name, value]) => [name.toLowerCase(), value.content]));
      const positional = (_, index) => String(args[Number(index) - 1] ?? "");
      return entry.message.replace(/\$\$|\$([1-9])|\$([a-z0-9_]+)\$/gi, (match, index, name) => {
        if (match === "$$") return "$";
        if (index) return positional(match, index);
        return typeof placeholders[name.toLowerCase()] === "string"
          ? placeholders[name.toLowerCase()].replace(/\$([1-9])/g, positional) : "";
      });
    }
  };
})();
