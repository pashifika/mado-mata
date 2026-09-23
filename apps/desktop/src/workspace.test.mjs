import {test} from 'node:test';
import assert from 'node:assert/strict';
import {applyIfCurrent,closeWorkspace,editDraft,freshWorkspace,ingestResults,inScope,matchesFilter,needsAttention,newDraft,openWorkspace,originLabel,retainClosed,selectProfile,viewLogs,workspaceLabel,CLOSED_LIMIT} from './workspace.ts';

const schema={type:'object',properties:{count:{type:'integer',default:1},mode:{type:'string'}}};
function selection(id,revision,path='/pkg/'+id,packageId='pkg-'+id){
  return {workspace_id:id,revision,package_path:path,profiles_error:null,profiles:[
    {version:1,id:'prof-'+id,name:'Saved '+id,package_id:packageId,schema_identity:'schema-1',values:{count:5}},
  ],package:{package_id:packageId,inventory_identity:'inv-'+id,schema_identity:'schema-1',runtime:'quickjs',schema,profiles:{fast:{options:{count:9}}},effective_defaults:null}};
}
function terminal(run,extra={}){
  return {run,state:'terminal',operation:'run',result:{status:'PASS',cleanup:{clean:true},forced:false,exit_code:0},error:null,progress:[],dropped_logs:0,workspace_id:'a',workspace_revision:1,...extra};
}

test('opening the same root again keeps the existing draft while a new revision resets it on purpose',()=>{
  const first=openWorkspace([],selection('a',1));
  const edited=[editDraft({...first.list[0],lane:'replay',descriptorPath:'/corpus.json'},{count:'7'})];
  const again=openWorkspace(edited,selection('a',1));
  assert.equal(again.reused,true);
  assert.equal(again.list,edited);
  const reinspected=openWorkspace(edited,selection('a',2));
  assert.equal(reinspected.reused,false);
  assert.deepEqual(reinspected.list[0].draft,{count:1});
  assert.equal(reinspected.list[0].revision,2);
  assert.equal(reinspected.list[0].touched,false);
  assert.equal(reinspected.list[0].lane,'replay');
  assert.equal(reinspected.list[0].descriptorPath,'/corpus.json');
  assert.ok(reinspected.list[0].draftRevision>edited[0].draftRevision);
});

test('two workspaces keep independent drafts and switching never touches the other',()=>{
  let list=openWorkspace(openWorkspace([],selection('a',1)).list,selection('b',1)).list;
  list=applyIfCurrent(list,{id:'a',revision:1},item=>selectProfile(item,'prof-a'));
  list=applyIfCurrent(list,{id:'b',revision:1},item=>newDraft(item,'fast'));
  assert.deepEqual(list[0].draft,{count:5});
  assert.equal(list[0].selectedId,'prof-a');
  assert.deepEqual(list[1].draft,{count:9});
  assert.equal(list[1].selectedId,null);
  assert.equal(list[1].name,'fast');
  assert.ok(list[0].touched&&list[1].touched);
});

test('a completion for a closed, replaced, or stale workspace is discarded without touching another tab',()=>{
  const open=openWorkspace(openWorkspace([],selection('a',1)).list,selection('b',1)).list;
  const update=item=>({...item,validation:{count:1}});
  const afterClose=applyIfCurrent(closeWorkspace(open,'a'),{id:'a',revision:1},update);
  assert.equal(afterClose.length,1);
  assert.equal(afterClose[0].validation,null);
  const reinspected=openWorkspace(open,selection('a',2)).list;
  assert.equal(applyIfCurrent(reinspected,{id:'a',revision:1},update),reinspected);
  const staleDraft=applyIfCurrent(open,{id:'a',revision:1,draftRevision:open[0].draftRevision+1},update);
  assert.equal(staleDraft,open);
  const applied=applyIfCurrent(open,{id:'a',revision:1,draftRevision:open[0].draftRevision},update);
  assert.deepEqual(applied[0].validation,{count:1});
  assert.equal(applied[1],open[1]);
});

test('same package ids from different roots stay distinguishable by location',()=>{
  const list=openWorkspace(openWorkspace([],selection('a',1,'/games/alpha','shared')).list,selection('b',1,'/games/beta','shared')).list;
  assert.equal(workspaceLabel(list[0],list),'shared · alpha');
  assert.equal(workspaceLabel(list[1],list),'shared · beta');
  assert.equal(workspaceLabel(list[0],[list[0]]),'shared');
});

test('origins report open, closed, unknown, and application scopes without reassigning events',()=>{
  const open=openWorkspace([],selection('a',1)).list;
  const closed=retainClosed([],{id:'z',revision:3,label:'pkg-z',result:null});
  assert.deepEqual(originLabel('a',open,closed),{kind:'open',label:'pkg-a'});
  assert.equal(originLabel('z',open,closed).kind,'closed');
  assert.equal(originLabel('gone',open,closed).kind,'unknown');
  assert.equal(originLabel(null,open,closed).kind,'application');
  let bounded=[];
  for (let index=0;index<CLOSED_LIMIT+3;index+=1) bounded=retainClosed(bounded,{id:'c'+index,revision:1,label:'c',result:null});
  assert.equal(bounded.length,CLOSED_LIMIT);
  assert.equal(bounded[0].id,'c3');
});

