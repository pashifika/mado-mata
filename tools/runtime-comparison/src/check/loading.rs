use super::{FixtureTree, case, plan, replace_entry};
use crate::inventory::Inventory;
use crate::model::Fault;
use crate::runner::{RunRecord, run_once};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn no_input(record: &RunRecord) -> bool {
    record.status == "FAIL"
        && !record.forced
        && record.cleanup["clean"] == true
        && record.observations["dispatches"] == 0
        && ["accepted", "receipts", "effects"].iter().all(|name| {
            record.observations[*name]
                .as_array()
                .is_some_and(Vec::is_empty)
        })
}

fn before_readiness(record: &RunRecord) -> bool {
    no_input(record)
        && record.observations["observations"] == 0
        && record.observations["operation_metrics"]["observe"]["count"] == 0
        && record.observations["logs"]
            .as_array()
            .is_some_and(Vec::is_empty)
}

fn missing_entries(rows: &mut Vec<Value>) -> Result<(), Fault> {
    for candidate in ["javascript", "lua"] {
        for entry in ["readiness", "workflow"] {
            let tree = FixtureTree::new(candidate)?;
            let path = tree.0.join("package.json");
            let mut manifest: Value = serde_json::from_slice(
                &std::fs::read(&path).map_err(|error| Fault::new("Fixture", error.to_string()))?,
            )
            .map_err(|error| Fault::new("Fixture", error.to_string()))?;
            let omitted = manifest["entries"]
                .as_object_mut()
                .ok_or_else(|| Fault::new("Fixture", "entry declarations must be an object"))?
                .remove(entry)
                .ok_or_else(|| Fault::new("Fixture", "entry to omit is absent"))?;
            std::fs::write(
                &path,
                serde_json::to_vec(&manifest)
                    .map_err(|error| Fault::new("Fixture", error.to_string()))?,
            )
            .map_err(|error| Fault::new("Fixture", error.to_string()))?;
            let result = Inventory::capture(
                &tree.0,
                &plan(candidate, "success", "template-first").limits,
            );
            let passed = result.as_ref().is_err_and(|fault| {
                fault.category == "Inventory"
                    && fault.message.contains("package.json")
                    && fault.message.contains("missing field")
                    && fault.message.contains(entry)
            });
            rows.push(json!({
                "id": format!("{candidate}-missing-{entry}-declaration"),
                "candidate": candidate, "lane": "controlled", "os": std::env::consts::OS,
                "status": if passed { "PASS" } else { "FAIL" },
                "oracle": "capture identifies the missing entry declaration and returns no executable inventory; package readiness and workflow cannot be invoked",
                "observed": {
                    "inventory_created": result.is_ok(), "fault": result.err(),
                    "manifest_entries": manifest["entries"], "omitted_entry": omitted,
                    "configuration_origin": format!("package.json.entries.{entry}")
                }
            }));
        }
    }
    Ok(())
}

