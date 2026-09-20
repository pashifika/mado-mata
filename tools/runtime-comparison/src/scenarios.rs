//! Trusted controlled-lane fixture selection. Never an input-authority override.
//! Sources are added to a captured inventory and receive a new content identity.

use crate::inventory::{Entry, Inventory};
use crate::model::Fault;
use serde_json::json;

pub fn select(inventory: &mut Inventory, scenario: &str) -> Result<(), Fault> {
    let language = inventory.metadata["runtime"].as_str().unwrap_or("");
    let (extension, source) = match language {
        "javascript" => ("js", javascript(scenario)?),
        "lua" => ("lua", lua(scenario)?),
        _ => {
            return Err(Fault::new(
                "Scenario",
                "Runtime scenarios require JavaScript or Lua inventory",
            ));
        }
    };
    let module = format!("scenario.{extension}");
    inventory.sources.insert(module.clone(), source);
    inventory.entries.workflow = Entry {
        module: module.clone(),
        function: "workflow".into(),
    };
    if scenario == "invalid-readiness"
        || scenario == "top-level-loop"
        || scenario == "top-level-input"
    {
        inventory.entries.readiness = Entry {
            module: module.clone(),
            function: "readiness".into(),
        };
    }
    if scenario == "module-cache" {
        inventory.sources.insert(
            format!("counter.{extension}"),
            if extension == "js" {
                "export const counter = { value: 0 };\n".into()
            } else {
                "return { value = 0 }\n".into()
            },
        );
    }
    if scenario == "module-cycle" {
        let (a, b) = if extension == "js" {
            (
                "import { read } from './cycle-b.js'; export let value = 7; export function answer() { return read(); }\n",
                "import { value } from './cycle-a.js'; export function read() { return value; }\n",
            )
        } else {
            (
                "local b = require('./cycle-b.lua'); return { answer = function() return b.value end }\n",
                "local a = require('./cycle-a.lua'); return { value = a.answer() }\n",
            )
        };
        inventory
            .sources
            .insert(format!("cycle-a.{extension}"), a.into());
        inventory
            .sources
            .insert(format!("cycle-b.{extension}"), b.into());
    }
    inventory.metadata["runtime_scenario"] = json!(scenario);
    inventory.refresh_identity()
}

fn javascript(scenario: &str) -> Result<String, Fault> {
    let source = match scenario {
        "readonly" => {
            r#"
import { workflow as normal } from './main.js';
export function workflow() {
    const snapshot = JSON.stringify(host.options);
    const options = host.options;
    const aliases = [options.recognition, options.recognition.roi, options.priorities];
    for (const mutate of [
        () => { aliases[0].threshold = 0; },
        () => { delete aliases[1].x; },
        () => { aliases[2].push('ocr'); },
        () => { aliases[2].reverse(); },
        () => { aliases[2][0] = 'bad'; },
        () => { host.options = {}; },
        () => { Object.setPrototypeOf(aliases[1], { x: 100 }); },
    ]) { try { mutate(); } catch (_) {} }
    if (JSON.stringify(host.options) !== snapshot) throw new Error('Options mutated');
    host.state.counter = 42;
    if (host.state.counter !== 42) throw new Error('Attempt state is not mutable');
    normal();
}
"#
        }
        "module-cache" => {
            r#"
export async function workflow() {
    const first = await import('./counter.js');
    if (first.counter.value !== 0) throw new Error('Module state survived an attempt');
    first.counter.value = 13;
    const second = await import('././counter.js');
    if (second !== first || second.counter.value !== 13) throw new Error('Normalized module cache split');
    host.call('log', { message: 'module-cache=shared;initial=0;value=13' });
}
"#
        }
        "module-cycle" => {
            r#"
import { answer } from './cycle-a.js';
export function workflow() {
    if (answer() !== 7) throw new Error('ES module live binding cycle failed');
    host.call('log', { message: 'cycle-answer=7' });
}
"#
        }
        "tight-loop" => "export function workflow() { for (;;) {} }",
        "catch-loop" => {
            "export function workflow() { for (;;) { try { try { for (;;) {} } catch (_) {} } catch (_) {} } }"
        }
        "promise-loop" => "export async function workflow() { for (;;) await Promise.resolve(); }",
        "microtask-loop" => {
            "export function workflow() { return new Promise(() => { function again() { queueMicrotask(again); } again(); }); }"
        }
        "unawaited" => {
            "export function workflow() { queueMicrotask(() => host.call('observe', {})); }"
        }
        "computed-import-refusal" => {
            r#"
import { workflow as normal } from './main.js';
export async function workflow() {
    normal();
    const forbidden = ['unapproved', 'module.js'].join('/');
    try { await import(forbidden); } catch (_) {}
    host.call('observe', {});
}
"#
        }
        "noncallable-entry" => "export const workflow = 42;",
        "invalid-readiness" => {
            "export function readiness() { return true; } export function workflow() { host.call('observe', {}); }"
        }
        "top-level-input" => {
            "const frame = host.call('observe', {}); host.call('submit', { observation: frame, actions: [{kind:'key_down',key:'A'}] }); export function readiness() { return 'Ready'; } export function workflow() {}"
        }
        "top-level-loop" => {
            "for (;;) {} export function readiness() { return 'Ready'; } export function workflow() {}"
        }
        "helper-error" => {
            "import { fail } from '@mado/helper'; export function workflow() { fail(); }"
        }
        "memory-limit" => {
            "export function workflow() { const retained = []; for (;;) retained.push(new Uint8Array(65536)); }"
        }
        "static-import-refusal" => "import './missing.js'; export function workflow() {}",
        "syntax-error" => "export function workflow( {",
        _ => {
            return Err(Fault::new(
                "Scenario",
                format!("Unsupported JavaScript scenario: {scenario}"),
            ));
        }
    };
    Ok(source.into())
}

