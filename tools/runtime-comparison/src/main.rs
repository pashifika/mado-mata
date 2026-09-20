mod check;
mod engine;
mod host;
mod inventory;
mod javascript;
mod lua;
mod model;
mod report;
mod runner;
mod scenarios;
mod typescript;

use model::{Fault, Plan};
use std::path::Path;

fn execute() -> Result<bool, Fault> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command] if command == "child" => runner::child(),
        [command] if command == "parent-probe" => runner::parent_probe(false),
        [command] if command == "parent-stop-probe" => runner::parent_probe(true),
        [command] if command == "target-probe" => {
            std::thread::sleep(std::time::Duration::from_secs(30));
            Ok(true)
        }
        [command] if command == "check" => {
            let result = check::run()?;
            let passed = result["check_passed"].as_bool() == Some(true);
            print_json(&result)?;
            Ok(passed)
        }
        [command, plan_path, package] if command == "run" => {
            let plan: Plan = runner::read_json(Path::new(plan_path), 65_536)?;
            plan.validate()?;
            let inventory = inventory::Inventory::capture(Path::new(package), &plan.limits)?;
            let result = runner::sample(&plan, &inventory)?;
            let passed = result.iter().all(|row| row.status == "PASS");
            print_json(&serde_json::json!({"version":1,"runs":result}))?;
            Ok(passed)
        }
        [command, path] if command == "report" => {
            let data = runner::read_json(Path::new(path), model::MAX_TRANSPORT_BYTES)?;
            print_json(&report::summarize(&data)?)?;
            Ok(true)
        }
        [] => {
            help();
            Ok(true)
        }
        [arg] if arg == "--help" || arg == "-h" => {
            help();
            Ok(true)
        }
        _ => Err(Fault::new(
            "Arguments",
            "usage: mado-runtime-comparison check | run PLAN PACKAGE | report RESULTS",
        )),
    }
}

fn help() {
    println!(
        "mado-runtime-comparison\n  check               Run controlled VM, host, and process scenarios\n  run PLAN PACKAGE    Execute an immutable, bounded comparison plan\n  report RESULTS      Summarize evidence without inferring native adoption\nNative operations require explicit finite authority; CI grants none."
    );
}

fn print_json(value: &serde_json::Value) -> Result<(), Fault> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)
        .map_err(|e| Fault::new("Output", e.to_string()))?;
    writeln!(stdout).map_err(|e| Fault::new("Output", e.to_string()))
}

fn main() {
    let code = match execute() {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    };
    std::process::exit(code);
}