fn ambient_imports(rows: &mut Vec<Value>) -> Result<(), Fault> {
    for (candidate, name, specifier, ambient_path) in [
        (
            "javascript",
            "ambient-file-refusal",
            "./ambient.js",
            "ambient.js",
        ),
        (
            "javascript",
            "ambient-package-refusal",
            "ambient-package",
            "node_modules/ambient-package/index.js",
        ),
        (
            "lua",
            "ambient-file-refusal",
            "./ambient.lua",
            "ambient.lua",
        ),
    ] {
        let tree = FixtureTree::new(candidate)?;
        let scenario = plan(candidate, "success", "template-first");
        let entry = if candidate == "lua" {
            "main.lua"
        } else {
            "main.js"
        };
        let source = if candidate == "lua" {
            format!(
                "return {{readiness=function() return 'Ready' end, workflow=function()\n\
                 host.call('log',{{message='ambient-request-started'}})\n\
                 local name='{specifier}'; pcall(function() require(name) end)\n\
                 local observation=host.call('observe',{{}})\n\
                 host.call('submit',{{observation=observation,actions={{{{kind='key_down',key='A'}}}}}})\n\
                 end}}"
            )
        } else {
            format!(
                "export function readiness(){{return 'Ready';}}\n\
                 export async function workflow(){{\n\
                 host.call('log',{{message:'ambient-request-started'}});\n\
                 const name='{specifier}'; try{{await import(name);}}catch{{}}\n\
                 const observation=host.call('observe',{{}});\n\
                 host.call('submit',{{observation,actions:[{{kind:'key_down',key:'A'}}]}});\n\
                 }}"
            )
        };
        std::fs::write(tree.0.join(entry), &source)
            .map_err(|error| Fault::new("Fixture", error.to_string()))?;
        let inventory = Inventory::capture(&tree.0, &scenario.limits)?;
        // A real authoring-tree file appears after capture, but never enters the run inventory.
        let path = tree.0.join(ambient_path);
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)
                .map_err(|error| Fault::new("Fixture", error.to_string()))?;
        }
        let ambient_source = if candidate == "lua" {
            "host.call('log',{message='ambient-module-evaluated'}); return {}"
        } else {
            "host.call('log',{message:'ambient-module-evaluated'}); export const value=1;"
        };
        std::fs::write(&path, ambient_source)
            .map_err(|error| Fault::new("Fixture", error.to_string()))?;
        if name == "ambient-package-refusal" {
            std::fs::write(
                tree.0.join("node_modules/ambient-package/package.json"),
                r#"{"name":"ambient-package","type":"module","main":"index.js"}"#,
            )
            .map_err(|error| Fault::new("Fixture", error.to_string()))?;
        }
        let resolution = inventory.resolve(entry, specifier);
        let record = run_once(&scenario, &inventory, None, false, None)?;
        let ambient_unchanged = std::fs::read_to_string(&path)
            .map_err(|error| Fault::new("Fixture", error.to_string()))?
            == ambient_source;
        let passed = no_input(&record)
            && ambient_unchanged
            && !inventory.sources.contains_key(ambient_path)
            && resolution.as_ref().is_err_and(|fault| {
                fault.category == "ImportRefused"
                    && fault.context["requester"] == entry
                    && fault.context["specifier"] == specifier
            })
            && record.primary.as_ref().is_some_and(|fault| {
                fault.category == "ImportRefused"
                    && fault.context["requester"] == entry
                    && fault.context["specifier"] == specifier
                    && fault.context["stage"] == "loading"
            })
            && record.observations["failure"]["category"] == "ImportRefused"
            && record.observations["logs"] == json!(["ambient-request-started"])
            && record.observations["observations"] == 1;
        let row = rows.len();
        case(
            rows,
            &format!("{candidate}-{name}"),
            record,
            passed,
            "an existing ambient file or package cannot satisfy a captured-inventory miss; its evaluation marker is absent and a caught refusal cannot admit later input",
        );
        rows[row]["loading_evidence"] = json!({
            "ambient_file": ambient_path, "ambient_content_unchanged": ambient_unchanged,
            "captured_ambient_source": inventory.sources.contains_key(ambient_path),
            "resolver_fault": resolution.err(), "original_entry": inventory.sources[entry]
        });
    }
    Ok(())
}

fn secondary_lua_loading(rows: &mut Vec<Value>) -> Result<(), Fault> {
    let tree = FixtureTree::new("lua")?;
    let scenario = plan("lua", "success", "template-first");
    let source = r#"return {
    readiness=function() return 'Ready' end,
    workflow=function()
        assert(load == nil and loadfile == nil and dofile == nil)
        assert(package == nil and string.dump == nil)
        local forbidden = "host.call('log',{message='secondary-code-evaluated'})"
        assert(not pcall(function() return load(forbidden)() end))
        assert(not pcall(function() return loadfile('secondary.lua')() end))
        assert(not pcall(function() return dofile('secondary.lua') end))
        assert(not pcall(function() return package.loadlib('secondary-native', 'open') end))
        assert(not pcall(function() return package.searchers[2]('secondary') end))
        host.call('log',{message='secondary-loaders-unavailable'})
        local observation=host.call('observe',{})
        local loader=_G.require
        local name='./secondary'..'.lua'
        pcall(function() loader(name) end)
        host.call('submit',{observation=observation,actions={{kind='key_down',key='A'}}})
    end
}"#;
    std::fs::write(tree.0.join("main.lua"), source)
        .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    let inventory = Inventory::capture(&tree.0, &scenario.limits)?;
    let ambient_source = "host.call('log',{message='secondary-code-evaluated'}); return {}";
    let ambient_path = tree.0.join("secondary.lua");
    std::fs::write(&ambient_path, ambient_source)
        .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    let record = run_once(&scenario, &inventory, None, false, None)?;
    let ambient_unchanged = std::fs::read_to_string(&ambient_path)
        .map_err(|error| Fault::new("Fixture", error.to_string()))?
        == ambient_source;
    let passed = no_input(&record)
        && ambient_unchanged
        && record.primary.as_ref().is_some_and(|fault| {
            fault.category == "ImportRefused"
                && fault.context["requester"] == "main.lua"
                && fault.context["specifier"] == "./secondary.lua"
                && fault.context["stage"] == "loading"
        })
        && record.observations["failure"]["category"] == "ImportRefused"
        && record.observations["logs"] == json!(["secondary-loaders-unavailable"])
        && record.observations["observations"] == 2;
    let row = rows.len();
    case(
        rows,
        "lua-secondary-loader-refusal",
        record,
        passed,
        "load, loadfile, dofile, bytecode export and package/native search routes are unavailable; aliased require still latches an inventory refusal before catch-and-continue input",
    );
    rows[row]["loading_evidence"] = json!({
        "ambient_file": "secondary.lua", "ambient_content_unchanged": ambient_unchanged,
        "original_entry": inventory.sources["main.lua"]
    });
    Ok(())
}

