import { Worker } from "node:worker_threads";

const HARD_LIMIT = 64 * 1024 * 1024;
const args = process.argv.slice(2);
const selfCheck = args.length === 1 && args[0] === "--self-check";
const fault = (category, message, context = {}) => ({ ok: false, fault: { category, message, context } });
let worker;
let deadlineTimer;
let parentTimer;
let stopping = false;
let completed = false;

// This thread never loads TypeScript or evaluates package source. It remains able
// to stop a compiler worker even while synchronous parsing/type checking is busy.
function abort(category, message, context = {}) {
  if (stopping) return;
  stopping = true;
  void worker?.terminate();
  const output = JSON.stringify(fault(category, message, context)) + "\n";
  process.stdout.write(output, () => process.exit(1));
  setTimeout(() => process.exit(1), 25);
}
process.stdout.on("error", () => process.exit(1));

try {
  if (!selfCheck && (args.length !== 6 || args[0] !== "--transport-limit" || args[2] !== "--deadline-ms" || args[4] !== "--owner-pid")) {
    throw new Error("Use --transport-limit <bytes> --deadline-ms <milliseconds> --owner-pid <pid> or --self-check");
  }
  const limit = selfCheck ? HARD_LIMIT : Number(args[1]);
  const duration = selfCheck ? 30_000 : Number(args[3]);
  if (!Number.isSafeInteger(limit) || limit < 1024 || limit > HARD_LIMIT
      || !Number.isSafeInteger(duration) || duration < 1 || duration > 3_600_000) {
    throw new Error("Invalid compiler transport/deadline bound");
  }
  const parent = selfCheck ? process.ppid : Number(args[5]);
  if (!Number.isSafeInteger(parent) || parent <= 1 || process.ppid !== parent) {
    throw Object.assign(new Error("Compiler owner identity is unavailable or already lost"), { code: "COMPILER_OWNER_LOST" });
  }
  deadlineTimer = setTimeout(() => abort("Timeout", "TypeScript compiler watchdog deadline expired"), duration);
  parentTimer = setInterval(() => {
    if (process.ppid !== parent) { abort("CompilerContainment", "Compiler owner exited"); return; }
    try { process.kill(parent, 0); } catch (error) {
      if (error.code === "EPERM") return; // Exists, but this identity cannot signal it.
      abort("CompilerContainment", error.code === "ESRCH" ? "Compiler owner exited" : "Compiler owner liveness is unavailable", { code: error.code });
    }
  }, 25);
  let input;
  if (!selfCheck) {
    input = await new Promise((resolve, reject) => {
      const chunks = [];
      let bytes = 0;
      let received = false;
      process.stdin.on("data", chunk => {
        if (received) { abort("CompilerProtocol", "Only one compiler request is allowed"); return; }
        bytes += chunk.length;
        if (bytes > limit) { reject(new Error("Compiler input exceeds its byte bound")); return; }
        const newline = chunk.indexOf(10);
        if (newline === -1) { chunks.push(chunk); return; }
        if (newline !== chunk.length - 1) { reject(new Error("Unexpected bytes after compiler request")); return; }
        chunks.push(chunk.subarray(0, newline));
        received = true;
        resolve(Buffer.concat(chunks).toString("utf8"));
      });
      process.stdin.on("end", () => {
        if (!completed) abort("CompilerContainment", "Compiler owner control pipe closed");
      });
      process.stdin.on("error", error => abort("CompilerContainment", error.message));
    });
    if (stopping) throw new Error("Compiler owner was lost before worker startup");
  }
  worker = new Worker(new URL("./worker.mjs", import.meta.url), {
    workerData: { input, limit, selfCheck },
    resourceLimits: { maxOldGenerationSizeMb: 256, maxYoungGenerationSizeMb: 16, stackSizeMb: 4 },
    env: {},
    execArgv: [],
  });
  const response = await new Promise((resolve, reject) => {
    worker.once("message", resolve);
    worker.once("error", reject);
    worker.once("exit", code => reject(new Error(`Compiler worker exited without a response (${code})`)));
  });
  if (typeof response.output !== "string" || Buffer.byteLength(response.output) + 1 > limit) {
    throw new Error("Compiler worker output exceeds its byte bound");
  }
  await worker.terminate();
  if (!stopping) {
    completed = true;
    process.stdin.destroy();
    await new Promise(resolve => process.stdout.write(response.output + "\n", resolve));
    process.exitCode = response.ok ? 0 : 1;
  }
} catch (error) {
  abort(error.code === "COMPILER_OWNER_LOST" ? "CompilerContainment" : error.code === "ERR_WORKER_OUT_OF_MEMORY" ? "CompilerLimit" : "Compiler", error.message);
} finally {
  clearTimeout(deadlineTimer);
  clearInterval(parentTimer);
  process.stdin.destroy();
  void worker?.terminate();
}
