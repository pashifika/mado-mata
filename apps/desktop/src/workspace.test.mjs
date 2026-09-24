import {test} from 'node:test';
import assert from 'node:assert/strict';
import {applyCatalog,applyCommand,applyIfCurrent,bindSelection,closeWorkspace,commandValues,deriveBound,displayNameError,editDraft,ingestResults,inScope,internalNameError,isBound,matchesFilter,needsAttention,newDraft,originLabel,retainClosed,selectProfile,updateBound,viewLogs,workspaceFromView,workspaceLabel,CLOSED_LIMIT,UNSUPPORTED_SOURCE} from './workspace.ts';
import {LocalFault} from './i18n.ts';

const schema={type:'object',properties:{count:{type:'integer',default:1},mode:{type:'string'}}};
function profile(id,name,values,packageId='pkg-a',schemaIdentity='schema-1'){
  return {version:1,id,name,package_id:packageId,schema_identity:schemaIdentity,values};
}
function selection(id,revision,{path='/pkg/'+id,packageId='pkg-'+id,profiles=[profile('prof-'+id,'Saved '+id,{count:5},packageId)],schemaIdentity='schema-1',internal=id,display='Tab '+id}={}){
  return {workspace_id:id,revision,internal_name:internal,display_name:display,package_path:path,profiles_error:null,profiles,
    package:{package_id:packageId,inventory_identity:'inv-'+id,schema_identity:schemaIdentity,runtime:'quickjs',schema,profiles:{fast:{options:{count:9}}},effective_defaults:null}};
}
function view(id,{internal=id,display='Tab '+id,selection:bound=null,sourceError=null}={}){
  return {workspace_id:id,revision:bound?bound.revision:0,internal_name:internal,display_name:display,selection:bound,source_error:sourceError};
}
function terminal(run,extra={}){
  return {run,state:'terminal',operation:'run',result:{status:'PASS',cleanup:{clean:true},forced:false,exit_code:0},error:null,progress:[],dropped_logs:0,workspace_id:'a',workspace_revision:1,...extra};
}
// The host lists the whole store or nothing: one malformed file arrives as an empty catalog plus this fault.
const unreadable={category:'StorageFormat',message:'stored JSON is malformed or incompatible; original data was preserved',context:null};
const shared=profile('P','Review',{count:5},'shared');
// Two named Tabs bound to the same package source, each with its own Tab/package store.
function sameSourceTabs(){
  return [
    bindSelection(workspaceFromView(view('a')),selection('a',1,{path:'/games/alpha',packageId:'shared',profiles:[shared]})),
    bindSelection(workspaceFromView(view('b')),selection('b',1,{path:'/games/alpha',packageId:'shared',profiles:[shared]})),
  ];
}

test('a host view becomes an unbound, unusable-source or bound session without restoring any draft',()=>{
  const unbound=workspaceFromView(view('a'));
  assert.equal(unbound.bound,null);
  assert.equal(unbound.sourceError,null);
  assert.equal(isBound(unbound),false);
  assert.equal(unbound.revision,0);
  const archive={category:UNSUPPORTED_SOURCE,message:'custom archive',context:{internal_name:'a'}};
  const unsupported=workspaceFromView(view('a',{sourceError:archive}));
  assert.equal(unsupported.bound,null);
  assert.deepEqual(unsupported.sourceError,archive);
  const bound=workspaceFromView(view('c',{selection:selection('c',3)}));
  assert.equal(isBound(bound),true);
  assert.equal(bound.revision,3);
  assert.deepEqual(bound.bound.draft,{count:1});
  assert.equal(bound.bound.touched,false);
  assert.equal(bound.bound.selectedId,null);
  assert.equal(bound.inspectPath,'/pkg/c');
  assert.equal(bound.internalName,'c');
  assert.equal(bound.displayName,'Tab c');
});

