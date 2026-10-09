import {test} from 'node:test';
import assert from 'node:assert/strict';
import {acceptController,attemptOutcomes,nativeOutcome,retainLogs,defaultDraft,readDraft,verifiedCleanup,cleanupLabel,readEnvironment,environmentDraft,sameEnvironment,staleReasons,boundedText,faultSummary,readSettingsDraft,settingsDraftAfterSave,settingsDraftFrom,packageDestination,portableComponent,SUPPORTED_PROFILES,DEFAULT_NOTIFICATIONS} from './state.ts';
import {messages} from './i18n.ts';
import {OcrSetupSession,setupEnvironment} from './settings/ocr-setup.ts';

test('late predecessor result cannot replace the successor or its preparing state',()=>{
  const current={run:'next',state:'preparing',result:null};
  assert.equal(acceptController(current,{run:'old',state:'terminal',result:{status:'PASS'}},'next'),current);
  const running={run:'next',state:'running',result:null};
  assert.equal(acceptController(current,running,'next'),running);
});

test('an OS launch completing after Stop is shown while stale and foreign preparation answers are ignored',()=>{
  const stopping={run:'owned',state:'stopping',result:null,native_preparation:{status:'pending',phase:'launch_submission',launch:'not_requested'}};
  const late={...stopping,native_preparation:{status:'pending',phase:'launch_submission',launch:'accepted'}};
  assert.equal(acceptController(stopping,late,'owned'),late);
  // An answer read before Stop cannot revive the operation or rewind its status or stage.
  const stale={...stopping,state:'preparing',native_preparation:{status:'not_requested',phase:'readiness',launch:'not_requested'}};
  assert.equal(acceptController(late,stale,'owned'),late);
  // Another operation's progress never updates the owned one.
  const foreign={run:'other',state:'preparing',result:null,native_preparation:{status:'capture_ready',phase:'readiness',launch:'accepted'}};
  assert.equal(acceptController(late,foreign,'owned'),late);
  const settled={...late,state:'terminal',error:{category:'Cancelled',message:'attempt cancellation is latched',context:null}};
  assert.equal(acceptController(settled,{...settled,native_preparation:{status:'pending',phase:'waiting_for_window',launch:'accepted'}},'owned'),settled);
});

test('the same Run never accepts an older attempt or a stale continuation after Stop or settlement',()=>{
  const current={run:'owned',state:'running',result:null,attempts:[],native_preparation:{attempt:2,status:'capture_ready',phase:'workflow',launch:'accepted'}};
  const old={...current,state:'terminal',native_preparation:{attempt:1,status:'capture_ready',phase:'workflow',launch:'not_requested'}};
  assert.equal(acceptController(current,old,'owned'),current);
  const stopping={...current,state:'stopping'};
  const recovering={...current,state:'recovering',native_preparation:{...current.native_preparation,phase:'recovering'}};
  assert.equal(acceptController(stopping,recovering,'owned'),stopping);
  const terminal={...current,state:'terminal',result:{status:'PASS'}};
  assert.equal(acceptController(terminal,current,'owned'),terminal);
  assert.equal(acceptController(current,recovering,'owned'),recovering);
});

test('retained attempt projection preserves original typed exit and independent successor Script, receipts and cleanup',()=>{
  const first={attempt:1,status:'FAIL',stage:'workflow',entry_outcome:'FailedOrNotStarted',primary:{category:'TargetExited',message:'private evidence',context:{exit_reason:'reused_pid',process_lifetime:'private'}},
    native_preparation:{attempt:1,phase:'workflow',status:'capture_ready',launch:'not_requested'},cleanup:{clean:true},forced:false,exit_code:0,observations:{receipts:[{status:'completed'}]}};
  const second={attempt:2,status:'PASS',stage:'workflow',entry_outcome:'Returned',primary:null,
    native_preparation:{attempt:2,phase:'workflow',status:'capture_ready',launch:'accepted'},cleanup:{clean:true},forced:false,exit_code:0,observations:{receipts:[]}};
  const live={run:'owned',state:'recovering',result:null,attempts:[first],progress:[],native_preparation:{attempt:1,phase:'settling',status:'capture_ready',launch:'not_requested'}};
  const original=attemptOutcomes(live);
  assert.equal(original[0].exitReason,'reused_pid');
  assert.equal(original[0].primary,'TargetExited');
  const final={...live,state:'terminal',result:{status:'PASS',recovery_count:1},attempts:[first,second]};
  const projected=attemptOutcomes(final);
  assert.deepEqual(projected.map(value=>[value.attempt,value.status,value.stage,value.entry,value.launch,value.receipts]),
    [[1,'FAIL','workflow','FailedOrNotStarted','not_requested',1],[2,'PASS','workflow','Returned','accepted',0]]);
  assert.equal(projected[0].cleanup,messages.en.validation.clean);
  assert.equal(projected[1].cleanup,messages.en.validation.clean);
  assert.deepEqual(projected.map(value=>value.phase),['workflow','workflow']);
  assert.equal(projected[0].retained,first);
  assert.equal(projected[1].retained,second);
  assert.equal(first.primary.context.process_lifetime,'private');
  const incomplete=attemptOutcomes({...final,attempts:[first,{...second,forced:true}]});
  assert.equal(incomplete[1].cleanup,messages.en.validation.incomplete);
});

test('attempt summaries do not infer confirmed exit or Script completion from diagnostic text or capture-ready state',()=>{
  const summary={attempt:1,status:'FAIL',stage:'workflow',primary:{category:'TargetLost',message:'TargetExited: bound process absent',context:{exit_reason:'absent'}},
    native_preparation:{attempt:1,phase:'readiness',status:'capture_ready',launch:'accepted'},cleanup:{clean:true},forced:false,exit_code:0};
  const projected=attemptOutcomes({attempts:[summary,{...summary,attempt:2},summary,{...summary,attempt:3}]});
  assert.equal(projected.length,2);
  assert.equal(projected[0].exitReason,null);
  assert.equal(projected[0].entry,null);
  assert.equal(projected[0].receipts,null);
});
for (const {scenario,category,reason,expected} of [
  {scenario:'positive absence is retained as confirmed exit',category:'TargetExited',reason:'absent',expected:'absent'},
  {scenario:'positive PID reuse is retained as old-lifetime exit',category:'TargetExited',reason:'reused_pid',expected:'reused_pid'},
  {scenario:'verified same-lifetime zombie is retained as terminated',category:'TargetExited',reason:'zombie',expected:'zombie'},
  {scenario:'unknown reason cannot classify confirmed exit',category:'TargetExited',reason:'unknown',expected:null},
  {scenario:'generic target loss cannot inherit a typed exit reason',category:'TargetLost',reason:'zombie',expected:null},
]) {
  test(scenario,()=>{
    const [summary]=attemptOutcomes({attempts:[{attempt:1,primary:{category,context:{exit_reason:reason}}}]});
    assert.equal(summary.exitReason,expected);
  });
}


