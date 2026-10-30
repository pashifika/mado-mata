use crate::inventory::{Entry, Inventory};
use crate::model::{Control, Fault, Limits};
use crate::owned_child::{ChildStdio, Environment, OwnedChild};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const MAX_COMPILER_BYTES: usize = 64 * 1024 * 1024;
const COMPILER_HEAP_MIB: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionCandidate {
    pub module: String,
    pub function: String,
    pub signature: String,
    pub line: u64,
    pub column: u64,
    pub required_arguments: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct FunctionInvocation {
    pub module: String,
    pub function: String,
    pub arguments: Vec<Value>,
}

struct ArgumentBudget(usize);

impl Write for ArgumentBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("arguments exceed 64 KiB"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bounded_arguments<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Value>, D::Error> {
    use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
    struct Seed<'a> {
        depth: usize,
        budget: &'a mut ArgumentBudget,
    }
    impl Seed<'_> {
        fn scalar<E: serde::de::Error>(self, value: Value) -> Result<Value, E> {
            serde_json::to_writer(self.budget, &value).map_err(E::custom)?;
            Ok(value)
        }
        fn punctuation<E: serde::de::Error>(&mut self, byte: u8) -> Result<(), E> {
            self.budget.write_all(&[byte]).map_err(E::custom)
        }
    }
    impl<'de> DeserializeSeed<'de> for Seed<'_> {
        type Value = Value;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Value, D::Error> {
            if self.depth > 32 {
                return Err(serde::de::Error::custom("argument nesting exceeds 32"));
            }
            deserializer.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Seed<'_> {
        type Value = Value;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("bounded JSON arguments")
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
            self.scalar(Value::Null)
        }
        fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
            self.scalar(value.into())
        }
        fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
            self.scalar(value.into())
        }
        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
            self.scalar(value.into())
        }
        fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
            let number = serde_json::Number::from_f64(value)
                .ok_or_else(|| E::custom("nonfinite JSON number"))?;
            self.scalar(Value::Number(number))
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
            serde_json::to_writer(self.budget, value).map_err(E::custom)?;
            Ok(Value::String(value.into()))
        }
        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
            self.scalar(Value::String(value))
        }
        fn visit_seq<A: SeqAccess<'de>>(mut self, mut sequence: A) -> Result<Value, A::Error> {
            self.punctuation(b'[')?;
            let mut values = Vec::new();
            while let Some(value) = sequence.next_element_seed(Seed {
                depth: self.depth + 1,
                budget: &mut *self.budget,
            })? {
                if self.depth == 0 && values.len() == 32 {
                    return Err(serde::de::Error::custom("at most 32 arguments are allowed"));
                }
                if !values.is_empty() {
                    self.punctuation(b',')?;
                }
                values.push(value);
            }
            self.punctuation(b']')?;
            Ok(Value::Array(values))
        }
        fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<Value, A::Error> {
            self.punctuation(b'{')?;
            let mut values = serde_json::Map::new();
            while let Some(key) = map.next_key::<String>()? {
                if !values.is_empty() {
                    self.punctuation(b',')?;
                }
                serde_json::to_writer(&mut *self.budget, &key).map_err(serde::de::Error::custom)?;
                self.punctuation(b':')?;
                let value = map.next_value_seed(Seed {
                    depth: self.depth + 1,
                    budget: &mut *self.budget,
                })?;
                values.insert(key, value);
            }
            self.punctuation(b'}')?;
            Ok(Value::Object(values))
        }
    }
    let value = deserializer.deserialize_seq(Seed {
        depth: 0,
        budget: &mut ArgumentBudget(64 * 1024),
    })?;
    match value {
        Value::Array(values) => Ok(values),
        _ => Err(serde::de::Error::custom("arguments must be a JSON array")),
    }
}

impl<'de> Deserialize<'de> for FunctionInvocation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            module: String,
            function: String,
            #[serde(default, deserialize_with = "bounded_arguments")]
            arguments: Vec<Value>,
        }
        struct InvocationVisitor;
        impl<'de> serde::de::Visitor<'de> for InvocationVisitor {
            type Value = Fields;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a function invocation object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Fields, A::Error> {
                Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))
            }
        }
        let fields = deserializer.deserialize_map(InvocationVisitor)?;
        let invocation = Self {
            module: fields.module,
            function: fields.function,
            arguments: fields.arguments,
        };
        invocation.validate().map_err(serde::de::Error::custom)?;
        Ok(invocation)
    }
}

