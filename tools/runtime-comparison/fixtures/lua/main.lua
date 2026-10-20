local decisions = require('./decisions.lua')

local function readiness()
    local observation = host.call('observe', {})
    host.call('release', { id = observation.id })
    return 'Ready'
end

local function workflow()
    host.state.iterations = (host.state.iterations or 0) + 1
    local options = host.options
    local before = host.call('observe', {})
    host.call('asset', { id = options.recognition.asset })
    local template = host.call('recognize', {
        observation = before, kind = 'template', asset = options.recognition.asset, roi = options.recognition.roi,
    })
    local ocr = host.call('recognize', {
        observation = before, kind = 'ocr', roi = options.recognition.roi,
    })
    local decision = decisions.decide(options, template, ocr)
    host.state.decision = decision
    if decision ~= nil then
        local accepted = host.call('submit', {
            observation = before,
            actions = options.actions[decision],
        })
        local receipt = host.call('settle', { id = accepted.id })
        if options.postcondition.wait_ms > 0 then
            host.call('wait', { duration_ms = options.postcondition.wait_ms })
        end
        local after = host.call('observe', {})
        local postcondition = host.call('postcondition', {
            observation = after, checkpoint = before,
            expected = options.postcondition.expected, roi = options.postcondition.roi,
        })
        host.call('log', { message = 'decision=' .. decision .. ';receipt=' .. receipt.status .. ';postcondition=' .. tostring(postcondition.satisfied) })
        host.call('release', { id = after.id })
        host.call('release', { id = accepted.id })
    end
    if template ~= nil then host.call('release', { id = template.id }) end
    if ocr ~= nil then host.call('release', { id = ocr.id }) end
    host.call('release', { id = before.id })
end

return { readiness = readiness, workflow = workflow }