const cancelled={category:'Cancelled',message:'attempt cancellation is latched',context:null};
function fault(category) {
  return {category,message:`${category} fixture`,context:null};
}
function settledNative(preparation,{error=null,result=null,state='terminal'}={}) {
  return {run:'owned',state,operation:'run',result,error,progress:[],dropped_logs:0,workspace_id:'a',workspace_revision:1,native_preparation:preparation ? {attempt:1,...preparation} : null,attempts:[]};
}
for (const {scenario, view, expected} of [
  {scenario:'Stop after an accepted launch is no stage failure but warns that the game can still open',
    view:settledNative({status:'pending',phase:'waiting_for_process',launch:'accepted'},{error:cancelled}),expected:{failure:null,cause:null,launch:'acceptedStopped'}},
  {scenario:'a window timeout after an accepted launch names the stage and the still-open game',
    view:settledNative({status:'pending',phase:'waiting_for_window',launch:'accepted'},{error:fault('Timeout')}),expected:{failure:'waiting_for_window',cause:null,launch:'accepted'}},
  {scenario:'a launch-stage timeout retains an independently accepted OS request',
    view:settledNative({status:'pending',phase:'launch_submission',launch:'accepted'},{error:fault('Timeout')}),expected:{failure:'launch_submission',cause:null,launch:'accepted'}},
  {scenario:'an unconfirmed submission names the launch stage without claiming acceptance',
    view:settledNative({status:'pending',phase:'launch_submission',launch:'uncertain'},{error:fault('NativeLaunchUncertain')}),expected:{failure:'launch_submission',cause:null,launch:'uncertain'}},
  {scenario:'a launch rejected after Stop keeps both the Stop and the rejection',
    view:settledNative({status:'pending',phase:'launch_submission',launch:'rejected'},{error:cancelled}),expected:{failure:null,cause:null,launch:'rejected'}},
  {scenario:'an absent game without launch approval names the missing-target remedy',
    view:settledNative({status:'pending',phase:'target_discovery',launch:'not_requested'},{error:fault('NativeTargetMissing')}),expected:{failure:'target_discovery',cause:'missing',launch:null}},
  {scenario:'several matching running copies name the ambiguity remedy',
    view:settledNative({status:'pending',phase:'target_discovery',launch:'not_requested'},{error:fault('NativeTargetAmbiguous')}),expected:{failure:'target_discovery',cause:'ambiguous',launch:null}},
  {scenario:'a preflight refusal before Script entry names preflight rather than a missing startup request',
    view:settledNative({status:'not_requested',phase:'preflight',launch:'not_requested'},{error:fault('Profile')}),expected:{failure:'preflight',cause:null,launch:null}},
  {scenario:'a startup timeout before the Script requested startup names the missing request',
    view:settledNative({status:'not_requested',phase:'readiness',launch:'not_requested'},{error:fault('Timeout')}),expected:{failure:'unrequested',cause:null,launch:null}},
  {scenario:'a Script failure while startup is pending is not reported as a Readiness criteria miss',
    view:settledNative({status:'pending',phase:'waiting_for_process',launch:'accepted'},{result:{status:'FAIL',primary:fault('Script')}}),expected:{failure:'readinessPending',cause:null,launch:'accepted'}},
  {scenario:'early Ready is a Script contract refusal, not an exact-window binding failure',
    view:settledNative({status:'pending',phase:'waiting_for_window',launch:'not_requested'},{error:fault('ReadinessContract')}),expected:{failure:'readinessPending',cause:null,launch:null}},
  {scenario:'a duplicate startup request does not blame the in-flight SDK initializer',
    view:settledNative({status:'pending',phase:'native_initialization',launch:'accepted'},{error:fault('NativeStartRefused')}),expected:{failure:'readinessPending',cause:null,launch:'accepted'}},
  {scenario:'a readiness timeout after capture became available names the Script criteria and keeps the launch',
    view:settledNative({status:'capture_ready',phase:'readiness',launch:'accepted'},{error:fault('Timeout')}),expected:{failure:'readinessCriteria',cause:null,launch:'accepted'}},
  {scenario:'Stop before the Script requested startup is no failure and claims no launch',
    view:settledNative({status:'not_requested',phase:'readiness',launch:'not_requested'},{error:cancelled}),expected:{failure:null,cause:null,launch:null}},
  {scenario:'a workflow failure in the settled result keeps the accepted launch visible',
    view:settledNative({status:'capture_ready',phase:'workflow',launch:'accepted'},{result:{status:'FAIL',primary:fault('Script')}}),expected:{failure:'workflow',cause:null,launch:'accepted'}},
  {scenario:'a successful launched run leaves nothing to act on',
    view:settledNative({status:'capture_ready',phase:'workflow',launch:'accepted'},{result:{status:'PASS',primary:null}}),expected:{failure:null,cause:null,launch:null}},
  {scenario:'a still-stopping operation has no settled projection yet',
    view:settledNative({status:'pending',phase:'launch_submission',launch:'accepted'},{state:'stopping'}),expected:null},
  {scenario:'an operation without Native preparation has no projection',
    view:settledNative(null,{error:cancelled}),expected:null},
]) {
  test(scenario,()=>{
    assert.deepEqual(nativeOutcome(view),expected);
  });
}

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

for (const locale of ['en','ja']) {
  test(`preparation cleanup remains distinct from unverified or forced cleanup in ${locale}`,()=>{
    const labels=messages[locale].validation;
    assert.equal(cleanupLabel(null,{cleanup:{clean:true,child_started:false}},locale),labels.cleanNoChild);
    assert.equal(cleanupLabel(null,{cleanup:{clean:true}},locale),labels.unverified);
    assert.equal(cleanupLabel(null,{cleanup:{clean:false,child_started:true}},locale),labels.incomplete);
    assert.equal(cleanupLabel({status:'FAIL',cleanup:{clean:true},forced:true,exit_code:0},{},locale),labels.incomplete);
  });
}

for (const {scenario,draft} of [
  {scenario:'an empty environment draft is unconfigured',draft:environmentDraft(null)},
  {scenario:'whitespace-only environment paths are unconfigured',draft:{profile:'',model_root:' \t',runtime_path:' ',library_paths:'\n \t\r\n'}},
]) {
  test(scenario,()=>{
    assert.deepEqual(readEnvironment(draft),{environment:null,errors:{}});
  });
}

test('a selected environment profile requires every path field',()=>{
  const partial=readEnvironment({profile:SUPPORTED_PROFILES[0].profile,model_root:'  ',runtime_path:'',library_paths:''});
  assert.equal(partial.environment,null);
  assert.deepEqual(Object.keys(partial.errors).sort(),['library_paths','model_root','runtime_path']);
});

for (const {scenario,library_paths} of [
  {scenario:'a configured environment refuses zero reviewed libraries',library_paths:''},
  {scenario:'a configured environment refuses 65 reviewed libraries',library_paths:Array.from({length:65},(_,index)=>`/lib/${index}.dylib`).join('\n')},
]) {
  test(scenario,()=>{
    const draft={profile:SUPPORTED_PROFILES[0].profile,model_root:' /models ',runtime_path:'/rt.dylib',library_paths};
    const original=structuredClone(draft);
    const parsed=readEnvironment(draft);
    assert.equal(parsed.environment,null);
    assert.deepEqual(Object.keys(parsed.errors),['library_paths']);
    assert.deepEqual(draft,original);
  });
}

for (const {scenario,libraries} of [
  {scenario:'a configured environment accepts one reviewed library',libraries:['/lib/a.dylib']},
  {scenario:'a configured environment accepts 64 reviewed libraries despite blank lines',libraries:Array.from({length:64},(_,index)=>`/lib/${index}.dylib`)},
]) {
  test(scenario,()=>{
    const draft={profile:SUPPORTED_PROFILES[0].profile,model_root:'/models',runtime_path:'/rt.dylib',library_paths:`\n${libraries.map(path=>` ${path} `).join('\r\n\n')}\n`};
    const parsed=readEnvironment(draft);
    assert.deepEqual(parsed.errors,{});
    assert.deepEqual(parsed.environment?.native_library_paths,libraries);
  });
}

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

