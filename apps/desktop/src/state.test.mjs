import {test} from 'node:test';
import assert from 'node:assert/strict';
import {acceptController,retainLogs,defaultDraft} from './state.ts';

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
