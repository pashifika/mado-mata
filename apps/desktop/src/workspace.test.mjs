import {test} from 'node:test';
import assert from 'node:assert/strict';
import {applyCommand,applyIfCurrent,closeWorkspace,editDraft,freshWorkspace,ingestResults,inScope,matchesFilter,needsAttention,newDraft,openWorkspace,originLabel,retainClosed,selectProfile,shareCatalog,viewLogs,workspaceLabel,CLOSED_LIMIT} from './workspace.ts';

const schema={type:'object',properties:{count:{type:'integer',default:1},mode:{type:'string'}}};
function profile(id,name,values,packageId='pkg-a',schemaIdentity='schema-1'){
  return {version:1,id,name,package_id:packageId,schema_identity:schemaIdentity,values};
}
function selection(id,revision,path='/pkg/'+id,packageId='pkg-'+id,profiles=[profile('prof-'+id,'Saved '+id,{count:5},packageId)],schemaIdentity='schema-1'){
  return {workspace_id:id,revision,package_path:path,profiles_error:null,profiles,
    package:{package_id:packageId,inventory_identity:'inv-'+id,schema_identity:schemaIdentity,runtime:'quickjs',schema,profiles:{fast:{options:{count:9}}},effective_defaults:null}};
}
function terminal(run,extra={}){
  return {run,state:'terminal',operation:'run',result:{status:'PASS',cleanup:{clean:true},forced:false,exit_code:0},error:null,progress:[],dropped_logs:0,workspace_id:'a',workspace_revision:1,...extra};
}
// Two roots of one package/schema, as in /games/alpha and /games/beta, both listing saved profile P.
const shared=profile('P','Review',{count:5},'shared');
function sharedRoots(){
  const alpha=openWorkspace([],selection('a',1,'/games/alpha','shared',[shared]));
  return openWorkspace(alpha,selection('b',1,'/games/beta','shared',[shared]));
}
// The host lists the whole store or nothing: one malformed file arrives as an empty catalog plus this fault.
const unreadable={category:'StorageFormat',message:'stored JSON is malformed or incompatible; original data was preserved',context:null};
function unreadableListing(revision){
  return {...selection('a',revision,'/games/alpha','shared',[]),profiles_error:unreadable};
}

test('opening the same root again keeps the existing draft and takes the fresh catalog; a new revision resets on purpose',()=>{
  const first=openWorkspace([],selection('a',1));
  const edited=[editDraft({...first[0],lane:'replay',descriptorPath:'/corpus.json'},{count:'7'})];
  const refreshed=[profile('prof-a','Saved a',{count:5}),profile('prof-new','Added elsewhere',{count:2})];
  const again=openWorkspace(edited,selection('a',1,undefined,undefined,refreshed));
  assert.equal(again.length,1);
  assert.equal(again[0].revision,1);
  assert.deepEqual(again[0].draft,{count:'7'});
  assert.equal(again[0].draftRevision,edited[0].draftRevision);
  assert.equal(again[0].lane,'replay');
  assert.deepEqual(again[0].profiles.map(item=>item.id),['prof-a','prof-new']);
  const reinspected=openWorkspace(edited,selection('a',2));
  assert.deepEqual(reinspected[0].draft,{count:1});
  assert.equal(reinspected[0].revision,2);
  assert.equal(reinspected[0].touched,false);
  assert.equal(reinspected[0].lane,'replay');
  assert.equal(reinspected[0].descriptorPath,'/corpus.json');
  assert.ok(reinspected[0].draftRevision>edited[0].draftRevision);
});

test('a profile saved through one root reaches the other root of the same package/schema and no other',()=>{
  let list=openWorkspace(sharedRoots(),selection('c',1,'/games/gamma','shared',[],'schema-2'));
  list=applyIfCurrent(list,{id:'b',revision:1},item=>editDraft(item,{count:'3'}));
  const updated=profile('P','Review',{count:7},'shared');
  const synced=shareCatalog(list,'a',{profiles:[updated],profiles_error:null});
  assert.deepEqual(synced[1].profiles,[updated]);
  assert.deepEqual(synced[2].profiles,[]);
  assert.deepEqual(synced[1].draft,{count:'3'});
  assert.equal(synced[1].selectedId,null);
  assert.equal(synced[1].notice,null);
});

