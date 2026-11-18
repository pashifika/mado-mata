// Installed before evaluating any package module. Scope IDs and the raw bridge
// stay in this attempt's closure; only the host registry authorizes operations.
(() => {
    const raw = host.call;
    const live = Object.create(null);
    const freeze = Object.freeze;
    const ErrorType = Error;
    let root;

    function context({ id, deadline_ms }) {
        const state = { retired: null };
        if (id !== 0) live[id] = state;
        function invoke(op, method, args) {
            if (state.retired !== null) {
                const fault = new ErrorType(state.retired.cause);
                fault.category = state.retired.category;
                throw fault;
            }
            return raw(id, op, method, args);
        }
        const cause = () => state.retired === null ? invoke('status', 'context', {}) : state.retired.cause;
        const view = {
            __proto__: null,
            deadline_ms,
            get done() { return cause() !== null; },
            get cause() { return cause(); },
            child(bounds = {}) { return context(invoke('child', 'context', bounds)); },
            call(method, args) { return invoke('call', method, args); },
        };
        if (id !== 0) {
            view.cancel = cause => {
                if (state.retired === null) invoke('cancel', 'context', { cause });
            };
            view.close = () => {
                if (state.retired !== null) return;
                const closed = invoke('close', 'context', {});
                for (let i = 0; i < closed.length; i++) {
                    const item = closed[i];
                    live[item.id].retired = item;
                    delete live[item.id];
                }
            };
        }
        return freeze(view);
    }

    host.call = (method, args) => {
        const value = raw(0, 'host', method, args);
        if (method !== 'context') return value;
        return root ??= context(value);
    };
})();
