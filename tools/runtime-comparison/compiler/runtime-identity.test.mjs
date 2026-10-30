import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { Worker } from "node:worker_threads";

const manifest = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));

async function inspectWithNodeIdentity(t, nodeVersion) {
  const worker = new Worker(`
    const { workerData } = require("node:worker_threads");
    Object.defineProperty(process.versions, "node", { value: workerData.nodeVersion });
    import(workerData.workerUrl);
  `, {
    eval: true,
    execArgv: ["--experimental-vm-modules"],
    env: {},
    workerData: {
      nodeVersion,
      workerUrl: new URL("./worker.mjs", import.meta.url).href,
      limit: 1024 * 1024,
      selfCheck: false,
      input: JSON.stringify({
        operation: "inspect-javascript",
        sources: { "main.js": "import { value } from './dependency.js';" },
      }),
    },
  });
  t.after(() => worker.terminate());
  const response = await new Promise((resolve, reject) => {
    worker.once("message", resolve);
    worker.once("error", reject);
    worker.once("exit", code => reject(new Error(`Worker exited without a response: ${code}`)));
  });
  return JSON.parse(response.output);
}

for (const nodeVersion of ["24.18.0", "23.0.0"]) {
  test(`Node ${nodeVersion} is refused before returning import facts`, { timeout: 5000 }, async t => {
    // Substitute only the advertised version; this does not execute an old Node binary.
    const result = await inspectWithNodeIdentity(t, nodeVersion);
    assert.equal(result.ok, false);
    assert.equal(result.fault.category, "CompilerIdentity");
    assert.equal(result.fault.context.node, nodeVersion);
    assert.equal(Object.hasOwn(result, "value"), false);
  });
}

test("the pinned Node runtime inspects dependency imports", { timeout: 5000 }, async t => {
  const result = await inspectWithNodeIdentity(t, manifest.engines.node);
  assert.equal(result.ok, true);
  assert.deepEqual(result.value.imports.map(({ from, specifier, kind }) => ({ from, specifier, kind })), [
    { from: "main.js", specifier: "./dependency.js", kind: "module" },
  ]);
});
