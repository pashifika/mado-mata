import {test} from 'node:test';
import assert from 'node:assert/strict';
import {applyRefresh,applySave,discardFile,fileDirty,openSession,replaceFile,saveTicket,selectFile} from './authoring.ts';
import {constraintValue,currentValues,editValues,emptySchema,fileTree,folderAncestors,formatJson,loadValues,nodeIssues,parseJson,presetIssues,readManifest,renameProperty,typedText,
  repairNode,repairPreset,schemaIssues,sourceMapFacts,topDefaults,withEntry,withHelper,withProperty,withRequired,withTarget,withTopDefaults,withType,withoutProperty} from './metadata.ts';

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
  assert.equal(manifest.undo.length,0);
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
  state=applyRefresh(state,view(presetFiles('{"package_id":"example.a","schema_version":1,"options":{"ratio":"2."}}'),'rev-3'));
  assert.deepEqual(state.drafts.get(PRESET_PATH).typed,[]);
  assert.deepEqual(shown(state).stored.map(entry=>entry.path),['$.ratio']);
  // Duplicate, Return to Edit without a local view, or another owner opens a new session from the saved bytes.
  assert.deepEqual(shown(openSession(view(presetFiles(ticket.text)))).stored.map(entry=>entry.path),['$.ratio']);
});