const checkedEnvironment=readEnvironment({profile:SUPPORTED_PROFILES[0].profile,model_root:'/models',runtime_path:'/rt.dylib',library_paths:'/lib/a.dylib'}).environment;
const workspace={workspace_id:'ws-1',revision:1};
const association={operation:'desktop-1',workspace,environment:checkedEnvironment,descriptorPath:'/corpus/a.json',packageInventoryIdentity:'inv-1'};
const unchanged={saved:checkedEnvironment,draftDirty:false,workspace,descriptorPath:'/corpus/a.json',packageInventoryIdentity:'inv-1'};
for (const {scenario,current,reasons} of [
  {scenario:'nothing changed since the check',current:unchanged,reasons:0},
  {scenario:'the environment was saved again with another path',current:{...unchanged,saved:{...checkedEnvironment,model_root:'/models-2'}},reasons:1},
  {scenario:'the draft has unsaved edits even though saved settings match',current:{...unchanged,draftDirty:true},reasons:1},
  {scenario:'another descriptor is selected',current:{...unchanged,descriptorPath:null},reasons:1},
  {scenario:'another package is inspected',current:{...unchanged,packageInventoryIdentity:'inv-2',workspace:{workspace_id:'ws-2',revision:1}},reasons:1},
  {scenario:'the inspected package was forgotten',current:{...unchanged,packageInventoryIdentity:null,workspace:null},reasons:1},
  {scenario:'the same package inventory is selected through another workspace',current:{...unchanged,workspace:{workspace_id:'ws-2',revision:1}},reasons:1},
  {scenario:'the workspace was reinspected to a newer revision',current:{...unchanged,workspace:{workspace_id:'ws-1',revision:2}},reasons:1},
  {scenario:'the environment was cleared after the check',current:{...unchanged,saved:null,draftDirty:true},reasons:2},
]) {
  test(`check association: ${scenario}`,()=>{
    assert.equal(staleReasons(association,current).length,reasons);
  });
}

test('a completed settings Save preserves later locale and invalid input edits',()=>{
  const submitted={...settingsDraftFrom(null),locale:'ja'};
  const saved={version:1,package_path:null,...readSettingsDraft(submitted).settings};
  const current={...submitted,locale:'en',logLimit:'not a number'};
  const settled=settingsDraftAfterSave(current,submitted,saved);
  assert.equal(settled.locale,'en');
  assert.equal(settled.logLimit,'not a number');
  const parsed=readSettingsDraft(settled);
  assert.equal(parsed.settings,null);
  assert.ok(parsed.errors.logLimit);
});

const validSettingsDraft={locale:'en',logLimit:' 250 ',notifications:{...DEFAULT_NOTIFICATIONS},completionAutomatic:true,completionDelayMs:'100',captureCacheEnabled:false,environment:environmentDraft(checkedEnvironment),backupDirectory:'',packagesRoot:''};
test('a complete settings draft becomes one editable settings object without version or package hint',()=>{
  const parsed=readSettingsDraft(validSettingsDraft);
  assert.deepEqual(parsed.errors,{});
  assert.deepEqual(parsed.settings,{locale:'en',gui_log_limit:250,ocr_environment:checkedEnvironment,notifications:{visible_count:2,timeout_seconds:8,show_success:true},editor_completion:{automatic:true,delay_ms:100},capture_cache_enabled:false,backup_directory:null,packages_root:null});
  assert.equal(readSettingsDraft({...validSettingsDraft,environment:environmentDraft(null)}).settings.ocr_environment,null);
});

test('completion drafts use host values without previewing or mutating the saved preferences',()=>{
  const initial=settingsDraftFrom(null);
  assert.equal(initial.completionAutomatic,true);
  assert.equal(initial.completionDelayMs,'100');
  const saved={version:1,package_path:null,...readSettingsDraft(initial).settings,editor_completion:{automatic:false,delay_ms:0}};
  const original=structuredClone(saved);
  const draft=settingsDraftFrom(saved);
  assert.equal(draft.completionAutomatic,false);
  assert.equal(draft.completionDelayMs,'0');
  draft.completionAutomatic=true;
  draft.completionDelayMs='1000';
  assert.deepEqual(readSettingsDraft(draft).settings.editor_completion,{automatic:true,delay_ms:1000});
  assert.deepEqual(saved,original);
  const reopened=settingsDraftFrom(saved);
  assert.equal(reopened.completionAutomatic,false);
  assert.equal(reopened.completionDelayMs,'0');
});

for (const {scenario,automatic,delay,expected} of [
  {scenario:'zero opening delay with automatic disabled',automatic:false,delay:'0',expected:{automatic:false,delay_ms:0}},
  {scenario:'the default delay with automatic enabled',automatic:true,delay:'100',expected:{automatic:true,delay_ms:100}},
  {scenario:'the upper boundary with surrounding whitespace',automatic:false,delay:' 1000 ',expected:{automatic:false,delay_ms:1000}},
]) {
  test(`completion preferences accept ${scenario}`,()=>{
    const parsed=readSettingsDraft({...validSettingsDraft,completionAutomatic:automatic,completionDelayMs:delay});
    assert.deepEqual(parsed.errors,{});
    assert.deepEqual(parsed.settings.editor_completion,expected);
  });
}

test('settled completion preferences are reconstructed from the authoritative Save response',()=>{
  const submitted={...validSettingsDraft,completionAutomatic:false,completionDelayMs:' 250 '};
  const saved={version:1,package_path:null,...readSettingsDraft(submitted).settings};
  const settled=settingsDraftAfterSave(submitted,submitted,saved);
  assert.equal(settled.completionAutomatic,false);
  assert.equal(settled.completionDelayMs,'250');
  assert.deepEqual(readSettingsDraft(settled).settings.editor_completion,saved.editor_completion);
  assert.equal(submitted.completionDelayMs,' 250 ');
});

for (const {scenario,edit,expected,errors} of [
  {scenario:'an automatic switch edit',edit:{completionAutomatic:true},expected:{automatic:true,delay_ms:250},errors:[]},
  {scenario:'a valid delay edit',edit:{completionDelayMs:'1000'},expected:{automatic:false,delay_ms:1000},errors:[]},
  {scenario:'an invalid pending delay',edit:{completionDelayMs:''},expected:null,errors:['completionDelayMs']},
]) {
  test(`a completed Save preserves ${scenario} made after submission`,()=>{
    const submitted={...validSettingsDraft,completionAutomatic:false,completionDelayMs:'250'};
    const saved={version:1,package_path:null,...readSettingsDraft(submitted).settings};
    const current={...submitted,...edit};
    const settled=settingsDraftAfterSave(current,submitted,saved);
    assert.equal(settled,current);
    const parsed=readSettingsDraft(settled);
    assert.deepEqual(parsed.settings?.editor_completion ?? null,expected);
    assert.deepEqual(Object.keys(parsed.errors),errors);
    assert.deepEqual(saved.editor_completion,{automatic:false,delay_ms:250});
  });
}
test('cache preference persists through a saved settings draft without overriding a later edit',()=>{
  const initial=settingsDraftFrom(null);
  assert.equal(initial.captureCacheEnabled,false);
  const submitted={...initial,captureCacheEnabled:true};
  const saved={version:1,package_path:null,...readSettingsDraft(submitted).settings};
  assert.equal(saved.capture_cache_enabled,true);
  assert.equal(settingsDraftAfterSave(submitted,submitted,saved).captureCacheEnabled,true);
  const later={...submitted,captureCacheEnabled:false};
  assert.equal(settingsDraftAfterSave(later,submitted,saved).captureCacheEnabled,false);
});

test('the backup directory draft is blank for the default destination and otherwise saved as typed without padding',()=>{
  assert.equal(settingsDraftFrom(null).backupDirectory,'');
  assert.equal(settingsDraftFrom({version:1,gui_log_limit:1000,package_path:null,ocr_environment:null,notifications:DEFAULT_NOTIFICATIONS,editor_completion:{automatic:true,delay_ms:100},locale:'en',backup_directory:'/private/backups'}).backupDirectory,'/private/backups');
  assert.equal(settingsDraftFrom({version:1,gui_log_limit:1000,package_path:null,ocr_environment:null,
    notifications:DEFAULT_NOTIFICATIONS,editor_completion:{automatic:true,delay_ms:100},locale:'en',backup_directory:null}).captureCacheEnabled,false,
    'older saved settings without the flag default to opt-out');
  assert.equal(readSettingsDraft({...validSettingsDraft,backupDirectory:'   '}).settings.backup_directory,null);
  assert.equal(readSettingsDraft({...validSettingsDraft,backupDirectory:' /private/backups '}).settings.backup_directory,'/private/backups');
});

