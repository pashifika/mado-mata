import {test} from 'node:test';
import assert from 'node:assert/strict';
import {acceptController,retainLogs,defaultDraft,readDraft,verifiedCleanup,cleanupLabel,readEnvironment,environmentDraft,sameEnvironment,staleReasons,boundedText,faultSummary,SUPPORTED_PROFILES} from './state.ts';

test('late predecessor result cannot replace the successor or its preparing state',()=>{
  const current={run:'next',state:'preparing',result:null};
  assert.equal(acceptController(current,{run:'old',state:'terminal',result:{status:'PASS'}},'next'),current);
  const running={run:'next',state:'running',result:null};
  assert.equal(acceptController(current,running,'next'),running);
});

test('retention keeps newest items and trims immediately without mutating old state',()=>{
  const initial={items:[{sequence:1},{sequence:2},{sequence:3}],evicted:0};
  const small=retainLogs(initial,[],1);
  assert.deepEqual(small,{items:[{sequence:3}],evicted:2});
  assert.deepEqual(retainLogs(small,[{sequence:4},{sequence:5}],1),{items:[{sequence:5}],evicted:4});
  assert.equal(initial.items.length,3);
  assert.throws(()=>retainLogs(initial,[],0));
  assert.throws(()=>retainLogs(initial,[],1.5));
});

test('draft defaults do not recursively fill nested required fields',()=>{
  const schema={type:'object',properties:{group:{type:'object',default:{a:1},properties:{b:{type:'integer',default:2}}}}};
  const draft=defaultDraft(schema);
  assert.deepEqual(draft,{group:{a:1}});
  draft.group.a=3;
  assert.equal(schema.properties.group.default.a,1);
});

test('unsafe imported integers block draft submission instead of saving rounded values',()=>{
  const schema={type:'object',properties:{
    counter:{type:'integer'},nested:{type:'array',items:{type:'integer'}},
  }};
  const imported=JSON.parse('{"counter":9007199254740993,"nested":[-9007199254740993]}');
  const rejected=readDraft(schema,imported);
  assert.deepEqual(Object.keys(rejected.errors),['$.counter','$.nested[0]']);
  const supported=readDraft(schema,{counter:Number.MAX_SAFE_INTEGER,nested:[String(Number.MIN_SAFE_INTEGER)]});
  assert.deepEqual(supported.errors,{});
  assert.deepEqual(supported.values,{counter:Number.MAX_SAFE_INTEGER,nested:[Number.MIN_SAFE_INTEGER]});
});

for (const {scenario, type, input} of [
  {scenario:'number text cannot round an exact integer', type:'number', input:'9007199254740993'},
  {scenario:'negative integer text cannot round at the boundary', type:'number', input:'-9007199254740993'},
  {scenario:'integer text cannot round away a fractional tail', type:'integer', input:'1.0000000000000001'},
  {scenario:'integer text cannot underflow to zero', type:'integer', input:'1e-999'},
  {scenario:'number text cannot lose the sign of zero', type:'number', input:'-0.0'},
  {scenario:'an imported negative zero cannot become positive zero', type:'number', input:-0},
  {scenario:'integer text cannot lose the sign of zero', type:'integer', input:'-0e999'},
]) {
  test(scenario,()=>{
    const schema={type:'object',properties:{items:{type:'array',items:{type}}}};
    const result=readDraft(schema,{items:[input]});
    assert.deepEqual(Object.keys(result.errors),['$.items[0]']);
    assert.equal(result.values.items[0],input);
  });
}

test('number drafts preserve finite floats and exactly representable integer text',()=>{
  const schema={type:'object',properties:{values:{type:'array',items:{type:'number'}}}};
  const input=['9007199254740991','9007199254740992','0.1','1e100','1e18','1000000000000000100.0',1e18,1000000000000000100.0];
  const result=readDraft(schema,{values:input});
  assert.deepEqual(result.errors,{});
  assert.deepEqual(result.values.values,[9007199254740991,9007199254740992,0.1,1e100,1e18,1000000000000000100.0,1e18,1000000000000000100.0]);
});

test('integer editor accepts only lossless integral decimal and exponent values',()=>{
  const schema={type:'object',properties:{values:{type:'array',items:{type:'integer'}}}};
  const result=readDraft(schema,{values:['-1.0','1.2e1','9007199254740991.0','0e999']});
  assert.deepEqual(result.errors,{});
  assert.deepEqual(result.values.values,[-1,12,9007199254740991,0]);
});

test('verified cleanup requires independent successful exit without forced containment',()=>{
  const result={status:'FAIL',cleanup:{clean:true},forced:false,exit_code:0};
  assert.equal(verifiedCleanup(result),true);
  assert.equal(verifiedCleanup({...result,forced:true}),false);
  assert.equal(verifiedCleanup({...result,exit_code:1}),false);
  assert.equal(verifiedCleanup({...result,exit_code:null}),false);
  assert.equal(verifiedCleanup({...result,cleanup:{clean:false}}),false);
});

