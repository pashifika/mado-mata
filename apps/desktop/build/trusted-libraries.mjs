import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname, join} from 'node:path';
import ts from 'typescript';

const require = createRequire(import.meta.url);
const virtualId = 'virtual:mado-es2020';
const resolvedId = '\0' + virtualId;

// Build/test input only. The browser receives these strings, never a filesystem loader.
export function trustedLibraries() {
  if (ts.version !== '5.9.3') throw new Error('Script completion requires TypeScript 5.9.3');
  const directory = dirname(require.resolve('typescript/lib/lib.es2020.d.ts'));
  const libraries = Object.create(null);
  const capture = name => {
    if (Object.hasOwn(libraries, name)) return;
    if (!/^lib\.[a-z0-9.]+\.d\.ts$/.test(name)) throw new Error('Invalid trusted library reference');
    const source = readFileSync(join(directory, name), 'utf8');
    const parsed = ts.preProcessFile(source);
    if (parsed.referencedFiles.length || parsed.typeReferenceDirectives.length || parsed.importedFiles.length) {
      throw new Error('Unexpected trusted library dependency: ' + name);
    }
    libraries[name] = source;
    for (const reference of parsed.libReferenceDirectives) capture(`lib.${reference.fileName}.d.ts`);
  };
  capture('lib.es2020.d.ts');
  return libraries;
}

export function trustedLibrariesPlugin() {
  return {
    name: 'mado-trusted-es2020',
    resolveId(id) { if (id === virtualId) return resolvedId; },
    load(id) {
      if (id === resolvedId) return `export default ${JSON.stringify(trustedLibraries())};`;
    },
  };
}
