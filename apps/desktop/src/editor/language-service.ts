import ts from 'typescript';
import {definitions, methods, unknownOptionsDefinitions} from '../../../../tools/runtime-comparison/compiler/sdk.mjs';
import {AUTHORING_NON_IMAGE_BYTES, AUTHORING_SOURCE_BYTES} from '../authoring.ts';
import {parseJson, schemaIssues} from '../metadata.ts';
import {CANDIDATE_LIMIT, DECLARATION_BYTES, DETAIL_BYTES, RESPONSE_BYTES} from './completion-types.ts';
import type {CompletionCandidate, CompletionKey, CompletionRequest, CompletionResult} from './completion-types.ts';

export type TrustedLibraries = ReadonlyMap<string, string> | Readonly<Record<string, string>>;
const LIB = '/compiler/';
const SDK = '/sdk/host.d.ts';
const encoder = new TextEncoder();
const bytes = (text: string) => encoder.encode(text).length;
const preferences: ts.UserPreferences = {
  includeCompletionsForModuleExports: false,
  includeCompletionsForImportStatements: false,
  includePackageJsonAutoImports: 'off',
  includeCompletionsWithInsertText: true,
  includeCompletionsWithSnippetText: false,
  includeCompletionsWithClassMemberSnippets: false,
  includeCompletionsWithObjectLiteralMethodSnippets: false,
  includeAutomaticOptionalChainCompletions: false,
  allowTextChangesInNewFiles: false,
  quotePreference: 'double',
};
const settings: ts.CompilerOptions = {
  target: ts.ScriptTarget.ES2020,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  strict: true,
  noUncheckedIndexedAccess: true,
  exactOptionalPropertyTypes: true,
  allowJs: true,
  checkJs: true,
  noEmit: true,
  noResolve: true,
  noLib: true,
  types: [],
  typeRoots: [],
  skipLibCheck: false,
};

function libraryInventory(libraries: TrustedLibraries): Map<string, string> {
  const supplied = libraries instanceof Map ? libraries : new Map(Object.entries(libraries));
  const inventory = new Map<string, string>();
  const capture = (name: string) => {
    if (inventory.has(LIB + name)) return;
    if (!/^lib\.[a-z0-9.]+\.d\.ts$/.test(name)) throw new Error('Invalid trusted library reference');
    const source = supplied.get(name);
    if (typeof source !== 'string') throw new Error('Missing trusted library: ' + name);
    inventory.set(LIB + name, source);
    const parsed = ts.preProcessFile(source);
    if (parsed.referencedFiles.length || parsed.typeReferenceDirectives.length || parsed.importedFiles.length) {
      throw new Error('Unexpected trusted library dependency: ' + name);
    }
    for (const reference of parsed.libReferenceDirectives) capture(`lib.${reference.fileName}.d.ts`);
  };
  capture('lib.es2020.d.ts');
  if (inventory.size !== supplied.size) throw new Error('Only the trusted ES2020 closure is permitted');
  return inventory;
}

function nodeAt(file: ts.SourceFile, position: number): ts.Node {
  const visit = (node: ts.Node): ts.Node => ts.forEachChild(node, child =>
    child.getStart(file) < position && position <= child.end ? visit(child) : undefined) ?? node;
  return visit(file);
}

interface StringContext {quote: string; from: number; to: number}

function stringContext(node: ts.Node, file: ts.SourceFile, position: number): StringContext | null {
  if (!ts.isStringLiteralLike(node)) return null;
  const start = node.getStart(file);
  const quote = file.text[start];
  const closed = node.end > start + 1 && file.text[node.end - 1] === quote && !node.isUnterminated;
  const to = node.end - (closed ? 1 : 0);
  return position > start && position <= to ? {quote, from: start + 1, to} : null;
}

function sdkMethodLiteral(node: ts.Node, checker: ts.TypeChecker): boolean {
  if (!ts.isStringLiteralLike(node)) return false;
  let argument: ts.Node = node;
  while (ts.isParenthesizedExpression(argument.parent)) argument = argument.parent;
  const call = argument.parent;
  if (!ts.isCallExpression(call) || call.arguments[0] !== argument) return false;
  const declaration = checker.getResolvedSignature(call)?.declaration;
  // Inferred aliases retain this declaration; a locally declared lookalike does not.
  return declaration !== undefined && declaration.getSourceFile().fileName === SDK
    && ts.isMethodSignature(declaration) && ts.isIdentifier(declaration.name) && declaration.name.text === 'call';
}