fn lua(scenario: &str) -> Result<String, Fault> {
    let source = match scenario {
        "readonly" => {
            r#"
local normal = require('./main.lua').workflow
return { workflow = function()
    local options = host.options
    local threshold, x = options.recognition.threshold, options.recognition.roi.x
    local first, second, count = options.priorities[1], options.priorities[2], #options.priorities
    local alias = options.priorities
    for _, mutate in ipairs({
        function() options.recognition.threshold = 0 end,
        function() options.recognition.roi.x = nil end,
        function() alias[1] = 'bad' end,
        function() table.insert(alias, 'ocr') end,
        function() table.remove(alias, 1) end,
        function() table.sort(alias) end,
        function() host.options = {} end,
        function() host = {} end,
    }) do pcall(mutate) end
    assert(rawset == nil, 'rawset exposed')
    local iterator, state = pairs(options.recognition)
    assert(state == nil, 'pairs exposed backing state')
    for _, value in iterator, state do
        if type(value) == 'userdata' then pcall(function() value.x = 12 end) end
    end
    assert(options.recognition.threshold == threshold and options.recognition.roi.x == x, 'nested options mutated')
    assert(alias[1] == first and alias[2] == second and #alias == count, 'priority options mutated')
    host.state.counter = 42
    assert(host.state.counter == 42, 'attempt state is not mutable')
    normal()
end }
"#
        }
        "module-cache" => {
            r#"
return { workflow = function()
    local first = require('./counter.lua')
    assert(first.value == 0, 'module state survived an attempt')
    first.value = 13
    local second = require('././counter.lua')
    assert(first == second and second.value == 13, 'normalized module cache split')
    host.call('log', { message = 'module-cache=shared;initial=0;value=13' })
end }
"#
        }
        "module-cycle" => {
            "local cycle = require('./cycle-a.lua'); return { workflow = function() return cycle.answer() end }"
        }
        "tight-loop" => "return { workflow = function() while true do end end }",
        "catch-loop" => {
            "return { workflow = function() while true do pcall(function() xpcall(function() while true do end end, function() return 'caught' end) end) end end }"
        }
        "coroutine-loop" => {
            "return { workflow = function() local child = coroutine.create(function() while true do pcall(function() while true do end end) end end); coroutine.resume(child) end }"
        }
        "coroutine-wrap-loop" => {
            "return { workflow = function() coroutine.wrap(function() while true do end end)() end }"
        }
        "unawaited" => {
            "return { workflow = function() host.state.detached = coroutine.create(function() host.call('observe', {}) end) end }"
        }
        "computed-import-refusal" => {
            r#"
local normal = require('./main.lua').workflow
return { workflow = function()
    normal()
    local forbidden = table.concat({'unapproved', 'module.lua'}, '/')
    pcall(function() require(forbidden) end)
    host.call('observe', {})
end }
"#
        }
        "noncallable-entry" => "return { workflow = 42 }",
        "invalid-readiness" => {
            "return { readiness = function() return true end, workflow = function() host.call('observe', {}) end }"
        }
        "top-level-input" => {
            "local frame = host.call('observe', {}); host.call('submit', {observation=frame,actions={{kind='key_down',key='A'}}}); return {readiness=function() return 'Ready' end,workflow=function() end}"
        }
        "top-level-loop" => {
            "while true do end return {readiness=function() return 'Ready' end,workflow=function() end}"
        }
        "helper-error" => {
            "local helper = require('@mado/helper'); return { workflow = function() helper.fail() end }"
        }
        "memory-limit" => {
            "return { workflow = function() local retained = {}; local index = 0; while true do index = index + 1; retained[index] = string.rep('x', 65536) .. index end end }"
        }
        "static-import-refusal" => {
            "local missing = require('./missing.lua'); return { workflow = function() return missing end }"
        }
        "syntax-error" => "return { workflow = function(",
        _ => {
            return Err(Fault::new(
                "Scenario",
                format!("Unsupported Lua scenario: {scenario}"),
            ));
        }
    };
    Ok(source.into())
}
