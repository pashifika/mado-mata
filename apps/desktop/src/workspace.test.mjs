import {test} from 'node:test';
import assert from 'node:assert/strict';
import {applyCatalog,applyCommand,applyIfCurrent,bindSelection,closeWorkspace,commandValues,deriveBound,displayNameError,editDraft,hasWorkspaceEdits,ingestResults,inScope,internalNameError,isBound,matchesFilter,needsAttention,newDraft,originLabel,retainClosed,selectProfile,updateBound,viewLogs,workspaceFromView,workspaceLabel,CLOSED_LIMIT,UNSUPPORTED_SOURCE} from './workspace.ts';
import {LocalFault} from './i18n.ts';
import {beginTarget,checkedTarget,currentTargetDraft,discardTarget,editTarget,readTarget,readTargetDraft,removedTarget,savedTarget,targetDirty,targetExpectation,targetFailed,targetReadFailed,targetState,targetTicket} from './target.ts';

const schema={type:'object',properties:{count:{type:'integer',default:1},mode:{type:'string'}}};
function profile(id,name,values,packageId='pkg-a',schemaIdentity='schema-1'){
  return {version:1,id,name,package_id:packageId,schema_identity:schemaIdentity,values};
}
function selection(id,revision,{path='/pkg/'+id,packageId='pkg-'+id,profiles=[profile('prof-'+id,'Saved '+id,{count:5},packageId)],schemaIdentity='schema-1',internal=id,display='Tab '+id}={}){
  return {workspace_id:id,revision,internal_name:internal,display_name:display,package_path:path,profiles_error:null,profiles,
    package:{package_id:packageId,inventory_identity:'inv-'+id,schema_identity:schemaIdentity,runtime:'quickjs',schema,profiles:{fast:{options:{count:9}}},effective_defaults:null,target:null,target_identity:null}};
}
function view(id,{internal=id,display='Tab '+id,selection:bound=null,sourceError=null,savedPackage=null}={}){
  return {workspace_id:id,revision:bound?bound.revision:0,internal_name:internal,display_name:display,selection:bound,source_error:sourceError,saved_package:savedPackage};
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

test('a failed saved directory is kept visible and prefilled, a saved custom archive is never offered as a directory request, and binding adopts the inspected directory',()=>{
  const gone={category:'Package',message:'Package location cannot be resolved',context:{internal_name:'a',package_id:'pkg-a'}};
  const directory={package_id:'pkg-a',source:{kind:'directory',path:'/games/alpha'}};
  const unavailable=workspaceFromView(view('a',{sourceError:gone,savedPackage:directory}));
  assert.equal(unavailable.bound,null);
  assert.deepEqual(unavailable.savedPackage,directory);
  assert.equal(unavailable.inspectPath,'/games/alpha');
  // The retained reference conveys no authority: package commands stay refused until a real inspection binds.
  assert.throws(()=>commandValues(unavailable,undefined),error=>error instanceof LocalFault&&error.presentation.key==='unboundWorkspace');
  const archive={package_id:'pkg-b',source:{kind:'custom_archive',path:'/games/beta.mmpkg'}};
  const unsupported=workspaceFromView(view('b',{sourceError:{category:UNSUPPORTED_SOURCE,message:'custom archive',context:null},savedPackage:archive}));
  assert.deepEqual(unsupported.savedPackage,archive);
  assert.equal(unsupported.inspectPath,'');
  assert.equal(workspaceFromView(view('c')).savedPackage,null);
  const repaired=bindSelection(unavailable,selection('a',1,{path:'/games/alpha-moved',packageId:'pkg-a'}));
  assert.equal(repaired.sourceError,null);
  assert.deepEqual(repaired.savedPackage,{package_id:'pkg-a',source:{kind:'directory',path:'/games/alpha-moved'}});
  assert.equal(repaired.inspectPath,'/games/alpha-moved');
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

function targetSelection(id='a',revision=1,declaration={id:'game',window_title:'Exact title'}) {
  const selected=selection(id,revision,{packageId:'shared'});
  return {...selected,package:{...selected.package,target:declaration,target_identity:declaration ? 'declaration-1' : null}};
}
function targetConfiguration(path='/metadata/game') {
  return {platform:'macos',game:{kind:'executable',path},launcher:null,arguments:['','--literal','two words'],
    working_directory:null,window_title:'Exact title',input:{route:'process_directed',focus:'preserve',pointer_mode:'core_graphics',click_hold_ms:0}};
}
function targetView(state,{revision=0,configuration=null,compatible=true,id='binding-1'}={}) {
  return {context:state.context,compatible,
    record:{version:1,internal_name:state.context.internal_name,package_id:state.context.package_id,revision,
      binding:configuration ? {id,package_id:state.context.package_id,target_id:'game',declaration_identity:'declaration-1',configuration,
        resolution:{game:{path:configuration.game.path,executable:configuration.game.path},launcher:null,working_directory:null}} : null}};
}
function loadedTarget(id='a',configuration=targetConfiguration()) {
  const state=targetState(targetSelection(id));
  return readTarget(state,targetView(state,{revision:configuration ? 1 : 0,configuration}));
}
function targetCheck(ticket,path='/metadata/game') {
  return {context:ticket.context,...ticket.expected,check:{configuration_identity:'checked-configuration',
    resolution:{game:{path,executable:path},launcher:null,working_directory:null},previous_resolution:null,resolution_changed:false}};
}
function withTarget(tab,state) {
  return updateBound(tab,bound=>({...bound,target:state}));
}

test('target form preserves literal argument boundaries, empty arguments and independent game/launcher locations',()=>{
  const state=loadedTarget();
  const draft={...state.draft,separateLauncher:true,launcherKind:'bundle',launcherPath:'/metadata/Launcher.app',
    arguments:['','two words','"quoted"','$(literal)',''],workingDirectory:'/metadata/work'};
  const {configuration,errors}=readTargetDraft(draft);
  assert.deepEqual(errors,{});
  assert.deepEqual(configuration.arguments,['','two words','"quoted"','$(literal)','']);
  assert.deepEqual(configuration.game,{kind:'executable',path:'/metadata/game'});
  assert.deepEqual(configuration.launcher,{kind:'bundle',path:'/metadata/Launcher.app'});
  assert.equal(configuration.working_directory,'/metadata/work');
  const saved=readTarget(targetState(targetSelection()),targetView(state,{revision:2,configuration}));
  assert.deepEqual(saved.draft.arguments,configuration.arguments);
  assert.equal(saved.draft.launcherPath,'/metadata/Launcher.app');
  assert.equal(saved.observation,null);
  assert.equal(targetDirty(saved),false);
});

for (const {scenario,update,field} of [
  {scenario:'an unselected route',update:{route:''},field:'route'},
  {scenario:'an unselected focus policy',update:{focus:''},field:'focus'},
  {scenario:'an unselected process pointer mode',update:{pointerMode:''},field:'pointerMode'},
  {scenario:'system input with preserved focus',update:{route:'system',focus:'preserve'},field:'focus'},
  {scenario:'AppKit background with focused-only policy',update:{pointerMode:'appkit_background',focus:'require_focused'},field:'focus'},
  {scenario:'an empty hold field',update:{clickHold:''},field:'clickHold'},
  {scenario:'an out-of-range hold',update:{clickHold:'1001'},field:'clickHold'},
  {scenario:'a relative path',update:{gamePath:'~/game'},field:'gamePath'},
  {scenario:'a control character in an argument',update:{arguments:['line\nbreak']},field:'arguments'},
  {scenario:'an argument beyond its UTF-8 byte bound',update:{arguments:['あ'.repeat(342)]},field:'arguments'},
  {scenario:'too many empty arguments',update:{arguments:Array(33).fill('')},field:'arguments'},
]) {
  test(`target form refuses ${scenario} without guessing a policy or rewriting the draft`,()=>{
    const draft={...loadedTarget().draft,...update};
    const before=structuredClone(draft);
    const parsed=readTargetDraft(draft);
    assert.equal(parsed.configuration,null);
    assert.ok(parsed.errors[field]);
    assert.deepEqual(draft,before);
  });
}

test('system input explicitly selected with focused-only policy carries no process-pointer mode',()=>{
  const state=loadedTarget();
  const parsed=readTargetDraft({...state.draft,route:'system',focus:'require_focused'});
  assert.deepEqual(parsed.configuration.input,{route:'system',focus:'require_focused',pointer_mode:null,click_hold_ms:0});
});

test('target-only edits join close and Reinspect confirmation while controlled profile commands stay independent',()=>{
  const tab=bindSelection(workspaceFromView(view('a')),targetSelection());
  const loaded=withTarget(tab,loadedTarget());
  const facts=deriveBound(loaded.bound,null);
  assert.equal(loaded.bound.touched,false);
  assert.equal(hasWorkspaceEdits(loaded,facts),false);
  const edited=withTarget(loaded,editTarget(loaded.bound.target,{...loaded.bound.target.draft,gamePath:'/metadata/other'}));
  assert.equal(hasWorkspaceEdits(edited,deriveBound(edited.bound,null)),true);
  assert.deepEqual(commandValues(edited,deriveBound(edited.bound,null)),{count:1});
  const failed=withTarget(edited,targetReadFailed(edited.bound.target,unreadable));
  assert.deepEqual(commandValues(failed,deriveBound(failed.bound,null)),{count:1});
  assert.equal(deriveBound(failed.bound,null).startBlock,null);
  const discarded=withTarget(edited,discardTarget(edited.bound.target));
  assert.equal(hasWorkspaceEdits(discarded,deriveBound(discarded.bound,null)),false);
});

test('a metadata check completing after navigation stays with its issuing Tab even when both Tabs share a package',()=>{
  let tabs=['a','b'].map(id=>withTarget(bindSelection(workspaceFromView(view(id)),targetSelection(id)),loadedTarget(id)));
  tabs[1]=withTarget(tabs[1],editTarget(tabs[1].bound.target,{...tabs[1].bound.target.draft,gamePath:'/metadata/b-private'}));
  const beta=tabs[1];
  const ticket=targetTicket(tabs[0].bound.target);
  tabs=applyIfCurrent(tabs,{id:'a',revision:1},tab=>withTarget(tab,checkedTarget(tab.bound.target,ticket,targetCheck(ticket))));
  assert.equal(tabs[1],beta);
  assert.equal(tabs[1].bound.target.observation,null);
  assert.equal(tabs[1].bound.target.draft.gamePath,'/metadata/b-private');
  assert.equal(currentTargetDraft(tabs[0].bound.target,tabs[0].bound.target.observation.ticket),true);
});

test('a late check reports its original draft and cannot certify edits made while it was pending, even after an edit-back',()=>{
  let state=loadedTarget();
  const ticket=targetTicket(state);
  state=editTarget(state,{...state.draft,arguments:['new']});
  state=editTarget(state,{...state.draft,arguments:['','--literal','two words']});
  state=checkedTarget(state,ticket,targetCheck(ticket));
  assert.deepEqual(state.draft.arguments,['','--literal','two words']);
  assert.equal(state.observation.check.configuration_identity,'checked-configuration');
  assert.equal(currentTargetDraft(state,state.observation.ticket),false);
});

for (const {scenario,context} of [
  {scenario:'another Tab',context:{workspace:{workspace_id:'b',revision:1}}},
  {scenario:'an old workspace revision',context:{workspace:{workspace_id:'a',revision:0}}},
  {scenario:'another package',context:{package_id:'foreign'}},
  {scenario:'another declaration',context:{declaration_identity:'changed'}},
  {scenario:'another persisted Tab owner',context:{internal_name:'other'}},
]) {
  test(`a target response for ${scenario} cannot replace this saved view or check its draft`,()=>{
    const state=loadedTarget();
    const ticket=targetTicket(state);
    const response=targetCheck(ticket);
    response.context={...response.context,...context};
    assert.equal(checkedTarget(state,ticket,response),state);
    const saved=targetView(state,{revision:2,configuration:targetConfiguration('/metadata/foreign')});
    saved.context=response.context;
    assert.equal(readTarget(state,saved),state);
    assert.equal(savedTarget(state,ticket,{view:saved,check:response.check}),state);
  });
}

test('reinspection and a close/reopen reject the previous session completion and invalidate target check state',()=>{
  let tab=withTarget(bindSelection(workspaceFromView(view('a')),targetSelection()),loadedTarget());
  const ticket=targetTicket(tab.bound.target);
  tab=withTarget(tab,checkedTarget(tab.bound.target,ticket,targetCheck(ticket)));
  const reinspected=bindSelection(tab,targetSelection('a',2));
  assert.equal(reinspected.bound.target.observation,null);
  assert.equal(reinspected.bound.target.loaded,false);
  assert.equal(checkedTarget(reinspected.bound.target,ticket,targetCheck(ticket)),reinspected.bound.target);
  const newSession=bindSelection(workspaceFromView(view('fresh')),targetSelection('fresh',1));
  const list=[newSession];
  assert.equal(applyIfCurrent(list,{id:'a',revision:1},item=>withTarget(item,savedTarget(item.bound.target,ticket,{view:targetView(tab.bound.target),check:targetCheck(ticket).check}))),list);
});

test('a stale same-owner request requires explicit reload, preserves the draft, and Discard uses the reloaded record',()=>{
  let state=loadedTarget();
  state=editTarget(state,{...state.draft,gamePath:'/metadata/local-edit'});
  const ticket=targetTicket(state);
  state=targetFailed(state,ticket,{category:'TargetConflict',message:'Saved target record changed',context:null});
  assert.equal(state.reconcile,true);
  assert.equal(targetTicket(state),null);
  assert.equal(state.draft.gamePath,'/metadata/local-edit');
  const latest=targetView(state,{revision:3,configuration:targetConfiguration('/metadata/other-writer')});
  state=readTarget(state,latest);
  assert.deepEqual(targetExpectation(state),{revision:3,binding_id:'binding-1'});
  assert.equal(state.reconcile,false);
  assert.equal(state.draft.gamePath,'/metadata/local-edit');
  assert.equal(state.issue,null);
  assert.equal(discardTarget(state).draft.gamePath,'/metadata/other-writer');
  assert.equal(checkedTarget(state,ticket,targetCheck(ticket)),state);
});

test('successful save and failed reread are independent facts; retry reads without resaving or replacing newer edits',()=>{
  let state=loadedTarget();
  state=editTarget(state,{...state.draft,gamePath:'/metadata/saved-edit'});
  const ticket=targetTicket(state);
  const committed=targetView(state,{revision:2,configuration:readTargetDraft(state.draft).configuration});
  state=editTarget(state,{...state.draft,gamePath:'/metadata/later-edit'});
  state=savedTarget(state,ticket,{view:committed,check:targetCheck(ticket,'/metadata/saved-edit').check});
  state=targetReadFailed(state,unreadable);
  assert.equal(state.persisted,'saved');
  assert.equal(state.refreshRequired,true);
  assert.equal(state.readError,unreadable);
  assert.equal(targetTicket(state),null);
  assert.equal(state.view.record.revision,2);
  assert.equal(state.draft.gamePath,'/metadata/later-edit');
  assert.equal(currentTargetDraft(state,state.observation.ticket),false);
  state=readTarget(state,committed);
  assert.equal(state.persisted,'saved');
  assert.equal(state.refreshRequired,false);
  assert.equal(state.readError,null);
  assert.equal(state.draft.gamePath,'/metadata/later-edit');
  assert.deepEqual(targetExpectation(state),{revision:2,binding_id:'binding-1'});
  assert.equal(discardTarget(state).draft.gamePath,'/metadata/saved-edit');
});

test('failed initial read can recover the saved form without resetting storage or trusting a previous check',()=>{
  const state=targetState(targetSelection());
  const failed=targetReadFailed(state,unreadable);
  assert.equal(failed.view,null);
  assert.equal(targetExpectation(failed),null);
  const restored=readTarget(failed,targetView(state,{revision:7,configuration:targetConfiguration('/metadata/repaired')}));
  assert.equal(restored.draft.gamePath,'/metadata/repaired');
  assert.equal(restored.readError,null);
  assert.equal(restored.observation,null);
  assert.equal(targetDirty(restored),false);
});

test('changed resolution requires a review attributed to that draft, not a failure; edits invalidate the review without adopting the location',()=>{
  const initial=loadedTarget();
  const ticket=targetTicket(initial);
  const previous=initial.view.record.binding.resolution;
  const resolution={...previous,game:{path:'/metadata/redirected',executable:'/metadata/redirected'}};
  const state=targetFailed(initial,ticket,{category:'TargetResolutionChanged',message:'Review changed resolution',context:{previous_resolution:previous,resolution}});
  assert.equal(state.issue,null);
  assert.equal(state.reconcile,false);
  assert.deepEqual(state.review.previous,previous);
  assert.deepEqual(state.review.resolution,resolution);
  assert.equal(currentTargetDraft(state,state.review.ticket),true);
  assert.deepEqual(state.view.record.binding.resolution,previous);
  assert.deepEqual(targetExpectation(state),targetExpectation(initial));
  const edited=editTarget(state,{...state.draft,arguments:['changed']});
  assert.equal(edited.review,null);
  assert.equal(edited.observation,null);
  assert.deepEqual(edited.view.record.binding.resolution,previous);
});

for (const {scenario,context} of [
  {scenario:'no context',context:null},
  {scenario:'a missing resolution',context:{previous_resolution:null}},
  {scenario:'a non-object resolution',context:{previous_resolution:null,resolution:'/metadata/redirected'}},
  {scenario:'an array resolution',context:{previous_resolution:null,resolution:[]}},
]) {
  test(`a changed-resolution refusal with ${scenario} stays a failure that offers no reviewed Save`,()=>{
    const initial=loadedTarget();
    const ticket=targetTicket(initial);
    const error={category:'TargetResolutionChanged',message:'Review changed resolution',context};
    const state=targetFailed(initial,ticket,error);
    assert.equal(state.review,null);
    assert.equal(state.issue.fault,error);
    assert.equal(currentTargetDraft(state,state.issue.ticket),true);
    assert.equal(state.reconcile,false);
  });
}

test('a completed save survives its failed refresh and recovery reload, then retires on a later edit',()=>{
  let state=loadedTarget();
  state=editTarget(state,{...state.draft,gamePath:'/metadata/saved-edit'});
  const ticket=targetTicket(state);
  const committed=targetView(state,{revision:2,configuration:readTargetDraft(state.draft).configuration});
  state=savedTarget(beginTarget(state,'save'),ticket,{view:committed,check:targetCheck(ticket,'/metadata/saved-edit').check});
  state=targetReadFailed(state,unreadable);
  state=beginTarget({...state,operation:null},'read');
  assert.equal(state.persisted,'saved');
  assert.equal(state.readError,null);
  assert.equal(state.refreshRequired,true);
  state=targetReadFailed(state,unreadable);
  assert.equal(state.persisted,'saved');
  state=readTarget(beginTarget({...state,operation:null},'read'),committed);
  assert.equal(state.persisted,'saved');
  assert.equal(state.refreshRequired,false);
  assert.equal(targetDirty(state),false);
  const edited=editTarget(state,{...state.draft,gamePath:'/metadata/next-edit'});
  assert.equal(edited.persisted,null);
  assert.equal(edited.draft.gamePath,'/metadata/next-edit');
  assert.equal(edited.view.record.revision,2);
  assert.deepEqual(targetExpectation(edited),{revision:2,binding_id:'binding-1'});
});

for (const {operation,persisted,configuration,complete} of [
  {operation:'save',persisted:'saved',configuration:targetConfiguration(),complete:(state,ticket,view)=>savedTarget(state,ticket,{view,check:targetCheck(ticket).check})},
  {operation:'remove',persisted:'removed',configuration:null,complete:removedTarget},
]) {
  test(`edits after completed ${operation} retain its outcome until the owed refresh recovers`,()=>{
    let state=loadedTarget();
    const ticket=targetTicket(state);
    const committed=targetView(state,{revision:2,configuration});
    state=complete(beginTarget(state,operation),ticket,committed);
    state=editTarget(state,{...state.draft,arguments:['edited while refreshing']});
    assert.equal(state.persisted,persisted);
    state=targetReadFailed(state,unreadable);
    state=editTarget(state,{...state.draft,arguments:['edited after refresh failure']});
    assert.equal(state.persisted,persisted);
    assert.equal(state.readError,unreadable);
    assert.equal(state.refreshRequired,true);
    assert.equal(targetExpectation(state),null);
    state=beginTarget({...state,operation:null},'read');
    assert.equal(state.persisted,persisted);
    state=readTarget(state,committed);
    assert.deepEqual(state.draft.arguments,['edited after refresh failure']);
    assert.deepEqual(state.view,committed);
    assert.equal(state.refreshRequired,false);
    state=editTarget(state,{...state.draft,arguments:['edited after recovery']});
    assert.equal(state.persisted,null);
  });
}

test('an unrelated reload after a refreshed save retires the notice, so its own failure reads as a plain read fault',()=>{
  let state=loadedTarget();
  const ticket=targetTicket(state);
  const committed=targetView(state,{revision:2,configuration:targetConfiguration()});
  state=readTarget(savedTarget(beginTarget(state,'save'),ticket,{view:committed,check:targetCheck(ticket).check}),committed);
  assert.equal(state.persisted,'saved');
  state=beginTarget({...state,operation:null},'read');
  assert.equal(state.persisted,null);
  state=targetReadFailed(state,unreadable);
  assert.equal(state.persisted,null);
  assert.equal(state.readError,unreadable);
  assert.equal(state.refreshRequired,true);
  assert.equal(state.view.record.revision,2);
  assert.equal(readTarget(beginTarget({...state,operation:null},'read'),committed).persisted,null);
});

test('removal keeps an unsaved draft and revision progression until Discard; old checks cannot resurrect the removed record',()=>{
  let state=loadedTarget();
  state=editTarget(state,{...state.draft,gamePath:'/metadata/keep-draft'});
  const ticket=targetTicket(state);
  const removed=targetView(state,{revision:2});
  state=removedTarget(beginTarget(state,'remove'),ticket,removed);
  state=readTarget(state,removed);
  assert.equal(state.persisted,'removed');
  assert.equal(state.view.record.binding,null);
  assert.deepEqual(targetExpectation(state),{revision:2,binding_id:null});
  assert.equal(state.draft.gamePath,'/metadata/keep-draft');
  assert.equal(targetDirty(state),true);
  assert.equal(checkedTarget(state,ticket,targetCheck(ticket)),state);
  const discarded=discardTarget(state);
  assert.equal(discarded.persisted,null);
  assert.equal(discarded.draft.gamePath,'');
});

for (const {scenario,declaration} of [
  {scenario:'targetless',declaration:null},
  {scenario:'changed',declaration:{id:'new-target',window_title:'New exact title'}},
]) {
  test(`a ${scenario} declaration retains an incompatible record for removal but never adopts its configuration`,()=>{
    const old=loadedTarget();
    let state=targetState(targetSelection('a',2,declaration));
    const retained={...old.view,context:state.context,compatible:false};
    state=readTarget(state,retained);
    assert.equal(state.view.record.binding.id,'binding-1');
    assert.equal(state.draft.gamePath,'');
    assert.equal(state.draft.windowTitle,declaration?.window_title ?? '');
    assert.equal(targetDirty(state),false);
    const ticket=targetTicket(state);
    assert.deepEqual(ticket.expected,{revision:1,binding_id:'binding-1'});
    const cleared=targetView(state,{revision:2});
    state=readTarget(removedTarget(state,ticket,cleared),cleared);
    assert.equal(state.view.record.binding,null);
    assert.equal(state.persisted,'removed');
  });
}

test('saved-view reload repairs a read fault but cannot erase an unresolved metadata failure or the edited recipe',()=>{
  let state=loadedTarget();
  state=editTarget(state,{...state.draft,gamePath:'/metadata/missing'});
  const ticket=targetTicket(state);
  const metadata={category:'TargetMetadata',message:'Selected target metadata is unavailable',context:{field:'game',stage:'canonicalize'}};
  state=targetFailed(state,ticket,metadata);
  state=targetReadFailed(state,unreadable);
  state=readTarget(state,state.view);
  assert.equal(state.readError,null);
  assert.equal(state.issue.fault,metadata);
  assert.equal(state.draft.gamePath,'/metadata/missing');
  assert.equal(state.view.record.binding.configuration.game.path,'/metadata/game');
  assert.equal(currentTargetDraft(state,state.issue.ticket),true);
  assert.equal(state.observation,null);
});