test('packages root defaults, persisted drafts and edits made during Save remain distinct',()=>{
  const submitted={...validSettingsDraft,packagesRoot:' /private/packages '};
  const saved={version:1,package_path:null,...readSettingsDraft(submitted).settings};
  assert.equal(saved.packages_root,'/private/packages');
  assert.equal(settingsDraftAfterSave(submitted,submitted,saved).packagesRoot,'/private/packages');
  const changed={...submitted,packagesRoot:'/private/later'};
  assert.equal(settingsDraftAfterSave(changed,submitted,saved).packagesRoot,'/private/later');
  assert.equal(readSettingsDraft({...submitted,packagesRoot:'  '}).settings.packages_root,null);
  assert.equal(settingsDraftFrom(null).packagesRoot,'');
});

test('package destination preview joins only portable IDs beneath the saved root',()=>{
  assert.equal(packageDestination('/private/pkgs','example.starter'),'/private/pkgs/example.starter');
  assert.equal(packageDestination('C:\\Packages\\','demo'),'C:\\Packages\\demo');
  assert.equal(packageDestination('','demo'),null);
  for (const id of ['../escape','a/b','a\\b','.hidden','last.','CON.txt','com9','NODE_MODULES','a'.repeat(129)]) {
    assert.equal(portableComponent(id),false,id);
    assert.equal(packageDestination('/private/pkgs',id),null,id);
  }
});

