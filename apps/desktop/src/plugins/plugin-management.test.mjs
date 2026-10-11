import {test} from 'node:test';
import assert from 'node:assert/strict';
import {PluginManagement, canReview} from './plugin-management.ts';

function deferred(){
  let resolve,reject;
  const promise=new Promise((done,fail)=>{resolve=done; reject=fail;});
  return {promise,resolve,reject};
}
function observed(observation,state='absent',actions=['install'],outcome=null){
  return {included_version:'0.1.1',included_content:'sha256-A',minimum_omp_version:'18.8.7',
    target:{executable:'/bin/omp',version:'18.8.9',user_root:'/home/a/.omp'},installed:null,state,actions,observation,issue:null,outcome};
}
// The host seam: records every command; each answer is the next queued value or deferred promise.
function fakeHost(){
  const calls=[],answers=[];
  return {
    calls,answers,
    async inspect(executable){calls.push(['inspect',executable]); return answers.shift();},
    async apply(observation,action){calls.push(['apply',observation,action]); return answers.shift();},
  };
}

test('an admitted action survives dialog closure and reports only the host read-back',async()=>{
  const host=fakeHost();
  const session=new PluginManagement(host,()=>{});
  host.answers.push(observed('obs-1'));
  await session.refresh();
  const reply=deferred();
  host.answers.push(reply.promise);
  session.review('install');
  const applied=session.confirm();
  session.dismiss();
  session.review('install');
  await session.confirm();
  await session.refresh();
  assert.deepEqual(host.calls,[['inspect',null],['apply','obs-1','install']]);
  assert.equal(session.current().applying.action,'install');
  assert.equal(session.current().result,null);
  assert.equal(session.current().view.state,'absent');
  const outcome={action:'install',target:observed('obs-1').target,status:'unknown',issue:null};
  reply.resolve(observed('obs-2','current',['uninstall'],outcome));
  await applied;
  const state=session.current();
  assert.equal(state.applying,null);
  assert.equal(state.view.observation,'obs-2');
  assert.deepEqual(state.result.outcome,outcome);
  assert.ok(canReview(state,'uninstall'));
});

test('a newer observation or an executable edit withdraws the reviewed action',async()=>{
  const host=fakeHost();
  const session=new PluginManagement(host,()=>{});
  host.answers.push(observed('obs-1'));
  await session.refresh();
  session.review('install');
  const read=deferred();
  host.answers.push(read.promise);
  const refreshed=session.refresh();
  assert.equal(session.current().confirmation,null);
  assert.ok(session.current().withdrawn);
  await session.confirm();
  read.resolve(observed('obs-1'));
  await refreshed;
  await session.confirm();
  session.review('install');
  session.edit(' /opt/omp ');
  assert.equal(session.current().confirmation,null);
  assert.ok(session.current().withdrawn);
  await session.confirm();
  session.review('install');
  assert.equal(session.current().confirmation,null);
  host.answers.push(observed('obs-3'));
  await session.check();
  assert.ok(canReview(session.current(),'install'));
  assert.deepEqual(host.calls,[['inspect',null],['inspect',null],['inspect','/opt/omp']]);
});

test('a rejected request or failed read admits no further change until a fresh read',async()=>{
  const host=fakeHost();
  const session=new PluginManagement(host,()=>{});
  host.answers.push(observed('obs-1'));
  await session.refresh();
  const reply=deferred();
  host.answers.push(reply.promise);
  session.review('install');
  const applied=session.confirm();
  reply.reject({category:'StaleObservation',message:'changed',context:null});
  await applied;
  let state=session.current();
  assert.ok(state.stale);
  assert.equal(state.result.outcome,null);
  assert.equal(state.result.error.category,'StaleObservation');
  assert.ok(!canReview(state,'install'));
  session.review('install');
  await session.confirm();
  const read=deferred();
  host.answers.push(read.promise);
  const refreshing=session.refresh();
  read.reject({category:'PluginTarget',message:'unreadable',context:null});
  await refreshing;
  state=session.current();
  assert.equal(state.view,null);
  assert.equal(state.inspectError.category,'PluginTarget');
  assert.equal(state.result.error.category,'StaleObservation');
  host.answers.push(observed('obs-2'));
  await session.refresh();
  assert.ok(canReview(session.current(),'install'));
  assert.deepEqual(host.calls.filter(([command])=>command==='apply'),[['apply','obs-1','install']]);
});

test('a retained outcome stays attributed to its operation target after checking another executable',async()=>{
  const host=fakeHost();
  const session=new PluginManagement(host,()=>{});
  const original=observed('original');
  const outcome={action:'install',status:'verified',issue:null,target:original.target};
  host.answers.push(original);
  await session.refresh();
  host.answers.push({...observed('installed','current',['uninstall']),outcome});
  session.review('install');
  await session.confirm();
  session.edit('/opt/other-omp');
  const other={...observed('other','incompatible',[],outcome),target:{...original.target,executable:'/opt/other-omp',version:'18.8.6'}};
  host.answers.push(other);
  await session.check();
  assert.equal(session.current().view.target.executable,'/opt/other-omp');
  assert.deepEqual(session.current().result.target,original.target);
  assert.equal(session.current().result.outcome.status,'verified');
});