fn plugin_manifest_refusal(rows: &mut Vec<Value>) -> Result<(), Fault> {
    let tree = FixtureTree::new("typescript")?;
    let path = tree.0.join("package.json");
    let marker = tree.0.join("plugin-executed");
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(&path).map_err(|error| Fault::new("Fixture", error.to_string()))?,
    )
    .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    // This is the actual compiler self-check's plugin request shape. The public
    // capture boundary rejects it earlier than the compiler's defense-in-depth guard.
    manifest["compilerOptions"] = json!({"plugins": [{"name": "./unapproved-plugin.js"}]});
    manifest["sources"]
        .as_array_mut()
        .ok_or_else(|| Fault::new("Fixture", "sources must be an array"))?
        .push(json!("unapproved-plugin.js"));
    let plugin = format!(
        "require('node:fs').writeFileSync({}, 'executed'); module.exports = {{}};",
        serde_json::to_string(&marker).map_err(|error| Fault::new("Fixture", error.to_string()))?
    );
    std::fs::write(tree.0.join("unapproved-plugin.js"), &plugin)
        .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    std::fs::write(
        &path,
        serde_json::to_vec(&manifest).map_err(|error| Fault::new("Fixture", error.to_string()))?,
    )
    .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    let result = Inventory::capture(
        &tree.0,
        &plan("typescript", "success", "template-first").limits,
    );
    let marker_present = marker
        .try_exists()
        .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    let plugin_unchanged = std::fs::read_to_string(tree.0.join("unapproved-plugin.js"))
        .map_err(|error| Fault::new("Fixture", error.to_string()))?
        == plugin;
    let passed = !marker_present
        && plugin_unchanged
        && result.as_ref().is_err_and(|fault| {
            fault.category == "Inventory"
                && fault.message.contains("package.json")
                && fault.message.contains("compilerOptions")
        });
    rows.push(json!({
        "id": "typescript-plugin-manifest-refusal", "candidate": "typescript",
        "lane": "controlled", "os": std::env::consts::OS,
        "status": if passed { "PASS" } else { "FAIL" },
        "oracle": "the real package plugin request is rejected at capture with its package.json configuration origin and the existing plugin's execution marker is absent; this does not exercise the compiler guard",
        "observed": {
            "inventory_created": result.is_ok(), "fault": result.err(),
            "configuration_origin": "package.json.compilerOptions.plugins",
            "compilerOptions": manifest["compilerOptions"],
            "plugin_content_unchanged": plugin_unchanged, "plugin_marker_present": marker_present
        },
        "unverified": "CompilerPolicy through run_once: Manifest denies compilerOptions before a valid inventory can reach typescript::compile"
    }));
    Ok(())
}

