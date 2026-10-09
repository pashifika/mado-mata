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

function setupOperation({id='owned',active=false,stage='complete',result=null,error=null,cleanup_error=null}={}) {
  return {id,active,progress:{stage,resource_id:'models',bytes:100,total:100},result,error,cleanup_error};
}

function setupHarness(draft=validSettingsDraft, overrides={}) {
  const calls=[], published=[];
  const client={
    catalog:async()=>{calls.push({kind:'catalog'}); return overrides.catalog ? overrides.catalog() : {items:[],environment:null};},
    start:async(resourceId,environment)=>{calls.push({kind:'start',resourceId,environment}); return overrides.start ? overrides.start() : 'owned';},
    poll:async()=>{calls.push({kind:'poll'}); return overrides.poll ? overrides.poll() : setupOperation();},
    cancel:async(operationId)=>{calls.push({kind:'cancel',operationId}); if (overrides.cancel) await overrides.cancel();},
    openLink:async(resourceId,index)=>{calls.push({kind:'link',resourceId,index}); if (overrides.openLink) await overrides.openLink();},
    copyCommand:async(resourceId,index)=>{calls.push({kind:'copy',resourceId,index}); if (overrides.copyCommand) await overrides.copyCommand();},
  };
  const session=new OcrSetupSession(client,draft,state=>published.push(state));
  return {session,calls,published};
}

test('resource catalog and manual guidance never start a setup job or apply settings',async()=>{
  const {session,calls}=setupHarness();
  await session.load();
  await session.guidance('link','runtime',0);
  await session.guidance('copy','libraries',1);
  assert.deepEqual(calls,[{kind:'catalog'},{kind:'poll'},{kind:'link',resourceId:'runtime',index:0},{kind:'copy',resourceId:'libraries',index:1}]);
  assert.equal(session.snapshot.copied,'copy:libraries:1');
  assert.equal(session.snapshot.view.environment,null);
  assert.equal(session.apply(validSettingsDraft),null);
});

