import {test} from 'node:test';
import assert from 'node:assert/strict';
import {applyRefresh,applySave,beginComposition,beginPending,discardFile,editFile,endComposition,failCommand,fileDirty,openSession,redoFile,replaceFile,saveTicket,selectFile,
  undoFile} from './authoring.ts';
import {aliasIssue,constraintValue,currentValues,distributionTextIssue,editValues,emptySchema,fileTree,folderAncestors,formatJson,loadValues,nameRowIssues,nodeIssues,
  parseJson,presetIssues,readDistribution,readManifest,renameProperty,repairDistribution,typedText,repairNode,repairPreset,schemaIssues,sourceMapFacts,starterDistribution,
  topDefaults,withAliases,withDistribution,withDistributionText,withEntry,withHelper,withNameRows,withProperty,withRequired,withTarget,withTopDefaults,withType,
  withoutProperty} from './metadata.ts';

const owner={workspace:{workspace_id:'a',revision:1},token:'lease-1'};
// The host writes catalog-edited manifests compactly and without a final newline.
const MANIFEST='{"version":1,"package_id":"example.a","runtime":"typescript","sdk":"mado-host-v1","entry_contract":"ready-string-v1",'
  +'"entries":{"readiness":{"module":"main.ts","function":"readiness"},"workflow":{"module":"main.ts","function":"workflow"}},'
  +'"sources":["main.ts","src/lib/util.ts"],"schema":"schema.json","profiles":{"default":"profiles/default.json"},"assets":{},"source_maps":{},"dependencies":{}}';
function file(path,kind,text){
  return {path,kind,text,bytes:text===null?4:new TextEncoder().encode(text).length};
}
function view(files,revision='rev-1'){
  return {owner,package_path:'/pkg/a',package_id:'example.a',revision,files};
}
function draft(path,kind){
  return {path,kind,bytes:1,base:'',text:'',revision:0,undo:[],redo:[],group:null,composing:null,range:{start:0,end:0},diskChanged:false,missing:false,typed:[]};
}
const value=text=>{
  const document=parseJson(text);
  assert.equal(document.ok,true,text);
  return document.value;
};

test('JSON refusals carry a location, and lossy literals and repeated members are disclosed instead of silently rewritten',()=>{
  assert.deepEqual(parseJson('{\n  "a": 1,\n}').error,{problem:'unexpectedCharacter',line:3,column:1});
  assert.deepEqual(parseJson('{"a": [1, 2').error,{problem:'unexpectedEnd',line:1,column:12});
  assert.deepEqual(parseJson('{"a": 1} x').error,{problem:'trailingContent',line:1,column:10});
  assert.equal(parseJson('"\\ud800"').error.problem,'invalidString');
  assert.equal(parseJson('01').error.problem,'trailingContent');
  assert.equal(parseJson('1e400').error.problem,'invalidNumber');
  assert.equal(parseJson('['.repeat(129)+']'.repeat(129)).error.problem,'depth');
  const document=parseJson('{"min": 1.0, "big": 9007199254740993, "a": {"x": 1, "x": 2}, "__proto__": {"polluted": true}}');
  assert.deepEqual(document.numbers,['1.0','9007199254740993']);
  assert.deepEqual(document.duplicates,['$.a.x']);
  assert.equal(document.value.a.x,2);
  // A `__proto__` member is ordinary data, never the object's prototype.
  assert.equal(Object.hasOwn(document.value,'__proto__'),true);
  assert.equal(Object.getPrototypeOf(document.value),Object.prototype);
  assert.equal(JSON.parse(formatJson(null,document.value)).__proto__.polluted,true);
});

test('an unchanged or restored value keeps the saved bytes; a changed value keeps the saved style',()=>{
  const saved='{\n\t"min": 1.0,\n\t"name": "a"\n}\n';
  assert.equal(formatJson(saved,{name:'a',min:1}),saved);
  assert.equal(formatJson(saved,{min:1,name:'b'}),'{\n\t"min": 1,\n\t"name": "b"\n}\n');
  assert.equal(formatJson(MANIFEST,withEntry(value(MANIFEST),'workflow','function','run')).includes('\n'),false);
  assert.equal(formatJson('{bad',{a:1}),'{"a":1}');
  assert.equal(formatJson(null,{a:1}),'{\n  "a": 1\n}\n');
});

