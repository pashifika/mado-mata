//! Lua 5.4, with base/table/string/math/utf8 and controlled coroutines only.
//! Modules return a non-nil value; cycles are rejected with their import chain.
//! Each module has a requester-bound require; caches and globals are attempt-local.
//! Options use recursively readonly userdata: assignment and table mutation fail,
//! including aliases. No backing table is returned by pairs or serialization.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mlua::chunk::ChunkMode;
use mlua::{
    Function, HookTriggers, Lua, LuaOptions, LuaSerdeExt, MetaMethod, MultiValue, StdLib, Table,
    Thread, UserData, UserDataFields, UserDataMethods, Value, VmState,
};
use serde::Serialize;
use serde_json::{Value as Json, json};

use crate::host::Host;
use crate::inventory::Inventory;
use crate::model::{Fault, RuntimeMetrics};

#[derive(Serialize)]
#[serde(transparent)]
struct ReadOnly(Table);

impl UserData for ReadOnly {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |_, this, key: Value| {
            this.0.raw_get::<Value>(key)
        });
        methods.add_meta_method(
            MetaMethod::NewIndex,
            |_, _, _: (Value, Value)| -> mlua::Result<()> {
                Err(mlua::Error::runtime("Options are readonly"))
            },
        );
        methods.add_meta_method(MetaMethod::Len, |_, this, ()| Ok(this.0.raw_len()));
        methods.add_meta_method(MetaMethod::Pairs, |lua, this, ()| {
            // Iterator state is private; returning the backing table as the
            // standard next/state pair would make readonly trivially bypassable.
            let mut pairs = this
                .0
                .pairs::<Value, Value>()
                .collect::<mlua::Result<Vec<_>>>()?
                .into_iter();
            let next = lua.create_function_mut(move |_, _: MultiValue| {
                Ok(pairs.next().unwrap_or((Value::Nil, Value::Nil)))
            })?;
            Ok((next, Value::Nil, Value::Nil))
        });
    }
}

fn readonly(lua: &Lua, value: Value) -> mlua::Result<Value> {
    let Value::Table(table) = value else {
        return Ok(value);
    };
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, value) = pair?;
        table.raw_set(key, readonly(lua, value)?)?;
    }
    Ok(Value::UserData(lua.create_ser_userdata(ReadOnly(table))?))
}

struct HostView {
    options: Value,
    state: Table,
    call: Function,
}

impl UserData for HostView {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("options", |_, this| Ok(this.options.clone()));
        fields.add_field_method_get("state", |_, this| Ok(this.state.clone()));
        fields.add_field_method_get("call", |_, this| Ok(this.call.clone()));
    }
}

fn annotate(
    mut fault: Fault,
    inventory: &Inventory,
    module: &str,
    stage: &str,
    stack: Option<String>,
) -> Fault {
    if !fault.context.is_object() {
        fault.context = json!({"cause":fault.context});
    }
    fault.context["inventory"] = json!(inventory.identity);
    if fault.context.get("module").is_none() {
        fault.context["module"] = json!(module);
    }
    fault.context["stage"] = json!(stage);
    if let Some(stack) = stack {
        fault.context["stack"] = json!(stack);
    }
    fault.context["catalog"] = inventory.metadata["catalog"].clone();
    fault
}

fn script_fault(error: mlua::Error, inventory: &Inventory, module: &str, stage: &str) -> Fault {
    let category = match &error {
        mlua::Error::SyntaxError { .. } => "Syntax",
        mlua::Error::MemoryError(_) => "ResourceLimit",
        _ => "Script",
    };
    let text = error.to_string();
    annotate(
        Fault::new(category, text.clone()),
        inventory,
        module,
        stage,
        Some(text),
    )
}

struct Modules {
    inventory: Arc<Inventory>,
    host: Host,
    halted: Arc<AtomicBool>,
    compiled: RefCell<BTreeMap<String, Function>>,
    loaded: RefCell<BTreeMap<String, Value>>,
    loading: RefCell<Vec<String>>,
}