test('package commands are refused for a genuine named Tab without an inspected selection or with an unusable draft',()=>{
  const unbound=workspaceFromView(view('a'));
  assert.throws(()=>commandValues(unbound,undefined),error=>error instanceof LocalFault&&error.presentation.key==='unboundWorkspace');
  const unavailable=workspaceFromView(view('b',{sourceError:{category:'Package',message:'gone',context:null}}));
  assert.throws(()=>commandValues(unavailable,undefined),error=>error instanceof LocalFault&&error.presentation.key==='unboundWorkspace');
  const bound=bindSelection(unbound,selection('a',1));
  assert.deepEqual(commandValues(bound,deriveBound(bound.bound,null)),{count:1});
  const numeric=updateBound(bound,item=>editDraft(item,{count:'1.5'}));
  assert.throws(()=>commandValues(numeric,deriveBound(numeric.bound,null)),error=>error.presentation.key==='numericFields');
  const foreign=updateBound(bound,item=>({...item,profiles:[profile('X','Other schema',{count:2},'pkg-a','schema-2')],selectedId:'X'}));
  assert.throws(()=>commandValues(foreign,deriveBound(foreign.bound,null)),error=>error.presentation.key==='profileBinding');
  const replay=deriveBound({...bound.bound,lane:'replay'},null);
  assert.ok(replay.startBlock);
  assert.equal(deriveBound({...bound.bound,lane:'replay',descriptorPath:'/corpus.json'},{profile:'p'}).startBlock,null);
});

test('binding publishes the host revision, resolves a source fault and keeps execution configuration across reinspection',()=>{
  const failed=workspaceFromView(view('a',{sourceError:{category:'Package',message:'gone',context:null}}));
  const first=bindSelection(failed,selection('a',1),{key:'bound'});
  assert.equal(first.sourceError,null);
  assert.equal(first.revision,1);
  assert.deepEqual(first.notice,{key:'bound'});
  const edited=updateBound(first,bound=>editDraft({...bound,lane:'replay',descriptorPath:'/corpus.json'},{count:'7'}));
  const again=bindSelection(edited,selection('a',2));
  assert.equal(again.revision,2);
  assert.deepEqual(again.bound.draft,{count:1});
  assert.equal(again.bound.touched,false);
  assert.equal(again.bound.lane,'replay');
  assert.equal(again.bound.descriptorPath,'/corpus.json');
  assert.ok(again.bound.draftRevision>edited.bound.draftRevision);
  assert.equal(again.legacyImport,null);
});

test('a catalog from one Tab never reaches another Tab bound to the same package source',()=>{
  let list=sameSourceTabs();
  list=applyIfCurrent(list,{id:'b',revision:1},item=>updateBound(item,bound=>editDraft(selectProfile(bound,'P'),{count:'3'})));
  const updated=profile('P','Review',{count:7},'shared');
  const added=profile('Q','Only in A',{count:2},'shared');
  list=applyCommand(list,{id:'a',revision:1},{update:item=>item,catalog:{profiles:[updated,added],profiles_error:null}});
  assert.deepEqual(list[0].bound.profiles,[updated,added]);
  const beta=list[1];
  assert.deepEqual(beta.bound.profiles,[shared]);
  assert.deepEqual(beta.bound.draft,{count:'3'});
  assert.equal(beta.bound.selectedId,'P');
  assert.equal(beta.notice,null);
});

for (const {scenario,localName,expectedName} of [
  {scenario:'an unedited name follows the rename',localName:'Review',expectedName:'Reviewed again'},
  {scenario:'a locally edited name is preserved',localName:'Mine',expectedName:'Mine'},
]) {
  test(`a selected profile changed on disk keeps the local draft as a draft and ${scenario}`,()=>{
    let tab=sameSourceTabs()[0];
    tab=updateBound(tab,bound=>({...selectProfile(bound,'P'),name:localName}));
    tab=updateBound(tab,bound=>({...editDraft(bound,{count:'8'}),validation:{count:8}}));
    const before=tab.bound;
    const after=applyCatalog(tab,{profiles:[profile('P','Reviewed again',{count:7},'shared')],profiles_error:null});
    assert.equal(after.bound.selectedId,'P');
    assert.deepEqual(after.bound.draft,{count:'8'});
    assert.equal(after.bound.draftRevision,before.draftRevision);
    assert.deepEqual(after.bound.validation,{count:8});
    assert.equal(after.bound.name,expectedName);
    assert.deepEqual(after.bound.profiles[0].values,{count:7});
    assert.equal(after.notice.key,'updatedElsewhere');
  });
}

