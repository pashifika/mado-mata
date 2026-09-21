import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { parentPort, workerData } from "node:worker_threads";
import ts from "./node_modules/typescript/lib/typescript.js";
import { definitions, methods } from "./sdk.mjs";

const ROOT = "/inventory/";
const LIB = "/compiler/";
const SDK = "/sdk/host.d.ts";
const VERSION = "5.9.3";
const HARD_LIMIT = 64 * 1024 * 1024;
const digest = value => createHash("sha256").update(value).digest("hex");
const fail = (category, message, context = {}) => {
  throw Object.assign(new Error(message), { category, context });
};
const compilerIdentity = {
  version: ts.version,
  compiler_sha256: digest(readFileSync(new URL("./node_modules/typescript/lib/typescript.js", import.meta.url))),
  driver_sha256: digest(readFileSync(new URL("./compile.mjs", import.meta.url))),
  worker_sha256: digest(readFileSync(new URL(import.meta.url))),
  sdk_sha256: digest(readFileSync(new URL("./sdk.mjs", import.meta.url))),
  lock_sha256: digest(readFileSync(new URL("./package-lock.json", import.meta.url))),
  node: process.versions.node,
};

const options = Object.freeze({
  target: ts.ScriptTarget.ES2020,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  strict: true,
  noEmitOnError: true,
  noUncheckedIndexedAccess: true,
  exactOptionalPropertyTypes: true,
  skipLibCheck: false,
  allowJs: true,
  checkJs: false,
  types: [],
  typeRoots: [],
  lib: ["lib.es2020.d.ts"],
  noLib: false,
  importHelpers: true,
  sourceMap: true,
  inlineSources: true,
  newLine: ts.NewLineKind.LineFeed,
  rootDir: ROOT,
  outDir: "/emitted/",
});

function origin(file, position) {
  const point = file.getLineAndCharacterOfPosition(position);
  return { module: file.fileName.replace(ROOT, ""), line: point.line + 1, column: point.character + 1 };
}

function literalImport(node) {
  if (!ts.isCallExpression(node) || node.expression.kind !== ts.SyntaxKind.ImportKeyword) return;
  let argument = node.arguments[0];
  while (argument && ts.isParenthesizedExpression(argument)) argument = argument.expression;
  return argument && ts.isStringLiteralLike(argument) ? argument : undefined;
}

function inspectJavaScript(request) {
  const imports = [];
  for (const [id, source] of Object.entries(request.sources)) {
    if (typeof source !== "string" || (!id.endsWith(".js") && !id.endsWith(".d.ts"))) fail("CompilerPolicy", "Unsupported JavaScript source inventory member", { module: id });
    // AST inspection only: QuickJS remains the syntax/execution authority. Do not
    // apply TypeScript directives, configuration, type checking, or emit to JS.
    const file = ts.createSourceFile(ROOT + id, source, ts.ScriptTarget.ESNext, false, ts.ScriptKind.JS);
    const visit = node => {
      if (id.endsWith(".js") && (ts.isImportDeclaration(node) || ts.isExportDeclaration(node))
        && node.moduleSpecifier && ts.isStringLiteralLike(node.moduleSpecifier)) {
        imports.push({ from: id, specifier: node.moduleSpecifier.text, kind: "module", ...origin(file, node.moduleSpecifier.getStart(file)) });
      }
      const literal = literalImport(node);
      if (literal) imports.push({ from: id, specifier: literal.text, kind: "dynamic", ...origin(file, literal.getStart(file)) });
      ts.forEachChild(node, visit);
    };
    visit(file);
  }
  return { imports, identity: compilerIdentity };
}