test('structured manifest edits mark only the manifest dirty, restoring every field is clean again, and Save commits the form text',()=>{
  let state=openSession(view([file('package.json','manifest',MANIFEST),file('main.ts','source','export {};\n'),file('schema.json','schema','{}')]));
  const original=value(MANIFEST);
  state=replaceFile(state,'package.json',formatJson(MANIFEST,withEntry(original,'workflow','function','run')));
  let manifest=state.drafts.get('package.json');
  assert.equal(fileDirty(manifest),true);
  assert.equal(readManifest(value(manifest.text)).entries.workflow.function,'run');
  state=replaceFile(state,'package.json',formatJson(MANIFEST,withEntry(value(manifest.text),'workflow','function','workflow')));
  assert.equal(state.drafts.get('package.json').text,MANIFEST);
  assert.equal(fileDirty(state.drafts.get('package.json')),false);

  state=replaceFile(state,'package.json',formatJson(MANIFEST,withHelper(original,true)));
  const ticket=saveTicket(state,'package.json');
  // A later form edit before the reply stays unsaved.
  state=replaceFile(state,'package.json',formatJson(MANIFEST,withHelper(withEntry(original,'readiness','function','ready'),true)));
  state=applySave(state,ticket,{owner,committed_revision:'rev-2',view:null,refresh_error:null});
  manifest=state.drafts.get('package.json');
  assert.equal(manifest.base,ticket.text);
  assert.equal(fileDirty(manifest),true);
  state=discardFile(state,'package.json');
  assert.equal(state.drafts.get('package.json').text,ticket.text);
});

test('manifest intent edits preserve every other member and never write empty optional target fields',()=>{
  const original=value(MANIFEST);
  const helper=withHelper({...original,dependencies:{other:'2.0.0'}},true);
  assert.deepEqual(helper.dependencies,{other:'2.0.0','@mado/helper':'1.0.0'});
  assert.deepEqual(withHelper(helper,false).dependencies,{other:'2.0.0'});
  const declared=withTarget(original,{id:'game',windowTitle:null,bundleId:'com.example.game'});
  assert.deepEqual(declared.target,{id:'game',window_title:null,macos:{bundle_id:'com.example.game'}});
  assert.deepEqual(readManifest(declared).target,{id:'game',windowTitle:null,bundleId:'com.example.game'});
  assert.deepEqual(withTarget(declared,{id:'game',windowTitle:'Game',bundleId:null}).target,{id:'game',window_title:'Game'});
  const removed=withTarget(declared,null);
  assert.equal(Object.hasOwn(removed,'target'),false);
  assert.deepEqual(removed,original);
  assert.deepEqual(withEntry(original,'readiness','module','src/lib/util.ts').entries,
    {readiness:{module:'src/lib/util.ts',function:'readiness'},workflow:{module:'main.ts',function:'workflow'}});
});

test('the Files tree holds only scripts and assets, nested deterministically by folder',()=>{
  const drafts=['package.json:manifest','schema.json:schema','profiles/default.json:profile','maps/main.js.map:source_map','src/lib/b.ts:source',
    'main.ts:source','src/A.ts:source','src/lib/a.ts:source','images/logo.png:asset','Zeta.ts:source'].map(entry=>draft(...entry.split(':')));
  const shape=nodes=>nodes.map(node=>node.kind==='folder' ? {[node.path+'/']:shape(node.children)} : node.draft.path);
  assert.deepEqual(shape(fileTree(drafts)),[{'images/':['images/logo.png']},{'src/':[{'src/lib/':['src/lib/a.ts','src/lib/b.ts']},'src/A.ts']},'main.ts','Zeta.ts']);
  assert.deepEqual(folderAncestors('src/lib/a.ts'),['src','src/lib']);
  assert.deepEqual(folderAncestors('main.ts'),[]);
});

