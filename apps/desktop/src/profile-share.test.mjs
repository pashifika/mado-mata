import {test} from 'node:test';
import assert from 'node:assert/strict';
import {INITIAL_SHARE,ProfileShareSession,adoptSharedProfile,shareContext} from './profile-share.ts';
import {applyCatalog,applyIfCurrent,applySavedProfile,bindSelection,deriveBound,editDraft,newDraft,selectProfile,updateBound,workspaceFromView} from './workspace.ts';

const schema={type:'object',properties:{
  priority:{type:'array',items:{type:'string'},default:['ocr','template']},
  recognition:{type:'object',properties:{roi:{type:'object',properties:{x:{type:'integer'},y:{type:'integer'}}}}},
  note:{type:'string'},
}};
function profile(id,name,values,schemaIdentity='schema-1'){
  return {version:1,id,name,package_id:'pkg',schema_identity:schemaIdentity,values};
}
function boundTab(profiles){
  const selection={workspace_id:'a',revision:1,internal_name:'a',display_name:'Tab a',package_path:'/pkg',source:{kind:'directory',path:'/pkg'},profiles_error:null,profiles,
    package:{package_id:'pkg',inventory_identity:'inv',schema_identity:'schema-1',runtime:'quickjs',schema,profiles:{fast:{options:{priority:['template']}}},effective_defaults:null,target:null,target_identity:null}};
  return bindSelection(workspaceFromView({workspace_id:'a',revision:0,internal_name:'a',display_name:'Tab a',selection:null,source_error:null,saved_package:null,recovery:null}),selection);
}
function contextOf(tab){
  return shareContext(`${tab.id}:${tab.revision}`,tab.bound,deriveBound(tab.bound,null));
}
const flush=()=>new Promise(resolve=>setImmediate(resolve));
// Records every host request; each reply is settled explicitly so late and out-of-order completion is observable.
function harness(context){
  const calls=[];
  const replies=[];
  const request=call=>{
    calls.push(call);
    return new Promise((resolve,reject)=>replies.push({resolve,reject}));
  };
  const client={
    exportCode:(id,includeName)=>request({kind:'export',id,includeName}),
    decode:code=>request({kind:'decode',code}),
    copy:code=>request({kind:'copy',code}),
  };
  const view={state:INITIAL_SHARE};
  return {session:new ProfileShareSession(client,context,state=>{view.state=state;}),view,calls,replies};
}
const refusal={category:'ProfileShareSchema',message:'schema identity does not match the inspected package',context:{field:'schema_identity'}};
const sameName=profile('P1','Arena',{priority:['template','ocr'],recognition:{roi:{x:1,y:2}},note:'sender text'});
// The receiver's same-name saved profile, selected and edited without saving.
function editedReceiver(){
  const tab=updateBound(boundTab([sameName]),bound=>selectProfile(bound,'P1'));
  return updateBound(tab,bound=>editDraft(bound,{...bound.draft,note:'unsaved receiver edit'}));
}
// The Run page's profile-name input: a name-only edit that does not advance the draft counter.
const renamed=tab=>updateBound(tab,bound=>({...bound,name:'Renamed',touched:true}));

test('export reviews the saved record despite unsaved edits and copies only the displayed code on request',async()=>{
  const tab=editedReceiver();
  assert.equal(deriveBound(tab.bound,null).dirty,true);
  const {session,view,calls,replies}=harness(contextOf(tab));
  session.openExport();
  assert.deepEqual(calls,[{kind:'export',id:'P1',includeName:false}]);
  assert.equal(view.state.export.pending,true);
  assert.equal(view.state.export.code,null);
  session.includeName(true);
  assert.deepEqual(calls.at(-1),{kind:'export',id:'P1',includeName:true});
  replies[1].resolve('MMP1:{"named":true}');
  replies[0].resolve('MMP1:{"named":false}');
  await flush();
  assert.equal(view.state.export.code,'MMP1:{"named":true}','a late reply for the earlier name choice is dropped');
  assert.equal(calls.some(call=>call.kind==='copy'),false,'review alone never copies');

  const failed=session.copy();
  assert.deepEqual(calls.at(-1),{kind:'copy',code:'MMP1:{"named":true}'});
  replies[2].reject(refusal);
  await failed;
  assert.equal(view.state.export.copied,false);
  assert.deepEqual(view.state.export.copyError,refusal);
  assert.equal(view.state.export.error,null,'a copy failure is not an export failure');
  assert.equal(view.state.export.code,'MMP1:{"named":true}');
  const copied=session.copy();
  replies[3].resolve();
  await copied;
  assert.equal(view.state.export.copied,true);
  assert.equal(view.state.export.copyError,null);
});

