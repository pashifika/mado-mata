import {test} from 'node:test';
import assert from 'node:assert/strict';
import {parseField, patchMetadata, patchRecognition} from './structured-patch.ts';
import {MAX_EXPECTED_BYTES, mapRegion, openRecognition, regionFromEdges, renameDefinition, toggleTrial, undoRecognition} from './recognition.ts';

const owner={workspace:{workspace_id:'a',revision:1},token:'lease-1'};
const BASIS={frame_width:200,frame_height:100,content:{x:0,y:0,width:200,height:100}};
const box=(left,top,right,bottom)=>({left,top,right,bottom});
function definition(id,name,edges,kind='ocr'){
  return {id,name,revision:1,kind,region:regionFromEdges(edges,BASIS),expected:null,
    template:kind==='template'?{search_region:{u0:0,v0:0,u1:1,v1:1},threshold:0.9,max_results:8}:null,saved:null};
}
function loaded(){
  const document={version:1,rounding:1,basis:BASIS,definitions:[definition('r1','HP',box(10,10,30,30)),definition('r2','MP',box(50,10,70,30))],template_rights:null};
  return openRecognition({owner,revision:'rev-1',document,saved_document:document,capture_id:'c1',captures:[{capture_id:'c1',document}],
    saved_captures:[{capture_id:'c1',document}],migration_required:false,document_revision:1,basis_confirmed:true,other_bases_confirmed:true,
    frame:{id:'f1',width:200,height:100,revision:1,confirmed:true,historical:false,historical_capture_at_ms:null},
    capabilities:{max_ocr_zones:8,diagnostic_regions:256,diagnostic_bytes:262144,expected_bytes:4096,image_policy:{}},
    staged_crop_ids:[],stale_crop_ids:[],staged_crop_sources:{},configuration_revision:'cfg-1',trial:null});
}
const fields=(...items)=>items.map(item=>{
  const parsed=parseField(item);
  assert.notEqual(parsed,null,JSON.stringify(item));
  return parsed;
});

test('field operations accept only exact typed shapes and never a saved-asset reference',()=>{
  for (const item of [
    {field:'name'},{field:'name',value:'x',extra:1},{field:'name',value:'\ud800'},{field:'saved',value:{asset:'a'}},
    {field:'region',edges:{left:0,top:0,right:1}},{field:'region',edges:{left:0.5,top:0,right:1,bottom:1}},
    {field:'bound',node:['a'],key:'minimum',value:Number.NaN},{field:'option',name:'a',value:undefined},
    {field:'entry',entry:'other',part:'module',value:'main.ts'},{field:'repair',node:[],issue:{code:'keyword',key:1}},
    {field:'kind',value:'pixel'},{field:'rights',rights:{license:'x',created_by:'y',reviewed:true}},null,
  ]) assert.equal(parseField(item),null,JSON.stringify(item));
  assert.deepEqual(parseField({field:'target',target:{id:'game',windowTitle:'',bundleId:'com.example'}}),
    {field:'target',target:{id:'game',windowTitle:null,bundleId:'com.example'}});
  assert.deepEqual(parseField({field:'repair',issue:{code:'member',key:'note'}}),{field:'repair',issue:{code:'member',key:'note'}});
});

