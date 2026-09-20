//! QuickJS ES modules: native cycles/live bindings, attempt-local instances.
//! ECMAScript basics (including eval/Function) have the same inventory resolver.
//! No filesystem, network, Node, native loaders or timers are installed.
//! Options are recursively frozen; strict-mode mutation throws. max_actions also
//! bounds promise creation and executed jobs; detached jobs die with the VM.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::promise::PromiseHookType;
use rquickjs::{Context, Ctx, Exception, Function, Module, Object, Persistent, Runtime, Value};
use serde_json::{Value as Json, json};

use crate::host::Host;
use crate::inventory::{Entry, Inventory};
use crate::model::{Fault, RuntimeMetrics};

#[derive(Clone)]
struct Modules {
    inventory: Arc<Inventory>,
    host: Host,
    halted: Arc<AtomicBool>,
    compiled: Rc<RefCell<BTreeSet<String>>>,
}

impl Modules {
    fn refuse(&self, ctx: &Ctx<'_>, fault: Fault) -> rquickjs::Error {
        self.halted.store(true, Ordering::Release);
        let module = fault.context["requester"]
            .as_str()
            .unwrap_or("<loader>")
            .to_owned();
        let fault = annotate(fault, &self.inventory, &module, "loading", None);
        self.host.fail(fault.clone());
        match Exception::from_message(ctx.clone(), &fault.message) {
            Ok(exception) => {
                self.host.set_failure_stack(exception.stack());
                exception.throw()
            }
            Err(error) => error,
        }
    }
}

impl Resolver for Modules {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if attributes
            .as_ref()
            .is_some_and(|a| a.keys().next().is_some())
        {
            return Err(self.refuse(
                ctx,
                Fault::new("ImportRefused", "Import attributes are not supported")
                    .with_context(json!({"module":base,"specifier":name})),
            ));
        }
        self.inventory
            .resolve(base, name)
            .map_err(|fault| self.refuse(ctx, fault))
    }
}

impl Loader for Modules {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js>> {
        let source = self
            .inventory
            .source(name)
            .map_err(|fault| self.refuse(ctx, fault))?;
        self.compiled.borrow_mut().insert(name.to_owned());
        Module::declare(ctx.clone(), name, source)
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
    fault.context["source_mapping"] = json!("unavailable; generated locations retained");
    fault.context["catalog"] = inventory.metadata["catalog"].clone();
    fault
}

fn exception_fault(
    ctx: &Ctx<'_>,
    error: rquickjs::Error,
    inventory: &Inventory,
    module: &str,
    stage: &str,
) -> Fault {
    let allocation = matches!(error, rquickjs::Error::Allocation);
    let (message, stack) = if error.is_exception() {
        let value = ctx.catch();
        if let Some(exception) = value.as_exception() {
            (
                exception
                    .message()
                    .unwrap_or_else(|| "JavaScript exception".into()),
                exception.stack(),
            )
        } else if let Some(text) = value.as_string() {
            (
                text.to_string()
                    .unwrap_or_else(|_| "Unprintable thrown string".into()),
                None,
            )
        } else {
            (
                format!("Thrown JavaScript value of type {:?}", value.type_of()),
                None,
            )
        }
    } else {
        (error.to_string(), None)
    };
    let category = if allocation || message == "out of memory" {
        "ResourceLimit"
    } else if stage == "syntax" {
        "Syntax"
    } else {
        "Script"
    };
    annotate(
        Fault::new(category, message),
        inventory,
        module,
        stage,
        stack,
    )
}

fn check(host: &Host, deadline: &AtomicU64) -> Result<(), Fault> {
    if let Some(fault) = host.failure() {
        return Err(fault);
    }
    host.control().check()?;
    if host.control().elapsed_us() >= deadline.load(Ordering::Acquire) {
        let fault = Fault::new("Timeout", "Runtime stage deadline expired");
        host.fail(fault.clone());
        return Err(fault);
    }
    Ok(())
}

fn settle<'js>(
    ctx: &Ctx<'js>,
    mut value: Value<'js>,
    host: &Host,
    deadline: &AtomicU64,
    jobs: &mut u64,
) -> Result<Value<'js>, Fault> {
    loop {
        check(host, deadline)?;
        let Some(promise) = value.as_promise() else {
            return Ok(value);
        };
        if let Some(result) = promise.result::<Value>() {
            value = result.map_err(|error| Fault::new("Script", error.to_string()))?;
            continue;
        }
        if *jobs >= host.limits().max_actions as u64 {
            return Err(Fault::new(
                "ResourceLimit",
                "JavaScript job execution limit exceeded",
            ));
        }
        // No native queueMicrotask remains: every scheduled callback settles a
        // tracked promise. Ctx's API drains raw job errors, so inspect the host
        // latch immediately and inspect rejected promises at the boundary.
        if !ctx.execute_pending_job() {
            check(host, deadline)?;
            return Err(Fault::new(
                "PendingWork",
                "Entry promise cannot settle: no runnable inventory job",
            ));
        }
        *jobs += 1;
    }
}