test('schema form edits keep unsupported members until a deliberate repair and keep field order and required names',()=>{
  const root={version:1,type:'object',additionalProperties:false,required:['b','a'],properties:{
    a:{type:'number',minimum:0,maximum:1,default:0.5,format:'percent'},
    b:{type:'string',enum:['x','x']},
    'c.d':{type:'object',properties:{},required:[],additionalProperties:true}}};
  assert.deepEqual(schemaIssues(root).map(({path,issue})=>[path,issue.code]),[['$.a','keyword'],['$.b','enum'],['$["c.d"]','additionalProperties']]);
  // A type change drops the old constraints and default but not the unsupported member.
  assert.deepEqual(withType(root.properties.a,'integer'),{type:'integer',minimum:0,maximum:1,format:'percent'});
  assert.deepEqual(withType(root.properties.a,'array'),{type:'array',format:'percent',items:{type:'string'}});
  assert.deepEqual(repairNode(root.properties.a,{code:'keyword',key:'format'}),{type:'number',minimum:0,maximum:1,default:0.5});
  assert.deepEqual(repairNode(root.properties.b,{code:'enum'}),{type:'string',enum:['x']});
  const renamed=renameProperty(root,'a','ratio');
  assert.deepEqual(Object.keys(renamed.properties),['ratio','b','c.d']);
  assert.deepEqual(renamed.required,['b','ratio']);
  const removed=withoutProperty(renamed,'b');
  assert.deepEqual([Object.keys(removed.properties),removed.required],[['ratio','c.d'],['ratio']]);
  assert.deepEqual(Object.keys(withProperty(root,'a',{type:'boolean'}).properties),['a','b','c.d']);
  assert.deepEqual(nodeIssues({type:'string',minLength:2,maxLength:1}).map(issue=>issue.code),['reversed']);
  assert.deepEqual(nodeIssues({version:2,type:'array',items:{type:'string'}},true).map(issue=>issue.code),['version','rootType']);
  assert.deepEqual(schemaIssues(emptySchema()),[]);
});

test('only top-level defaults are edited as values; nested defaults and other members survive the round trip',()=>{
  const root={version:1,type:'object',additionalProperties:false,required:[],properties:{
    count:{type:'integer',default:1},
    roi:{type:'object',additionalProperties:false,required:[],properties:{x:{type:'integer',default:4}}},
    label:{type:'string'}}};
  const defaults=topDefaults(root);
  assert.deepEqual(defaults,{count:1});
  assert.deepEqual(withTopDefaults(root,defaults),root);
  const next=withTopDefaults(root,{label:'hello'});
  assert.equal(Object.hasOwn(next.properties.count,'default'),false);
  assert.equal(next.properties.label.default,'hello');
  assert.equal(next.properties.roi.properties.x.default,4);
});

test('preset identity problems are reported and repaired one member at a time without touching options',()=>{
  const preset={package_id:'other',schema_version:1,options:{count:'7'},note:'keep until removed'};
  assert.deepEqual(presetIssues(preset,'example.a',1).map(issue=>issue.code),['packageId','member']);
  const repaired=repairPreset(preset,{code:'packageId'},'example.a',1);
  assert.deepEqual(repaired,{package_id:'example.a',schema_version:1,options:{count:'7'},note:'keep until removed'});
  assert.deepEqual(repairPreset(repaired,{code:'member',key:'note'},'example.a',1),{package_id:'example.a',schema_version:1,options:{count:'7'}});
  assert.deepEqual(presetIssues({package_id:'example.a',options:[]},'example.a',1).map(issue=>issue.code),['schemaVersion','options']);
  assert.deepEqual(presetIssues([],'example.a',1),[{code:'root'}]);
});

test('source maps are read as facts and report what the host refuses',()=>{
  const facts=sourceMapFacts({version:3,file:'main.js',sources:['main.ts',4],names:['a'],mappings:'AAAA',sourceRoot:'/abs',sourcesContent:['x',null]});
  assert.deepEqual([facts.file,facts.sources,facts.names,facts.mappings,facts.embedded],['main.js',['main.ts'],1,4,1]);
  assert.deepEqual(facts.issues,[{code:'source',index:1},{code:'sourceRoot'}]);
  assert.deepEqual(sourceMapFacts({version:3,sources:[],names:[],mappings:''}).issues,[]);
});

test('a schema change makes existing strings under newly numeric fields stored mismatches that unrelated edits keep',()=>{
  const before={type:'object',additionalProperties:false,required:[],properties:{count:{type:'string'},message:{type:'string'}}};
  const after={...before,properties:{...before.properties,count:{type:'integer'}}};
  const options={count:'7',message:'hi'};
  // Refresh (or a schema edit) brings a new schema while the preset options are unchanged.
  let state=currentValues(loadValues(before,options),after,options);
  assert.deepEqual(state.stored.map(entry=>entry.path),['$.count']);
  state=editValues(state,{...state.draft,message:'hello'},{kind:'replace',path:'$.message'});
  assert.deepEqual(state.committed,{count:'7',message:'hello'});
  // Only a deliberate replacement makes the field ordinary numeric text.
  state=editValues(state,{...state.draft,count:''},{kind:'replace',path:'$.count'});
  state=editValues(state,{...state.draft,count:'8'},{kind:'replace',path:'$.count'});
  assert.deepEqual(state.committed,{count:8,message:'hello'});
});