test('schema fields change only the addressed node and keep unsupported members and saved layout',()=>{
  const saved='{\n\t"version": 1,\n\t"type": "object",\n\t"additionalProperties": false,\n\t"required": [],\n\t"properties": {\n'
    +'\t\t"ratio": {"type": "number", "format": "percent"},\n\t\t"list": {"type": "array", "items": {"type": "string", "enum": ["a"]}}\n\t}\n}\n';
  const scope={packageId:'example.a',schema:null};
  const changed=patchMetadata('schema',saved,saved,fields(
    {field:'bound',node:['ratio'],key:'minimum',value:0},
    {field:'enum',node:['list',null],values:['a','b']},
    {field:'addProperty',node:[],name:'count',type:'integer'},
    {field:'required',node:[],name:'count',required:true}),scope);
  const value=JSON.parse(changed);
  assert.deepEqual(value.properties.ratio,{type:'number',format:'percent',minimum:0});
  assert.deepEqual(value.properties.list.items.enum,['a','b']);
  assert.deepEqual(value.properties.count,{type:'integer'});
  assert.deepEqual(value.required,['count']);
  assert.match(changed,/^\{\n\t"version"/);
  // Restoring every value reproduces the saved bytes.
  const restored=patchMetadata('schema',changed,saved,fields(
    {field:'removeProperty',node:[],name:'count'},{field:'bound',node:['ratio'],key:'minimum',value:null},{field:'enum',node:['list',null],values:['a']}),scope);
  assert.equal(restored,saved);
  // Only reported issues repair; defaults wait until the schema has none; nodes must exist.
  assert.equal(JSON.parse(patchMetadata('schema',saved,saved,fields({field:'repair',node:['ratio'],issue:{code:'keyword',key:'format'}}),scope))
    .properties.ratio.format,undefined);
  for (const [field,code] of [
    [{field:'repair',node:['ratio'],issue:{code:'keyword',key:'type'}},'invalid_field'],
    [{field:'default',name:'ratio',value:1},'invalid_field'],
    [{field:'addProperty',node:[],name:'ratio',type:'string'},'invalid_field'],
    [{field:'bound',node:['missing'],key:'minimum',value:1},'invalid_field'],
    [{field:'type',node:[],type:'string'},'invalid_field'],
    [{field:'helper',enabled:true},'unsupported_field'],
  ]) assert.equal(patchMetadata('schema',saved,saved,fields(field),scope).code,code,JSON.stringify(field));
  assert.equal(patchMetadata('schema','{"type": "object",',null,fields({field:'addProperty',node:[],name:'a',type:'string'}),scope).code,'malformed_document');
});

test('manifest and preset fields follow their forms, including the current options schema draft',()=>{
  const manifest=JSON.stringify({version:1,package_id:'example.a',runtime:'typescript',entries:{workflow:{module:'main.ts',function:'workflow'}},
    sources:['main.ts','types.d.ts'],custom:{kept:true}});
  const next=JSON.parse(patchMetadata('manifest',manifest,manifest,fields(
    {field:'entry',entry:'workflow',part:'function',value:'run'},{field:'helper',enabled:true},
    {field:'target',target:{id:'game',windowTitle:null,bundleId:null}}),{packageId:'example.a',schema:null}));
  assert.deepEqual([next.entries.workflow,next.dependencies,next.target,next.custom],
    [{module:'main.ts',function:'run'},{'@mado/helper':'1.0.0'},{id:'game',window_title:null},{kept:true}]);
  assert.equal(patchMetadata('manifest',manifest,manifest,fields({field:'entry',entry:'workflow',part:'module',value:'types.d.ts'}),{packageId:'example.a',schema:null}).code,
    'invalid_field');
  const schema='{"version":1,"type":"object","additionalProperties":false,"required":[],"properties":{"ratio":{"type":"number"},"label":{"type":"string"}}}';
  const preset='{"package_id":"example.a","schema_version":1,"options":{"ratio":"1."},"note":"kept"}';
  const scope={packageId:'example.a',schema};
  const written=JSON.parse(patchMetadata('profile',preset,preset,fields({field:'option',name:'label',value:'fast'}),scope));
  assert.deepEqual(written,{package_id:'example.a',schema_version:1,options:{ratio:'1.',label:'fast'},note:'kept'});
  assert.equal(patchMetadata('profile',preset,preset,fields({field:'option',name:'undeclared',value:1}),scope).code,'invalid_field');
  assert.equal(patchMetadata('profile',preset,preset,fields({field:'option',name:'label',value:'x'}),{...scope,schema:'{"type":"string"}'}).code,'invalid_field');
  assert.equal(Object.hasOwn(JSON.parse(patchMetadata('profile',preset,preset,fields({field:'repair',issue:{code:'member',key:'note'}}),scope)),'note'),false);
  assert.equal(patchMetadata('profile',preset,preset,fields({field:'repair',issue:{code:'packageId'}}),scope).code,'invalid_field');
});

// A saved manifest with complete distribution metadata, internal tags and a declared target.
const DISTRIBUTED={version:1,package_id:'example.a',runtime:'typescript',sources:['main.ts'],target:{id:'game',window_title:'Game'},
  distribution:{version:1,release_version:'1.2.0-beta.1+build.5',name:'Example',description:'<b>text</b>',author_name:'A',license:'MIT',
    game:{id:'example-game',names:{en:'Example',ja:'例'},aliases:['EX','Example.exe']},tags:['internal','kept']}};
const MANIFEST_SCOPE={packageId:'example.a',schema:null};

test('distribution field operations accept only their exact typed shapes',()=>{
  for (const item of [
    {field:'distribution'},{field:'distribution',declared:'yes'},{field:'distribution',declared:true,part:'name',value:'x'},
    {field:'distribution',part:'tags',value:'x'},{field:'distribution',part:'version',value:'1'},{field:'distribution',part:'name',value:1},
    {field:'gameName',key:'en'},{field:'gameName',key:'en',name:1},{field:'gameName',key:'\ud800',name:'x'},
    {field:'gameAliases',aliases:'EX'},{field:'gameAliases',aliases:[1]},{field:'tags',tags:['x']},
  ]) assert.equal(parseField(item),null,JSON.stringify(item));
  assert.deepEqual(parseField({field:'distribution',declared:false}),{field:'distribution',declared:false});
  assert.deepEqual(parseField({field:'distribution',part:'game_id',value:'g'}),{field:'distribution',part:'game_id',value:'g'});
  assert.deepEqual(parseField({field:'gameName',key:'pt-br',name:null}),{field:'gameName',key:'pt-br',name:null});
  assert.deepEqual(parseField({field:'gameAliases',aliases:['EX']}),{field:'gameAliases',aliases:['EX']});
});

test('every other manifest field edit keeps distribution metadata and internal tags exactly',()=>{
  const saved=JSON.stringify(DISTRIBUTED);
  const next=JSON.parse(patchMetadata('manifest',saved,saved,fields({field:'helper',enabled:true},{field:'runtime',value:'javascript'},
    {field:'target',target:{id:'game',windowTitle:'Game',bundleId:'com.example.game'}}),MANIFEST_SCOPE));
  assert.deepEqual(next.distribution,DISTRIBUTED.distribution);
  // A distribution edit never touches the target declaration or the tags.
  const edited=JSON.parse(patchMetadata('manifest',saved,saved,fields({field:'distribution',part:'game_id',value:'other-game'},
    {field:'distribution',part:'release_version',value:'2.0.0'},{field:'gameAliases',aliases:['OG']}),MANIFEST_SCOPE));
  assert.deepEqual([edited.target,edited.distribution.tags,edited.distribution.game.names],[DISTRIBUTED.target,['internal','kept'],{en:'Example',ja:'例'}]);
  assert.deepEqual([edited.distribution.game.id,edited.distribution.release_version,edited.distribution.game.aliases],['other-game','2.0.0',['OG']]);
  // Removing only the edits restores the saved bytes.
  assert.equal(patchMetadata('manifest',JSON.stringify(edited),saved,fields({field:'distribution',part:'game_id',value:'example-game'},
    {field:'distribution',part:'release_version',value:'1.2.0-beta.1+build.5'},{field:'gameAliases',aliases:['EX','Example.exe']}),MANIFEST_SCOPE),saved);
});

test('a localized name is addressed by its canonical language and never overwrites or merges conflicting rows',()=>{
  const saved=JSON.stringify(DISTRIBUTED);
  const names=(text,...ops)=>{
    const result=patchMetadata('manifest',text,saved,fields(...ops),MANIFEST_SCOPE);
    return typeof result==='string' ? JSON.parse(result).distribution.game.names : result.code;
  };
  assert.deepEqual(names(saved,{field:'gameName',key:'PT_br',name:'Exemplo'},{field:'gameName',key:'EN',name:'Example!'}),
    {en:'Example!',ja:'例',['pt-BR']:'Exemplo'});
  assert.deepEqual(names(saved,{field:'gameName',key:'ja',name:null}),{en:'Example'});
  for (const [op,code] of [
    [{field:'gameName',key:'english',name:'x'},'invalid_field'],
    [{field:'gameName',key:'fr',name:null},'invalid_field'],
  ]) assert.equal(names(saved,op),code,JSON.stringify(op));
  // The person has two rows for the same language (kept as pairs) and an unfinished one: an agent can add another
  // language without touching them, but cannot pick one of the conflicting rows.
  const conflicting=JSON.stringify({...DISTRIBUTED,distribution:{...DISTRIBUTED.distribution,game:{...DISTRIBUTED.distribution.game,
    names:[['en','Example'],['en','Sample'],['','']]}}});
  assert.deepEqual(names(conflicting,{field:'gameName',key:'ko',name:'예'}),[['en','Example'],['en','Sample'],['',''],['ko','예']]);
  assert.equal(names(conflicting,{field:'gameName',key:'en',name:'Other'}),'invalid_field');
  assert.equal(names(conflicting,{field:'gameName',key:'en',name:null}),'invalid_field');
  const capped=JSON.stringify({...DISTRIBUTED,distribution:{...DISTRIBUTED.distribution,game:{...DISTRIBUTED.distribution.game,
    names:Object.fromEntries(Array.from({length:64},(_,index)=>[`en-${String(index).padStart(3,'0')}`,'x']))}}});
  assert.equal(names(capped,{field:'gameName',key:'ja',name:'x'}),'invalid_field');
  assert.equal(patchMetadata('manifest',saved,saved,fields({field:'gameAliases',aliases:Array(65).fill('x')}),MANIFEST_SCOPE).code,'invalid_field');
});

test('declaring and removing distribution metadata is explicit, and malformed parts change only through desktop repairs',()=>{
  const plain=JSON.stringify({version:1,package_id:'example.a',sources:['main.ts']});
  for (const op of [{field:'distribution',part:'name',value:'x'},{field:'gameName',key:'en',name:'x'},{field:'gameAliases',aliases:[]}]) {
    assert.equal(patchMetadata('manifest',plain,plain,fields(op),MANIFEST_SCOPE).code,'invalid_field',JSON.stringify(op));
  }
  const declared=patchMetadata('manifest',plain,plain,fields({field:'distribution',declared:true}),MANIFEST_SCOPE);
  assert.deepEqual(JSON.parse(declared).distribution,{version:1,release_version:'',name:'',description:'',author_name:'',license:'',game:{id:'',names:{},aliases:[]}});
  const saved=JSON.stringify(DISTRIBUTED);
  assert.equal(patchMetadata('manifest',saved,saved,fields({field:'distribution',declared:true}),MANIFEST_SCOPE),saved,'declaring keeps an existing object');
  assert.equal(patchMetadata('manifest',declared,plain,fields({field:'distribution',declared:false}),MANIFEST_SCOPE),plain);
  const malformed=JSON.stringify({...DISTRIBUTED,distribution:{...DISTRIBUTED.distribution,game:'example'}});
  for (const op of [{field:'distribution',part:'game_id',value:'x'},{field:'gameName',key:'en',name:'x'},{field:'gameAliases',aliases:[]}]) {
    assert.equal(patchMetadata('manifest',malformed,malformed,fields(op),MANIFEST_SCOPE).code,'invalid_field',JSON.stringify(op));
  }
  assert.equal(JSON.parse(patchMetadata('manifest',malformed,malformed,fields({field:'distribution',part:'name',value:'New'}),MANIFEST_SCOPE)).distribution.game,'example');
  assert.equal(patchMetadata('manifest',JSON.stringify({...DISTRIBUTED,distribution:'x'}),null,fields({field:'distribution',part:'name',value:'x'}),MANIFEST_SCOPE).code,
    'invalid_field');
});

test('recognition fields reuse geometry, limits, freshness and bounded Undo without taking the person\'s selection',()=>{
  // The person is renaming r1 (an open coalescing group) with r1 selected and one checked trial row.
  const state=toggleTrial(renameDefinition(loaded(),'r1','Health'),'r2');
  assert.equal(state.group,'name:r1');
  const undo=state.undo.length;
  const marks=state.marks.r1;
  const result=patchRecognition(state,{role:'definition',id:'r1'},fields(
    {field:'name',value:'HP bar'},{field:'expected',value:'100/100'},{field:'region',edges:box(12,10,40,32)}));
  const next=result.state;
  const r1=next.document.definitions[0];
  assert.deepEqual([r1.name,r1.expected,mapRegion(r1.region,BASIS)],['HP bar','100/100',box(12,10,40,32)]);
  assert.equal(next.undo.length,undo+3,'each operation is its own step, separate from the person\'s rename');
  assert.equal(next.group,null);
  assert.deepEqual([next.selected,next.trialIds,next.display],[state.selected,state.trialIds,state.display]);
  assert.notEqual(next.marks.r1,marks,'a changed region invalidates earlier trial evidence');
  assert.equal(undoRecognition(next).document.definitions[0].name,'HP bar','Undo reverses the latest operation first');
  // Refusals change nothing and name the existing limit or geometry rule.
  assert.equal(patchRecognition(state,{role:'definition',id:'r1'},fields({field:'expected',value:'x'.repeat(MAX_EXPECTED_BYTES+1)})).code,'invalid_field');
  assert.equal(patchRecognition(state,{role:'definition',id:'r1'},fields({field:'region',edges:box(190,10,210,30)})).code,'invalid_geometry');
  assert.equal(patchRecognition(state,{role:'definition',id:'r1'},fields({field:'search',edges:box(0,0,10,10)})).code,'invalid_field');
  assert.equal(patchRecognition(state,{role:'definition',id:'r1'},fields({field:'delete'},{field:'name',value:'x'})).code,'unknown_resource');
  const created=patchRecognition(state,{role:'context'},fields({field:'create',name:'Gold',edges:box(100,50,140,70)}));
  assert.deepEqual(created.created,['r3']);
  assert.equal(created.state.selected,'r1','creating does not select the new definition for the person');
  assert.deepEqual(mapRegion(created.state.document.definitions[2].region,BASIS),box(100,50,140,70));
  // A content change keeps the person's preview tool.
  const content=patchRecognition({...state,display:{zoom:'fit',tool:'content'}},{role:'basis'},fields({field:'content',content:{x:10,y:0,width:190,height:100}}));
  assert.deepEqual([content.state.document.basis.content,content.state.display.tool],[{x:10,y:0,width:190,height:100},'content']);
  // Same values are no change at all.
  assert.equal(patchRecognition(state,{role:'definition',id:'r2'},fields({field:'name',value:'MP'})).state,state);
});