impl Modules {
    fn refuse(&self, fault: Fault) -> mlua::Error {
        self.halted.store(true, Ordering::Release);
        let module = fault.context["requester"]
            .as_str()
            .or_else(|| fault.context["module"].as_str())
            .unwrap_or("<loader>")
            .to_owned();
        let fault = annotate(fault, &self.inventory, &module, "loading", None);
        self.host.fail(fault.clone());
        mlua::Error::external(fault)
    }

    fn load(&self, id: &str) -> mlua::Result<Value> {
        if let Some(fault) = self.host.failure() {
            return Err(mlua::Error::external(fault));
        }
        self.host.control().check().map_err(mlua::Error::external)?;
        if let Some(value) = self.loaded.borrow().get(id) {
            return Ok(value.clone());
        }
        if self.loading.borrow().iter().any(|name| name == id) {
            let mut chain = self.loading.borrow().clone();
            chain.push(id.to_owned());
            return Err(self.refuse(
                Fault::new("ImportCycle", "Lua module cycle is unsupported")
                    .with_context(json!({"module":id,"chain":chain})),
            ));
        }
        let function = self.compiled.borrow().get(id).cloned().ok_or_else(|| {
            self.refuse(
                Fault::new("ImportRefused", "Module has no captured Lua source")
                    .with_context(json!({"module":id})),
            )
        })?;
        self.loading.borrow_mut().push(id.to_owned());
        let result = function.call::<Value>(());
        self.loading.borrow_mut().pop();
        let value = result?;
        if value == Value::Nil {
            return Err(self.refuse(
                Fault::new("ImportContract", "Lua modules must return a non-nil value")
                    .with_context(json!({"module":id})),
            ));
        }
        self.loaded
            .borrow_mut()
            .insert(id.to_owned(), value.clone());
        Ok(value)
    }

    fn environment(self: &Rc<Self>, lua: &Lua, id: &str) -> mlua::Result<Table> {
        let weak = Rc::downgrade(self);
        let requester = id.to_owned();
        let require = lua.create_function(move |_, specifier: String| {
            let modules = weak
                .upgrade()
                .ok_or_else(|| mlua::Error::runtime("Attempt loader is closed"))?;
            let id = modules
                .inventory
                .resolve(&requester, &specifier)
                .map_err(|fault| modules.refuse(fault))?;
            modules.load(&id)
        })?;
        let globals = lua.globals();
        let env = lua.create_table()?;
        let meta = lua.create_table()?;
        let read_globals = globals.clone();
        let self_env = env.clone();
        meta.set(
            "__index",
            lua.create_function(move |_, (_, key): (Value, Value)| {
                if let Value::String(name) = &key {
                    if name.to_str()?.as_ref() == "require" {
                        return Ok(Value::Function(require.clone()));
                    }
                    if name.to_str()?.as_ref() == "_G" {
                        return Ok(Value::Table(self_env.clone()));
                    }
                }
                read_globals.raw_get::<Value>(key)
            })?,
        )?;
        meta.set(
            "__newindex",
            lua.create_function(move |_, (_, key, value): (Value, Value, Value)| {
                if let Value::String(name) = &key {
                    if ["host", "require", "_G"].contains(&name.to_str()?.as_ref()) {
                        return Err(mlua::Error::runtime("Runtime bindings are readonly"));
                    }
                }
                globals.raw_set(key, value)
            })?,
        )?;
        meta.set("__metatable", false)?;
        env.set_metatable(Some(meta))?;
        Ok(env)
    }
}

// Syntax is checked by Lua itself first. This lexer only enumerates literal
// require expressions; comments/quoted/long strings are never searched as code.
// Aliases and computed expressions remain runtime-only checks, not static proof.
#[derive(Clone, Copy)]
enum Token<'a> {
    Word(&'a str),
    Literal(&'a str),
    Punct(u8),
}

fn long_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut opening = start + 1;
    while bytes.get(opening) == Some(&b'=') {
        opening += 1;
    }
    if bytes.get(opening) != Some(&b'[') {
        return None;
    }
    let equals = opening - start - 1;
    let mut cursor = opening + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b']'
            && bytes
                .get(cursor + 1..cursor + 1 + equals)
                .is_some_and(|part| part.iter().all(|c| *c == b'='))
            && bytes.get(cursor + 1 + equals) == Some(&b']')
        {
            return Some(cursor + equals + 2);
        }
        cursor += 1;
    }
    None
}