function inspect(request) {
  // Package-controlled compiler configuration is never merged into application options.
  const forbidden = new Set(["plugins", "scripts", "install", "installers", "compilerOptions", "tsconfig", "typeAcquisition"]);
  for (const key of forbidden) {
    if (Object.hasOwn(request.metadata?.manifest ?? {}, key)) {
      fail("CompilerPolicy", "Package-controlled compiler execution/configuration is forbidden", { origin: `package.json.${key}` });
    }
  }
  for (const key of forbidden) if (Object.hasOwn(request, key)) fail("CompilerPolicy", "Compiler request configuration is application-owned", { origin: key });
  const imports = [];
  const functions = {};
  for (const [id, source] of Object.entries(request.sources)) {
    if (typeof source !== "string" || (!id.endsWith(".ts") && !id.endsWith(".js"))) fail("CompilerPolicy", "Unsupported source inventory member", { module: id });
    const file = ts.createSourceFile(ROOT + id, source, ts.ScriptTarget.ES2020, true, id.endsWith(".js") ? ts.ScriptKind.JS : ts.ScriptKind.TS);
    if (file.libReferenceDirectives.length) fail("CompilerPolicy", "Source library directives cannot widen the compiler library inventory", origin(file, file.libReferenceDirectives[0].pos));
    const add = (specifier, node, kind = "module") => imports.push({ from: id, specifier, kind, ...origin(file, typeof node.getStart === "function" ? node.getStart(file) : node.pos) });
    for (const reference of file.typeReferenceDirectives) add(reference.fileName, reference, "type");
    for (const reference of file.referencedFiles) add(reference.fileName, reference, "reference");
    functions[id] = [];
    const visit = node => {
      if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteralLike(node.moduleSpecifier)) add(node.moduleSpecifier.text, node.moduleSpecifier);
      if (ts.isImportTypeNode(node) && ts.isLiteralTypeNode(node.argument) && ts.isStringLiteralLike(node.argument.literal)) add(node.argument.literal.text, node.argument.literal);
      if (ts.isImportEqualsDeclaration(node)) fail("CompilerPolicy", "Only ECMAScript imports are supported", origin(file, node.getStart(file)));
      const literal = literalImport(node);
      if (literal) add(literal.text, literal);
      if (ts.isFunctionLike(node) && node.body) {
        const start = file.getLineAndCharacterOfPosition(node.getStart(file));
        const end = file.getLineAndCharacterOfPosition(node.end);
        let name = node.name?.getText(file);
        if (!name && ts.isVariableDeclaration(node.parent)) name = node.parent.name.getText(file);
        if (name) functions[id].push({ name, start_line: start.line + 1, start_column: start.character + 1, end_line: end.line + 1, end_column: end.character + 1 });
      }
      ts.forEachChild(node, visit);
    };
    visit(file);
  }
  return { imports, functions, identity: compilerIdentity };
}

function capturedLibraries() {
  const libraries = new Map();
  const capture = name => {
    if (libraries.has(LIB + name)) return;
    if (!/^lib\.[a-z0-9.]+\.d\.ts$/.test(name)) fail("CompilerIdentity", "Invalid compiler library reference");
    const source = readFileSync(new URL(`./node_modules/typescript/lib/${name}`, import.meta.url), "utf8");
    libraries.set(LIB + name, source);
    const parsed = ts.preProcessFile(source);
    for (const reference of parsed.libReferenceDirectives) capture(`lib.${reference.fileName}.d.ts`);
    if (parsed.referencedFiles.length || parsed.typeReferenceDirectives.length || parsed.importedFiles.length) fail("CompilerIdentity", "Unexpected compiler library dependency", { module: name });
  };
  capture("lib.es2020.d.ts");
  return libraries;
}