impl FunctionInvocation {
    pub fn validate(&self) -> Result<(), Fault> {
        fn depth(value: &Value, level: usize) -> bool {
            level <= 32
                && match value {
                    Value::Array(values) => values.iter().all(|value| depth(value, level + 1)),
                    Value::Object(values) => values.values().all(|value| depth(value, level + 1)),
                    _ => true,
                }
        }
        if self.module.is_empty()
            || self.module.len() > 4096
            || self.function.is_empty()
            || self.function.len() > 4096
            || self.arguments.len() > 32
            || !self.arguments.iter().all(|value| depth(value, 1))
        {
            return Err(Fault::new(
                "FunctionArguments",
                "Function identity or arguments exceed their bounds",
            ));
        }
        serde_json::to_writer(&mut ArgumentBudget(64 * 1024), &self.arguments)
            .map_err(|_| Fault::new("FunctionArguments", "Arguments exceed 64 KiB of JSON"))?;
        Ok(())
    }
}

pub fn function_candidates(
    inventory: &Inventory,
    control: &Control,
) -> Result<Vec<FunctionCandidate>, Fault> {
    control.check()?;
    inventory.validate()?;
    if !matches!(
        inventory.metadata["runtime"].as_str(),
        Some("typescript" | "javascript")
    ) {
        return Err(Fault::new(
            "FunctionEntry",
            "Function execution requires TypeScript or JavaScript",
        ));
    }
    let result = serde_json::from_value(transport(
        &json!({"operation":"function-candidates", "sources":inventory.sources}),
        MAX_COMPILER_BYTES,
        control.deadline(),
        Some(control),
    )?)
    .map_err(|error| Fault::new("CompilerProtocol", error.to_string()))?;
    control.check()?;
    Ok(result)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedFunction {
    pub invocation: FunctionInvocation,
    pub entry: Entry,
}

#[derive(Deserialize)]
struct Import {
    from: String,
    specifier: String,
    kind: String,
    line: u64,
    column: u64,
}

#[derive(Deserialize)]
struct Inspection {
    imports: Vec<Import>,
    identity: Value,
}

#[derive(Deserialize)]
struct Compilation {
    sources: BTreeMap<String, String>,
    source_maps: BTreeMap<String, String>,
    compiler: Value,
    entry: Option<Entry>,
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    value: Option<Value>,
    fault: Option<Fault>,
}

fn worker_deadline_fault(
    deadline: Instant,
    control: Option<&Control>,
    message: &'static str,
) -> Fault {
    // Only the shared operation deadline can inherit the supervisor's Stop.
    control
        .filter(|control| control.deadline() == deadline)
        .and_then(|control| control.check().err())
        .unwrap_or_else(|| Fault::new("Timeout", message))
}

fn transport(
    request: &impl Serialize,
    limit: usize,
    deadline: Instant,
    control: Option<&Control>,
) -> Result<Value, Fault> {
    if let Some(control) = control {
        control.check()?;
    }
    let input = serde_json::to_vec(request)
        .map_err(|error| Fault::new("CompilerProtocol", error.to_string()))?;
    if input.len() >= limit {
        return Err(Fault::new(
            "CompilerLimit",
            "compiler request exceeds its byte bound",
        ));
    }
    if Instant::now() >= deadline {
        return Err(worker_deadline_fault(
            deadline,
            control,
            "compiler worker deadline expired",
        ));
    }
    if !std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/compiler/node_modules/typescript/lib/typescript.js"
    ))
    .is_file()
    {
        return Err(Fault::new(
            "Blocked",
            "application-owned TypeScript 5.9.3 is not installed",
        )
        .with_context(
            json!({"requires": "npm ci --ignore-scripts in tools/runtime-comparison/compiler"}),
        ));
    }
    #[cfg(windows)]
    let node = compiler_node()?;
    #[cfg(not(windows))]
    let node = "node";
    let mut command = Command::new(node);
    // Resolve the trusted compiler installation without inheriting Node hooks.
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    // Windows needs its platform installation directory, not NODE_OPTIONS/NODE_PATH.
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
        .arg("--no-addons")
        .arg("--disable-proto=throw")
        .arg(format!("--max-old-space-size={COMPILER_HEAP_MIB}"))
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/compiler/compile.mjs"))
        .arg("--transport-limit")
        .arg(limit.to_string())
        .arg("--deadline-ms")
        .arg(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .max(1)
                .to_string(),
        )
        .arg("--owner-pid")
        .arg(std::process::id().to_string());
    let mut child = OwnedChild::spawn(&mut command, ChildStdio::Piped, Environment::Cleared).map_err(|error| {
        Fault::new("Blocked", "application-owned TypeScript compiler could not start")
            .with_context(json!({"cause": error, "requires": "Node 24 and npm ci --ignore-scripts in compiler/"}))
    })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| Fault::new("CompilerProtocol", "missing compiler stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Fault::new("CompilerProtocol", "missing compiler stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Fault::new("CompilerProtocol", "missing compiler stderr"))?;
    let (send, receive) = mpsc::channel();
    let output_send = send.clone();
    let error_send = send.clone();
    let (keepalive, released) = mpsc::sync_channel::<()>(0);
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        let result = stdin
            .write_all(&input)
            .and_then(|()| stdin.write_all(b"\n"));
        let _ = send.send(("stdin", result.map(|()| Vec::new())));
        // EOF is a lifetime signal, not the request delimiter.
        let _ = released.recv();
    });
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = output_send.send(("stdout", result));
    });
    let error_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr.take(16_385).read_to_end(&mut bytes).map(|_| bytes);
        let _ = error_send.send(("stderr", result));
    });
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut finished = 0;
    let mut failure = None;
    let mut status = None;
    loop {
        while let Ok((stream, result)) = receive.try_recv() {
            finished += 1;
            match result {
                Ok(bytes) if bytes.len() > if stream == "stderr" { 16_384 } else { limit } => {
                    failure = Some(Fault::new(
                        "CompilerLimit",
                        format!("compiler {stream} exceeds its byte bound"),
                    ));
                }
                Ok(bytes) if stream == "stdout" => output = bytes,
                Ok(bytes) if stream == "stderr" => errors = bytes,
                Ok(_) => {}
                Err(error) => failure = Some(Fault::new("CompilerTransport", error.to_string())),
            }
        }
        match child.try_wait() {
            Ok(Some(exit)) => status = Some(exit),
            Ok(None) => {}
            Err(error) => failure = Some(Fault::new("CompilerTransport", error.to_string())),
        }
        if failure.is_none() {
            failure = control.and_then(|control| control.check().err());
        }
        if failure.is_some() || (status.is_some() && finished == 3) {
            break;
        }
        if Instant::now() >= deadline {
            failure = Some(worker_deadline_fault(
                deadline,
                control,
                "compiler worker exceeded its deadline",
            ));
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    if status.is_none() {
        if let Err(error) = child.kill() {
            failure = Some(
                Fault::new("CompilerContainment", "compiler termination failed")
                    .with_context(json!({"cause": error.to_string(), "primary": failure})),
            );
        }
        if let Err(error) = child.wait() {
            failure = Some(
                Fault::new("CompilerContainment", "compiler reaping failed")
                    .with_context(json!({"cause": error.to_string(), "primary": failure})),
            );
        }
    }
    drop(keepalive);
    for worker in [writer, reader, error_reader] {
        if worker.join().is_err() && failure.is_none() {
            failure = Some(Fault::new(
                "CompilerTransport",
                "compiler pipe worker panicked",
            ));
        }
    }
    if let Some(fault) = failure {
        return Err(fault);
    }
    let reply: Reply = serde_json::from_slice(&output).map_err(|error| {
        Fault::new(
            "CompilerProtocol",
            "compiler returned no valid bounded response",
        )
        .with_context(
            json!({"cause": error.to_string(), "stderr": String::from_utf8_lossy(&errors)}),
        )
    })?;
    if !reply.ok {
        return Err(reply
            .fault
            .unwrap_or_else(|| Fault::new("CompilerProtocol", "compiler failure has no cause")));
    }
    if !status.is_some_and(|exit| exit.success()) {
        return Err(Fault::new(
            "CompilerProtocol",
            "compiler reported success but exited unsuccessfully",
        ));
    }
    reply
        .value
        .ok_or_else(|| Fault::new("CompilerProtocol", "compiler response has no value"))
}

#[cfg(windows)]
fn compiler_node() -> Result<std::path::PathBuf, Fault> {
    if let Some(path) = std::env::var_os("MADO_COMPILER_NODE") {
        let path = std::path::PathBuf::from(path);
        if !path.is_absolute()
            || !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return Err(Fault::new(
                "Blocked",
                "The owned compiler requires an absolute Node executable",
            ));
        }
        return path
            .canonicalize()
            .and_then(|path| {
                if path.is_file() {
                    Ok(path)
                } else {
                    Err(std::io::Error::other(
                        "Node executable is not a regular file",
                    ))
                }
            })
            .map_err(|error| Fault::new("Blocked", error.to_string()));
    }
    node_from_path()
}

