import { choose } from '@mado/helper';

export function decide(options, template, ocr) {
    const available = {
        template: template !== null && template.score >= options.recognition.threshold,
        ocr: ocr !== null && ocr.score >= options.recognition.threshold,
    };
    return choose(options.priorities, available);
}