function virtualHost(request, libraries, declarations, emitted) {
  const files = new Map(libraries);
  for (const [id, source] of Object.entries(request.sources)) files.set(ROOT + id, source);
  files.set(SDK, declarations);
  const resolution = (specifier, containingFile) => {
    const from = containingFile.startsWith(ROOT) ? containingFile.slice(ROOT.length) : null;
    const id = from === null ? undefined : request.resolutions?.[from]?.[specifier];
    if (!id || !Object.hasOwn(request.sources, id)) return undefined;
    const declaration = id.endsWith(".js") ? id.slice(0, -3) + ".d.ts" : id;
    const selected = Object.hasOwn(request.sources, declaration) ? declaration : id;
    return { resolvedFileName: ROOT + selected, extension: selected.endsWith(".d.ts") ? ts.Extension.Dts : selected.endsWith(".ts") ? ts.Extension.Ts : ts.Extension.Js, isExternalLibraryImport: selected.startsWith("@") };
  };
  const host = {
    getSourceFile: (file, target) => {
      if (!files.has(file)) return undefined;
      const source = ts.createSourceFile(file, files.get(file), target, true);
      if (file.startsWith(ROOT)) {
        source.referencedFiles = source.referencedFiles.map(reference => {
          const resolved = resolution(reference.fileName, file);
          if (!resolved) fail("ImportRefused", "Reference is absent from the authorized resolution table", origin(source, reference.pos));
          return { ...reference, fileName: resolved.resolvedFileName };
        });
      }
      return source;
    },
    getDefaultLibFileName: () => LIB + "lib.es2020.d.ts",
    getDefaultLibLocation: () => LIB.slice(0, -1),
    getCurrentDirectory: () => ROOT,
    getCanonicalFileName: file => file,
    useCaseSensitiveFileNames: () => true,
    getNewLine: () => "\n",
    fileExists: file => files.has(file),
    readFile: file => files.get(file),
    directoryExists: directory => [...files.keys()].some(file => file.startsWith(directory + "/")),
    getDirectories: () => [],
    realpath: file => file,
    resolveModuleNames: (names, containingFile) => names.map(name => resolution(name, containingFile)),
    resolveTypeReferenceDirectives: (names, containingFile) => names.map(name => {
      const resolved = resolution(typeof name === "string" ? name : name.fileName, containingFile);
      return resolved?.extension === ts.Extension.Dts ? { resolvedFileName: resolved.resolvedFileName, primary: true } : undefined;
    }),
    writeFile: (name, text, _bom, _error, sourceFiles) => {
      // Runtime catalog bytes are already captured and must not be transformed.
      if (!sourceFiles?.some(file => file.fileName.endsWith(".ts") && !file.isDeclarationFile)) return;
      if (!name.startsWith("/emitted/")) fail("CompilerPolicy", "Compiler emitted outside its virtual output directory");
      const id = name.slice("/emitted/".length);
      if (Object.hasOwn(emitted, id)) fail("CompilerPolicy", "Conflicting emitted module", { module: id });
      emitted[id] = text;
    },
  };
  return { host, files };
}

function diagnosticRecord(diagnostic, functions) {
  const location = diagnostic.file && diagnostic.start !== undefined ? origin(diagnostic.file, diagnostic.start) : {};
  const enclosing = functions[location.module]?.findLast(fn =>
    location.line >= fn.start_line && location.line <= fn.end_line
    && (location.line !== fn.start_line || location.column >= fn.start_column)
    && (location.line !== fn.end_line || location.column <= fn.end_column));
  return {
    code: diagnostic.code,
    category: ts.DiagnosticCategory[diagnostic.category],
    message: ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"),
    ...location,
    function: enclosing?.name ?? null,
    ...(diagnostic.relatedInformation ? { related: diagnostic.relatedInformation.map(item => diagnosticRecord(item, functions)) } : {}),
  };
}

function diagnostics(program, functions = {}) {
  return ts.getPreEmitDiagnostics(program).map(diagnostic => diagnosticRecord(diagnostic, functions));
}