test('a selected profile deleted on disk becomes an explained unsaved draft with its values kept',()=>{
  const tab=updateBound(sameSourceTabs()[0],bound=>selectProfile(bound,'P'));
  const after=applyCatalog(tab,{profiles:[],profiles_error:null});
  assert.equal(after.bound.selectedId,null);
  assert.deepEqual(after.bound.profiles,[]);
  assert.deepEqual(after.bound.draft,{count:5});
  assert.equal(after.bound.name,'Review');
  assert.equal(after.bound.touched,true);
  assert.deepEqual(after.notice,{key:'deletedElsewhere',args:['Review']});
});

test('a failed listing keeps the last catalog, selection and draft and reports only the fault; a readable one recovers',()=>{
  let tab=updateBound(sameSourceTabs()[0],bound=>editDraft(selectProfile(bound,'P'),{count:'4'}));
  tab=applyCatalog(tab,{profiles:[],profiles_error:unreadable});
  assert.deepEqual(tab.bound.profiles,[shared]);
  assert.equal(tab.bound.selectedId,'P');
  assert.deepEqual(tab.bound.draft,{count:'4'});
  assert.deepEqual(tab.bound.profilesError,unreadable);
  const added=profile('Q','Added on disk',{count:1},'shared');
  tab=applyCatalog(tab,{profiles:[shared,added],profiles_error:null});
  assert.deepEqual(tab.bound.profiles,[shared,added]);
  assert.equal(tab.bound.profilesError,null);
  assert.equal(tab.bound.selectedId,'P');
  assert.deepEqual(tab.bound.draft,{count:'4'});
});

test('a ProfileRejected listing is a real partial catalog',()=>{
  const rejected={category:'ProfileRejected',message:'Some saved profiles are incompatible; their files were preserved',context:{rejected:[{profile_id:'P'}]}};
  const tab=updateBound(sameSourceTabs()[0],bound=>selectProfile(bound,'P'));
  const after=applyCatalog(tab,{profiles:[],profiles_error:rejected});
  assert.deepEqual(after.bound.profiles,[]);
  assert.equal(after.bound.selectedId,null);
  assert.deepEqual(after.bound.draft,{count:5});
  assert.deepEqual(after.bound.profilesError,rejected);
});

test('an unchanged catalog or an unbound Tab is left as it is',()=>{
  const tab=updateBound(sameSourceTabs()[0],bound=>editDraft(selectProfile(bound,'P'),{count:'2'}));
  assert.equal(applyCatalog(tab,{profiles:[shared],profiles_error:null}),tab);
  const unbound=workspaceFromView(view('u'));
  assert.equal(applyCatalog(unbound,{profiles:[shared],profiles_error:null}),unbound);
  assert.equal(updateBound(unbound,bound=>({...bound,name:'x'})),unbound);
});

for (const {scenario,update} of [
  {scenario:'validation',update:item=>updateBound(item,bound=>({...bound,validation:{count:5}}))},
  {scenario:'a failed profile mutation',update:item=>({...item,error:unreadable})},
]) {
  test(`${scenario} in one Tab cannot roll back another Tab after a successful write whose refresh failed`,()=>{
    const added=profile('Q','Saved despite refresh failure',{count:2},'shared');
    let list=applyCommand(sameSourceTabs(),{id:'a',revision:1},{
      update:item=>updateBound(item,bound=>({...bound,profiles:[shared,added],selectedId:added.id})),
      catalog:{profiles:[],profiles_error:unreadable},
    });
    list=applyCommand(list,{id:'b',revision:1},{update});
    assert.deepEqual(list[0].bound.profiles,[shared,added]);
    assert.deepEqual(list[0].bound.profilesError,unreadable);
    assert.equal(list[0].bound.selectedId,'Q');
    assert.deepEqual(list[1].bound.profiles,[shared]);
    assert.equal(list[1].bound.profilesError,null);
  });
}