fn generated_helper(rows: &mut Vec<Value>, base: &Inventory) -> Result<(), Fault> {
    // Use the same decorator construct as compiler/worker.mjs selfCheck. With
    // the application's importHelpers option it needs the unauthorized tslib.
    let source = "function decorate(value: Function, context: ClassDecoratorContext) {}\n\
                  @decorate export class Example {}\n\
                  export function readiness(): MadoReady {host.call('log',{message:'readiness-entered'});return 'Ready';}\n\
                  export function workflow(): void {host.call('log',{message:'workflow-entered'});}";
    let inventory = replace_entry(base, source)?;
    let record = run_once(
        &plan("typescript", "success", "template-first"),
        &inventory,
        None,
        false,
        None,
    )?;
    let passed = before_readiness(&record)
        && record.metrics["compiled_inventory_identity"] == inventory.identity
        && record.primary.as_ref().is_some_and(|fault| {
            fault.category == "TypeScript"
                && fault.context["stage"] == "compilation"
                && fault.context["catalog"] == inventory.metadata["catalog"]
                && fault.context["inventory"] == inventory.identity
                && fault.context["diagnostics"]
                    .as_array()
                    .is_some_and(|diagnostics| {
                        diagnostics.iter().any(|diagnostic| {
                            diagnostic["code"] == 2354
                                && diagnostic["module"] == inventory.entries.workflow.module
                                && diagnostic["line"] == 2
                                && diagnostic["message"]
                                    .as_str()
                                    .is_some_and(|message| message.contains("tslib"))
                        })
                    })
        });
    let row = rows.len();
    case(
        rows,
        "typescript-generated-helper-refusal",
        record,
        passed,
        "the real TypeScript compiler refuses the decorator's generated tslib dependency at its original source location before module evaluation, readiness or input",
    );
    rows[row]["loading_evidence"] = json!({
        "original_module": inventory.entries.workflow.module,
        "original_source": inventory.sources[&inventory.entries.workflow.module]
    });
    Ok(())
}

fn generated_import(rows: &mut Vec<Value>, base: &Inventory) -> Result<(), Fault> {
    let source = r#"export function readiness(): MadoReady { return 'Ready'; }
export async function workflow(): Promise<void> {
    host.call('log', {message: 'generated-workflow-started'});
    const observation = host.call('observe', {});
    const name: string = ['node', 'fs'].join(':');
    try { await import(name); } catch {}
    host.call('submit', {observation, actions: [{kind: 'key_down', key: 'A'}]});
}"#;
    let inventory = replace_entry(base, source)?;
    let record = run_once(
        &plan("typescript", "success", "template-first"),
        &inventory,
        None,
        false,
        None,
    )?;
    let passed = no_input(&record)
        && record.metrics["compiled_inventory_identity"]
            .as_str()
            .is_some_and(|identity| identity != inventory.identity)
        && record.primary.as_ref().is_some_and(|fault| {
            fault.category == "ImportRefused"
                && fault.context["requester"] == "main.js"
                && fault.context["specifier"] == "node:fs"
                && fault.context["stage"] == "loading"
                && fault.context["typescript"]["original_inventory"] == inventory.identity
                && fault.context["inventory"] == record.metrics["compiled_inventory_identity"]
        })
        && record.observations["failure"]["category"] == "ImportRefused"
        && record.observations["logs"] == json!(["generated-workflow-started"])
        && record.observations["observations"] == 2;
    let row = rows.len();
    case(
        rows,
        "typescript-generated-import-refusal",
        record,
        passed,
        "actual compiled JavaScript reaches workflow but its computed Node import is refused by the same runtime resolver; catching it cannot grant ambient authority or submit input",
    );
    rows[row]["loading_evidence"] = json!({
        "original_module": inventory.entries.workflow.module,
        "original_source": inventory.sources[&inventory.entries.workflow.module]
    });
    Ok(())
}