fn literal_imports<'a>(source: &'a str) -> Vec<&'a str> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"--") {
            i += 2;
            if let Some(end) = long_string_end(bytes, i) {
                i = end;
            } else {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
        } else if bytes[i] == b'\'' || bytes[i] == b'"' {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            tokens.push(Token::Literal(&source[start..i]));
        } else if let Some(end) = long_string_end(bytes, i) {
            i = end;
            tokens.push(Token::Literal(&source[start..i]));
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(Token::Word(&source[start..i]));
        } else {
            tokens.push(Token::Punct(bytes[i]));
            i += 1;
        }
    }
    let mut imports = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if !matches!(token, Token::Word("require")) {
            continue;
        }
        match tokens.get(i + 1..) {
            Some([Token::Literal(literal), ..]) => imports.push(*literal),
            Some(
                [
                    Token::Punct(b'('),
                    Token::Literal(literal),
                    Token::Punct(b')'),
                    ..,
                ],
            ) => imports.push(*literal),
            _ => {}
        }
    }
    imports
}

// mlua catches Rust unwinds at its C boundary, transports them safely, and
// resumes them on return to Rust. With catch_rust_panics(false), pcall/xpcall
// cannot turn this private interruption marker into a recoverable Lua error.
struct Interrupted(Fault);

fn coroutine_library(
    lua: &Lua,
    host: &Host,
    halted: &Arc<AtomicBool>,
    resumes: Rc<Cell<u64>>,
) -> mlua::Result<()> {
    let library: Table = lua.globals().get("coroutine")?;
    let created = Rc::new(Cell::new(0usize));
    for (name, wrap) in [("create", false), ("wrap", true)] {
        let created = created.clone();
        let host = host.clone();
        let halted = halted.clone();
        let resumes = resumes.clone();
        library.set(
            name,
            lua.create_function(move |lua, function: Function| {
                if created.get() >= host.limits().max_actions {
                    halted.store(true, Ordering::Release);
                    let fault =
                        Fault::new("ResourceLimit", "Lua coroutine creation limit exceeded");
                    host.fail(fault.clone());
                    return Err(mlua::Error::external(fault));
                }
                created.set(created.get() + 1);
                let thread = lua.create_thread(function)?;
                if !wrap {
                    return Ok(Value::Thread(thread));
                }
                let host = host.clone();
                let resumes = resumes.clone();
                Ok(Value::Function(lua.create_function(
                    move |_, args: MultiValue| resume(&host, &resumes, &thread, args),
                )?))
            })?,
        )?;
    }
    let host = host.clone();
    library.set(
        "resume",
        lua.create_function(move |_, (thread, args): (Thread, MultiValue)| {
            match resume(&host, &resumes, &thread, args) {
                Ok(mut values) => {
                    values.push_front(Value::Boolean(true));
                    Ok(values)
                }
                Err(error) => Ok(MultiValue::from_vec(vec![
                    Value::Boolean(false),
                    Value::Error(Box::new(error)),
                ])),
            }
        })?,
    )?;
    Ok(())
}

fn resume(
    host: &Host,
    resumes: &Cell<u64>,
    thread: &Thread,
    args: MultiValue,
) -> mlua::Result<MultiValue> {
    host.control().check().map_err(mlua::Error::external)?;
    if let Some(fault) = host.failure() {
        return Err(mlua::Error::external(fault));
    }
    if resumes.get() >= host.limits().max_actions as u64 {
        let fault = Fault::new("ResourceLimit", "Lua coroutine resume limit exceeded");
        host.fail(fault.clone());
        resume_unwind(Box::new(Interrupted(fault)));
    }
    resumes.set(resumes.get() + 1);
    thread.resume(args)
}