test('text being typed into a field that stays numeric remains editable across schema rewrites; a field that stops being numeric keeps the document value',()=>{
  const number={type:'object',additionalProperties:false,required:[],properties:{ratio:{type:'number'}}};
  let typing=editValues(loadValues(number,{}),{ratio:'1.'},{kind:'replace',path:'$.ratio'});
  assert.deepEqual(typing.committed,{ratio:'1.'});
  // A defaults edit rewrites the schema itself, so the editor sees a new schema (and the recorded provenance) after
  // each keystroke.
  const rewritten={...number,properties:{ratio:{type:'number',default:'1.'}}};
  const mounted=currentValues(typing,rewritten,{ratio:'1.'},typedText(typing));
  assert.deepEqual([mounted.draft,mounted.stored],[{ratio:'1.'},[]]);
  // Discard clears the recorded provenance: a still-mounted form reads the same text as stored data.
  assert.deepEqual(currentValues(typing,rewritten,{ratio:'1.'},[]).stored.map(entry=>entry.path),['$.ratio']);
  typing=editValues(mounted,{ratio:'1.5'},{kind:'replace',path:'$.ratio'});
  assert.deepEqual(typing.committed,{ratio:1.5});
  let typed=editValues(loadValues(number,{}),{ratio:'12'},{kind:'replace',path:'$.ratio'});
  typed=currentValues(typed,{...number,properties:{ratio:{type:'string'}}},{ratio:12});
  assert.deepEqual(typed.draft,{ratio:12});
});

test('a __proto__ field is an own member in the schema, its required list and option values',()=>{
  const schema=withRequired(withProperty(emptySchema(),'__proto__',{type:'string'}),'__proto__',true);
  assert.equal(Object.hasOwn(schema.properties,'__proto__'),true);
  assert.deepEqual(schema.required,['__proto__']);
  assert.deepEqual(schemaIssues(schema),[]);
  const state=editValues(loadValues(schema,{}),{['__proto__']:'value'},{kind:'replace',path:'$.__proto__'});
  assert.equal(Object.hasOwn(state.committed,'__proto__'),true);
  const written=parseJson(formatJson(null,{package_id:'example.a',schema_version:1,options:state.committed})).value;
  assert.equal(Object.hasOwn(written.options,'__proto__'),true);
  assert.equal(written.options.__proto__,'value');
});

test('typed constraints become numbers only when exact; imprecise or invalid text stays visible text',()=>{
  assert.equal(constraintValue(' 12 '),12);
  assert.equal(constraintValue('1e3'),1000);
  assert.equal(constraintValue('1.5'),1.5);
  assert.equal(constraintValue('9007199254740992'),9007199254740992);
  for (const text of ['9007199254740993','9.007199254740993e15','1e400','-0','1.','abc']) assert.equal(constraintValue(text),text);
  assert.equal(constraintValue('  '),undefined);
  // The kept text is reported as an invalid bound instead of being saved as a rounded number.
  assert.deepEqual(nodeIssues({type:'array',items:{type:'string'},minItems:constraintValue('9007199254740993')}).map(issue=>issue.code),['bound']);
});

const RATIO_SCHEMA='{"version":1,"type":"object","additionalProperties":false,"required":[],"properties":{"ratio":{"type":"number"}}}';
const PRESET='{"package_id":"example.a","schema_version":1,"options":{}}';
const PRESET_PATH='profiles/default.json';
function presetFiles(preset=PRESET){
  return [file('package.json','manifest',MANIFEST),file('main.ts','source','export {};\n'),file('schema.json','schema',RATIO_SCHEMA),file(PRESET_PATH,'profile',preset)];
}
// What the preset form does when shown: load from the draft and its provenance, apply one edit, write the document.
function typeOption(state,text){
  const draft=state.drafts.get(PRESET_PATH);
  const preset=value(draft.text);
  const form=editValues(loadValues(value(RATIO_SCHEMA),preset.options,draft.typed),{...preset.options,ratio:text},{kind:'replace',path:'$.ratio'});
  return replaceFile(state,PRESET_PATH,formatJson(draft.base,{...preset,options:form.committed}),typedText(form));
}
function shown(state){
  const draft=state.drafts.get(PRESET_PATH);
  return loadValues(value(RATIO_SCHEMA),value(draft.text).options,draft.typed);
}

