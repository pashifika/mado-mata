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
    host.call('asset', { id = 'marker' })
    local template = host.call('recognize', {
        observation = before, kind = 'template', asset = 'marker', roi = options.recognition.roi,
    })
    local ocr = host.call('recognize', {
        observation = before, kind = 'ocr', roi = options.recognition.roi,
    })
    local decision = decisions.decide(options, template, ocr)
    host.state.decision = decision
    if decision ~= nil then
        local key = options.actions[decision]
        local accepted = host.call('submit', {
            observation = before,
            actions = {{ kind = 'key_down', key = key }, { kind = 'key_up', key = key }},
        })
        local receipt = host.call('settle', { id = accepted.id })
        local after = host.call('observe', {})
        local postcondition = host.call('postcondition', {
            observation = after, checkpoint = before, expected = options.postcondition,
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