function escapedContent(value: string, quote: string): string {
  let escaped = JSON.stringify(value).slice(1, -1).replace(/\u2028/g, '\\u2028').replace(/\u2029/g, '\\u2029');
  if (quote === "'") escaped = escaped.replace(/'/g, "\\'");
  if (quote === '`') escaped = escaped.replace(/`/g, '\\`').replace(/\$\{/g, '\\${');
  return escaped;
}

function completionEdit(entry: ts.CompletionEntry, info: ts.CompletionInfo, file: ts.SourceFile, position: number,
  node: ts.Node, literal: StringContext | null): CompletionCandidate | null {
  let insertText = entry.insertText ?? entry.name;
  let span = entry.replacementSpan ?? info.optionalReplacementSpan;
  if (literal) {
    // Literal alternatives are already escaped by TypeScript; property symbols are raw names.
    if (entry.kind !== ts.ScriptElementKind.string) insertText = escapedContent(entry.name, literal.quote);
    else if (literal.quote === '`') insertText = insertText.replace(/\$\{/g, '\\${');
    span = {start: literal.from, length: literal.to - literal.from};
  }
  if (!span) {
    const start = ts.isIdentifier(node) ? node.getStart(file) : position;
    span = {start, length: ts.isIdentifier(node) ? node.end - start : 0};
  }
  const from = span.start;
  const to = span.start + span.length;
  if (!Number.isSafeInteger(from) || !Number.isSafeInteger(to) || from < 0 || from > position
    || to < position || to > file.text.length) return null;
  return {label: entry.name, insertText, from, to, kind: entry.kind};
}

// One active document, with no resolver or host fallback to ts.sys.
export class ScriptLanguageService {
  private readonly files: Map<string, string>;
  private readonly snapshots = new Map<string, ts.IScriptSnapshot>();
  private readonly versions = new Map<string, number>();
  private readonly libraries: string[];
  private readonly service: ts.LanguageService;
  private active = '/script/active.ts';
  private path: string | null = null;
  private owner: string | null = null;
  private source: string | undefined;
  private schema: string | null | undefined;
  private optionsAvailable = false;
  private version = 0;
  private disposed = false;

  constructor(libraries: TrustedLibraries) {
    if (ts.version !== '5.9.3') throw new Error('Script completion requires TypeScript 5.9.3');
    this.files = libraryInventory(libraries);
    this.libraries = [...this.files.keys()];
    this.put(SDK, unknownOptionsDefinitions());
    const host: ts.LanguageServiceHost = {
      getCompilationSettings: () => settings,
      getProjectVersion: () => String(this.version),
      getScriptFileNames: () => [this.active, SDK, ...this.libraries],
      getScriptVersion: file => String(this.versions.get(file) ?? 0),
      getScriptSnapshot: file => {
        const text = this.files.get(file);
        if (text === undefined) return undefined;
        let snapshot = this.snapshots.get(file);
        if (!snapshot) { snapshot = ts.ScriptSnapshot.fromString(text); this.snapshots.set(file, snapshot); }
        return snapshot;
      },
      getScriptKind: file => file.endsWith('.js') ? ts.ScriptKind.JS : ts.ScriptKind.TS,
      getCurrentDirectory: () => '/script/',
      getDefaultLibFileName: () => LIB + 'lib.es2020.d.ts',
      getNewLine: () => '\n',
      useCaseSensitiveFileNames: () => true,
      readFile: file => this.files.get(file),
      fileExists: file => this.files.has(file),
      directoryExists: directory => directory === '/script' || directory === '/sdk' || directory === '/compiler',
      readDirectory: () => [],
      getDirectories: () => [],
      realpath: file => file,
      resolveModuleNames: names => names.map(() => undefined),
      resolveTypeReferenceDirectives: names => names.map(() => undefined),
    };
    this.service = ts.createLanguageService(host);
  }

  complete(request: CompletionRequest): CompletionResult {
    if (this.disposed) throw new Error('Language service is disposed');
    const {key, position} = request;
    if (request.kind !== 'complete' || !/\.(ts|js)$/.test(key.path)) throw new Error('Unsupported completion source');
    if (this.owner !== key.owner && (request.source === undefined || request.schema === undefined)) {
      throw new Error('A new completion owner requires its current source and schema');
    }
    if (this.path !== key.path && request.source === undefined) throw new Error('A new source path requires its document');
    const source = request.source ?? this.source;
    const schema = request.schema === undefined ? this.schema : request.schema;
    if (typeof source !== 'string' || schema === undefined) throw new Error('Missing completion context');
    if (!Number.isSafeInteger(position) || position < 0 || position > source.length) throw new Error('Invalid completion position');
    if (source.length > AUTHORING_SOURCE_BYTES || (schema !== null && schema.length > AUTHORING_SOURCE_BYTES)) {
      throw new Error('Completion input exceeds its byte bound');
    }
    const sourceBytes = bytes(source);
    const schemaBytes = schema === null ? 0 : bytes(schema);
    // The client additionally accounts for the package's other non-image drafts.
    if (sourceBytes > AUTHORING_SOURCE_BYTES || schemaBytes > AUTHORING_SOURCE_BYTES
      || sourceBytes + schemaBytes > AUTHORING_NON_IMAGE_BYTES) {
      throw new Error('Completion input exceeds its byte bound');
    }
    if (schema !== this.schema) {
      const parsed = schema === null ? null : parseJson(schema);
      this.optionsAvailable = parsed !== null && parsed.ok && schemaIssues(parsed.value).length === 0;
      const declarations = parsed?.ok && this.optionsAvailable ? definitions(parsed.value) : unknownOptionsDefinitions();
      if (bytes(declarations) > DECLARATION_BYTES) throw new Error('Completion declarations exceed their byte bound');
      this.put(SDK, declarations);
      this.schema = schema;
    }
    const active = key.path.endsWith('.js') ? '/script/active.js' : '/script/active.ts';
    if (this.active !== active) {
      this.files.delete(this.active);
      this.snapshots.delete(this.active);
      this.versions.delete(this.active);
      this.active = active;
    }
    this.put(this.active, source);
    this.source = source;
    this.path = key.path;
    this.owner = key.owner;
    return this.analyze(key, position);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.service.dispose();
    this.files.clear();
    this.snapshots.clear();
    this.versions.clear();
    this.source = undefined;
    this.schema = undefined;
  }

  private put(file: string, source: string): void {
    if (this.files.get(file) === source) return;
    this.files.set(file, source);
    this.snapshots.delete(file);
    this.versions.set(file, (this.versions.get(file) ?? 0) + 1);
    this.version += 1;
  }

  private analyze(key: CompletionKey, position: number): CompletionResult {
    const result: CompletionResult = {key: {...key}, candidates: [], capped: false, optionsAvailable: this.optionsAvailable};
    let size = bytes(JSON.stringify({kind: 'result', result}));
    if (size > RESPONSE_BYTES) throw new Error('Completion identity exceeds the response byte bound');
    const info = this.service.getCompletionsAtPosition(this.active, position, preferences);
    if (!info) return result;
    const program = this.service.getProgram();
    const file = program?.getSourceFile(this.active);
    if (!file) throw new Error('Missing active completion source');
    const node = nodeAt(file, position);
    const literal = stringContext(node, file, position);
    const sdkLiteral = literal !== null && program !== undefined && sdkMethodLiteral(node, program.getTypeChecker());
    result.capped = info.isIncomplete === true;
    for (const entry of info.entries) {
      if (entry.hasAction || entry.source || entry.isSnippet || entry.isImportStatementCompletion || entry.isFromUncheckedFile) continue;
      if (result.candidates.length === CANDIDATE_LIMIT) { result.capped = true; break; }
      const candidate = completionEdit(entry, info, file, position, node, literal);
      if (!candidate) continue;
      const details = this.service.getCompletionEntryDetails(this.active, position, entry.name, undefined, entry.source, preferences, entry.data);
      if (details?.codeActions?.length) continue;
      const method = sdkLiteral && Object.hasOwn(methods, entry.name) ? methods[entry.name] : undefined;
      const detail = method
        ? `host.call(method: ${JSON.stringify(entry.name)}, args: ${method[0]}): ${method[1]}`
        : ts.displayPartsToString(details?.displayParts);
      const documentation = method ? method[2] : ts.displayPartsToString(details?.documentation);
      if (bytes(detail) + bytes(documentation) > DETAIL_BYTES) candidate.detailOmitted = true;
      else {
        if (detail) candidate.detail = detail;
        if (documentation) candidate.documentation = documentation;
      }
      const added = bytes(JSON.stringify(candidate)) + (result.candidates.length > 0 ? 1 : 0);
      if (size + added > RESPONSE_BYTES) { result.capped = true; continue; }
      size += added;
      result.candidates.push(candidate);
    }
    return result;
  }
}