test('a catalog returned for an obsolete revision is discarded',()=>{
  const list=[bindSelection(sameSourceTabs()[0],selection('a',2,{packageId:'shared',profiles:[shared]}))];
  const result=applyCommand(list,{id:'a',revision:1},{update:item=>updateBound(item,bound=>({...bound,profiles:[]})),catalog:{profiles:[],profiles_error:null}});
  assert.equal(result,list);
  assert.deepEqual(result[0].bound.profiles,[shared]);
});

test('two Tabs keep independent drafts and switching never touches the other',()=>{
  let list=[bindSelection(workspaceFromView(view('a')),selection('a',1)),bindSelection(workspaceFromView(view('b')),selection('b',1))];
  list=applyIfCurrent(list,{id:'a',revision:1},item=>updateBound(item,bound=>selectProfile(bound,'prof-a')));
  list=applyIfCurrent(list,{id:'b',revision:1},item=>updateBound(item,bound=>newDraft(bound,'fast')));
  assert.deepEqual(list[0].bound.draft,{count:5});
  assert.equal(list[0].bound.selectedId,'prof-a');
  assert.deepEqual(list[1].bound.draft,{count:9});
  assert.equal(list[1].bound.selectedId,null);
  assert.equal(list[1].bound.name,'fast');
  assert.ok(list[0].bound.touched&&list[1].bound.touched);
});

test('a completion for a closed, rebound, stale or reopened workspace is discarded without touching another Tab',()=>{
  const open=[bindSelection(workspaceFromView(view('a')),selection('a',1)),bindSelection(workspaceFromView(view('b')),selection('b',1))];
  const update=item=>updateBound(item,bound=>({...bound,validation:{count:1}}));
  const afterClose=applyIfCurrent(closeWorkspace(open,'a'),{id:'a',revision:1},update);
  assert.equal(afterClose.length,1);
  assert.equal(afterClose[0].bound.validation,null);
  const rebound=[bindSelection(open[0],selection('a',2)),open[1]];
  assert.equal(applyIfCurrent(rebound,{id:'a',revision:1},update),rebound);
  const staleDraft=applyIfCurrent(open,{id:'a',revision:1,draftRevision:open[0].bound.draftRevision+1},update);
  assert.equal(staleDraft,open);
  // The same saved Tab reopened under a fresh session ID shares only its names with the old session.
  const reopened=[open[1],workspaceFromView(view('a2',{internal:'a',display:'Tab a'}))];
  assert.equal(applyIfCurrent(reopened,{id:'a',revision:1},update),reopened);
  const applied=applyIfCurrent(open,{id:'a',revision:1,draftRevision:open[0].bound.draftRevision},update);
  assert.deepEqual(applied[0].bound.validation,{count:1});
  assert.equal(applied[1],open[1]);
});

test('labels are display names and equal display names are disambiguated by internal name',()=>{
  const list=[workspaceFromView(view('a',{internal:'alpha',display:'Review'})),workspaceFromView(view('b',{internal:'Beta_run',display:'Review'})),workspaceFromView(view('c',{internal:'gamma',display:'Other'}))];
  assert.equal(workspaceLabel(list[0],list),'Review · alpha');
  assert.equal(workspaceLabel(list[1],list),'Review · Beta_run');
  assert.equal(workspaceLabel(list[2],list),'Other');
  assert.equal(workspaceLabel(list[0],[list[0]]),'Review');
  const astral=workspaceFromView(view('d',{internal:'d',display:'😀 game'}));
  assert.equal(workspaceLabel(astral,[astral]),'😀 game');
});