test('host results replace only changed entries and drop owners that are no longer open',()=>{
  const open=openWorkspace(openWorkspace([],selection('a',1)).list,selection('b',1)).list;
  const first=ingestResults({},[{workspace:{workspace_id:'a',revision:1},controller:terminal('run-1')}],open);
  const same=ingestResults(first,[{workspace:{workspace_id:'a',revision:1},controller:terminal('run-1')}],open);
  assert.equal(same,first);
  const successor=ingestResults(first,[
    {workspace:{workspace_id:'a',revision:1},controller:terminal('run-2')},
    {workspace:{workspace_id:'b',revision:1},controller:terminal('run-3',{workspace_id:'b'})},
    {workspace:{workspace_id:'closed',revision:1},controller:terminal('run-0',{workspace_id:'closed'})},
  ],open);
  assert.equal(successor.a.view.run,'run-2');
  assert.equal(successor.b.view.run,'run-3');
  assert.equal(successor.closed,undefined);
  const olderRevision=ingestResults(successor,[{workspace:{workspace_id:'a',revision:1},controller:terminal('run-2')}],openWorkspace(open,selection('a',2)).list);
  assert.equal(olderRevision.a.ref.revision,1);
});

for (const {scenario,view,attention} of [
  {scenario:'a clean successful terminal outcome',view:terminal('r'),attention:false},
  {scenario:'a terminal preparation fault',view:{...terminal('r'),result:null,error:{category:'StaleIdentity',message:'x',context:null}},attention:true},
  {scenario:'a primary script failure with clean cleanup',view:terminal('r',{result:{status:'FAIL',primary:{category:'JavaScript'},cleanup:{clean:true},forced:false,exit_code:0}}),attention:true},
  {scenario:'incomplete cleanup after a passing script',view:terminal('r',{result:{status:'PASS',cleanup:{clean:false},forced:true,exit_code:0}}),attention:true},
  {scenario:'a running operation',view:{...terminal('r'),state:'running',result:null},attention:false},
  {scenario:'no operation at all',view:null,attention:false},
]) {
  test(`attention derives from ${scenario}`,()=>{
    assert.equal(needsAttention(view),attention);
  });
}

const entries=[
  {sequence:1,time_ms:0,source:'Rust',level:'INFO',run:null,workspace_id:null,code:'application.ready',message:'Application ready',fields:{secret:'hidden-token'}},
  {sequence:2,time_ms:0,source:'Script',level:'WARN',run:'run-1',workspace_id:'a',code:'script.log',message:'No match',fields:{recognized_text:'private words'}},
  {sequence:3,time_ms:0,source:'Rust',level:'ERROR',run:'run-1',workspace_id:'a',code:'run.terminal',message:'Run failed',fields:null},
  {sequence:4,time_ms:0,source:'Rust',level:'INFO',run:'run-2',workspace_id:'b',code:'run.terminal',message:'Run passed',fields:null},
];

test('log scopes filter the one shared store by explicit attribution only',()=>{
  assert.deepEqual(entries.filter(entry=>inScope(entry,{kind:'workspace',id:'a'})).map(entry=>entry.sequence),[2,3]);
  assert.deepEqual(entries.filter(entry=>inScope(entry,{kind:'application'})).map(entry=>entry.sequence),[1]);
  assert.equal(entries.filter(entry=>inScope(entry,{kind:'all'})).length,4);
});

test('search matches display fields case-insensitively and never diagnostic fields',()=>{
  assert.equal(matchesFilter(entries[1],{text:'no MATCH',level:''}),true);
  assert.equal(matchesFilter(entries[1],{text:'run-1',level:''}),true);
  assert.equal(matchesFilter(entries[1],{text:'private',level:''}),false);
  assert.equal(matchesFilter(entries[0],{text:'hidden-token',level:''}),false);
  assert.equal(matchesFilter(entries[2],{text:'',level:'error'}),true);
  assert.equal(matchesFilter(entries[2],{text:'',level:'warn'}),false);
  const view=viewLogs(entries,{kind:'workspace',id:'a'},{text:'failed',level:'error'});
  assert.equal(view.scoped.length,2);
  assert.deepEqual(view.shown.map(entry=>entry.sequence),[3]);
});

test('fresh workspaces start from schema defaults with pristine state',()=>{
  const workspace=freshWorkspace(selection('a',1));
  assert.deepEqual(workspace.draft,{count:1});
  assert.equal(workspace.touched,false);
  assert.equal(workspace.busy,null);
  assert.equal(workspace.page,'run');
});