test('a preparation fault is clean only when the backend settled before any child',()=>{
  assert.equal(cleanupLabel(null,{cleanup:{clean:true,child_started:false}}),'Clean · no child started');
  assert.equal(cleanupLabel(null,{cleanup:{clean:true}}),'Unverified');
  assert.equal(cleanupLabel(null,{cleanup:{clean:false,child_started:true}}),'Incomplete / not clean');
  assert.equal(cleanupLabel({status:'FAIL',cleanup:{clean:true},forced:true,exit_code:0},{}),'Incomplete / not clean');
});

test('a blank environment draft means unconfigured, not a partially filled tuple',()=>{
  assert.deepEqual(readEnvironment(environmentDraft(null)),{environment:null,errors:{}});
  const partial=readEnvironment({profile:SUPPORTED_PROFILES[0].profile,model_root:'  ',runtime_path:'',library_paths:''});
  assert.equal(partial.environment,null);
  assert.deepEqual(Object.keys(partial.errors).sort(),['model_root','runtime_path']);
});

test('a saved profile outside the supported set cannot be re-saved unchanged',()=>{
  const saved={model:'other',profile:'other-profile',language:'x',provider:'cuda',runtime_profile:'y',model_root:'/models',runtime_path:'/rt.dylib',native_library_paths:[]};
  const parsed=readEnvironment(environmentDraft(saved));
  assert.equal(parsed.environment,null);
  assert.ok(parsed.errors.profile);
});

test('environment draft round-trips through the fixed supported tuple with trimmed library lines',()=>{
  const draft={profile:SUPPORTED_PROFILES[1].profile,model_root:' /models ',runtime_path:'/rt/libonnxruntime.dylib',library_paths:'\n /lib/a.dylib \n\n/lib/b.dylib\r\n'};
  const {environment,errors}=readEnvironment(draft);
  assert.deepEqual(errors,{});
  assert.deepEqual(environment,{
    model:SUPPORTED_PROFILES[1].model,profile:SUPPORTED_PROFILES[1].profile,
    language:'horizontal-ja-basic-latin-ascii-digits-ui-symbols-v1',provider:'cpu',runtime_profile:'onnxruntime-1.29.0-api17-cpu',
    model_root:'/models',runtime_path:'/rt/libonnxruntime.dylib',native_library_paths:['/lib/a.dylib','/lib/b.dylib'],
  });
  assert.equal(sameEnvironment(readEnvironment(environmentDraft(environment)).environment,environment),true);
  assert.equal(sameEnvironment(environment,{...environment,native_library_paths:['/lib/b.dylib','/lib/a.dylib']}),false);
});

const checkedEnvironment=readEnvironment({profile:SUPPORTED_PROFILES[0].profile,model_root:'/models',runtime_path:'/rt.dylib',library_paths:''}).environment;
const association={operation:'desktop-1',environment:checkedEnvironment,descriptorPath:'/corpus/a.json',packageInventoryIdentity:'inv-1'};
const unchanged={saved:checkedEnvironment,draftDirty:false,descriptorPath:'/corpus/a.json',packageInventoryIdentity:'inv-1'};
for (const {scenario,current,reasons} of [
  {scenario:'nothing changed since the check',current:unchanged,reasons:0},
  {scenario:'the environment was saved again with another path',current:{...unchanged,saved:{...checkedEnvironment,model_root:'/models-2'}},reasons:1},
  {scenario:'the draft has unsaved edits even though saved settings match',current:{...unchanged,draftDirty:true},reasons:1},
  {scenario:'another descriptor is selected',current:{...unchanged,descriptorPath:null},reasons:1},
  {scenario:'another package is inspected',current:{...unchanged,packageInventoryIdentity:'inv-2'},reasons:1},
  {scenario:'the environment was cleared after the check',current:{...unchanged,saved:null,draftDirty:true},reasons:2},
]) {
  test(`check association: ${scenario}`,()=>{
    assert.equal(staleReasons(association,current).length,reasons);
  });
}


test('private disclosure is bounded and reports what was cut',()=>{
  assert.deepEqual(boundedText('abcdef',4),{text:'abcd',truncated:2});
  assert.deepEqual(boundedText('abc',3),{text:'abc',truncated:0});
  assert.throws(()=>boundedText('abc',0));
});

test('ordinary replay fault summaries do not disclose recognized text or diagnostic context',()=>{
  const privateText='private recorded recognition';
  const summary=faultSummary({
    category:'JavaScript',message:privateText,
    context:{stage:'workflow',recognized_text:privateText,source:{private_detail:privateText}},
  });
  assert.ok(summary.includes('JavaScript'));
  assert.ok(summary.includes('workflow'));
  assert.ok(!summary.includes(privateText));
  assert.ok(!summary.includes('private_detail'));
});