test('opening a second root of the same package refreshes the first root without resetting its draft',()=>{
  const alpha=applyIfCurrent(openWorkspace([],selection('a',1,'/games/alpha','shared',[shared])),{id:'a',revision:1},item=>editDraft(item,{count:'3'}));
  const list=openWorkspace(alpha,selection('b',1,'/games/beta','shared',[shared,profile('Q','Added on disk',{count:1},'shared')]));
  assert.deepEqual(list[0].profiles.map(item=>item.id),['P','Q']);
  assert.deepEqual(list[0].draft,{count:'3'});
  assert.equal(list[0].draftRevision,alpha[0].draftRevision);
});

for (const {scenario,localName,expectedName} of [
  {scenario:'an unedited name follows the rename',localName:'Review',expectedName:'Reviewed again'},
  {scenario:'a locally edited name is preserved',localName:'Mine',expectedName:'Mine'},
]) {
  test(`a selected profile updated elsewhere keeps the local draft as a draft and ${scenario}`,()=>{
    let list=applyIfCurrent(sharedRoots(),{id:'b',revision:1},item=>({...selectProfile(item,'P'),name:localName}));
    list=applyIfCurrent(list,{id:'b',revision:1},item=>({...editDraft(item,{count:'8'}),validation:{count:8}}));
    const before=list[1];
    list=shareCatalog(list,'a',{profiles:[profile('P','Reviewed again',{count:7},'shared')],profiles_error:null});
    const beta=list[1];
    assert.equal(beta.selectedId,'P');
    assert.deepEqual(beta.draft,{count:'8'});
    assert.equal(beta.draftRevision,before.draftRevision);
    assert.deepEqual(beta.validation,{count:8});
    assert.equal(beta.name,expectedName);
    assert.deepEqual(beta.profiles[0].values,{count:7});
  });
}

test('a selected profile deleted elsewhere becomes an explained unsaved draft with its values kept',()=>{
  let list=applyIfCurrent(sharedRoots(),{id:'b',revision:1},item=>selectProfile(item,'P'));
  list=shareCatalog(list,'a',{profiles:[],profiles_error:null});
  const beta=list[1];
  assert.equal(beta.selectedId,null);
  assert.deepEqual(beta.profiles,[]);
  assert.deepEqual(beta.draft,{count:5});
  assert.equal(beta.name,'Review');
  assert.equal(beta.touched,true);
});

test('reopening a root whose listing failed keeps its catalog, selection and draft, shows the fault, and recovers on the next readable listing',()=>{
  let list=applyIfCurrent(sharedRoots(),{id:'a',revision:1},item=>editDraft(selectProfile(item,'P'),{count:'4'}));
  list=openWorkspace(list,unreadableListing(1));
  const alpha=list[0];
  assert.deepEqual(alpha.profiles,[shared]);
  assert.equal(alpha.selectedId,'P');
  assert.equal(alpha.name,'Review');
  assert.deepEqual(alpha.draft,{count:'4'});
  assert.deepEqual(alpha.profilesError,unreadable);
  assert.deepEqual(list[1].profiles,[shared]);
  const added=profile('Q','Added on disk',{count:1},'shared');
  list=openWorkspace(list,selection('a',1,'/games/alpha','shared',[shared,added]));
  assert.deepEqual(list[0].profiles,[shared,added]);
  assert.equal(list[0].profilesError,null);
  assert.equal(list[0].selectedId,'P');
  assert.deepEqual(list[0].draft,{count:'4'});
  assert.deepEqual(list[1].profiles,[shared,added]);
});

