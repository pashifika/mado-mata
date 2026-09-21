import {test} from 'node:test';
import assert from 'node:assert/strict';
import {acceptController,retainLogs,defaultDraft,readDraft,verifiedCleanup} from './state.ts';

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