pub fn run(inventory: Arc<Inventory>, host: Host) -> Result<RuntimeMetrics, Fault> {
    inventory.validate()?;
    let limits = host.limits();
    if limits.vm_bytes == 0 || limits.max_actions == 0 {
        return Err(Fault::new(
            "ResourceLimit",
            "VM memory and coroutine limits must be positive",
        ));
    }
    let lua = Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE,
        LuaOptions::default().catch_rust_panics(false),
    )
    .map_err(|error| Fault::new("Runtime", error.to_string()))?;
    lua.set_memory_limit(limits.vm_bytes)
        .map_err(|error| Fault::new("ResourceLimit", error.to_string()))?;
    if lua.used_memory() > limits.vm_bytes {
        return Err(Fault::new(
            "ResourceLimit",
            "Lua initialization exceeds memory limit",
        ));
    }
    let halted = Arc::new(AtomicBool::new(false));
    let deadline = Arc::new(AtomicU64::new(limits.duration_ms.saturating_mul(1000)));
    let control = host.control();
    let interrupt_halted = halted.clone();
    let interrupt_deadline = deadline.clone();
    lua.set_global_hook(
        HookTriggers::new().every_nth_instruction(1000),
        move |_, debug| {
            let fault = control.check().err().or_else(|| {
                if control.elapsed_us() >= interrupt_deadline.load(Ordering::Acquire) {
                    Some(Fault::new("Timeout", "Runtime stage deadline expired"))
                } else if interrupt_halted.load(Ordering::Acquire) {
                    Some(Fault::new("RuntimeTerminated", "Host failure is latched"))
                } else {
                    None
                }
            });
            if let Some(mut fault) = fault {
                fault.context =
                    json!({"module":debug.source().source.as_deref(),"line":debug.current_line()});
                resume_unwind(Box::new(Interrupted(fault)));
            }
            Ok(VmState::Continue)
        },
    )
    .map_err(|error| Fault::new("Runtime", error.to_string()))?;
    let resumes = Rc::new(Cell::new(0));
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), Fault> {
        let globals = lua.globals();
        // Base load/dofile/loadfile would reopen ambient files or accept bytecode.
        // No original function is exposed before package code starts.
        for name in [
            "load",
            "loadfile",
            "dofile",
            "require",
            "rawset",
            "print",
            "warn",
            "collectgarbage",
        ] {
            globals
                .set(name, Value::Nil)
                .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?;
        }
        let string: Table = globals
            .get("string")
            .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?;
        string
            .set("dump", Value::Nil)
            .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?;
        coroutine_library(&lua, &host, &halted, resumes.clone())
            .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?;
        let bridge_host = host.clone();
        let bridge_inventory = inventory.clone();
        let bridge_halted = halted.clone();
        let call = lua
            .create_function(move |lua, (method, args): (String, Value)| {
                let args: Json = lua.from_value(args)?;
                match bridge_host.call(&method, args) {
                    Ok(Json::Null) => Ok(Value::Nil),
                    Ok(value) => lua.to_value(&value),
                    Err(fault) => {
                        let stack = lua.traceback(None, 1).ok().map(|s| s.to_string_lossy());
                        let fault =
                            annotate(fault, &bridge_inventory, "<host-call>", &method, stack);
                        bridge_halted.store(true, Ordering::Release);
                        bridge_host.fail(fault.clone());
                        Err(mlua::Error::external(fault))
                    }
                }
            })
            .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?;
        let options = readonly(
            &lua,
            lua.to_value(&host.options())
                .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?,
        )
        .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?;
        let state = lua
            .create_table()
            .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?;
        globals
            .set(
                "host",
                lua.create_userdata(HostView {
                    options,
                    state,
                    call,
                })
                .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?,
            )
            .map_err(|error| script_fault(error, &inventory, "<host>", "setup"))?;
        let modules = Rc::new(Modules {
            inventory: inventory.clone(),
            host: host.clone(),
            halted: halted.clone(),
            compiled: RefCell::new(BTreeMap::new()),
            loaded: RefCell::new(BTreeMap::new()),
            loading: RefCell::new(Vec::new()),
        });
        for (id, source) in &inventory.sources {
            host.control().check()?;
            if !id.ends_with(".lua") {
                continue;
            }
            let environment = modules
                .environment(&lua, id)
                .map_err(|error| script_fault(error, &inventory, id, "syntax"))?;
            let function = lua
                .load(source)
                .set_name(format!("@{id}"))
                .set_mode(ChunkMode::Text)
                .set_environment(environment)
                .into_function()
                .map_err(|error| script_fault(error, &inventory, id, "syntax"))?;
            modules.compiled.borrow_mut().insert(id.clone(), function);
            for literal in literal_imports(source) {
                // Only a lexer-confirmed literal is evaluated; no package code.
                let specifier: String = lua
                    .load(format!("return {literal}"))
                    .set_mode(ChunkMode::Text)
                    .eval()
                    .map_err(|error| script_fault(error, &inventory, id, "static-import"))?;
                inventory.resolve(id, &specifier).map_err(|fault| {
                    host.fail(fault.clone());
                    fault
                })?;
            }
        }
        let mut entries = Vec::new();
        for entry in [&inventory.entries.readiness, &inventory.entries.workflow] {
            let value = modules
                .load(&entry.module)
                .map_err(|error| script_fault(error, &inventory, &entry.module, "instantiation"))?;
            let value = match value {
                Value::Table(table) => table
                    .get::<Value>(entry.function.as_str())
                    .map_err(|error| script_fault(error, &inventory, &entry.module, "entry"))?,
                _ => Value::Nil,
            };
            match value {
                Value::Function(function) => entries.push(function),
                _ => {
                    return Err(annotate(
                        Fault::new(
                            "Entry",
                            format!(
                                "{}::{} is not callable ({})",
                                entry.module,
                                entry.function,
                                value.type_name()
                            ),
                        ),
                        &inventory,
                        &entry.module,
                        "entry",
                        None,
                    ));
                }
            }
        }
        if let Some(fault) = host.failure() {
            return Err(fault);
        }
        host.begin_readiness()?;
        deadline.store(
            host.control()
                .elapsed_us()
                .saturating_add(limits.readiness_ms.saturating_mul(1000)),
            Ordering::Release,
        );
        let ready: Value = entries[0].call(()).map_err(|error| {
            script_fault(
                error,
                &inventory,
                &inventory.entries.readiness.module,
                "readiness",
            )
        })?;
        if !matches!(&ready, Value::String(text) if text.as_bytes().as_ref() == b"Ready") {
            return Err(Fault::new(
                "ReadinessContract",
                "Readiness must return the literal string Ready",
            ));
        }
        host.control().check()?;
        if host.control().elapsed_us() >= deadline.load(Ordering::Acquire) {
            return Err(Fault::new("Timeout", "Readiness deadline expired"));
        }
        if let Some(fault) = host.failure() {
            return Err(fault);
        }
        host.begin_workflow()?;
        deadline.store(limits.duration_ms.saturating_mul(1000), Ordering::Release);
        entries[1].call::<Value>(()).map_err(|error| {
            script_fault(
                error,
                &inventory,
                &inventory.entries.workflow.module,
                "workflow",
            )
        })?;
        Ok(())
    }));
    host.control().admission.store(false, Ordering::Release);
    // Resume unrelated Rust panics, never misclassify a host implementation bug.
    let result = match result {
        Ok(result) => result,
        Err(payload) => match payload.downcast::<Interrupted>() {
            Ok(interrupted) => Err(annotate(
                interrupted.0,
                &inventory,
                "<runtime>",
                "interruption",
                None,
            )),
            Err(payload) => resume_unwind(payload),
        },
    };
    if let Some(fault) = host.failure() {
        return Err(fault);
    }
    result?;
    host.control().check()?;
    Ok(RuntimeMetrics {
        vm_bytes: Some(lua.used_memory()),
        jobs_executed: resumes.get(),
        source_diagnostic: Some(
            json!({"runtime":"mlua 0.12.1 / Lua 5.4", "cycles":"rejected with import chain", "coroutine_limit":limits.max_actions,"resume_limit":limits.max_actions,"detached_coroutines":"discarded at entry return"}),
        ),
    })
}
