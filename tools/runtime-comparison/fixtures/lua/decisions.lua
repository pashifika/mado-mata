local helper = require('@mado/helper')

return {
    decide = function(options, template, ocr)
        local available = {
            template = template ~= nil and template.score >= options.recognition.threshold,
            ocr = ocr ~= nil and ocr.score >= options.recognition.threshold,
        }
        return helper.choose(options.priorities, available)
    end,
}