test('Recheck preserves partial dependency hints and unsupported nonblank profiles without relaxing Save',()=>{
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

for (const {scenario,resourceId} of [
  {scenario:'a download',resourceId:'models'},
  {scenario:'a Recheck',resourceId:'inspect'},
]) {
  test(`closing before ${scenario} returns cancels only its late operation, never the successor`,async()=>{
    const start=deferredSetup();
    const old=setupHarness(validSettingsDraft,{start:()=>start.promise});
    const pending=old.session.start(resourceId,validSettingsDraft);
    assert.equal(old.session.busy,true);
    old.session.close();
    const publications=old.published.length;
    const successor=setupHarness(validSettingsDraft,{start:async()=> 'successor'});
    await successor.session.start('inspect',validSettingsDraft);
    start.resolve('predecessor');
    await pending;
    assert.deepEqual(old.calls.filter(call=>call.kind==='cancel'),[{kind:'cancel',operationId:'predecessor'}]);
    assert.equal(old.published.length,publications);
    assert.equal(old.session.apply(validSettingsDraft),null);
    assert.equal(successor.session.snapshot.id,'successor');
    assert.deepEqual(successor.calls.filter(call=>call.kind==='cancel'),[]);
  });
}

test('Cancel before Start returns remains latched and publication still reports retained verified files',async()=>{
  const start=deferredSetup();
  const original=structuredClone(validSettingsDraft);
  const {session,calls}=setupHarness(validSettingsDraft,{start:()=>start.promise});
  const pending=session.start('models',validSettingsDraft);
  await session.cancel();
  assert.deepEqual(calls.filter(call=>call.kind==='cancel'),[]);
  start.resolve('owned');
  await pending;
  assert.deepEqual(calls.filter(call=>call.kind==='cancel'),[{kind:'cancel',operationId:'owned'}]);
  assert.equal(session.busy,true);
  await session.poll();
  assert.equal(session.snapshot.operation.progress.stage,'complete');
  assert.equal(session.busy,false);
  assert.equal(session.currentEnvironment(validSettingsDraft),null);
  assert.deepEqual(validSettingsDraft,original);
  session.close();
  assert.equal(calls.filter(call=>call.kind==='cancel').length,1);
});

test('a foreign poll result is never adopted and Cancel still targets only the dialog-owned ID',async()=>{
  const {session,calls}=setupHarness(validSettingsDraft,{poll:async()=>setupOperation({id:'foreign',result:{items:[],environment:checkedEnvironment}})});
  await session.start('inspect',validSettingsDraft);
  await session.poll();
  assert.equal(session.snapshot.unavailable,true);
  assert.equal(session.snapshot.operation,null);
  assert.equal(session.currentEnvironment(validSettingsDraft),null);
  await session.cancel();
  assert.deepEqual(calls.filter(call=>call.kind==='cancel'),[{kind:'cancel',operationId:'owned'}]);
});

test('an inspection finishing after close cannot publish into a replacement dialog',async()=>{
  const result=deferredSetup();
  const old=setupHarness(validSettingsDraft,{poll:()=>result.promise});
  await old.session.start('inspect',validSettingsDraft);
  const pending=old.session.poll();
  old.session.close();
  const publications=old.published.length;
  const successor=setupHarness(validSettingsDraft,{start:async()=> 'successor',poll:async()=>setupOperation({id:'successor',result:{items:[],environment:checkedEnvironment}})});
  await successor.session.start('inspect',validSettingsDraft);
  await successor.session.poll();
  result.resolve(setupOperation({result:{items:[],environment:{...checkedEnvironment,runtime_path:'/old-runtime'}}}));
  await pending;
  assert.equal(old.published.length,publications);
  assert.equal(old.session.currentEnvironment(validSettingsDraft),null);
  assert.deepEqual(successor.session.currentEnvironment(validSettingsDraft),checkedEnvironment);
  assert.deepEqual(old.calls.filter(call=>call.kind==='cancel'),[{kind:'cancel',operationId:'owned'}]);
});

for (const {scenario,edit} of [
  {scenario:'an OCR path edit',edit:draft=>({...draft,environment:{...draft.environment,runtime_path:'/edited-runtime'}})},
  {scenario:'an unrelated settings edit',edit:draft=>({...draft,backupDirectory:'/edited-backups'})},
]) {
  test(`${scenario} invalidates an in-flight Recheck even when the draft is later reverted`,async()=>{
    const result=deferredSetup();
    const {session}=setupHarness(validSettingsDraft,{poll:()=>result.promise});
    await session.start('inspect',validSettingsDraft);
    const pending=session.poll();
    const edited=edit(validSettingsDraft);
    session.observeDraft(edited);
    result.resolve(setupOperation({result:{items:[],environment:checkedEnvironment}}));
    await pending;
    assert.equal(session.currentEnvironment(edited),null);
    assert.equal(session.apply(edited),null);
    assert.equal(session.currentEnvironment(validSettingsDraft),null);
    assert.equal(session.apply(validSettingsDraft),null);
  });
}

test('Use applies only a current complete OCR proposal, keeps other edits and leaves saved settings unchanged',async()=>{
  const saved={version:1,package_path:null,...readSettingsDraft(validSettingsDraft).settings};
  const original=structuredClone(saved);
  const draft={...validSettingsDraft,backupDirectory:'/draft-backups',logLimit:'500'};
  const environment={...checkedEnvironment,model_root:'/managed-models',runtime_path:'/reviewed-runtime'};
  const {session,calls}=setupHarness(draft,{poll:async()=>setupOperation({result:{items:[],environment}})});
  await session.start('inspect',draft);
  assert.deepEqual(calls[0],{kind:'start',resourceId:'inspect',environment:checkedEnvironment});
  await session.poll();
  const applied=session.apply(draft);
  assert.deepEqual(applied.environment,environmentDraft(environment));
  assert.equal(applied.backupDirectory,draft.backupDirectory);
  assert.equal(applied.logLimit,draft.logLimit);
  assert.equal(applied.notifications,draft.notifications);
  assert.deepEqual(readSettingsDraft(applied).settings.ocr_environment,environment);
  assert.deepEqual(saved,original);
  assert.equal(session.snapshot.applied,true);
  assert.equal(session.currentEnvironment(applied),null);
  session.close();
  assert.deepEqual(calls.filter(call=>call.kind==='cancel'),[]);
});

test('an edit immediately before Use refuses the proposal and a fresh Recheck can recover',async()=>{
  const {session}=setupHarness(validSettingsDraft,{poll:async()=>setupOperation({result:{items:[],environment:checkedEnvironment}})});
  await session.start('inspect',validSettingsDraft);
  await session.poll();
  const edited={...validSettingsDraft,environment:{...validSettingsDraft.environment,runtime_path:'/new-runtime'}};
  assert.equal(session.apply(edited),null);
  await session.start('inspect',edited);
  await session.poll();
  assert.deepEqual(session.currentEnvironment(edited),checkedEnvironment);
});

for (const {scenario,stage,error,cleanup_error,expectedBusy} of [
  {scenario:'a cancelled Recheck',stage:'cancelled',error:{category:'Cancelled',message:'cancelled',context:null},cleanup_error:null,expectedBusy:false},
  {scenario:'a failed Recheck',stage:'failed',error:{category:'Storage',message:'unreadable',context:null},cleanup_error:null,expectedBusy:false},
  {scenario:'incomplete Recheck cleanup',stage:'complete',error:null,cleanup_error:{category:'Storage',message:'cleanup failed',context:null},expectedBusy:true},
]) {
  test(`${scenario} remains visible and cannot supply a settings proposal`,async()=>{
    const {session}=setupHarness(validSettingsDraft,{poll:async()=>setupOperation({stage,error,cleanup_error,result:{items:[],environment:checkedEnvironment}})});
    await session.start('inspect',validSettingsDraft);
    await session.poll();
    assert.equal(session.snapshot.operation.error,error);
    assert.equal(session.snapshot.operation.cleanup_error,cleanup_error);
    assert.equal(session.busy,expectedBusy);
    assert.equal(session.currentEnvironment(validSettingsDraft),null);
    assert.equal(session.apply(validSettingsDraft),null);
  });
}

test('polling is single-flight and transport failures do not release setup ownership',async()=>{
  const result=deferredSetup();
  const {session,calls}=setupHarness(validSettingsDraft,{poll:()=>result.promise});
  await session.start('models',validSettingsDraft);
  const pending=session.poll();
  await session.poll();
  assert.equal(calls.filter(call=>call.kind==='poll').length,1);
  const failure={category:'Transport',message:'temporarily unavailable',context:null};
  result.reject(failure);
  await pending;
  assert.equal(session.snapshot.pollError,failure);
  assert.equal(session.busy,true);
  result.promise=Promise.resolve(setupOperation());
  await session.poll();
  assert.equal(session.snapshot.pollError,null);
  assert.equal(session.busy,false);
});

test('a failed cancellation is actionable and a terminal poll does not admit new work before Cancel returns',async()=>{
  const failure={category:'Transport',message:'cancel unavailable',context:null};
  const cancellation=deferredSetup();
  let failed=true;
  const {session,calls}=setupHarness(validSettingsDraft,{cancel:async()=>{if (failed) throw failure; await cancellation.promise;}});
  await session.start('models',validSettingsDraft);
  await session.cancel();
  assert.equal(session.snapshot.error,failure);
  assert.equal(session.snapshot.cancelPending,false);
  assert.equal(session.busy,true);
  failed=false;
  const pending=session.cancel();
  await session.poll();
  assert.equal(session.snapshot.operation.active,false);
  assert.equal(session.busy,true);
  await session.start('inspect',validSettingsDraft);
  assert.equal(calls.filter(call=>call.kind==='start').length,1);
  cancellation.resolve();
  await pending;
  assert.equal(session.snapshot.error,null);
  assert.equal(session.busy,false);
});

test('reopening observes a settling predecessor without cancelling it or using its old proposal',async()=>{
  let operation=setupOperation({id:'predecessor',active:true,stage:'verifying',result:{items:[],environment:checkedEnvironment}});
  const {session,calls}=setupHarness(validSettingsDraft,{poll:async()=>operation});
  await session.load();
  assert.equal(session.snapshot.id,'predecessor');
  assert.equal(session.snapshot.owned,false);
  assert.equal(session.busy,true);
  assert.equal(session.currentEnvironment(validSettingsDraft),null);
  await session.cancel();
  await session.start('inspect',validSettingsDraft);
  assert.deepEqual(calls.filter(call=>call.kind==='cancel'||call.kind==='start'),[]);
  operation=setupOperation({id:'predecessor',result:{items:[],environment:checkedEnvironment}});
  await session.poll();
  assert.equal(session.busy,false);
  assert.equal(session.currentEnvironment(validSettingsDraft),null);
  assert.equal(session.apply(validSettingsDraft),null);
  await session.start('inspect',validSettingsDraft);
  operation=setupOperation({result:{items:[],environment:checkedEnvironment}});
  await session.poll();
  assert.equal(session.snapshot.owned,true);
  assert.deepEqual(session.currentEnvironment(validSettingsDraft),checkedEnvironment);
});

test('closing a progress-only observer never cancels a predecessor or its successor',async()=>{
  let operation=setupOperation({id:'predecessor',active:true,stage:'downloading'});
  const {session,calls}=setupHarness(validSettingsDraft,{poll:async()=>operation});
  await session.load();
  operation=setupOperation({id:'successor',active:true,stage:'verifying'});
  await session.poll();
  assert.equal(session.snapshot.id,'successor');
  assert.equal(session.snapshot.owned,false);
  session.close();
  assert.deepEqual(calls.filter(call=>call.kind==='cancel'),[]);
});

test('reopened incomplete cleanup stays visible and blocks new setup until restart',async()=>{
  const cleanup_error={category:'Storage',message:'could not remove staging',context:null};
  const {session,calls}=setupHarness(validSettingsDraft,{poll:async()=>setupOperation({id:'predecessor',cleanup_error})});
  await session.load();
  assert.equal(session.busy,true);
  assert.equal(session.snapshot.operation.cleanup_error,cleanup_error);
  await session.start('inspect',validSettingsDraft);
  await session.cancel();
  session.close();
  assert.deepEqual(calls.filter(call=>call.kind==='cancel'||call.kind==='start'),[]);
  assert.equal(session.currentEnvironment(validSettingsDraft),null);
});

test('retrying initial status failure remains read-only and discovers the still-settling predecessor',async()=>{
  const failure={category:'Transport',message:'status unavailable',context:null};
  let failed=true;
  const {session,calls}=setupHarness(validSettingsDraft,{poll:async()=>{
    if (failed) throw failure;
    return setupOperation({id:'predecessor',active:true,stage:'downloading'});
  }});
  await session.load();
  assert.equal(session.snapshot.pollError,failure);
  assert.deepEqual(session.snapshot.view,{items:[],environment:null});
  failed=false;
  await session.load();
  assert.equal(session.snapshot.pollError,null);
  assert.equal(session.busy,true);
  assert.equal(session.snapshot.owned,false);
  assert.equal(session.snapshot.id,'predecessor');
  assert.deepEqual(calls.filter(call=>call.kind==='start'||call.kind==='cancel'),[]);
});