fn callable<'js>(
    ctx: &Ctx<'js>,
    module: &Object<'js>,
    entry: &Entry,
    inventory: &Inventory,
) -> Result<Function<'js>, Fault> {
    let value: Value = module
        .get(entry.function.as_str())
        .map_err(|error| exception_fault(ctx, error, inventory, &entry.module, "entry"))?;
    value.clone().into_function().ok_or_else(|| {
        annotate(
            Fault::new(
                "Entry",
                format!(
                    "{}::{} is not callable ({:?})",
                    entry.module,
                    entry.function,
                    value.type_of()
                ),
            ),
            inventory,
            &entry.module,
            "entry",
            None,
        )
    })
}

fn host_call<'js>(
    ctx: Ctx<'js>,
    host: Host,
    inventory: Arc<Inventory>,
    halted: Arc<AtomicBool>,
) -> rquickjs::Result<Function<'js>> {
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, method: String, args: Value<'js>| {
            let args = ctx
                .json_stringify(args)?
                .ok_or_else(|| Exception::throw_type(&ctx, "Host arguments must be JSON"))?
                .to_string()?;
            let args: Json = serde_json::from_str(&args)
                .map_err(|error| Exception::throw_type(&ctx, &error.to_string()))?;
            match host.call(&method, args) {
                Ok(value) => ctx.json_parse(value.to_string()),
                Err(fault) => {
                    let fault = annotate(fault, &inventory, "<host-call>", &method, None);
                    halted.store(true, Ordering::Release);
                    host.fail(fault.clone());
                    let exception = Exception::from_message(ctx.clone(), &fault.message)?;
                    host.set_failure_stack(exception.stack());
                    exception
                        .as_object()
                        .set("category", fault.category.as_str())?;
                    exception
                        .as_object()
                        .set("context", ctx.json_parse(fault.context.to_string())?)?;
                    Err(exception.throw())
                }
            }
        },
    )
}