fn original_dependency(rows: &mut Vec<Value>) -> Result<(), Fault> {
    let tree = FixtureTree::new("typescript")?;
    let scenario = plan("typescript", "success", "template-first");
    let entry = "import { choose } from './decisions.js';\n\
                 export function readiness(): MadoReady {host.call('log',{message:'readiness-entered'});return 'Ready';}\n\
                 export function workflow(): void {choose();}";
    // An unused value import, not a type-only import: erasing the import from
    // emitted JavaScript must not hide the original helper's dependency.
    let helper = "import { concealed } from './forbidden.js';\n\
                  export function choose(): void {host.call('log',{message:'helper-entered'});}";
    std::fs::write(tree.0.join("main.ts"), entry)
        .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    std::fs::write(tree.0.join("decisions.ts"), helper)
        .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    let inventory = Inventory::capture(&tree.0, &scenario.limits)?;
    let ambient_source =
        "host.call('log',{message:'forbidden-module-evaluated'}); export const concealed=1;";
    let ambient_path = tree.0.join("forbidden.ts");
    std::fs::write(&ambient_path, ambient_source)
        .map_err(|error| Fault::new("Fixture", error.to_string()))?;
    let entry_resolution = inventory.resolve("main.ts", "./decisions.js");
    let record = run_once(&scenario, &inventory, None, false, None)?;
    let ambient_unchanged = std::fs::read_to_string(&ambient_path)
        .map_err(|error| Fault::new("Fixture", error.to_string()))?
        == ambient_source;
    let passed = before_readiness(&record)
        && ambient_unchanged
        && entry_resolution
            .as_deref()
            .is_ok_and(|module| module == "decisions.ts")
        && record.metrics["compiled_inventory_identity"] == inventory.identity
        && record.primary.as_ref().is_some_and(|fault| {
            fault.category == "ImportRefused"
                && fault.context["inventory"] == inventory.identity
                && fault.context["module"] == "decisions.ts"
                && fault.context["line"] == 1
                && fault.context["specifier"] == "./forbidden.js"
                && fault.context["kind"] == "module"
                && fault.context["cause"]["requester"] == "decisions.ts"
                && fault.context["cause"]["destination"] == "forbidden.js"
                && fault.context["cause"]["chain"] == json!(["decisions.ts", "./forbidden.js"])
        });
    let row = rows.len();
    case(
        rows,
        "typescript-original-dependency-refusal",
        record,
        passed,
        "compiler inspection resolves the original unused value import before emission can erase or inline it, retaining the original requesting helper, source location and refused dependency edge with no readiness or input",
    );
    rows[row]["loading_evidence"] = json!({
        "original_sources": {
            "main.ts": inventory.sources["main.ts"], "decisions.ts": inventory.sources["decisions.ts"]
        },
        "entry_resolution": entry_resolution.ok(), "ambient_file": "forbidden.ts",
        "ambient_content_unchanged": ambient_unchanged
    });
    Ok(())
}

fn helper_diagnostics(
    rows: &mut Vec<Value>,
    inventories: &BTreeMap<&str, Inventory>,
) -> Result<(), Fault> {
    for candidate in ["javascript", "typescript", "lua"] {
        for held in [false, true] {
            let (entry, helper, helper_id) = if candidate == "lua" {
                (
                    "local helper = require('./decisions.lua')\n\
                     local function workflow()\n\
                       local observation = host.call('observe', {})\n\
                       host.call('query', {observation=observation,kind='ocr',roi=host.options.recognition.roi,expected='READY'})\n\
                       helper.fail()\n\
                     end\n\
                     return {readiness=function() return 'Ready' end,workflow=workflow}",
                    "local helper = require('@mado/helper')\n\
                     local function fail()\n\
                       helper.fail()\n\
                     end\n\
                     return {fail=fail}",
                    "decisions.lua",
                )
            } else {
                (
                    "import { fail } from './decisions.js';\n\
                     export function readiness(){return 'Ready' as const;}\n\
                     export function workflow(){\n\
                       const observation = host.call('observe', {});\n\
                       host.call('query', {observation,kind:'ocr',roi:host.options.recognition.roi,expected:'READY'});\n\
                       fail();\n\
                     }",
                    "import { fail as approvedFail } from '@mado/helper';\n\
                     export function fail(){\n\
                       approvedFail();\n\
                     }",
                    if candidate == "typescript" {
                        "decisions.ts"
                    } else {
                        "decisions.js"
                    },
                )
            };
            let entry = if candidate == "javascript" {
                entry.replace(" as const", "")
            } else {
                entry.to_owned()
            };
            let mut inventory = replace_entry(&inventories[candidate], &entry)?;
            inventory.sources.insert(helper_id.into(), helper.into());
            inventory.refresh_identity()?;
            let record = run_once(
                &plan(
                    candidate,
                    if held { "held-work" } else { "success" },
                    "template-first",
                ),
                &inventory,
                None,
                false,
                None,
            )?;
            let approved = &inventory.metadata["catalog"]["@mado/helper"];
            let approved_id = approved["entry"]
                .as_str()
                .ok_or_else(|| Fault::new("Fixture", "approved helper entry is absent"))?;
            let emitted_helper = if candidate == "lua" {
                "decisions.lua"
            } else {
                "decisions.js"
            };
            let emitted_entry = if candidate == "lua" {
                "main.lua"
            } else {
                "main.js"
            };
            let passed = record.status == "FAIL"
                && record.forced == held
                && (record.cleanup["clean"] == true) != held
                && record.entry_outcome == "FailedOrNotStarted"
                && record.primary.as_ref().is_some_and(|fault| {
                    fault.category == "Script"
                        && fault.context["module"] == approved_id
                        && fault.context["line"] == if candidate == "lua" { 4 } else { 3 }
                        && fault.context["catalog"]["@mado/helper"] == *approved
                        && fault.context["catalog"]["@mado/order"]
                            == inventory.metadata["catalog"]["@mado/order"]
                        && fault.context["stack"].as_str().is_some_and(|stack| {
                            stack.contains(&format!("{approved_id}:"))
                                && stack.contains(&format!("{emitted_helper}:"))
                                && stack.contains(&format!("{emitted_entry}:"))
                        })
                        && (candidate != "typescript"
                            || fault.context["typescript"]["frames"]
                                .as_array()
                                .is_some_and(|frames| {
                                    frames.iter().any(|frame| {
                                        frame["generated"]["module"] == approved_id
                                            && frame["original"].is_null()
                                    }) && frames.iter().any(|frame| {
                                        frame["original"]["module"] == "decisions.ts"
                                            && frame["original"]["line"] == 3
                                            && frame["original"]["function"] == "fail"
                                    })
                                }))
                });
            case(
                rows,
                &format!(
                    "{candidate}-approved-helper-diagnostic-{}",
                    if held { "incomplete" } else { "clean" }
                ),
                record,
                passed,
                "approved helper failure retains its admitted version/content, internal caller and entry stack; original TypeScript caller locations and unmapped approved JavaScript remain distinct, as do clean and forced cleanup",
            );
        }
    }
    Ok(())
}