test('a readable sibling catalog recovers an earlier listing fault without replacing either draft',()=>{
  let list=applyIfCurrent(sharedRoots(),{id:'a',revision:1},item=>editDraft(selectProfile(item,'P'),{count:'4'}));
  list=openWorkspace(list,unreadableListing(1));
  list=applyIfCurrent(list,{id:'b',revision:1},item=>editDraft(selectProfile(item,'P'),{count:'6'}));
  const updated=profile('P','Review',{count:7},'shared');
  list=openWorkspace(list,selection('b',1,'/games/beta','shared',[updated]));
  assert.equal(list[0].profilesError,null);
  assert.deepEqual(list.map(item=>item.profiles),[[updated],[updated]]);
  assert.deepEqual(list.map(item=>item.draft),[{count:'4'},{count:'6'}]);
  assert.deepEqual(list.map(item=>item.selectedId),['P','P']);
});

test('a failed listing in one root leaves the other root as it was; the next readable listing reaches it',()=>{
  let list=applyIfCurrent(sharedRoots(),{id:'b',revision:1},item=>editDraft(selectProfile(item,'P'),{count:'6'}));
  // Reinspect in alpha: the new revision takes the host's listing as it is, here empty plus the fault.
  const failed=unreadableListing(2);
  list=applyCommand(list,{id:'a',revision:1},{update:item=>freshWorkspace(failed,item),catalog:failed});
  assert.deepEqual(list[0].profiles,[]);
  assert.deepEqual(list[0].profilesError,unreadable);
  const beta=list[1];
  assert.equal(beta.selectedId,'P');
  assert.deepEqual(beta.profiles,[shared]);
  assert.deepEqual(beta.draft,{count:'6'});
  assert.equal(beta.notice,null);
  assert.equal(beta.profilesError,null);
  const added=profile('Q','Added on disk',{count:1},'shared');
  const recovered=selection('a',3,'/games/alpha','shared',[shared,added]);
  list=applyCommand(list,{id:'a',revision:2},{update:item=>freshWorkspace(recovered,item),catalog:recovered});
  assert.deepEqual(list[1].profiles,[shared,added]);
  assert.equal(list[1].selectedId,'P');
  assert.deepEqual(list[1].draft,{count:'6'});
  assert.equal(list[1].notice,null);
});

test('a ProfileRejected listing is a real partial catalog and reaches the other root',()=>{
  const rejected={category:'ProfileRejected',message:'Some saved profiles are incompatible; their files were preserved',context:{rejected:[{profile_id:'P'}]}};
  let list=applyIfCurrent(sharedRoots(),{id:'b',revision:1},item=>selectProfile(item,'P'));
  const partial={...selection('a',2,'/games/alpha','shared',[]),profiles_error:rejected};
  list=applyCommand(list,{id:'a',revision:1},{update:item=>freshWorkspace(partial,item),catalog:partial});
  const beta=list[1];
  assert.deepEqual(beta.profiles,[]);
  assert.equal(beta.selectedId,null);
  assert.deepEqual(beta.draft,{count:5});
  assert.equal(beta.name,'Review');
  assert.deepEqual(beta.profilesError,rejected);
});

test('an unchanged catalog or an unknown source leaves same-package roots as they were',()=>{
  const list=applyIfCurrent(sharedRoots(),{id:'b',revision:1},item=>editDraft(selectProfile(item,'P'),{count:'2'}));
  for (const synced of [shareCatalog(list,'a',{profiles:[shared],profiles_error:null}),shareCatalog(list,'gone',{profiles:[],profiles_error:null})]) {
    const beta=synced[1];
    assert.equal(beta.selectedId,'P');
    assert.deepEqual(beta.profiles,[shared]);
    assert.deepEqual(beta.draft,{count:'2'});
    assert.equal(beta.notice,null);
    assert.equal(synced[0].notice,list[0].notice);
  }
});