function compile(request) {
  const inspected = inspect(request);
  if (!request.compiler_identity || Object.keys(request.compiler_identity).length !== Object.keys(compilerIdentity).length
      || Object.entries(compilerIdentity).some(([key, value]) => request.compiler_identity[key] !== value)) {
    fail("CompilerIdentity", "Compiler changed between inspection and compilation");
  }
  for (const imported of inspected.imports) {
    if (!request.resolutions?.[imported.from]?.[imported.specifier]) fail("ImportRefused", "Source dependency is absent from the authorized resolution table", imported);
  }
  const libraries = capturedLibraries();
  const declarations = definitions(request.schema);
  const emitted = Object.create(null);
  const { host, files } = virtualHost(request, libraries, declarations, emitted);
  const roots = Object.keys(request.sources).map(id => ROOT + id).concat(SDK);
  const program = ts.createProgram(roots, options, host);
  const errors = diagnostics(program, inspected.functions);
  if (errors.length) fail("TypeScript", "TypeScript compilation failed", { diagnostics: errors, compiler: compilerIdentity });
  const result = program.emit();
  if (result.emitSkipped || result.diagnostics.length) fail("TypeScript", "TypeScript emit failed", {
    diagnostics: result.diagnostics.map(diagnostic => diagnosticRecord(diagnostic, inspected.functions)),
    compiler: compilerIdentity,
  });
  const sources = Object.create(null);
  const source_maps = Object.create(null);
  for (const [id, content] of Object.entries(emitted)) {
    if (!id.endsWith(".map")) { sources[id] = content; continue; }
    const map = JSON.parse(content);
    // Keep canonical inventory IDs, not virtual filesystem paths, in captured maps.
    map.sources = map.sources.map(source => {
      const path = ts.normalizePath(ts.combinePaths("/emitted/", ts.getDirectoryPath(id), source));
      if (!path.startsWith(ROOT) || !files.has(path)) fail("CompilerPolicy", "Source map refers outside captured sources");
      return path.slice(ROOT.length);
    });
    map.sourceRoot = "";
    source_maps[id.slice(0, -4)] = JSON.stringify(map);
  }
  for (const id of Object.keys(sources)) {
    if (Object.hasOwn(request.sources, id)) fail("CompilerPolicy", "Emitted JavaScript collides with captured source", { module: id });
  }
  return {
    sources, source_maps,
    compiler: {
      ...compilerIdentity,
      options,
      libraries: Object.fromEntries([...libraries].map(([id, source]) => [id.slice(LIB.length), { sha256: digest(source), bytes: Buffer.byteLength(source) }])),
      declarations,
      declarations_sha256: digest(declarations),
      functions: inspected.functions,
      resolution_sha256: digest(JSON.stringify(request.resolutions)),
      resolutions: request.resolutions,
      source_map_policy: "canonical-inventory-ids-v1",
    },
  };
}