fn host_fault_diagnostics(
    rows: &mut Vec<Value>,
    inventories: &BTreeMap<&str, Inventory>,
) -> Result<(), Fault> {
    for candidate in ["javascript", "typescript", "lua"] {
        for caught in [false, true] {
            let (entry, helper, helper_id) = if candidate == "lua" {
                let call = if caught {
                    "pcall(function() helper.recognize(observation) end)\n\
                     host.call('observe', {})"
                } else {
                    "local thread = coroutine.create(function() helper.recognize(observation) end)\n\
                     local ok, cause = coroutine.resume(thread)\n\
                     if not ok then error(cause) end"
                };
                (
                    format!("local helper = require('./decisions.lua')\n\
                             local function workflow()\n\
                               local observation = host.call('observe', {{}})\n\
                               host.call('log', {{message=observation.id}})\n\
                               local sequence = host.call('submit', {{observation=observation,actions={{{{kind='key_down',key='A'}}}}}})\n\
                               host.call('settle', {{id=sequence.id}})\n\
                               {call}\n\
                             end\n\
                             return {{readiness=function() return 'Ready' end,workflow=workflow}}"),
                    "local function recognize(observation)\n\
                       host.call('recognize', {observation=observation,kind='template',asset='marker',roi=host.options.recognition.roi})\n\
                     end\n\
                     return {recognize=recognize}".to_owned(),
                    "decisions.lua",
                )
            } else {
                let call = if caught {
                    "try { recognize(observation); } catch {}\n\
                     host.call('observe', {});"
                } else {
                    "await recognize(observation);"
                };
                let typed = candidate == "typescript";
                let asynchronous = if caught { "" } else { "async " };
                let scheduled = if caught {
                    ""
                } else {
                    "await Promise.resolve();\n"
                };
                (
                    format!(
                        "import {{ recognize }} from './decisions.js';\n\
                             export function readiness(){{return 'Ready'{};}}\n\
                             export {asynchronous}function workflow(){{\n\
                               const observation = host.call('observe', {{}});\n\
                               host.call('log', {{message:observation.id}});\n\
                               const sequence = host.call('submit', {{observation,actions:[{{kind:'key_down',key:'A'}}]}});\n\
                               host.call('settle', {{id:sequence.id}});\n\
                               {call}\n\
                             }}",
                        if typed { " as const" } else { "" }
                    ),
                    format!(
                        "export {asynchronous}function recognize(observation{}){{\n{scheduled}\
                               host.call('recognize', {{observation,kind:'template',asset:'marker',roi:host.options.recognition.roi}});\n\
                             }}",
                        if typed { ": MadoObservation" } else { "" }
                    ),
                    if typed {
                        "decisions.ts"
                    } else {
                        "decisions.js"
                    },
                )
            };
            let mut inventory = replace_entry(&inventories[candidate], &entry)?;
            inventory.sources.insert(helper_id.into(), helper);
            inventory.refresh_identity()?;
            let record = run_once(
                &plan(candidate, "backend-failure", "template-first"),
                &inventory,
                None,
                false,
                None,
            )?;
            let emitted_helper = if candidate == "lua" {
                "decisions.lua"
            } else {
                "decisions.js"
            };
            let source_line = if candidate == "lua" || caught { 2 } else { 3 };
            let passed = record.status == "FAIL"
                && !record.forced
                && record.cleanup["clean"] == true
                && record.observations["observations"] == 2
                && record.observations["receipts"]
                    .as_array()
                    .is_some_and(|receipts| {
                        receipts.len() == 1 && receipts[0]["status"] == "Submitted"
                    })
                && record.cleanup["release_outcomes"]
                    .as_array()
                    .is_some_and(|releases| {
                        releases
                            .iter()
                            .any(|release| release["key"] == "A" && release["released"] == true)
                    })
                && record.primary.as_ref().is_some_and(|fault| {
                    fault.category == "Backend"
                        && fault.message == "controlled recognition backend failure"
                        && fault.context["cause"] == "injected-backend-failure"
                        && fault.context["operation"] == "template"
                        && fault.context["observation"] == record.observations["logs"][0]
                        && fault.context["module"] == emitted_helper
                        && fault.context["line"] == source_line
                        && fault.context["stack"].as_str().is_some_and(|stack| {
                            stack.contains(&format!("{emitted_helper}:{source_line}"))
                        })
                        && (candidate != "typescript"
                            || fault.context["typescript"]["frames"]
                                .as_array()
                                .is_some_and(|frames| {
                                    frames.iter().any(|frame| {
                                        frame["original"]["module"] == helper_id
                                            && frame["original"]["line"] == source_line
                                            && frame["original"]["function"] == "recognize"
                                    })
                                }))
                });
            case(
                rows,
                &format!(
                    "{candidate}-host-cause-diagnostic-{}",
                    if caught { "caught" } else { "uncaught" }
                ),
                record,
                passed,
                "controlled backend failure preserves the typed message/cause, operation, observation and helper callsite through a promise job or Lua coroutine, and through caught synchronous failures; prior input receipt and verified release remain separate from execution failure",
            );
        }
    }
    Ok(())
}

