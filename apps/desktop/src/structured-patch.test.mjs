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
