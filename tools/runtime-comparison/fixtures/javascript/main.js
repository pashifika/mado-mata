import { decide } from './decisions.js';

export function readiness() {
    const observation = host.call('observe', {});
    host.call('release', { id: observation.id });
    return 'Ready';
}

export function workflow() {
    host.state.iterations = (host.state.iterations ?? 0) + 1;
    const options = host.options;
    const before = host.call('observe', {});
    host.call('asset', { id: options.recognition.asset });
    const template = host.call('recognize', {
        observation: before, kind: 'template', asset: options.recognition.asset, roi: options.recognition.roi,
    });
    const ocr = host.call('recognize', {
        observation: before, kind: 'ocr', roi: options.recognition.roi,
    });
    const decision = decide(options, template, ocr);
    host.state.decision = decision;
    if (decision !== null) {
        const accepted = host.call('submit', {
            observation: before,
            actions: options.actions[decision],
        });
        const receipt = host.call('settle', { id: accepted.id });
        if (options.postcondition.wait_ms > 0) {
            host.call('wait', { duration_ms: options.postcondition.wait_ms });
        }
        const after = host.call('observe', {});
        const postcondition = host.call('postcondition', {
            observation: after, checkpoint: before,
            expected: options.postcondition.expected, roi: options.postcondition.roi,
        });
        host.call('log', { message: `decision=${decision};receipt=${receipt.status};postcondition=${postcondition.satisfied}` });
        host.call('release', { id: after.id });
        host.call('release', { id: accepted.id });
    }
    for (const recognition of [template, ocr]) {
        if (recognition !== null) host.call('release', { id: recognition.id });
    }
    host.call('release', { id: before.id });
}