test('typed numeric form text stays editable after leaving the form and returning; loaded strings stay mismatches',()=>{
  let state=selectFile(openSession(view(presetFiles())),PRESET_PATH,null);
  state=typeOption(state,'1.');
  assert.deepEqual(state.drafts.get(PRESET_PATH).typed,[{path:'$.ratio',text:'1.'}]);
  // Another file (or Logs, settings, Return to Edit) unmounts the form; the session keeps the provenance.
  state=selectFile(selectFile(state,'main.ts',null),PRESET_PATH,null);
  assert.deepEqual([shown(state).stored,shown(state).draft.ratio],[[],'1.']);
  state=typeOption(state,'1.5');
  assert.deepEqual([value(state.drafts.get(PRESET_PATH).text).options.ratio,state.drafts.get(PRESET_PATH).typed],[1.5,[]]);
  // The same bytes read from disk are stored data for deliberate replacement.
  let loaded=openSession(view(presetFiles('{"package_id":"example.a","schema_version":1,"options":{"ratio":"1."}}')));
  assert.deepEqual(shown(loaded).stored.map(entry=>entry.path),['$.ratio']);
  // Replacing it and typing the same text again makes it the operator's text, although the file is clean again.
  loaded=typeOption(typeOption(loaded,''),'1.');
  assert.equal(fileDirty(loaded.drafts.get(PRESET_PATH)),false);
  assert.deepEqual(shown(loaded).stored,[]);
});

test('Discard, a changed disk read and a new session do not resurrect typed provenance; Save and other structured edits keep it',()=>{
  let state=typeOption(openSession(view(presetFiles())),'1.');
  const draft=state.drafts.get(PRESET_PATH);
  state=replaceFile(state,PRESET_PATH,formatJson(draft.base,{...value(draft.text),note:'kept until repaired'}));
  assert.deepEqual(state.drafts.get(PRESET_PATH).typed,[{path:'$.ratio',text:'1.'}]);
  assert.deepEqual(discardFile(state,PRESET_PATH).drafts.get(PRESET_PATH).typed,[]);
  // Save commits the typed text; the refreshed read of the same bytes keeps it editable.
  state=typeOption(openSession(view(presetFiles())),'1.');
  const ticket=saveTicket(state,PRESET_PATH);
  state=applySave(state,ticket,{owner,committed_revision:'rev-2',view:view(presetFiles(ticket.text),'rev-2'),refresh_error:null});
  assert.deepEqual(shown(state).stored,[]);
  // Other bytes on disk replace the clean draft together with its provenance.
  state=applyRefresh(state,view(presetFiles('{"package_id":"example.a","schema_version":1,"options":{"ratio":"2."}}'),'rev-3'),true);
  assert.deepEqual(state.drafts.get(PRESET_PATH).typed,[]);
  assert.deepEqual(shown(state).stored.map(entry=>entry.path),['$.ratio']);
  // Duplicate, Return to Edit without a local view, or another owner opens a new session from the saved bytes.
  assert.deepEqual(shown(openSession(view(presetFiles(ticket.text)))).stored.map(entry=>entry.path),['$.ratio']);
});

test('form Undo and Redo restore incomplete numeric text with its provenance in chronological steps',()=>{
  let state=typeOption(typeOption(openSession(view(presetFiles())),'1.'),'1.5');
  assert.deepEqual(state.drafts.get(PRESET_PATH).typed,[]);
  state=undoFile(state,PRESET_PATH);
  // Editable operator text again, not a stored type mismatch.
  assert.deepEqual([shown(state).stored,shown(state).draft.ratio],[[],'1.']);
  state=redoFile(state,PRESET_PATH);
  assert.deepEqual([value(state.drafts.get(PRESET_PATH).text).options.ratio,state.drafts.get(PRESET_PATH).typed],[1.5,[]]);
  state=discardFile(typeOption(state,'2.'),PRESET_PATH);
  state=undoFile(state,PRESET_PATH);
  assert.deepEqual([shown(state).stored,shown(state).draft.ratio],[[],'2.']);
  // A form IME composition follows the input and becomes one step when it ends.
  state=beginComposition(state,PRESET_PATH,{start:0,end:0});
  state=typeOption(typeOption(state,'2.5'),'2.75');
  assert.equal(value(state.drafts.get(PRESET_PATH).text).options.ratio,2.75);
  state=endComposition(state,PRESET_PATH);
  state=undoFile(state,PRESET_PATH);
  assert.deepEqual([shown(state).stored,shown(state).draft.ratio],[[],'2.']);
});