function selfCheck() {
  const schema = JSON.parse(readFileSync(new URL("../fixtures/typescript/schema.json", import.meta.url), "utf8"));
  const request = source => ({ sources: { "main.ts": source }, schema, metadata: {}, resolutions: {}, compiler_identity: compilerIdentity });
  const cases = [];
  const accepted = request("export function readiness(): MadoReady { return 'Ready'; }\nexport function workflow() { return host.call('observe', {}); }\n");
  const valid = compile(accepted);
  const validMap = JSON.parse(valid.source_maps["main.js"]);
  if (!valid.sources["main.js"] || validMap.sources[0] !== "main.ts" || validMap.sourcesContent?.[0] !== accepted.sources["main.ts"]) throw new Error("Compiled code or original-source map missing");
  cases.push("strict compilation and captured original sources");
  const reject = (name, input, category, code) => {
    let fault;
    try { compile(input); } catch (error) { fault = error; }
    if (!fault || fault.category !== category || (category === "TypeScript" && !fault.context.diagnostics?.some(item => (!code || item.code === code) && item.module === "main.ts" && item.line > 0))) throw new Error(`Expected attributable refusal: ${name}`);
    cases.push(name);
  };
  reject("unknown option", request("host.options.unknown; export {};"), "TypeScript", 2339);
  reject("nested readonly option", request("host.options.recognition.threshold = 0; export {};"), "TypeScript", 2540);
  reject("readonly priority list", request("host.options.priorities.push('template'); export {};"), "TypeScript", 2339);
  reject("restricted priority value", request("const value: MadoOptions['priorities'][number] = 'invalid'; export { value };"), "TypeScript", 2322);
  reject("ambient Node API", request("export const value = process.cwd();"), "TypeScript");
  reject("ambient type directive", request("/// <reference types=\"node\" />\nexport {};"), "ImportRefused");
  reject("erased forbidden type import", request("import type { Secret } from 'ambient-package'; export type Value = Secret;"), "ImportRefused");
  reject("missing generated helper", request("function decorate(value: Function, context: ClassDecoratorContext) {}\n@decorate export class Example {}"), "TypeScript", 2354);
  reject("plugin configuration", { ...accepted, metadata: { manifest: { compilerOptions: { plugins: [{ name: "unapproved" }] } } } }, "CompilerPolicy");
  reject("package install script", { ...accepted, metadata: { manifest: { scripts: { install: "unapproved" } } } }, "CompilerPolicy");
  compile({ ...accepted, metadata: { manifest: { profiles: { scripts: "profiles/scripts.json" }, assets: { plugins: { path: "assets/plugins.png" } } } } });
  cases.push("profile and asset identifiers are not compiler configuration");
  const declarations = definitions(schema);
  for (const [probe, expected] of [
    ["host.options.", Object.keys(schema.properties)],
    ["host.call('", Object.keys(methods)],
  ]) {
    const { host, files } = virtualHost(request(probe), capturedLibraries(), declarations, {});
    const service = ts.createLanguageService({
      ...host,
      getCompilationSettings: () => options,
      getScriptFileNames: () => [ROOT + "main.ts", SDK],
      getScriptVersion: () => "1",
      getScriptSnapshot: id => files.has(id) ? ts.ScriptSnapshot.fromString(files.get(id)) : undefined,
    });
    try {
      const entries = service.getCompletionsAtPosition(ROOT + "main.ts", probe.length, {})?.entries ?? [];
      const names = new Set(entries.map(entry => entry.name));
      for (const name of expected) if (!names.has(name)) throw new Error(`Missing generated completion: ${name}`);
    } finally { service.dispose(); }
  }
  cases.push("schema-derived option and SDK method completions");
  return { status: "PASS", compiler: compilerIdentity, cases };
}


try {
  if (ts.version !== VERSION || !process.versions.node.startsWith("24.")) {
    fail("CompilerIdentity", "The application requires TypeScript 5.9.3 and Node 24", compilerIdentity);
  }
  if (!Number.isSafeInteger(workerData.limit) || workerData.limit < 1 || workerData.limit > HARD_LIMIT) {
    fail("CompilerPolicy", "Invalid compiler worker output bound");
  }
  let value;
  if (workerData.selfCheck) {
    value = selfCheck();
  } else {
    const request = JSON.parse(workerData.input);
    value = request.operation === "inspect" ? inspect(request)
      : request.operation === "inspect-javascript" ? inspectJavaScript(request)
      : request.operation === "compile" ? compile(request)
      : fail("CompilerPolicy", "Unknown compiler operation");
  }
  const output = JSON.stringify(workerData.selfCheck ? value : { ok: true, value });
  if (Buffer.byteLength(output) + 1 > workerData.limit) fail("CompilerLimit", "Compiler output exceeds its bound");
  parentPort.postMessage({ output, ok: true });
} catch (error) {
  const context = error.context == null ? {} : typeof error.context === "object" && !Array.isArray(error.context) ? { ...error.context } : { cause: error.context };
  context.compiler ??= compilerIdentity;
  context.stack ??= error.stack ?? null;
  if (error.code !== undefined) context.code ??= error.code;
  if (error.cause !== undefined) context.cause ??= error.cause instanceof Error
    ? { name: error.cause.name, message: error.cause.message, stack: error.cause.stack, code: error.cause.code }
    : error.cause;
  const fault = { category: error.category ?? "Compiler", message: error.message, context };
  let output = JSON.stringify({ ok: false, fault });
  if (Buffer.byteLength(output) + 1 > workerData.limit) output = JSON.stringify({ ok: false, fault: { category: "CompilerLimit", message: "Compiler diagnostics exceed the output bound", context: {} } });
  parentPort.postMessage({ output, ok: false });
} finally {
  parentPort.close();
}