fn mapping_integrity(rows: &mut Vec<Value>, inventory: &Inventory) -> Result<(), Fault> {
    let compiled = crate::typescript::compile(
        inventory,
        &plan("typescript", "success", "template-first").limits,
    )?;
    let map = json!({
        "version":3,"file":"decisions.js","sources":["decisions.ts"],"names":[],
        "sourcesContent":[inventory.sources["decisions.ts"]],"mappings":"AAAA"
    });
    for scenario in [
        "captured-source",
        "missing-map",
        "changed-source",
        "ambiguous-line",
        "unmapped-span",
        "backward-column",
        "message-not-frame",
    ] {
        let mut fixture = compiled.clone();
        let mut map = map.clone();
        let mut stack = "    at recognize (decisions.js:1:1)".to_owned();
        match scenario {
            "changed-source" => map["sourcesContent"][0] = json!("different authoring content"),
            "ambiguous-line" => {
                map["mappings"] = json!("AAAA,CACA");
                stack = "    at recognize (decisions.js:1)".into();
            }
            "unmapped-span" => {
                map["mappings"] = json!("AAAA,C");
                stack = "    at recognize (decisions.js:1)".into();
            }
            "backward-column" => {
                map["mappings"] = json!("AAAA,DAAA");
                stack = "    at recognize (decisions.js:1:2)".into();
            }
            "message-not-frame" => stack = "backend mentioned decisions.js:1:1, not a frame".into(),
            _ => {}
        }
        fixture
            .source_maps
            .insert("decisions.js".into(), map.to_string());
        if scenario == "missing-map" {
            fixture.source_maps.remove("decisions.js");
        }
        fixture.metadata["runtime_scenario"] = json!(format!("source-map-decoder-{scenario}"));
        fixture.refresh_identity()?;
        let primary = Fault::new("NativeBackend", "controlled typed native-shaped failure")
            .with_context(json!({
                "operation":"recognize","query":"query-17","run":"run-3","attempt":2,
                "observation":{"session":4,"frame":5,"geometry":6},
                "cause":{"category":"CapturePermission","message":"permission refused"},
                "receipt":{"status":"Partial","cleanup":{"clean":false,"remaining":["A"]}},
                "stack":stack
            }));
        let mapped = crate::typescript::map_fault(&fixture, primary.clone());
        let mut retained = mapped.context.clone();
        retained
            .as_object_mut()
            .expect("fault context is an object")
            .remove("typescript");
        let frames = mapped.context["typescript"]["frames"]
            .as_array()
            .ok_or_else(|| Fault::new("Fixture", "mapping result has no frame array"))?;
        let available = scenario == "captured-source";
        let passed = mapped.category == primary.category
            && mapped.message == primary.message
            && retained == primary.context
            && mapped.context["typescript"]["mapping"]
                == if available {
                    "available"
                } else {
                    "unavailable"
                }
            && if scenario == "message-not-frame" {
                frames.is_empty()
            } else {
                frames.len() == 1
                    && frames[0]["generated"]["module"] == "decisions.js"
                    && frames[0]["generated"]["line"] == 1
                    && if available {
                        frames[0]["original"]["module"] == "decisions.ts"
                            && frames[0]["original"]["line"] == 1
                    } else {
                        frames[0]["original"].is_null()
                    }
            };
        rows.push(json!({
            "id":format!("typescript-source-mapping-{scenario}"),
            "candidate":"typescript","lane":"controlled","os":std::env::consts::OS,
            "status":if passed {"PASS"} else {"FAIL"},
            "oracle":"only captured, unambiguous mapped coordinates receive original attribution; generated frames and the full typed primary/native-shaped cause/partial cleanup context are retained without interpreting message text as a frame",
            "observed":mapped,"native_qualification":false,
            "loading_evidence":{"kind":"controlled source-map decoder mutation over compiled inventory","fixture":scenario}
        }));
    }
    Ok(())
}