test('same-byte numeric provenance is undoable, including a composition, without undoing an earlier form edit',()=>{
  const saved='{"package_id":"example.a","schema_version":1,"options":{"ratio":"1."}}';
  const initial=openSession(view(presetFiles(saved)));
  assert.deepEqual(shown(initial).stored.map(entry=>entry.path),['$.ratio']);
  let state=typeOption(initial,'1.');
  assert.equal(state.drafts.get(PRESET_PATH).text,saved);
  assert.deepEqual([shown(state).stored,shown(state).draft.ratio],[[],'1.']);
  state=typeOption(state,'1.5');
  state=undoFile(state,PRESET_PATH);
  assert.deepEqual([shown(state).stored,shown(state).draft.ratio],[[],'1.']);
  state=undoFile(state,PRESET_PATH);
  assert.deepEqual(shown(state).stored.map(entry=>entry.path),['$.ratio']);
  state=redoFile(state,PRESET_PATH);
  assert.deepEqual([shown(state).stored,shown(state).draft.ratio],[[],'1.']);
  state=undoFile(state,PRESET_PATH);
  state=beginComposition(state,PRESET_PATH,{start:0,end:0});
  state=typeOption(state,'1.');
  state=endComposition(state,PRESET_PATH);
  state=undoFile(state,PRESET_PATH);
  assert.deepEqual(shown(state).stored.map(entry=>entry.path),['$.ratio']);
  state=redoFile(state,PRESET_PATH);
  assert.deepEqual([shown(state).stored,shown(state).draft.ratio],[[],'1.']);
  // The discarded redo branch (the earlier 1.5 edit) must not return after the new composition.
  assert.equal(redoFile(state,PRESET_PATH),state);
});

// A saved manifest (two-space style) with release/search metadata, internal tags and a declared target. Its keys are
// valid but not canonical, as an author may have written them by hand.
const DISTRIBUTION={version:1,release_version:'1.2.0-rc.1+build.7',name:'Example',description:'<b>Plain</b> text',author_name:'A',license:'MIT',
  game:{id:'example-game',names:{EN:'Example','zh-hant':'範例'},aliases:['EX','Example.exe']},tags:['internal','kept']};
const DISTRIBUTED=JSON.stringify({...value(MANIFEST),target:{id:'game',window_title:'Game'},distribution:DISTRIBUTION},null,2)+'\n';
function distributedSession(text=DISTRIBUTED){
  return openSession(view([file('package.json','manifest',text),file('main.ts','source','export {};\n')]));
}
// What the manifest form does: rewrite the whole draft from the edited value, with the saved text's style.
function formEdit(state,change){
  const current=state.drafts.get('package.json');
  return replaceFile(state,'package.json',formatJson(current.base,change(value(current.text))));
}
const manifestOf=state=>value(state.drafts.get('package.json').text);
const rowsOf=manifest=>readDistribution(manifest.distribution).game.names;