#[cfg(windows)]
pub(crate) fn node_from_path() -> Result<std::path::PathBuf, Fault> {
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path).filter(|directory| directory.is_absolute()) {
            let node = directory.join("node.exe");
            match std::fs::metadata(&node) {
                Ok(metadata) if metadata.is_file() => {
                    return node
                        .canonicalize()
                        .map_err(|error| Fault::new("Blocked", error.to_string()));
                }
                Ok(_) => {
                    return Err(Fault::new(
                        "Blocked",
                        "Node executable is not a regular file",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(Fault::new("Blocked", error.to_string())),
            }
        }
    }
    Err(Fault::new(
        "Blocked",
        "Node 24.22.2 node.exe is required on an absolute PATH entry",
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedModules {
    inventory_identity: String,
    parser: Value,
    pub(crate) function: Option<PreparedFunction>,
}

impl PreparedModules {
    pub(crate) fn capture(
        inventory: &Arc<Inventory>,
        limits: &Limits,
        control: &Arc<Control>,
    ) -> Result<Self, Fault> {
        let parser = validate_javascript_modules(inventory, limits, control)?;
        Ok(Self {
            inventory_identity: inventory.identity.clone(),
            parser,
            function: None,
        })
    }

    pub(crate) fn capture_function(
        inventory: &Inventory,
        invocation: &FunctionInvocation,
        limits: &Limits,
        control: &Arc<Control>,
    ) -> Result<(Inventory, Self), Fault> {
        invocation.validate()?;
        inventory.validate()?;
        let (compiled, entry) = match inventory.metadata["runtime"].as_str() {
            Some("typescript") => compile_invocation(inventory, limits, control, Some(invocation))?,
            Some("javascript") => {
                #[derive(Deserialize)]
                struct Bridge {
                    source: String,
                    entry: Entry,
                }
                let bridge: Bridge = serde_json::from_value(transport(
                    &json!({"operation":"bridge-javascript", "sources":inventory.sources, "function":invocation}),
                    MAX_COMPILER_BYTES, control.deadline(), Some(control),
                )?).map_err(|error| Fault::new("CompilerProtocol", error.to_string()))?;
                let mut compiled = inventory.clone();
                compiled.metadata["original_inventory"] = json!(inventory.identity);
                compiled
                    .sources
                    .insert(invocation.module.clone(), bridge.source);
                compiled.refresh_identity()?;
                (compiled, Some(bridge.entry))
            }
            _ => {
                return Err(Fault::new(
                    "FunctionEntry",
                    "Function execution requires TypeScript or JavaScript",
                ));
            }
        };
        let compiled = Arc::new(compiled);
        let mut modules = Self::capture(&compiled, limits, control)?;
        modules.function = Some(PreparedFunction {
            invocation: invocation.clone(),
            entry: entry
                .ok_or_else(|| Fault::new("CompilerProtocol", "Missing function bridge"))?,
        });
        let compiled = Arc::try_unwrap(compiled)
            .map_err(|_| Fault::new("Runtime", "Function preflight retained inventory"))?;
        Ok((compiled, modules))
    }

    pub(crate) fn parser(&self, inventory: &Inventory) -> Result<&Value, Fault> {
        if inventory.identity != self.inventory_identity
            || inventory.metadata["runtime"] != "javascript"
        {
            return Err(Fault::new(
                "StaleIdentity",
                "prepared modules do not match the captured executable inventory",
            ));
        }
        Ok(&self.parser)
    }
}

/// Inspect JavaScript with the pinned parser without type checking or emitting.
/// The worker shares the attempt's remaining deadline and cancellation latch.
pub(crate) fn validate_javascript_imports(
    inventory: &Arc<Inventory>,
    limits: &Limits,
    control: &Arc<Control>,
) -> Result<Value, Fault> {
    inspect_javascript(inventory, limits, control, false)
}

/// Check target syntax and link the requested executable graph without evaluation.
pub(crate) fn validate_javascript_modules(
    inventory: &Arc<Inventory>,
    limits: &Limits,
    control: &Arc<Control>,
) -> Result<Value, Fault> {
    inspect_javascript(inventory, limits, control, true)
}

fn inspect_javascript(
    inventory: &Arc<Inventory>,
    limits: &Limits,
    control: &Arc<Control>,
    link: bool,
) -> Result<Value, Fault> {
    control.check()?;
    let deadline = control.deadline();
    let limit = limits
        .snapshot_bytes
        .saturating_mul(8)
        .saturating_add(1024 * 1024)
        .min(MAX_COMPILER_BYTES);
    // Capture candidate declaration edges only for desktop linking. The runtime
    // import-path preflight retains its existing handling of declaration sources.
    let mut request = json!({
        "operation": "inspect-javascript", "sources": inventory.sources,
        "include_declaration_modules": link,
        "entries": inventory.entries,
    });
    let inspection: Inspection =
        serde_json::from_value(transport(&request, limit, deadline, Some(control))?)
            .map_err(|error| Fault::new("CompilerProtocol", error.to_string()))?;
    control.check()?;
    let resolve = |import: &Import| {
        control.check()?;
        inventory
            .resolve(&import.from, &import.specifier)
            .map_err(|mut fault| {
                fault.context["module"] = json!(import.from);
                fault.context["line"] = json!(import.line);
                fault.context["column"] = json!(import.column);
                fault.context["kind"] = json!(import.kind);
                fault.context["parser"] = inspection.identity.clone();
                fault
            })
    };
    if link {
        let mut imports: BTreeMap<&str, Vec<&Import>> = BTreeMap::new();
        for import in &inspection.imports {
            imports.entry(&import.from).or_default().push(import);
        }
        let mut pending: Vec<String> = inventory
            .sources
            .keys()
            .filter(|id| id.ends_with(".js"))
            .cloned()
            .collect();
        let mut resolutions: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        while let Some(module) = pending.pop() {
            control.check()?;
            if resolutions.contains_key(&module) {
                continue;
            }
            let mut edges = BTreeMap::new();
            if let Some(imports) = imports.get(module.as_str()) {
                for import in imports {
                    let destination = resolve(import)?;
                    pending.push(destination.clone());
                    edges.insert(import.specifier.clone(), destination);
                }
            }
            // Empty rows also mark visited leaves. Each captured module is
            // expanded once, so re-exports and cycles cannot grow the traversal.
            resolutions.insert(module, edges);
        }
        // QuickJS remains the syntax authority, including declaration-suffixed
        // modules requested through literal import(). Neither engine evaluates.
        crate::javascript::validate_syntax(
            inventory.clone(),
            limits,
            control.clone(),
            resolutions.keys().map(String::as_str),
        )?;
        request["operation"] = json!("link-javascript");
        request["resolutions"] = serde_json::to_value(resolutions)
            .map_err(|error| Fault::new("CompilerProtocol", error.to_string()))?;
        request["compiler_identity"] = inspection.identity.clone();
        transport(&request, limit, deadline, Some(control)).map_err(|mut fault| {
            fault.context["inventory"] = json!(inventory.identity);
            fault.context["catalog"] = inventory.metadata["catalog"].clone();
            fault
        })?;
        control.check()?;
    } else {
        for import in &inspection.imports {
            resolve(import)?;
        }
    }
    Ok(json!({
        "identity": inspection.identity,
        "transport_bytes": limit,
        "heap_mib": COMPILER_HEAP_MIB,
        "deadline": "remaining-attempt",
    }))
}

/// Compile only the immutable inventory, before creating the JavaScript VM.
pub fn compile(inventory: &Inventory, limits: &Limits) -> Result<Inventory, Fault> {
    limits.validate()?;
    compile_with_control(inventory, limits, &Control::new(limits))
}

/// Compile within an already reserved operation's cancellation and deadline.
pub(crate) fn compile_with_control(
    inventory: &Inventory,
    limits: &Limits,
    control: &Control,
) -> Result<Inventory, Fault> {
    compile_invocation(inventory, limits, control, None).map(|(inventory, _)| inventory)
}

fn compile_invocation(
    inventory: &Inventory,
    limits: &Limits,
    control: &Control,
    function: Option<&FunctionInvocation>,
) -> Result<(Inventory, Option<Entry>), Fault> {
    control.check()?;
    inventory.validate()?;
    limits.validate()?;
    let deadline = control.deadline();
    let limit = limits
        .snapshot_bytes
        .saturating_mul(8)
        .saturating_add(1024 * 1024)
        .min(MAX_COMPILER_BYTES);
    let mut request = json!({
        "operation": "inspect", "sources": inventory.sources,
        "schema": inventory.schema, "metadata": inventory.metadata,
    });
    request["function"] = json!(function);
    let attribute = |mut fault: Fault| {
        if !fault.context.is_object() {
            fault.context = json!({"cause": fault.context});
        }
        fault.context["inventory"] = json!(inventory.identity);
        fault.context["catalog"] = inventory.metadata["catalog"].clone();
        fault.context["stage"] = json!("compilation");
        fault
    };
    let inspection: Inspection = serde_json::from_value(
        transport(&request, limit, deadline, Some(control)).map_err(&attribute)?,
    )
    .map_err(|error| attribute(Fault::new("CompilerProtocol", error.to_string())))?;
    let mut resolutions: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for import in inspection.imports {
        control.check()?;
        let destination =
            inventory
                .resolve(&import.from, &import.specifier)
                .map_err(|mut fault| {
                    fault.context = json!({
                        "inventory": inventory.identity, "module": import.from,
                        "line": import.line, "column": import.column, "specifier": import.specifier,
                        "kind": import.kind, "cause": fault.context,
                    });
                    attribute(fault)
                })?;
        resolutions
            .entry(import.from)
            .or_default()
            .insert(import.specifier, destination);
    }
    request["operation"] = json!("compile");
    request["resolutions"] = serde_json::to_value(resolutions)
        .map_err(|error| attribute(Fault::new("CompilerProtocol", error.to_string())))?;
    request["compiler_identity"] = inspection.identity;
    let compilation: Compilation = serde_json::from_value(
        transport(&request, limit, deadline, Some(control)).map_err(&attribute)?,
    )
    .map_err(|error| attribute(Fault::new("CompilerProtocol", error.to_string())))?;
    let mut compiled = inventory.clone();
    compiled.metadata["original_sources"] = serde_json::to_value(&inventory.sources)
        .map_err(|error| attribute(Fault::new("CompilerProtocol", error.to_string())))?;
    compiled.metadata["original_source_maps"] = serde_json::to_value(&inventory.source_maps)
        .map_err(|error| attribute(Fault::new("CompilerProtocol", error.to_string())))?;
    compiled.metadata["original_inventory"] = json!(inventory.identity);
    compiled.metadata["runtime"] = json!("javascript");
    compiled.metadata["compiler"] = compilation.compiler;
    compiled.metadata["compiler"]["transport_bytes"] = json!(limit);
    compiled.metadata["compiler"]["heap_mib"] = json!(COMPILER_HEAP_MIB);
    compiled.metadata["compiler"]["duration_ms"] = json!(limits.duration_ms);
    compiled
        .sources
        .retain(|id, _| !id.ends_with(".ts") || id.ends_with(".d.ts"));
    compiled.sources.extend(compilation.sources);
    compiled
        .source_maps
        .retain(|id, _| compiled.sources.contains_key(id));
    compiled.source_maps.extend(compilation.source_maps);
    for entry in [
        &mut compiled.entries.readiness,
        &mut compiled.entries.workflow,
    ] {
        if let Some(stem) = entry.module.strip_suffix(".ts") {
            entry.module = format!("{stem}.js");
        }
    }
    control.check()?;
    compiled.refresh_identity()?;
    Ok((compiled, compilation.entry))
}

fn vlq(segment: &str) -> Option<Vec<i64>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = Vec::with_capacity(5);
    let mut value = 0_u64;
    let mut shift = 0_u32;
    for byte in segment.bytes() {
        let digit = ALPHABET.iter().position(|candidate| *candidate == byte)? as u64;
        if shift >= 60 {
            return None;
        }
        value |= (digit & 31) << shift;
        if digit & 32 == 0 {
            let magnitude = i64::try_from(value >> 1).ok()?;
            result.push(if value & 1 == 0 {
                magnitude
            } else {
                -magnitude
            });
            value = 0;
            shift = 0;
        } else {
            shift += 5;
        }
    }
    (shift == 0).then_some(result)
}

fn original_location(
    inventory: &Inventory,
    module: &str,
    line: u64,
    column: Option<u64>,
) -> Option<Value> {
    let map: Value = serde_json::from_str(inventory.source_maps.get(module)?).ok()?;
    if map["version"].as_u64()? != 3
        || map["sourceRoot"]
            .as_str()
            .is_some_and(|root| !root.is_empty())
    {
        return None;
    }
    let mappings = map["mappings"].as_str()?;
    let mut source = 0_i64;
    let mut source_line = 0_i64;
    let mut source_column = 0_i64;
    let mut selected = None;
    let target_line = line.checked_sub(1)?;
    let generated_text = inventory
        .sources
        .get(module)?
        .lines()
        .nth(usize::try_from(target_line).ok()?)?;
    if column.is_some_and(|column| {
        column == 0 || column > generated_text.encode_utf16().count() as u64 + 1
    }) {
        return None;
    }
    for (generated_line, row) in mappings.split(';').enumerate() {
        if generated_line as u64 > target_line {
            break;
        }
        let mut generated_column = 0_i64;
        for segment in row.split(',').filter(|segment| !segment.is_empty()) {
            let fields = vlq(segment)?;
            if !matches!(fields.len(), 1 | 4 | 5) {
                return None;
            }
            generated_column = generated_column.checked_add(fields[0])?;
            if fields[0] < 0 {
                return None;
            }
            if fields.len() >= 4 {
                source = source.checked_add(fields[1])?;
                source_line = source_line.checked_add(fields[2])?;
                source_column = source_column.checked_add(fields[3])?;
            }
            if generated_line as u64 == target_line {
                if column.is_none() && fields.len() == 1 {
                    // The reported line may point into an unmapped span.
                    return None;
                }
                if let Some(column) = column {
                    if generated_column > i64::try_from(column.checked_sub(1)?).ok()? {
                        break;
                    }
                    selected = (fields.len() >= 4).then_some((source, source_line, source_column));
                } else if fields.len() >= 4 && selected.is_none() {
                    // Without a generated column, only a line attribution is justified.
                    selected = Some((source, source_line, -1));
                } else if fields.len() >= 4
                    && selected.is_some_and(|(file, row, _)| file != source || row != source_line)
                {
                    return None;
                }
            }
        }
    }
    let (source, source_line, source_column) = selected?;
    let source_id = map["sources"]
        .get(usize::try_from(source).ok()?)?
        .as_str()?;
    let captured = inventory.metadata["original_sources"][source_id].as_str()?;
    if map["sourcesContent"]
        .get(usize::try_from(source).ok()?)?
        .as_str()?
        != captured
    {
        return None;
    }
    let line = u64::try_from(source_line).ok()?.checked_add(1)?;
    let column = u64::try_from(source_column)
        .ok()
        .and_then(|column| column.checked_add(1));
    let source_text = captured.lines().nth(usize::try_from(source_line).ok()?)?;
    if column.is_some_and(|column| column > source_text.encode_utf16().count() as u64 + 1) {
        return None;
    }
    let mut original =
        json!({"module": source_id, "line": line, "column": column, "function": Value::Null});
    if let Some(functions) = inventory.metadata["compiler"]["functions"][source_id].as_array() {
        let mut matches = 0;
        for function in functions {
            let start = function["start_line"].as_u64()?;
            let end = function["end_line"].as_u64()?;
            if line >= start
                && line <= end
                && column.is_none_or(|column| {
                    (line != start || column >= function["start_column"].as_u64().unwrap_or(1))
                        && (line != end
                            || column <= function["end_column"].as_u64().unwrap_or(u64::MAX))
                })
            {
                original["function"] = function["name"].clone();
                matches += 1;
            }
        }
        if column.is_none() && matches != 1 {
            original["function"] = Value::Null;
        }
    }
    Some(original)
}

fn mapped_frame(inventory: &Inventory, module: &str, line: u64, column: Option<u64>) -> Value {
    json!({
        "generated": {"module": module, "line": line, "column": column},
        "original": original_location(inventory, module, line, column),
    })
}

/// Enrich attribution without changing the typed primary fault or its native cause.
pub fn map_fault(inventory: &Inventory, mut fault: Fault) -> Fault {
    // Compiler/preflight diagnostics already name original inputs. There is no
    // generated inventory to map if compilation never completed.
    if inventory.metadata["original_inventory"].as_str().is_none() {
        return fault;
    }
    let mut frames = Vec::new();
    if let (Some(module), Some(line)) = (
        fault.context["module"].as_str(),
        fault.context["line"].as_u64(),
    ) {
        frames.push(mapped_frame(
            inventory,
            module,
            line,
            fault.context["column"].as_u64(),
        ));
    }
    if let Some(stack) = fault.context["stack"].as_str() {
        for row in stack.lines().take(128) {
            if let Some(frame) = crate::javascript::source_frame(inventory, row) {
                frames.push(mapped_frame(
                    inventory,
                    frame["module"].as_str().expect("parsed module"),
                    frame["line"].as_u64().expect("parsed line"),
                    frame["column"].as_u64(),
                ));
            }
        }
    }
    if !fault.context.is_object() {
        fault.context = json!({"cause": fault.context});
    }
    fault.context["typescript"] = json!({
        "inventory": inventory.identity,
        "original_inventory": inventory.metadata["original_inventory"],
        "compiler": inventory.metadata["compiler"]["version"],
        "compiler_sha256": inventory.metadata["compiler"]["compiler_sha256"],
        "mapping": if frames.iter().any(|frame| !frame["original"].is_null()) { "available" } else { "unavailable" },
        "frames": frames,
    });
    fault
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Plan;

    struct StopDuringEncoding<'a>(&'a Control);

    impl Serialize for StopDuringEncoding<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            assert!(Instant::now() < self.0.deadline());
            self.0.cancel();
            // Expire the real deadline during encoding, before any worker spawn.
            thread::sleep(self.0.deadline().saturating_duration_since(Instant::now()));
            while Instant::now() < self.0.deadline() {
                std::hint::spin_loop();
            }
            serializer.serialize_unit()
        }
    }

    #[test]
    fn stop_during_encoding_wins_over_the_shared_worker_deadline() {
        let plan: Plan =
            serde_json::from_str(include_str!("../fixtures/manual-plan.json")).unwrap();
        let control = Control::with_deadline(&plan.limits, Instant::now() + Duration::from_secs(5));
        let fault = transport(
            &StopDuringEncoding(&control),
            64,
            control.deadline(),
            Some(&control),
        )
        .unwrap_err();
        assert_eq!(fault.category, "Cancelled");
        assert_eq!(control.admit_launch().unwrap_err().category, "Cancelled");
    }
}

#[cfg(test)]
mod function_tests;