for (const {scenario,draft,field} of [
  {scenario:'a zero log limit',draft:{...validSettingsDraft,logLimit:'0'},field:'logLimit'},
  {scenario:'a log limit above 10000',draft:{...validSettingsDraft,logLimit:'10001'},field:'logLimit'},
  {scenario:'a non-integer log limit',draft:{...validSettingsDraft,logLimit:'1e3'},field:'logLimit'},
  {scenario:'three visible cards',draft:{...validSettingsDraft,notifications:{...DEFAULT_NOTIFICATIONS,visible_count:3}},field:'visibleCount'},
  {scenario:'a ten second timeout',draft:{...validSettingsDraft,notifications:{...DEFAULT_NOTIFICATIONS,timeout_seconds:10}},field:'timeoutSeconds'},
  {scenario:'a nonboolean automatic completion flag',draft:{...validSettingsDraft,completionAutomatic:'false'},field:'completionAutomatic'},
  {scenario:'a missing automatic completion flag',draft:{...validSettingsDraft,completionAutomatic:undefined},field:'completionAutomatic'},
  {scenario:'an empty completion delay',draft:{...validSettingsDraft,completionDelayMs:''},field:'completionDelayMs'},
  {scenario:'a negative completion delay',draft:{...validSettingsDraft,completionDelayMs:'-1'},field:'completionDelayMs'},
  {scenario:'a fractional completion delay',draft:{...validSettingsDraft,completionDelayMs:'0.5'},field:'completionDelayMs'},
  {scenario:'a completion delay above 1000',draft:{...validSettingsDraft,completionDelayMs:'1001'},field:'completionDelayMs'},
  {scenario:'an exponential completion delay',draft:{...validSettingsDraft,completionDelayMs:'1e2'},field:'completionDelayMs'},
  {scenario:'a partial environment',draft:{...validSettingsDraft,environment:{...environmentDraft(checkedEnvironment),model_root:''}},field:'model_root'},
  {scenario:'an unsupported language',draft:{...validSettingsDraft,locale:'fr'},field:'locale'},
  {scenario:'a null language',draft:{...validSettingsDraft,locale:null},field:'locale'},
  {scenario:'a relative packages root',draft:{...validSettingsDraft,packagesRoot:'packages'},field:'packagesRoot'},
  {scenario:'packages root traversal',draft:{...validSettingsDraft,packagesRoot:'/private/../config'},field:'packagesRoot'},
  {scenario:'packages root controls',draft:{...validSettingsDraft,packagesRoot:'/private/new\nline'},field:'packagesRoot'},
  {scenario:'an oversized packages root',draft:{...validSettingsDraft,packagesRoot:`/${'あ'.repeat(1400)}`},field:'packagesRoot'},
]) {
  test(`settings draft refuses ${scenario} without producing a save payload`,()=>{
    const parsed=readSettingsDraft(draft);
    assert.equal(parsed.settings,null);
    assert.ok(parsed.errors[field]);
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

function deferredSetup() {
  let resolve, reject;
  const promise=new Promise((yes,no)=>{resolve=yes; reject=no;});
  return {promise,resolve,reject};
}

function setupView({environment=null,resolved={},items=null,native_methods=['homebrew','folders']}={}) {
  return {
    items:items ?? ['rapidocr-models','onnxruntime','native-libraries'].map(id=>({
      id,name:{en:id,ja:id},state:'missing',detail:{en:'fixture',ja:'fixture'},downloadable:id!=='native-libraries',links:[],commands:[],
    })),
    environment,
    resolved:{model_root:environment?.model_root ?? null,runtime_path:environment?.runtime_path ?? null,native_library_paths:environment?.native_library_paths ?? null,...resolved},
    native_methods,
  };
}

function setupOperation({id='owned',active=false,stage='complete',resource_id='rapidocr-models',result=null,error=null,cleanup_error=null}={}) {
  return {id,active,progress:{stage,resource_id,bytes:100,total:100},result,error,cleanup_error};
}

function setupHarness(initial=validSettingsDraft, overrides={}) {
  const calls=[], published=[], adopted=[];
  let draft=initial;
  const client={
    catalog:async()=>{calls.push({kind:'catalog'}); return overrides.catalog ? overrides.catalog() : setupView();},
    start:async(resourceId,environment,nativeSelection)=>{calls.push({kind:'start',resourceId,environment,nativeSelection}); return overrides.start ? overrides.start() : 'owned';},
    poll:async()=>{calls.push({kind:'poll'}); return overrides.poll ? overrides.poll() : setupOperation();},
    cancel:async(operationId)=>{calls.push({kind:'cancel',operationId}); if (overrides.cancel) await overrides.cancel();},
    pickFolder:async()=>{calls.push({kind:'pickFolder'}); return overrides.pickFolder ? overrides.pickFolder() : '/installation';},
    openLink:async(resourceId,index)=>{calls.push({kind:'link',resourceId,index}); if (overrides.openLink) await overrides.openLink();},
    copyCommand:async(resourceId,index)=>{calls.push({kind:'copy',resourceId,index}); if (overrides.copyCommand) await overrides.copyCommand();},
  };
  const session=new OcrSetupSession(client,draft,state=>published.push(state),next=>{draft=next; adopted.push(next);});
  return {session,calls,published,adopted,get draft(){return draft;},edit(next){draft=next; session.observeDraft(next);}};
}

const blankSetupDraft={...validSettingsDraft,environment:environmentDraft(null)};

test('resource catalog and installation guidance stay read-only',async()=>{
  const harness=setupHarness(blankSetupDraft);
  await harness.session.load();
  await harness.session.guidance('link','onnxruntime',0);
  await harness.session.guidance('copy','native-libraries',1);
  assert.deepEqual(harness.calls,[{kind:'catalog'},{kind:'poll'},{kind:'link',resourceId:'onnxruntime',index:0},{kind:'copy',resourceId:'native-libraries',index:1}]);
  assert.equal(harness.session.snapshot.copied,'copy:native-libraries:1');
  assert.equal(harness.session.snapshot.view.environment,null);
  assert.deepEqual(harness.adopted,[]);
  assert.equal(harness.draft,blankSetupDraft);
});

test('Recheck preserves partial hints and unsupported nonblank profiles without relaxing Save',()=>{
  assert.equal(setupEnvironment(environmentDraft(null)),null);
  const partial={profile:'',model_root:'',runtime_path:' /runtime ',library_paths:' /library-a \n\n /library-b '};
  const hints=setupEnvironment(partial);
  assert.equal(hints.profile,'');
  assert.equal(hints.runtime_path,'/runtime');
  assert.deepEqual(hints.native_library_paths,['/library-a','/library-b']);
  assert.equal(readEnvironment(partial).environment,null);
  assert.equal(setupEnvironment({...partial,profile:'unsupported'}).profile,'unsupported');
  assert.equal(setupEnvironment({...partial,profile:SUPPORTED_PROFILES[1].profile}).model,SUPPORTED_PROFILES[1].model);
});

for (const {scenario,resourceId,resolved,expected} of [
  {scenario:'models',resourceId:'rapidocr-models',resolved:{model_root:'/managed-models'},expected:{profile:SUPPORTED_PROFILES[0].profile,model_root:'/managed-models',runtime_path:'',library_paths:''}},
  {scenario:'runtime',resourceId:'onnxruntime',resolved:{runtime_path:'/managed-runtime'},expected:{profile:SUPPORTED_PROFILES[0].profile,model_root:'',runtime_path:'/managed-runtime',library_paths:''}},
]) {
  test(`a ${scenario} download automatically selects its verified path with all manual fields blank`,async()=>{
    const original=structuredClone(blankSetupDraft);
    const result=setupView({resolved});
    const harness=setupHarness(blankSetupDraft,{poll:async()=>setupOperation({result})});
    await harness.session.start(resourceId,harness.draft);
    assert.deepEqual(harness.calls[0],{kind:'start',resourceId,environment:null,nativeSelection:null});
    await harness.session.poll();
    assert.deepEqual(harness.draft.environment,expected);
    assert.equal(harness.adopted.length,1);
    assert.equal(harness.session.snapshot.adopted,true);
    assert.equal(harness.session.snapshot.checkedRevision,harness.session.draftRevision);
    assert.equal(readSettingsDraft(harness.draft).settings,null);
    assert.deepEqual(blankSetupDraft,original);
    await harness.session.poll();
    assert.equal(harness.adopted.length,1);
    assert.equal(harness.calls.filter(call=>call.kind==='poll').length,1);
  });
}

test('independent downloads and native discovery complete one draft without saving or resetting earlier item statuses',async()=>{
  const saved={version:1,package_path:null,...readSettingsDraft(blankSetupDraft).settings};
  const original=structuredClone(saved);
  let result=setupView({resolved:{model_root:'/managed-models'}});
  result.items[0].state='verified';
  const harness=setupHarness(blankSetupDraft,{poll:async()=>setupOperation({result})});
  await harness.session.load();
  await harness.session.start('rapidocr-models',harness.draft);
  await harness.session.poll();
  result=setupView({resolved:{model_root:'/managed-models',native_library_paths:['/opencv/core','/opencv/imgproc','/opencv/imgcodecs']}});
  result.items[0].state='verified';
  result.items[2].state='verified';
  await harness.session.useHomebrew(harness.draft);
  await harness.session.poll();
  result=setupView({resolved:{runtime_path:'/managed-runtime'}});
  result.items[1].state='verified';
  await harness.session.start('onnxruntime',harness.draft);
  await harness.session.poll();
  assert.deepEqual(harness.draft.environment,{
    profile:SUPPORTED_PROFILES[0].profile,model_root:'/managed-models',runtime_path:'/managed-runtime',
    library_paths:'/opencv/core\n/opencv/imgproc\n/opencv/imgcodecs',
  });
  assert.deepEqual(harness.session.snapshot.view.items.map(item=>item.state),['verified','verified','verified']);
  assert.deepEqual(harness.session.snapshot.view.resolved,{
    model_root:'/managed-models',runtime_path:'/managed-runtime',native_library_paths:['/opencv/core','/opencv/imgproc','/opencv/imgcodecs'],
  });
  assert.equal(readSettingsDraft(harness.draft).settings.ocr_environment.runtime_path,'/managed-runtime');
  assert.deepEqual(saved,original);
  assert.equal(harness.adopted.length,3);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start').map(call=>call.nativeSelection),[null,{method:'homebrew'},null]);
});

for (const {scenario,profile} of [
  {scenario:'a supported nondefault profile',profile:SUPPORTED_PROFILES[1].profile},
  {scenario:'an explicit unsupported profile',profile:'unsupported-profile'},
  {scenario:'an explicit whitespace profile',profile:' '},
]) {
  test(`automatic model selection preserves ${scenario}`,async()=>{
    const initial={...blankSetupDraft,environment:{...blankSetupDraft.environment,profile}};
    const harness=setupHarness(initial,{poll:async()=>setupOperation({result:setupView({resolved:{model_root:'/models'}})})});
    await harness.session.start('rapidocr-models',initial);
    await harness.session.poll();
    assert.equal(harness.draft.environment.profile,profile);
    assert.equal(harness.draft.environment.model_root,'/models');
    assert.equal(readSettingsDraft(harness.draft).settings,null);
  });
}

test('a download adopts only its own resource and preserves current explicit dependency paths',async()=>{
  const result=setupView({resolved:{model_root:'/new-models',runtime_path:'/old-runtime',native_library_paths:['/old-native']}});
  const harness=setupHarness(validSettingsDraft,{poll:async()=>setupOperation({result})});
  await harness.session.start('rapidocr-models',harness.draft);
  await harness.session.poll();
  assert.equal(harness.draft.environment.model_root,'/new-models');
  assert.equal(harness.draft.environment.runtime_path,validSettingsDraft.environment.runtime_path);
  assert.equal(harness.draft.environment.library_paths,validSettingsDraft.environment.library_paths);
  assert.equal(harness.adopted.length,1);
});

test('inspection fills verified partial paths but never replaces nonblank explicit paths',async()=>{
  const initial={...blankSetupDraft,environment:{...blankSetupDraft.environment,runtime_path:'/explicit-runtime'}};
  const result=setupView({resolved:{model_root:'/models',runtime_path:'/different-runtime',native_library_paths:['/native']}});
  const harness=setupHarness(initial,{poll:async()=>setupOperation({result})});
  await harness.session.start('inspect',initial);
  await harness.session.poll();
  assert.deepEqual(harness.draft.environment,{profile:SUPPORTED_PROFILES[0].profile,model_root:'/models',runtime_path:'/explicit-runtime',library_paths:'/native'});
  assert.equal(harness.session.snapshot.view.environment,null);
  assert.equal(harness.session.snapshot.checkedRevision,harness.session.draftRevision);
});

test('a successful download selects the default profile even when its verified path is already present',async()=>{
  const initial={...blankSetupDraft,environment:{...blankSetupDraft.environment,model_root:'/models'}};
  const harness=setupHarness(initial,{poll:async()=>setupOperation({result:setupView({resolved:{model_root:'/models'}})})});
  await harness.session.start('rapidocr-models',initial);
  await harness.session.poll();
  assert.equal(harness.draft.environment.profile,SUPPORTED_PROFILES[0].profile);
  assert.equal(harness.draft.environment.model_root,'/models');
  assert.equal(harness.adopted.length,1);
  await harness.session.poll();
  assert.equal(harness.adopted.length,1);
});

test('advanced manual Recheck explicitly leaves native discovery and keeps the supplied library list',async()=>{
  const harness=setupHarness(blankSetupDraft);
  await harness.session.load();
  await harness.session.useHomebrew(harness.draft);
  await harness.session.poll();
  harness.edit(validSettingsDraft);
  await harness.session.useManual(harness.draft);
  assert.equal(harness.session.snapshot.nativeSelection,null);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start').at(-1),{
    kind:'start',resourceId:'inspect',environment:checkedEnvironment,nativeSelection:null,
  });
  await harness.session.poll();
  assert.deepEqual(harness.draft.environment,validSettingsDraft.environment);
  assert.deepEqual(harness.adopted,[]);
});

test('automatic adoption merges unrelated edits made during the operation into the current draft',async()=>{
  const pending=deferredSetup();
  const harness=setupHarness(blankSetupDraft,{poll:()=>pending.promise});
  await harness.session.start('rapidocr-models',harness.draft);
  const polling=harness.session.poll();
  const edited={...harness.draft,backupDirectory:'/draft-backups',logLimit:'500',notifications:{...harness.draft.notifications,show_success:false}};
  harness.edit(edited);
  pending.resolve(setupOperation({result:setupView({resolved:{model_root:'/models'}})}));
  await polling;
  assert.equal(harness.draft.backupDirectory,edited.backupDirectory);
  assert.equal(harness.draft.logLimit,edited.logLimit);
  assert.equal(harness.draft.notifications,edited.notifications);
  assert.equal(harness.draft.environment.model_root,'/models');
  assert.equal(harness.adopted.length,1);
});

for (const {scenario,resourceId} of [
  {scenario:'a model download',resourceId:'rapidocr-models'},
  {scenario:'a runtime download',resourceId:'onnxruntime'},
  {scenario:'an inspection',resourceId:'inspect'},
]) {
  test(`an OCR edit and revert fences ${scenario} and a fresh inspection can recover`,async()=>{
    const pending=deferredSetup();
    let result=pending.promise;
    const harness=setupHarness(blankSetupDraft,{poll:()=>result});
    await harness.session.start(resourceId,harness.draft);
    const polling=harness.session.poll();
    harness.edit({...blankSetupDraft,environment:{...blankSetupDraft.environment,runtime_path:'/new-runtime'}});
    harness.edit(blankSetupDraft);
    pending.resolve(setupOperation({result:setupView({resolved:{model_root:'/models',runtime_path:'/runtime'}})}));
    await polling;
    assert.deepEqual(harness.adopted,[]);
    assert.equal(harness.draft,blankSetupDraft);
    assert.notEqual(harness.session.snapshot.checkedRevision,harness.session.draftRevision);
    result=Promise.resolve(setupOperation({result:setupView({resolved:{model_root:'/models',runtime_path:'/runtime'}})}));
    await harness.session.start('inspect',harness.draft);
    await harness.session.poll();
    assert.equal(harness.draft.environment.model_root,'/models');
    assert.equal(harness.draft.environment.runtime_path,'/runtime');
    assert.equal(harness.adopted.length,1);
  });
}

for (const {scenario,resourceId} of [
  {scenario:'a download',resourceId:'rapidocr-models'},
  {scenario:'a Recheck',resourceId:'inspect'},
]) {
  test(`closing before ${scenario} returns cancels only its late operation, never the successor`,async()=>{
    const start=deferredSetup();
    const old=setupHarness(blankSetupDraft,{start:()=>start.promise});
    const pending=old.session.start(resourceId,old.draft);
    assert.equal(old.session.busy,true);
    old.session.close();
    const publications=old.published.length;
    const successor=setupHarness(blankSetupDraft,{start:async()=> 'successor'});
    await successor.session.start('inspect',successor.draft);
    start.resolve('predecessor');
    await pending;
    assert.deepEqual(old.calls.filter(call=>call.kind==='cancel'),[{kind:'cancel',operationId:'predecessor'}]);
    assert.equal(old.published.length,publications);
    assert.deepEqual(old.adopted,[]);
    assert.equal(successor.session.snapshot.id,'successor');
    assert.deepEqual(successor.calls.filter(call=>call.kind==='cancel'),[]);
  });
}

test('Cancel before Start remains latched, and complete retained models can be reused by explicit Recheck',async()=>{
  const start=deferredSetup();
  const result=setupView({resolved:{model_root:'/retained-models'}});
  const harness=setupHarness(blankSetupDraft,{start:()=>start.promise,poll:async()=>setupOperation({result})});
  const pending=harness.session.start('rapidocr-models',harness.draft);
  await harness.session.cancel();
  assert.deepEqual(harness.calls.filter(call=>call.kind==='cancel'),[]);
  start.resolve('owned');
  await pending;
  assert.deepEqual(harness.calls.filter(call=>call.kind==='cancel'),[{kind:'cancel',operationId:'owned'}]);
  assert.equal(harness.session.busy,true);
  await harness.session.poll();
  assert.equal(harness.session.snapshot.operation.progress.stage,'complete');
  assert.equal(harness.session.busy,false);
  assert.deepEqual(harness.adopted,[]);
  assert.equal(harness.draft,blankSetupDraft);
  assert.equal(harness.session.snapshot.view.resolved.model_root,'/retained-models');
  await harness.session.start('inspect',harness.draft);
  await harness.session.poll();
  assert.equal(harness.draft.environment.model_root,'/retained-models');
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start').map(call=>call.resourceId),['rapidocr-models','inspect']);
  harness.session.close();
  assert.equal(harness.calls.filter(call=>call.kind==='cancel').length,1);
});

test('a foreign poll result is never adopted and Cancel targets only the dialog-owned ID',async()=>{
  const harness=setupHarness(blankSetupDraft,{poll:async()=>setupOperation({id:'foreign',result:setupView({environment:checkedEnvironment})})});
  await harness.session.start('inspect',harness.draft);
  await harness.session.poll();
  assert.equal(harness.session.snapshot.unavailable,true);
  assert.equal(harness.session.snapshot.operation,null);
  assert.deepEqual(harness.adopted,[]);
  await harness.session.cancel();
  assert.deepEqual(harness.calls.filter(call=>call.kind==='cancel'),[{kind:'cancel',operationId:'owned'}]);
});

test('an inspection finishing after close cannot publish or fill a replacement dialog',async()=>{
  const pending=deferredSetup();
  const old=setupHarness(blankSetupDraft,{poll:()=>pending.promise});
  await old.session.start('inspect',old.draft);
  const polling=old.session.poll();
  old.session.close();
  const publications=old.published.length;
  const successor=setupHarness(blankSetupDraft,{start:async()=> 'successor',poll:async()=>setupOperation({id:'successor',result:setupView({environment:checkedEnvironment})})});
  await successor.session.start('inspect',successor.draft);
  await successor.session.poll();
  pending.resolve(setupOperation({result:setupView({environment:{...checkedEnvironment,runtime_path:'/old-runtime'}})}));
  await polling;
  assert.equal(old.published.length,publications);
  assert.deepEqual(old.adopted,[]);
  assert.deepEqual(successor.draft.environment,environmentDraft(checkedEnvironment));
  assert.deepEqual(old.calls.filter(call=>call.kind==='cancel'),[{kind:'cancel',operationId:'owned'}]);
});

for (const {scenario,stage,error,cleanup_error,expectedBusy} of [
  {scenario:'a cancelled inspection',stage:'cancelled',error:{category:'Cancelled',message:'cancelled',context:null},cleanup_error:null,expectedBusy:false},
  {scenario:'a failed inspection',stage:'failed',error:{category:'Storage',message:'unreadable',context:null},cleanup_error:null,expectedBusy:false},
  {scenario:'incomplete inspection cleanup',stage:'complete',error:null,cleanup_error:{category:'Storage',message:'cleanup failed',context:null},expectedBusy:true},
]) {
  test(`${scenario} remains visible without automatically filling paths`,async()=>{
    const harness=setupHarness(blankSetupDraft,{poll:async()=>setupOperation({stage,error,cleanup_error,result:setupView({environment:checkedEnvironment})})});
    await harness.session.start('inspect',harness.draft);
    await harness.session.poll();
    assert.equal(harness.session.snapshot.operation.error,error);
    assert.equal(harness.session.snapshot.operation.cleanup_error,cleanup_error);
    assert.equal(harness.session.busy,expectedBusy);
    assert.deepEqual(harness.adopted,[]);
    assert.equal(harness.draft,blankSetupDraft);
  });
}

test('Homebrew discovery automatically fills native paths and repeats the selected method',async()=>{
  const result=setupView({resolved:{native_library_paths:['/homebrew/core','/homebrew/imgproc','/homebrew/imgcodecs']}});
  const harness=setupHarness(blankSetupDraft,{poll:async()=>setupOperation({result})});
  await harness.session.load();
  await harness.session.useHomebrew(harness.draft);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start'),[{kind:'start',resourceId:'inspect',environment:null,nativeSelection:{method:'homebrew'}}]);
  await harness.session.poll();
  assert.equal(harness.draft.environment.library_paths,'/homebrew/core\n/homebrew/imgproc\n/homebrew/imgcodecs');
  assert.equal(harness.draft.environment.profile,SUPPORTED_PROFILES[0].profile);
  assert.equal(harness.draft.environment.model_root,'');
  assert.equal(harness.draft.environment.runtime_path,'');
  await harness.session.start('inspect',harness.draft);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start').map(call=>call.nativeSelection),[{method:'homebrew'},{method:'homebrew'}]);
});

test('choosing an installation folder fills the native list without editing individual paths',async()=>{
  const result=setupView({resolved:{native_library_paths:['/installation/core','/installation/codec']}});
  const harness=setupHarness(blankSetupDraft,{poll:async()=>setupOperation({result})});
  await harness.session.load();
  await harness.session.pickFolder(harness.draft);
  assert.deepEqual(harness.session.snapshot.nativeSelection,{method:'folders',paths:['/installation']});
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start'),[{kind:'start',resourceId:'inspect',environment:null,nativeSelection:{method:'folders',paths:['/installation']}}]);
  await harness.session.poll();
  assert.equal(harness.draft.environment.library_paths,'/installation/core\n/installation/codec');
  await harness.session.start('inspect',harness.draft);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start').at(-1).nativeSelection,{method:'folders',paths:['/installation']});
});

test('an explicitly added dependency folder is retained through failure and used on each subsequent inspection',async()=>{
  const paths=['/opencv','/extra-dependencies'];
  let operation=setupOperation({stage:'failed',error:{category:'Storage',message:'missing dependency',context:null}});
  const harness=setupHarness(blankSetupDraft,{pickFolder:async()=>paths.shift(),poll:async()=>operation});
  await harness.session.load();
  await harness.session.pickFolder(harness.draft);
  await harness.session.poll();
  assert.deepEqual(harness.adopted,[]);
  assert.deepEqual(harness.session.snapshot.nativeSelection,{method:'folders',paths:['/opencv']});
  await harness.session.pickFolder(harness.draft,true);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start').at(-1).nativeSelection,{method:'folders',paths:['/opencv','/extra-dependencies']});
  operation=setupOperation({result:setupView({resolved:{native_library_paths:['/opencv/core','/extra-dependencies/codec']}})});
  await harness.session.poll();
  assert.equal(harness.draft.environment.library_paths,'/opencv/core\n/extra-dependencies/codec');
  await harness.session.start('inspect',harness.draft);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start').at(-1).nativeSelection,{method:'folders',paths:['/opencv','/extra-dependencies']});
});

test('cancelling the folder picker keeps the existing source choice and never starts inspection',async()=>{
  const harness=setupHarness(blankSetupDraft,{pickFolder:async()=>null});
  await harness.session.load();
  await harness.session.useHomebrew(harness.draft);
  await harness.session.poll();
  const starts=harness.calls.filter(call=>call.kind==='start').length;
  await harness.session.pickFolder(harness.draft);
  assert.equal(harness.session.snapshot.pickerPending,false);
  assert.deepEqual(harness.session.snapshot.nativeSelection,{method:'homebrew'});
  assert.equal(harness.calls.filter(call=>call.kind==='start').length,starts);
  assert.deepEqual(harness.adopted,[]);
});

test('a folder picker result after an OCR edit and revert is ignored',async()=>{
  const picker=deferredSetup();
  const harness=setupHarness(blankSetupDraft,{pickFolder:()=>picker.promise});
  await harness.session.load();
  const choosing=harness.session.pickFolder(harness.draft);
  assert.equal(harness.session.busy,true);
  await harness.session.start('rapidocr-models',harness.draft);
  harness.edit({...blankSetupDraft,environment:{...blankSetupDraft.environment,model_root:'/new-models'}});
  harness.edit(blankSetupDraft);
  picker.resolve('/old-folder');
  await choosing;
  assert.equal(harness.session.snapshot.pickerStale,true);
  assert.equal(harness.session.snapshot.nativeSelection,null);
  assert.equal(harness.session.busy,false);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start'),[]);
  assert.deepEqual(harness.adopted,[]);
});

test('unrelated draft edits while choosing a folder are preserved by its automatic inspection',async()=>{
  const picker=deferredSetup();
  const result=setupView({resolved:{native_library_paths:['/opencv/core']}});
  const harness=setupHarness(blankSetupDraft,{pickFolder:()=>picker.promise,poll:async()=>setupOperation({result})});
  await harness.session.load();
  const choosing=harness.session.pickFolder(harness.draft);
  harness.edit({...harness.draft,backupDirectory:'/new-backups'});
  picker.resolve('/opencv');
  await choosing;
  await harness.session.poll();
  assert.equal(harness.draft.backupDirectory,'/new-backups');
  assert.equal(harness.draft.environment.library_paths,'/opencv/core');
  assert.equal(harness.session.snapshot.pickerStale,false);
});

for (const {scenario,stop} of [
  {scenario:'close',stop:session=>session.close()},
  {scenario:'Cancel setup',stop:session=>session.cancel()},
]) {
  test(`a late folder answer after ${scenario} cannot enter a successor dialog or picker`,async()=>{
    const picker=deferredSetup();
    const old=setupHarness(blankSetupDraft,{pickFolder:()=>picker.promise});
    await old.session.load();
    const choosing=old.session.pickFolder(old.draft);
    await stop(old.session);
    const successor=setupHarness(blankSetupDraft,{pickFolder:async()=>'/successor-folder'});
    await successor.session.load();
    await successor.session.pickFolder(successor.draft);
    picker.resolve('/old-folder');
    await choosing;
    assert.deepEqual(old.calls.filter(call=>call.kind==='start'),[]);
    assert.equal(old.session.snapshot.nativeSelection,null);
    assert.deepEqual(successor.session.snapshot.nativeSelection,{method:'folders',paths:['/successor-folder']});
  });
}

test('cancelling one picker fences its answer even after a new picker starts in the same session',async()=>{
  const first=deferredSetup(), second=deferredSetup();
  let count=0;
  const harness=setupHarness(blankSetupDraft,{pickFolder:()=>++count===1 ? first.promise : second.promise});
  await harness.session.load();
  const old=harness.session.pickFolder(harness.draft);
  await harness.session.cancel();
  const current=harness.session.pickFolder(harness.draft);
  first.resolve('/old-folder');
  await old;
  assert.equal(harness.session.snapshot.pickerPending,true);
  assert.equal(harness.session.snapshot.nativeSelection,null);
  second.resolve('/current-folder');
  await current;
  assert.deepEqual(harness.session.snapshot.nativeSelection,{method:'folders',paths:['/current-folder']});
  assert.equal(harness.calls.filter(call=>call.kind==='start').length,1);
});

test('duplicate folders do not expand approval and eight approved folders refuse a ninth picker',async()=>{
  const answers=['/folder-1','/folder-1',...Array.from({length:7},(_,index)=>`/folder-${index+2}`)];
  const harness=setupHarness(blankSetupDraft,{pickFolder:async()=>answers.shift()});
  await harness.session.load();
  await harness.session.pickFolder(harness.draft);
  await harness.session.poll();
  await harness.session.pickFolder(harness.draft,true);
  await harness.session.poll();
  assert.deepEqual(harness.session.snapshot.nativeSelection,{method:'folders',paths:['/folder-1']});
  for (let index=0;index<7;index++) {
    await harness.session.pickFolder(harness.draft,true);
    await harness.session.poll();
  }
  assert.deepEqual(harness.session.snapshot.nativeSelection.paths,Array.from({length:8},(_,index)=>`/folder-${index+1}`));
  const picks=harness.calls.filter(call=>call.kind==='pickFolder').length;
  await harness.session.pickFolder(harness.draft,true);
  assert.equal(harness.calls.filter(call=>call.kind==='pickFolder').length,picks);
});

test('folder picker failures remain actionable without dropping the source selection',async()=>{
  const failure={category:'Transport',message:'picker unavailable',context:null};
  const harness=setupHarness(blankSetupDraft,{pickFolder:async()=>{throw failure;}});
  await harness.session.load();
  await harness.session.pickFolder(harness.draft);
  assert.equal(harness.session.snapshot.error,failure);
  assert.equal(harness.session.snapshot.pickerPending,false);
  assert.equal(harness.session.busy,false);
  assert.equal(harness.session.snapshot.nativeSelection,null);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start'),[]);
});

test('platform method admission never opens an unsupported native picker or Homebrew search',async()=>{
  const harness=setupHarness(blankSetupDraft,{catalog:async()=>setupView({native_methods:[]})});
  await harness.session.load();
  await harness.session.useHomebrew(harness.draft);
  await harness.session.pickFolder(harness.draft);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start'||call.kind==='pickFolder'),[]);
});

test('polling stays single-flight and transport failures do not release ownership',async()=>{
  const pending=deferredSetup();
  let result=pending.promise;
  const harness=setupHarness(blankSetupDraft,{poll:()=>result});
  await harness.session.start('rapidocr-models',harness.draft);
  const polling=harness.session.poll();
  await harness.session.poll();
  assert.equal(harness.calls.filter(call=>call.kind==='poll').length,1);
  const failure={category:'Transport',message:'temporarily unavailable',context:null};
  pending.reject(failure);
  await polling;
  assert.equal(harness.session.snapshot.pollError,failure);
  assert.equal(harness.session.busy,true);
  result=Promise.resolve(setupOperation());
  await harness.session.poll();
  assert.equal(harness.session.snapshot.pollError,null);
  assert.equal(harness.session.busy,false);
});

test('failed cancellation can be retried and terminal polling admits no work before Cancel returns',async()=>{
  const failure={category:'Transport',message:'cancel unavailable',context:null};
  const cancellation=deferredSetup();
  let failed=true;
  const harness=setupHarness(blankSetupDraft,{cancel:async()=>{if (failed) throw failure; await cancellation.promise;},poll:async()=>setupOperation({result:setupView({environment:checkedEnvironment})})});
  await harness.session.start('rapidocr-models',harness.draft);
  await harness.session.cancel();
  assert.equal(harness.session.snapshot.error,failure);
  assert.equal(harness.session.snapshot.cancelPending,false);
  assert.equal(harness.session.busy,true);
  failed=false;
  const cancelling=harness.session.cancel();
  await harness.session.poll();
  assert.equal(harness.session.snapshot.operation.active,false);
  assert.equal(harness.session.busy,true);
  assert.deepEqual(harness.adopted,[]);
  await harness.session.start('inspect',harness.draft);
  assert.equal(harness.calls.filter(call=>call.kind==='start').length,1);
  cancellation.resolve();
  await cancelling;
  assert.equal(harness.session.snapshot.error,null);
  assert.equal(harness.session.busy,false);
});

test('reopening observes a settling predecessor without adopting it, then explicit Recheck can reuse its files',async()=>{
  let operation=setupOperation({id:'predecessor',active:true,stage:'verifying',result:setupView({environment:checkedEnvironment})});
  const harness=setupHarness(blankSetupDraft,{poll:async()=>operation});
  await harness.session.load();
  assert.equal(harness.session.snapshot.id,'predecessor');
  assert.equal(harness.session.snapshot.owned,false);
  assert.equal(harness.session.busy,true);
  assert.deepEqual(harness.adopted,[]);
  await harness.session.cancel();
  await harness.session.start('inspect',harness.draft);
  assert.deepEqual(harness.calls.filter(call=>call.kind==='cancel'||call.kind==='start'),[]);
  operation=setupOperation({id:'predecessor',result:setupView({environment:checkedEnvironment})});
  await harness.session.poll();
  assert.equal(harness.session.busy,false);
  assert.deepEqual(harness.adopted,[]);
  await harness.session.start('inspect',harness.draft);
  operation=setupOperation({result:setupView({environment:checkedEnvironment})});
  await harness.session.poll();
  assert.equal(harness.session.snapshot.owned,true);
  assert.deepEqual(harness.draft.environment,environmentDraft(checkedEnvironment));
});

test('closing a progress-only observer never cancels a predecessor or its successor',async()=>{
  let operation=setupOperation({id:'predecessor',active:true,stage:'downloading'});
  const harness=setupHarness(blankSetupDraft,{poll:async()=>operation});
  await harness.session.load();
  operation=setupOperation({id:'successor',active:true,stage:'verifying'});
  await harness.session.poll();
  assert.equal(harness.session.snapshot.id,'successor');
  assert.equal(harness.session.snapshot.owned,false);
  harness.session.close();
  assert.deepEqual(harness.calls.filter(call=>call.kind==='cancel'),[]);
});

test('reopened incomplete cleanup stays visible and blocks new setup until restart',async()=>{
  const cleanup_error={category:'Storage',message:'could not remove staging',context:null};
  const harness=setupHarness(blankSetupDraft,{poll:async()=>setupOperation({id:'predecessor',cleanup_error})});
  await harness.session.load();
  assert.equal(harness.session.busy,true);
  assert.equal(harness.session.snapshot.operation.cleanup_error,cleanup_error);
  await harness.session.start('inspect',harness.draft);
  await harness.session.cancel();
  harness.session.close();
  assert.deepEqual(harness.calls.filter(call=>call.kind==='cancel'||call.kind==='start'),[]);
  assert.deepEqual(harness.adopted,[]);
});

test('retrying initial status failure remains read-only and finds the still-settling predecessor',async()=>{
  const failure={category:'Transport',message:'status unavailable',context:null};
  let failed=true;
  const harness=setupHarness(blankSetupDraft,{poll:async()=>{
    if (failed) throw failure;
    return setupOperation({id:'predecessor',active:true,stage:'downloading'});
  }});
  await harness.session.load();
  assert.equal(harness.session.snapshot.pollError,failure);
  assert.deepEqual(harness.session.snapshot.view,setupView());
  failed=false;
  await harness.session.load();
  assert.equal(harness.session.snapshot.pollError,null);
  assert.equal(harness.session.busy,true);
  assert.equal(harness.session.snapshot.owned,false);
  assert.equal(harness.session.snapshot.id,'predecessor');
  assert.deepEqual(harness.calls.filter(call=>call.kind==='start'||call.kind==='cancel'),[]);
});