test('opening distribution metadata never normalizes it, and a form edit keeps tags, target and every other member',()=>{
  let state=distributedSession();
  const opened=state.drafts.get('package.json');
  const distribution=readDistribution(manifestOf(state).distribution);
  assert.deepEqual(distribution.game.names,[{key:'EN',name:'Example'},{key:'zh-hant',name:'範例'}]);
  assert.deepEqual(nameRowIssues(distribution.game.names).map(issue=>issue.key),[{code:'canonical',canonical:'en'},{code:'canonical',canonical:'zh-Hant'}]);
  assert.deepEqual([distribution.issues,distribution.pairs],[[],false]);
  assert.deepEqual([opened.text,fileDirty(opened),opened.undo.length],[DISTRIBUTED,false,0],'reading the form changes nothing');
  state=formEdit(state,manifest=>withDistributionText(manifest,'name','Example 2'));
  const edited=manifestOf(state);
  assert.deepEqual([edited.distribution.tags,edited.target,edited.entries,edited.distribution.game.names,edited.distribution.description],
    [['internal','kept'],{id:'game',window_title:'Game'},value(MANIFEST).entries,{EN:'Example','zh-hant':'範例'},'<b>Plain</b> text']);
  assert.match(state.drafts.get('package.json').text,/^\{\n {2}"version": 1,\n/);
  // The deliberate commit of one key normalizes only that key.
  state=formEdit(state,manifest=>withNameRows(manifest,rowsOf(manifest).map(row=>row.key==='EN' ? {...row,key:'en'} : row)));
  assert.deepEqual(Object.entries(manifestOf(state).distribution.game.names),[['en','Example'],['zh-hant','範例']]);
  // Restoring every value is the saved text again.
  state=formEdit(formEdit(state,manifest=>withNameRows(manifest,[{key:'EN',name:'Example'},{key:'zh-hant',name:'範例'}])),
    manifest=>withDistributionText(manifest,'name','Example'));
  assert.equal(fileDirty(state.drafts.get('package.json')),false);
});

test('empty, conflicting and array-index name rows stay in the manifest draft in order without overwrite',()=>{
  let manifest={...value(MANIFEST),distribution:starterDistribution()};
  manifest=withNameRows(manifest,[{key:'',name:''}]);
  assert.deepEqual(manifest.distribution.game.names,{'':''});
  manifest=withNameRows(manifest,[{key:'',name:''},{key:'',name:''}]);
  assert.deepEqual(manifest.distribution.game.names,[['',''],['','']],'a second new row cannot share the first one\'s member');
  const typed=[{key:'en',name:'Example'},{key:'1',name:'One'},{key:'en',name:'Sample'}];
  manifest=withNameRows(manifest,typed);
  assert.deepEqual(manifest.distribution.game.names,[['en','Example'],['1','One'],['en','Sample']]);
  assert.deepEqual([rowsOf(manifest),readDistribution(manifest.distribution).pairs],[typed,true]);
  assert.deepEqual(nameRowIssues(typed).map(issue=>issue.key),[{code:'duplicate',row:3},{code:'invalid'},{code:'duplicate',row:1}]);
  // Distinct keys are an object again, in row order; canonically equal keys still conflict visibly.
  const resolved=[{key:'en',name:'Example'},{key:'ja',name:'例'},{key:'EN',name:'Sample'}];
  manifest=withNameRows(manifest,resolved);
  assert.deepEqual(Object.entries(manifest.distribution.game.names),[['en','Example'],['ja','例'],['EN','Sample']]);
  assert.equal(readDistribution(manifest.distribution).pairs,false);
  assert.deepEqual(nameRowIssues(resolved).map(issue=>issue.key),[{code:'duplicate',row:3},null,{code:'duplicate',row:1}]);
  // A pure array index would be moved first by an object, so it stays a pair; `__proto__` is an ordinary member.
  assert.deepEqual(withNameRows(manifest,[{key:'ja',name:'例'},{key:'7',name:'x'}]).distribution.game.names,[['ja','例'],['7','x']]);
  const proto=withNameRows(manifest,[{key:'__proto__',name:'x'}]).distribution.game.names;
  assert.deepEqual([Object.hasOwn(proto,'__proto__'),Object.getPrototypeOf(proto)],[true,Object.prototype]);
  // Valid keys outside the suggestions are accepted; names are required and bounded.
  assert.deepEqual(nameRowIssues([{key:'fr-CA',name:'Exemple'},{key:'sl-biske-rozaj',name:'x'}]),[{key:null,name:null},{key:null,name:null}]);
  assert.deepEqual(nameRowIssues([{key:'ja',name:''},{key:'ko',name:'x'.repeat(257)},{key:'',name:'y'}]).map(issue=>[issue.key,issue.name]),
    [[null,'empty'],[null,'length'],[{code:'empty'},null]]);
});

test('selected-resource history, Save refusal and a delayed Save keep conflicting rows and tags without touching another file',()=>{
  let state=distributedSession();
  state=editFile(state,'main.ts',{text:'export {};\n// x\n',start:16,end:16},{start:11,end:11},{type:'insertText',data:'// x\n',composing:false});
  const main=state.drafts.get('main.ts');
  state=formEdit(state,manifest=>withNameRows(manifest,[...rowsOf(manifest),{key:'',name:''}]));
  const added=state.drafts.get('package.json').text;
  state=formEdit(state,manifest=>withNameRows(manifest,rowsOf(manifest).map((row,index)=>index===2 ? {key:'EN',name:'Sample'} : row)));
  const conflicting=state.drafts.get('package.json').text;
  assert.deepEqual(manifestOf(state).distribution.game.names,[['EN','Example'],['zh-hant','範例'],['EN','Sample']]);
  // Another field's edit keeps the conflicting rows and the tags.
  state=formEdit(state,manifest=>withHelper(manifest,true));
  assert.deepEqual([manifestOf(state).distribution.game.names[2],manifestOf(state).distribution.tags],[['EN','Sample'],['internal','kept']]);
  // The host refuses a Save of the ambiguous map; the draft and its history stay.
  const refused=saveTicket(state,'package.json');
  state=failCommand(beginPending(state,{kind:'save',path:'package.json'}),refused.token,{category:'Inventory',message:'invalid package.json',context:null});
  assert.deepEqual([state.drafts.get('package.json').text,state.pending],[refused.text,null]);
  state=undoFile(state,'package.json');
  assert.equal(state.drafts.get('package.json').text,conflicting);
  state=undoFile(state,'package.json');
  assert.equal(state.drafts.get('package.json').text,added);
  state=undoFile(state,'package.json');
  assert.equal(state.drafts.get('package.json').text,DISTRIBUTED);
  state=redoFile(redoFile(state,'package.json'),'package.json');
  assert.equal(state.drafts.get('package.json').text,conflicting);
  assert.equal(state.drafts.get('main.ts'),main,'the manifest history never touches another resource');
  // A Save of revision A answered after draft B leaves B dirty on base A.
  state=formEdit(state,manifest=>withNameRows(manifest,[{key:'en',name:'Example'},{key:'zh-Hant',name:'範例'}]));
  const ticket=saveTicket(state,'package.json');
  state=beginPending(state,{kind:'save',path:'package.json'});
  state=formEdit(state,manifest=>withNameRows(manifest,[...rowsOf(manifest),{key:'en',name:'Later'}]));
  const later=state.drafts.get('package.json').text;
  state=applySave(state,ticket,{owner,committed_revision:'rev-2',view:null,refresh_error:null});
  const saved=state.drafts.get('package.json');
  assert.deepEqual([saved.base,saved.text,fileDirty(saved),state.revision],[ticket.text,later,true,'rev-2']);
  assert.deepEqual(manifestOf(state).distribution.game.names,[['en','Example'],['zh-Hant','範例'],['en','Later']]);
  // Discard restores the committed A; Undo brings B back.
  state=undoFile(discardFile(state,'package.json'),'package.json');
  assert.equal(state.drafts.get('package.json').text,later);
});

test('distribution text, aliases and repairs follow the host bounds and change only what they name',()=>{
  for (const [part,text,issue] of [
    ['release_version','1.2.3',null],['release_version','1.0.0-alpha.1+build.001',null],['release_version','18446744073709551616.0.0',null],
    ['release_version','',`required`],['release_version','1.2',`format`],['release_version','01.2.3','format'],['release_version','1.2.3-01','format'],
    ['release_version','v1.2.3','format'],['release_version',`1.0.0+${'a'.repeat(123)}`,'length'],
    ['name','','required'],['name','名'.repeat(85),null],['name','名'.repeat(86),'length'],['author_name','','required'],['license','','required'],
    ['description','',null],['description','x'.repeat(8193),'length'],['game_id','','required'],['game_id','example-game',null],['game_id','.hidden','format'],
  ]) assert.equal(distributionTextIssue(part,text),issue,`${part}: ${text.slice(0,24)}`);
  assert.deepEqual(['EX','Example.exe','','x'.repeat(257),'/Applications/Game.app','~/game','C:\\Games\\game.exe','\\\\server\\share','game/launcher']
    .map(aliasIssue),[null,null,'empty','length','path','path','path','path',null]);
  const manifest={version:1,package_id:'example.a',distribution:{version:2,name:7,extra:true,game:{id:'g',names:'en',aliases:['A',1],other:1},tags:['kept']}};
  const found=readDistribution(manifest.distribution);
  assert.deepEqual(found.issues,[{code:'version'},{code:'member',key:'extra'},{code:'type',key:'name'},{code:'gameMember',key:'other'},
    {code:'type',key:'names'},{code:'type',key:'aliases'}]);
  assert.deepEqual([found.game,found.text.name,found.text.game_id],[{names:null,aliases:null},'','g']);
  const repaired=found.issues.reduce(repairDistribution,manifest);
  assert.deepEqual(repaired.distribution,{version:1,name:'',game:{id:'g',names:{},aliases:[]},tags:['kept']});
  assert.deepEqual(readDistribution({...manifest.distribution,game:'x'}).game,null);
  assert.deepEqual(repairDistribution({distribution:{game:'x'}},{code:'type',key:'game'}).distribution.game,{id:'',names:{},aliases:[]});
  const numbered={distribution:{version:1,game:{id:5,names:{},aliases:[]}}};
  assert.deepEqual([readDistribution(numbered.distribution).issues,readDistribution(numbered.distribution).text.game_id],[[{code:'type',key:'id'}],'']);
  assert.deepEqual(repairDistribution(numbered,{code:'type',key:'id'}).distribution.game,{id:'',names:{},aliases:[]});
  // Aliases are replaced as a list; removing the whole object is explicit.
  assert.deepEqual(withAliases(manifest,['B']).distribution.game,{id:'g',names:'en',aliases:['B'],other:1});
  assert.equal(Object.hasOwn(withDistribution(manifest,false),'distribution'),false);
  assert.equal(withDistribution(manifest,true),manifest);
});