test('saving after a failed reinspection shares the full recovered catalog, not just the newly saved profile',()=>{
  let list=applyIfCurrent(sharedRoots(),{id:'b',revision:1},item=>editDraft(selectProfile(item,'P'),{count:'6'}));
  const failed=unreadableListing(2);
  list=applyCommand(list,{id:'a',revision:1},{update:item=>freshWorkspace(failed,item),catalog:failed});
  const added=profile('Q','Recovered save',{count:2},'shared');
  list=applyCommand(list,{id:'a',revision:2},{
    update:item=>({...item,profiles:[added],selectedId:added.id,name:added.name,draft:added.values}),
    catalog:{profiles:[shared,added],profiles_error:null},
  });
  assert.deepEqual(list.map(item=>item.profiles),[[shared,added],[shared,added]]);
  assert.deepEqual(list.map(item=>item.profilesError),[null,null]);
  assert.deepEqual(list.map(item=>item.selectedId),['Q','P']);
  assert.deepEqual(list[1].draft,{count:'6'});
});

for (const {scenario,update} of [
  {scenario:'validation',update:item=>({...item,validation:{count:5}})},
  {scenario:'a failed profile mutation',update:item=>({...item,error:unreadable})},
]) {
  test(`${scenario} cannot roll back another root after a successful write whose refresh failed`,()=>{
    const added=profile('Q','Saved despite refresh failure',{count:2},'shared');
    let list=applyCommand(sharedRoots(),{id:'a',revision:1},{
      update:item=>({...item,profiles:[shared,added],selectedId:added.id}),
      catalog:{profiles:[],profiles_error:unreadable},
    });
    list=applyCommand(list,{id:'b',revision:1},{update});
    assert.deepEqual(list[0].profiles,[shared,added]);
    assert.deepEqual(list[0].profilesError,unreadable);
    assert.equal(list[0].selectedId,'Q');
    assert.deepEqual(list[1].profiles,[shared]);
    assert.equal(list[1].profilesError,null);
  });
}

test('a catalog returned for an obsolete revision cannot overwrite compatible siblings',()=>{
  const list=openWorkspace(sharedRoots(),selection('a',2,'/games/alpha','shared',[shared]));
  const result=applyCommand(list,{id:'a',revision:1},{
    update:item=>({...item,profiles:[]}),
    catalog:{profiles:[],profiles_error:null},
  });
  assert.equal(result,list);
  assert.deepEqual(result[1].profiles,[shared]);
});

test('two workspaces keep independent drafts and switching never touches the other',()=>{
  let list=openWorkspace(openWorkspace([],selection('a',1)),selection('b',1));
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
  const open=openWorkspace(openWorkspace([],selection('a',1)),selection('b',1));
  const update=item=>({...item,validation:{count:1}});
  const afterClose=applyIfCurrent(closeWorkspace(open,'a'),{id:'a',revision:1},update);
  assert.equal(afterClose.length,1);
  assert.equal(afterClose[0].validation,null);
  const reinspected=openWorkspace(open,selection('a',2));
  assert.equal(applyIfCurrent(reinspected,{id:'a',revision:1},update),reinspected);
  const staleDraft=applyIfCurrent(open,{id:'a',revision:1,draftRevision:open[0].draftRevision+1},update);
  assert.equal(staleDraft,open);
  const applied=applyIfCurrent(open,{id:'a',revision:1,draftRevision:open[0].draftRevision},update);
  assert.deepEqual(applied[0].validation,{count:1});
  assert.equal(applied[1],open[1]);
});

test('same package ids from different roots stay distinguishable by location',()=>{
  const list=openWorkspace(openWorkspace([],selection('a',1,'/games/alpha','shared')),selection('b',1,'/games/beta','shared'));
  assert.equal(workspaceLabel(list[0],list),'shared · alpha');
  assert.equal(workspaceLabel(list[1],list),'shared · beta');
  assert.equal(workspaceLabel(list[0],[list[0]]),'shared');
});

test('origins report open, closed, unknown, and application scopes without reassigning events',()=>{
  const open=openWorkspace([],selection('a',1));
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
  const open=openWorkspace(openWorkspace([],selection('a',1)),selection('b',1));
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
  const olderRevision=ingestResults(successor,[{workspace:{workspace_id:'a',revision:1},controller:terminal('run-2')}],openWorkspace(open,selection('a',2)));
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