fn compiler_helper_diagnostic(rows: &mut Vec<Value>, base: &Inventory) -> Result<(), Fault> {
    let mut inventory = replace_entry(
        base,
        "import { fail } from './decisions.js';\n\
         export function readiness(): MadoReady {return 'Ready';}\n\
         export function workflow(): void {fail();}",
    )?;
    inventory.sources.insert(
        "decisions.ts".into(),
        "export function fail(): void {\n\
           host.options.unknown;\n\
         }"
        .into(),
    );
    inventory.refresh_identity()?;
    let record = run_once(
        &plan("typescript", "success", "template-first"),
        &inventory,
        None,
        false,
        None,
    )?;
    let passed = before_readiness(&record)
        && record.primary.as_ref().is_some_and(|fault| {
            fault.category == "TypeScript"
                && fault.context["inventory"] == inventory.identity
                && fault.context["compiler"]["version"] == "5.9.3"
                && fault.context["diagnostics"]
                    .as_array()
                    .is_some_and(|diagnostics| {
                        diagnostics.iter().any(|diagnostic| {
                            diagnostic["code"] == 2339
                                && diagnostic["module"] == "decisions.ts"
                                && diagnostic["line"] == 2
                                && diagnostic["function"] == "fail"
                        })
                    })
                && fault.context["stack"]
                    .as_str()
                    .is_some_and(|stack| stack.contains("worker.mjs:"))
        });
    case(
        rows,
        "typescript-compiler-helper-diagnostic",
        record,
        passed,
        "a real compiler error in an internal helper retains its original module, function, line and diagnostic code plus the separate compiler implementation stack; preflight admits no readiness or input",
    );
    Ok(())
}

pub(super) fn run(
    rows: &mut Vec<Value>,
    inventories: &BTreeMap<&str, Inventory>,
) -> Result<(), Fault> {
    missing_entries(rows)?;
    ambient_imports(rows)?;
    secondary_lua_loading(rows)?;
    plugin_manifest_refusal(rows)?;
    generated_helper(rows, &inventories["typescript"])?;
    generated_import(rows, &inventories["typescript"])?;
    original_dependency(rows)?;
    helper_diagnostics(rows, inventories)?;
    host_fault_diagnostics(rows, inventories)?;
    mapping_integrity(rows, &inventories["typescript"])?;
    compiler_helper_diagnostic(rows, &inventories["typescript"])?;
    Ok(())
}