pub fn run(inventory: Arc<Inventory>, host: Host) -> Result<RuntimeMetrics, Fault> {
    inventory.validate()?;
    let limits = host.limits();
    if limits.vm_bytes == 0 || limits.max_actions == 0 {
        return Err(Fault::new(
            "ResourceLimit",
            "VM memory and job limits must be positive",
        ));
    }
    let runtime = Runtime::new().map_err(|error| Fault::new("Runtime", error.to_string()))?;
    // Requires the default QuickJS allocator, not rquickjs's rust-alloc feature.
    runtime.set_memory_limit(limits.vm_bytes);
    runtime.set_max_stack_size(256 * 1024);
    let halted = Arc::new(AtomicBool::new(false));
    let deadline = Arc::new(AtomicU64::new(limits.duration_ms.saturating_mul(1000)));
    let control = host.control();
    let interrupt_halted = halted.clone();
    let interrupt_deadline = deadline.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        interrupt_halted.load(Ordering::Acquire)
            || control.check().is_err()
            || control.elapsed_us() >= interrupt_deadline.load(Ordering::Acquire)
    })));
    let mut modules = Modules {
        inventory: inventory.clone(),
        host: host.clone(),
        halted: halted.clone(),
        compiled: Rc::new(RefCell::new(BTreeSet::new())),
    };
    runtime.set_loader(modules.clone(), modules.clone());
    let promise_count = Arc::new(AtomicUsize::new(0));
    let count = promise_count.clone();
    let promise_host = host.clone();
    let promise_halted = halted.clone();
    runtime.set_promise_hook(Some(Box::new(move |_, event, _, _| {
        if event == PromiseHookType::Init
            && count.fetch_add(1, Ordering::Relaxed) >= limits.max_actions
        {
            promise_halted.store(true, Ordering::Release);
            promise_host.fail(Fault::new(
                "ResourceLimit",
                "JavaScript promise creation limit exceeded",
            ));
        }
    })));
    let rejections = Rc::new(RefCell::new(
        Vec::<(Persistent<Value<'static>>, Fault)>::new(),
    ));
    let rejected = rejections.clone();
    let rejection_inventory = inventory.clone();
    let rejection_host = host.clone();
    runtime.set_host_promise_rejection_tracker(Some(Box::new(
        move |ctx, promise, reason, handled| {
            // Retain exact identities: an unreferenced rejected promise can otherwise
            // be freed and its address reused before a later handler notification.
            let promise = Persistent::save(&ctx, promise);
            if handled {
                rejected
                    .borrow_mut()
                    .retain(|(identity, _)| identity != &promise);
            } else if rejected.borrow().len() < limits.max_actions {
                let (message, stack) = reason
                    .as_exception()
                    .map(|e| {
                        (
                            e.message().unwrap_or_else(|| "Rejected promise".into()),
                            e.stack(),
                        )
                    })
                    .unwrap_or_else(|| (format!("Rejected promise: {:?}", reason.type_of()), None));
                let fault = annotate(
                    Fault::new("Script", message),
                    &rejection_inventory,
                    "<promise>",
                    "job",
                    stack,
                );
                rejected.borrow_mut().push((promise, fault));
            } else {
                rejection_host.fail(Fault::new(
                    "ResourceLimit",
                    "unhandled rejection capacity exceeded",
                ));
            }
        },
    )));
    let context =
        Context::full(&runtime).map_err(|error| Fault::new("Runtime", error.to_string()))?;
    let mut jobs = 0;
    let result = context.with(|ctx| {
        let call = host_call(ctx.clone(), host.clone(), inventory.clone(), halted.clone())
            .map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?;
        let host_object = Object::new(ctx.clone()).map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?;
        host_object.set("call", call).map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?;
        host_object.set("options", ctx.json_parse(host.options().to_string()).map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?)
            .map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?;
        host_object.set("state", Object::new(ctx.clone()).map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?)
            .map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?;
        ctx.globals().prop("host", host_object).map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?;
        ctx.eval::<(), _>(r#"
            (() => {
                function freeze(value) {
                    if (value !== null && typeof value === 'object') {
                        for (const key of Object.keys(value)) freeze(value[key]);
                        Object.freeze(value);
                    }
                    return value;
                }
                freeze(host.options);
                Object.freeze(host);
                const resolve = Promise.resolve.bind(Promise);
                const then = Function.call.bind(Promise.prototype.then);
                Object.defineProperty(globalThis, 'queueMicrotask', {
                    value: callback => { then(resolve(), callback); }, writable: false, configurable: false
                });
            })();
        "#).map_err(|error| exception_fault(&ctx, error, &inventory, "<host>", "setup"))?;

        // QuickJS resolves the static closure even in COMPILE_ONLY mode.
        // Track recursive loader calls so preflight never declares a second
        // module instance under the same normalized identity.
        for name in inventory.sources.keys() {
            check(&host, &deadline)?;
            if name.ends_with(".js") && !modules.compiled.borrow().contains(name) {
                modules.load(&ctx, name, None)
                    .map_err(|error| exception_fault(&ctx, error, &inventory, name, "syntax"))?;
            }
        }
        let mut entries = Vec::new();
        for entry in [&inventory.entries.readiness, &inventory.entries.workflow] {
            let promise = Module::import(&ctx, entry.module.as_str())
                .map_err(|error| exception_fault(&ctx, error, &inventory, &entry.module, "instantiation"))?;
            let namespace = settle(&ctx, promise.into_value(), &host, &deadline, &mut jobs).map_err(|fault| {
                if ctx.has_exception() { exception_fault(&ctx, rquickjs::Error::Exception, &inventory, &entry.module, "instantiation") } else { fault }
            })?;
            let namespace = namespace.into_object().ok_or_else(|| Fault::new("Entry", "Entry module has no namespace"))?;
            entries.push(callable(&ctx, &namespace, entry, &inventory)?);
        }
        check(&host, &deadline)?;
        host.begin_readiness()?;
        deadline.store(host.control().elapsed_us().saturating_add(limits.readiness_ms.saturating_mul(1000)), Ordering::Release);
        let ready: Value = entries[0].call(()).map_err(|error| exception_fault(&ctx, error, &inventory, &inventory.entries.readiness.module, "readiness"))?;
        let ready = settle(&ctx, ready, &host, &deadline, &mut jobs).map_err(|fault| {
            if ctx.has_exception() { exception_fault(&ctx, rquickjs::Error::Exception, &inventory, &inventory.entries.readiness.module, "readiness") } else { fault }
        })?;
        if ready.as_string().and_then(|value| value.to_string().ok()).as_deref() != Some("Ready") {
            return Err(Fault::new("ReadinessContract", "Readiness must return the literal string Ready"));
        }
        check(&host, &deadline)?;
        host.begin_workflow()?;
        deadline.store(limits.duration_ms.saturating_mul(1000), Ordering::Release);
        let value: Value = entries[1].call(()).map_err(|error| exception_fault(&ctx, error, &inventory, &inventory.entries.workflow.module, "workflow"))?;
        settle(&ctx, value, &host, &deadline, &mut jobs).map_err(|fault| {
            if ctx.has_exception() { exception_fault(&ctx, rquickjs::Error::Exception, &inventory, &inventory.entries.workflow.module, "workflow") } else { fault }
        })?;
        // Entry settlement, not draining the job queue, ends ordinary authority.
        host.control().admission.store(false, Ordering::Release);
        check(&host, &deadline)?;
        if let Some((_, fault)) = rejections.borrow().first() {
            return Err(fault.clone());
        }
        Ok(())
    });
    runtime.set_host_promise_rejection_tracker(None);
    rejections.borrow_mut().clear();
    host.control().admission.store(false, Ordering::Release);
    if let Some(fault) = host.failure() {
        return Err(fault);
    }
    host.control().check()?;
    if host.control().elapsed_us() >= deadline.load(Ordering::Acquire) {
        return Err(Fault::new("Timeout", "Runtime stage deadline expired"));
    }
    result?;
    Ok(RuntimeMetrics {
        vm_bytes: usize::try_from(runtime.memory_usage().memory_used_size).ok(),
        jobs_executed: jobs,
        source_diagnostic: Some(
            json!({"runtime":"rquickjs 0.14.0", "cycles":"ECMAScript live bindings", "detached_jobs":"discarded at entry settlement", "promise_limit":limits.max_actions, "job_limit":limits.max_actions}),
        ),
    })
}