test('export is offered only for an eligible saved selection and a withdrawn review ignores its late reply',async()=>{
  const tab=updateBound(boundTab([sameName,profile('P0','Old schema',{},'schema-0')]),bound=>selectProfile(bound,'P1'));
  for (const [scenario,source] of [
    ['new draft',updateBound(tab,bound=>newDraft(bound))],
    ['preset draft',updateBound(tab,bound=>newDraft(bound,'fast'))],
    ['profile saved under another schema',updateBound(tab,bound=>selectProfile(bound,'P0'))],
  ]) {
    const {session,view,calls}=harness(contextOf(source));
    session.openExport();
    assert.deepEqual(calls,[],scenario);
    assert.equal(view.state.export,null,scenario);
  }

  for (const [scenario,withdraw] of [
    ['cancel',session=>session.closeExport()],
    ['another saved selection',session=>session.observe({...contextOf(tab),source:profile('P2','Other',{})})],
    ['saved record updated',session=>session.observe({...contextOf(tab),source:{...sameName,values:{...sameName.values,note:'saved later'}}})],
    ['reinspected owner',session=>session.observe({...contextOf(tab),owner:'a:2'})],
    ['page left',session=>session.close()],
  ]) {
    const {session,view,calls,replies}=harness(contextOf(tab));
    session.openExport();
    withdraw(session);
    replies[0].resolve('MMP1:{}');
    await flush();
    assert.equal(view.state.export?.code ?? null,null,scenario);
    await session.copy();
    assert.deepEqual(calls.map(call=>call.kind),['export'],scenario);
  }

  const {session,view}=harness(contextOf(tab));
  session.openExport();
  session.observe(contextOf(renamed(updateBound(tab,bound=>editDraft(bound,{...bound.draft,note:'draft only'})))));
  assert.notEqual(view.state.export,null,'draft edits do not change what export reads');
});

test('refused, cancelled and stale decodes leave nothing to use',async()=>{
  const tab=editedReceiver();
  const context=contextOf(tab);
  const code='MMP1:{"package_id":"pkg"}';
  const preview={package_id:'pkg',schema_identity:'schema-1',values:{priority:['ocr','template']}};

  const refused=harness(context);
  refused.session.openImport();
  refused.session.edit(code);
  const decoding=refused.session.decode();
  refused.replies[0].reject(refusal);
  await decoding;
  assert.deepEqual(refused.view.state.import.error,refusal);
  assert.equal(refused.view.state.import.text,code,'the pasted text stays for correction');
  assert.equal(refused.session.take(),null);

  for (const [scenario,invalidate] of [
    ['cancel',session=>session.cancelImport()],
    ['text edited',session=>session.edit(`${code} `)],
    ['text edited and restored',session=>{session.edit('MMP1:'); session.edit(code);}],
    ['values edited',session=>session.observe(contextOf(updateBound(tab,bound=>editDraft(bound,{...bound.draft,note:'later'}))))],
    ['name edited',session=>session.observe(contextOf(renamed(tab)))],
    ['selected profile deleted',session=>session.observe(contextOf(applyCatalog(tab,{profiles:[],profiles_error:null})))],
    ['reinspected owner',session=>session.observe({...context,owner:'a:2'})],
    ['schema changed',session=>session.observe({...context,schemaIdentity:'schema-2'})],
    ['page left',session=>session.close()],
  ]) {
    const {session,view,calls,replies}=harness(context);
    session.openImport();
    session.edit(code);
    const late=session.decode();
    invalidate(session);
    replies[0].resolve(preview);
    await late;
    assert.equal(view.state.import?.preview ?? null,null,scenario);
    assert.equal(session.take(),null,scenario);
    assert.deepEqual(calls,[{kind:'decode',code}],scenario);
  }

  const shown=harness(context);
  shown.session.openImport();
  shown.session.edit(code);
  const decoded=shown.session.decode();
  shown.replies[0].resolve(preview);
  await decoded;
  assert.deepEqual(shown.view.state.import.preview,preview);
  shown.session.observe(contextOf(renamed(tab)));
  assert.equal(shown.view.state.import.preview,null,'a shown preview is withdrawn by a name-only edit');
  assert.equal(shown.view.state.import.stale,true);
  assert.equal(shown.session.take(),null);
});

