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
        let record = run_once(&scenario, &inventory, None, false)?;
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
    let record = run_once(&scenario, &inventory, None, false)?;
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
    )?;
    let passed = before_readiness(&record)
        && record.metrics["compiled_inventory_identity"] == inventory.identity
        && record.primary.as_ref().is_some_and(|fault| {
            fault.category == "TypeScript"
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
    let record = run_once(&scenario, &inventory, None, false)?;
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
    Ok(())
}