for (const {scenario,value,error} of [
  {scenario:'an empty internal name',value:'',error:'internalEmpty'},
  {scenario:'digits including the first character',value:'0tab9',error:null},
  {scenario:'a non-ASCII digit',value:'tab１',error:'internalChars'},
  {scenario:'a space in an internal name',value:'my tab',error:'internalChars'},
  {scenario:'leading whitespace is not trimmed',value:' tab',error:'internalChars'},
  {scenario:'a 65-character internal name',value:'a'.repeat(65),error:'internalLong'},
  {scenario:'a 64-character internal name',value:'0'+'Ab1'.repeat(20)+'_-9',error:null},
  {scenario:'a single letter',value:'z',error:null},
]) {
  test(`internal names mirror the host rule for ${scenario}`,()=>{
    assert.equal(internalNameError(value),error);
  });
}

for (const {scenario,value,error} of [
  {scenario:'a blank display name',value:'   \u3000',error:'displayBlank'},
  {scenario:'an omitted display name',value:'',error:null},
  {scenario:'a control character',value:'Review\u0007',error:'displayControl'},
  {scenario:'a C1 control character',value:'Review\u0085',error:'displayControl'},
  {scenario:'80 supplementary-plane scalars',value:'😀'.repeat(80),error:null},
  {scenario:'81 supplementary-plane scalars',value:'😀'.repeat(81),error:'displayLong'},
  {scenario:'80 BMP characters',value:'あ'.repeat(80),error:null},
  {scenario:'a name with inner spaces and punctuation',value:'Night run · 夜',error:null},
]) {
  test(`display names count Unicode scalars and refuse controls for ${scenario}`,()=>{
    assert.equal(displayNameError(value),error);
  });
}

test('origins report open, closed, unknown, and application scopes without reassigning events',()=>{
  const open=[workspaceFromView(view('a',{internal:'alpha',display:'Alpha'}))];
  const closed=retainClosed([],{id:'z',revision:3,label:'Zeta',result:null});
  assert.deepEqual(originLabel('a',open,closed),{kind:'open',label:'Alpha'});
  assert.equal(originLabel('z',open,closed).kind,'closed');
  assert.equal(originLabel('gone',open,closed).kind,'unknown');
  assert.equal(originLabel(null,open,closed).kind,'application');
  let bounded=[];
  for (let index=0;index<CLOSED_LIMIT+3;index+=1) bounded=retainClosed(bounded,{id:'c'+index,revision:1,label:'c',result:null});
  assert.equal(bounded.length,CLOSED_LIMIT);
  assert.equal(bounded[0].id,'c3');
});

test('host results replace only changed entries and drop owners that are no longer open',()=>{
  const open=[bindSelection(workspaceFromView(view('a')),selection('a',1)),bindSelection(workspaceFromView(view('b')),selection('b',1))];
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
  const olderRevision=ingestResults(successor,[{workspace:{workspace_id:'a',revision:1},controller:terminal('run-2')}],[bindSelection(open[0],selection('a',2)),open[1]]);
  assert.equal(olderRevision.a.ref.revision,1);
});

for (const {scenario,view:controller,attention} of [
  {scenario:'a clean successful terminal outcome',view:terminal('r'),attention:false},
  {scenario:'a terminal preparation fault',view:{...terminal('r'),result:null,error:{category:'StaleIdentity',message:'x',context:null}},attention:true},
  {scenario:'a primary script failure with clean cleanup',view:terminal('r',{result:{status:'FAIL',primary:{category:'JavaScript'},cleanup:{clean:true},forced:false,exit_code:0}}),attention:true},
  {scenario:'incomplete cleanup after a passing script',view:terminal('r',{result:{status:'PASS',cleanup:{clean:false},forced:true,exit_code:0}}),attention:true},
  {scenario:'a running operation',view:{...terminal('r'),state:'running',result:null},attention:false},
  {scenario:'no operation at all',view:null,attention:false},
]) {
  test(`attention derives from ${scenario}`,()=>{
    assert.equal(needsAttention(controller),attention);
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
  const result=viewLogs(entries,{kind:'workspace',id:'a'},{text:'failed',level:'error'});
  assert.equal(result.scoped.length,2);
  assert.deepEqual(result.shown.map(entry=>entry.sequence),[3]);
});