test('a reviewed code replaces only the reviewed draft as an unbound draft, and Save new profile keeps the same-name record',async()=>{
  let tab=editedReceiver();
  const before=tab.bound;
  const facts=deriveBound(before,null);
  assert.equal(before.touched&&facts.dirty,true,'Use is announced as discarding unsaved edits');
  const pasted='  MMP1:{"package_id":"pkg"}\n';
  const shared={package_id:'pkg',schema_identity:'schema-1',name:'Arena',
    values:{priority:['ocr','template'],recognition:{roi:{x:40,y:8}},note:'sender text'}};
  const {session,view,calls,replies}=harness(contextOf(tab));
  session.openImport();
  session.edit(pasted);
  const decoding=session.decode();
  replies[0].resolve(shared);
  await decoding;
  assert.deepEqual(view.state.import.preview,shared);
  assert.equal(tab.bound,before,'decoding leaves the draft alone');

  const taken=session.take();
  assert.equal(view.state.import,null);
  assert.deepEqual(calls,[{kind:'decode',code:pasted}],'the original text is decoded; nothing is saved or started');

  // Use is refused when the draft changed after review, even without a draft-counter change.
  for (const [scenario,changed] of [
    ['name edited',renamed(tab)],
    ['values edited',updateBound(tab,bound=>editDraft(bound,{...bound.draft,note:'later edit'}))],
    ['new draft',updateBound(tab,bound=>newDraft(bound))],
    ['saved as another profile',updateBound(tab,bound=>applySavedProfile(bound,profile('P3','Arena',bound.draft),bound.draftRevision))],
    ['selected profile deleted',applyCatalog(tab,{profiles:[],profiles_error:null})],
  ]) {
    assert.equal(adoptSharedProfile(changed.bound,taken.shared,taken.reviewed),null,scenario);
  }
  assert.equal(adoptSharedProfile(before,{...shared,schema_identity:'schema-0'},taken.reviewed),null,'another schema is never adopted');

  [tab]=applyIfCurrent([tab],{id:'a',revision:1},item=>({...item,bound:adoptSharedProfile(item.bound,taken.shared,taken.reviewed)}));
  assert.equal(tab.bound.selectedId,null);
  assert.equal(tab.bound.name,'Arena');
  assert.deepEqual(tab.bound.draft,shared.values);
  assert.equal(tab.bound.profiles,before.profiles,'the saved catalog is untouched');
  const adopted=deriveBound(tab.bound,null);
  assert.equal(adopted.selectedProfile,undefined);
  assert.deepEqual(adopted.parsed.values,shared.values);

  // Save new profile names no saved ID; the host's new record joins the catalog beside the same-name one.
  const saved=applySavedProfile(tab.bound,profile('P2','Arena',shared.values),tab.bound.draftRevision);
  assert.deepEqual(saved.profiles.map(item=>item.id).sort(),['P1','P2']);
  assert.deepEqual(saved.profiles.find(item=>item.id==='P1'),sameName);
  assert.equal(saved.selectedId,'P2');
  assert.equal(saved.touched,false);
});
