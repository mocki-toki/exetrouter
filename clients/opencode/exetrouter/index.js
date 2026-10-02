// OpenCode V2 bridge for the custom provider named "exetrouter".
// Match its built-in ChatGPT adapter: this backend rejects output token caps.
export default {
  id: "exetrouter-opencode",
  async setup(context) {
    const registrations = [];
    for (const name of ["context", "compaction"]) {
      registrations.push(await context.session.hook(name, (event) => {
        delete event.options.maxTokens;
      }, {providerID: "exetrouter"}));
    }
    registrations.push(await context.session.hook("retry", (event) => {
      event.decision = {retry: false};
    }, {providerID: "exetrouter"}));
    return async () => {
      await Promise.all(registrations.map((registration) => registration.dispose()));
    };
  },
};
