import {test} from 'node:test';
import assert from 'node:assert/strict';
import {INITIAL_BOOTSTRAP,PollGate,initialSettings,reconstructed,reconstructionBlock,reduceBootstrap,restoreBlock,restoreOutcome,surface} from './bootstrap.ts';
import {DEFAULT_NOTIFICATIONS} from './state.ts';

const fault={category:'StorageFormat',message:'settings.json is not valid JSON',context:{stage:'settings'}};
function status(overrides={}){
  return {state:'ready',stage:'ready',root:'/data',legacy_root:null,fault:null,settings:null,application_available:true,pending_restore:false,catalog:null,...overrides};
}
function catalog(...ids){
  return {open:ids.map(id=>({workspace_id:id,revision:0,internal_name:id,display_name:id,selection:null,source_error:null,saved_package:null})),closed:[],faults:[]};
}
const recovery=status({state:'recovery',stage:'settings',fault,application_available:false});
const idle={active:false,authoring:false,command:false,anyDirty:false};

test('only a usable Application admits the shell; missing settings enter Setup and invalid ones enter Recovery',()=>{
  assert.equal(surface(null),'loading');
  // The host's transient first read is neither Setup nor Recovery; no defaults or Initialize are offered.
  assert.equal(surface(status({state:'loading',stage:'loading',root:null,application_available:false})),'loading');
  assert.equal(surface(status({state:'setup',stage:'settings',application_available:false})),'setup');
  assert.equal(surface(recovery),'recovery');
  assert.equal(surface(status()),'shell');
  // A later settings fault keeps the running Application: the shell stays and Recovery is shown inside it.
  assert.equal(surface(status({state:'recovery',stage:'settings',fault})),'shell');
});

test('a recovery cause survives form edits, presentation changes and dismissing a refused action',()=>{
  let ui=reduceBootstrap(INITIAL_BOOTSTRAP,{type:'status',status:recovery});
  ui=reduceBootstrap(ui,{type:'presentation',locale:'ja'});
  ui=reduceBootstrap(ui,{type:'setup',draft:{...ui.setup,backupDirectory:'/backups'}});
  ui=reduceBootstrap(ui,{type:'restore',draft:{...ui.restore,archivePath:'/archive'}});
  ui=reduceBootstrap(ui,{type:'snapshotDestination',value:'/tmp'});
  ui=reduceBootstrap(ui,{type:'actionFailed',fault:{category:'Admission',message:'refused',context:null}});
  ui=reduceBootstrap(ui,{type:'dismissActionError'});
  assert.deepEqual(ui.status.fault,fault);
  assert.equal(ui.status.stage,'settings');
  assert.equal(ui.actionError,null);
  assert.equal(ui.presentation,'ja');
  assert.equal(ui.setup.locale,'en');
  assert.equal(ui.restore.archivePath,'/archive');
});

test('a new pending action clears only the previous transport error and never the host status',()=>{
  let ui=reduceBootstrap(INITIAL_BOOTSTRAP,{type:'status',status:recovery});
  ui=reduceBootstrap(ui,{type:'actionFailed',fault:{category:'Admission',message:'refused',context:null}});
  ui=reduceBootstrap(ui,{type:'pending',action:'retrying'});
  assert.equal(ui.actionError,null);
  assert.equal(ui.pending,'retrying');
  assert.equal(ui.status,recovery);
  const settled=reduceBootstrap(reduceBootstrap(ui,{type:'actionFailed',fault}),{type:'pending',action:null});
  assert.deepEqual(settled.actionError,fault);
});

test('a dispatched snapshot keeps its result after any view change',()=>{
  const receipt={path:'/data/backups/app.config.1700000000',generation:'g1',files:3,bytes:512};
  let ui=reduceBootstrap(INITIAL_BOOTSTRAP,{type:'snapshotPending'});
  assert.equal(ui.snapshotPending,true);
  ui=reduceBootstrap(ui,{type:'status',status:status()});
  ui=reduceBootstrap(ui,{type:'snapshotSettled',outcome:{kind:'receipt',receipt}});
  assert.equal(ui.snapshotPending,false);
  assert.deepEqual(ui.snapshotOutcome,{kind:'receipt',receipt});
  const failed=reduceBootstrap(ui,{type:'snapshotSettled',outcome:{kind:'fault',fault}});
  assert.deepEqual(failed.snapshotOutcome,{kind:'fault',fault});
});

test('Initialize writes the saved-language draft and documented defaults, not the temporary presentation',()=>{
  const ui=reduceBootstrap(reduceBootstrap(INITIAL_BOOTSTRAP,{type:'presentation',locale:'ja'}),{type:'setup',draft:{locale:'en',backupDirectory:'  ',startFresh:false}});
  assert.deepEqual(initialSettings(ui.setup),{locale:'en',gui_log_limit:1000,ocr_environment:null,notifications:DEFAULT_NOTIFICATIONS,backup_directory:null});
  assert.equal(initialSettings({locale:'ja',backupDirectory:' /private/backups ',startFresh:true}).backup_directory,'/private/backups');
  assert.equal(initialSettings({locale:'ja',backupDirectory:'',startFresh:true}).locale,'ja');
});

test('only a successful constructing result with fresh session IDs replaces the current session',()=>{
  const current=[{id:'old-1'},{id:'old-2'}];
  assert.equal(reconstructed(status({catalog:catalog('new-1')}),current),true);
  assert.equal(reconstructed(status({catalog:catalog()}),current),true);
  assert.equal(reconstructed(status({catalog:catalog('new-1')}),[]),true);
  // Same IDs: a refused retry or a plain refresh of the same Application.
  assert.equal(reconstructed(status({catalog:catalog('old-1','old-2')}),current),false);
  // A transiently refused catalog, a retained fault or a non-Ready state never rebuilds the session.
  assert.equal(reconstructed(status({catalog:null}),current),false);
  assert.equal(reconstructed(status({catalog:null,stage:'workspace',fault:{category:'WorkspaceBusy',message:'busy',context:null}}),current),false);
  assert.equal(reconstructed(status({state:'recovery',fault,catalog:catalog('new-1')}),current),false);
  assert.equal(reconstructed(status({fault,catalog:catalog('new-1')}),current),false);
});

for (const {scenario,current,draft,admission,block} of [
  {scenario:'a pending restore journal',current:status({pending_restore:true}),draft:{archivePath:'/a',confirm:true,discard:false},admission:idle,block:'pendingRestore'},
  {scenario:'an active operation',current:status(),draft:{archivePath:'/a',confirm:true,discard:false},admission:{...idle,active:true},block:'active'},
  {scenario:'an Edit lease even with discard confirmed',current:status(),draft:{archivePath:'/a',confirm:true,discard:true},admission:{...idle,authoring:true},block:'authoring'},
  {scenario:'a pending command',current:status(),draft:{archivePath:'/a',confirm:true,discard:false},admission:{...idle,command:true},block:'command'},
  {scenario:'a blank archive path',current:status(),draft:{archivePath:'  ',confirm:true,discard:false},admission:idle,block:'archivePath'},
  {scenario:'a missing scope confirmation',current:status(),draft:{archivePath:'/a',confirm:false,discard:false},admission:idle,block:'confirm'},
  {scenario:'dirty drafts without discard confirmation',current:status(),draft:{archivePath:'/a',confirm:true,discard:false},admission:{...idle,anyDirty:true},block:'discard'},
  {scenario:'a clean retained Application without session disposal',current:status(),draft:{archivePath:'/a',confirm:true,discard:false},admission:idle,block:'discard'},
  {scenario:'a clean retained Application with session disposal',current:status(),draft:{archivePath:'/a',confirm:true,discard:true},admission:idle,block:null},
  {scenario:'dirty drafts with discard confirmation',current:status(),draft:{archivePath:'/a',confirm:true,discard:true},admission:{...idle,anyDirty:true},block:null},
  {scenario:'a complete idle request',current:recovery,draft:{archivePath:'/a',confirm:true,discard:false},admission:idle,block:null},
]) {
  test(`restore admission: ${scenario}`,()=>{
    assert.equal(restoreBlock(current,draft,admission),block);
  });
}

test('retry and recovery require settled operations and explicit retained-session disposal',()=>{
  assert.equal(reconstructionBlock(status(),{...idle,active:true},true),'active');
  assert.equal(reconstructionBlock(status(),{...idle,command:true},true),'command');
  assert.equal(reconstructionBlock(recovery,{...idle,authoring:true},true),'authoring');
  assert.equal(reconstructionBlock(recovery,{...idle,anyDirty:true},false),'discard');
  assert.equal(reconstructionBlock(recovery,{...idle,anyDirty:true},true),null);
  assert.equal(reconstructionBlock(status(),idle,false),'discard');
  assert.equal(reconstructionBlock(status(),idle,true),null);
  assert.equal(reconstructionBlock(recovery,idle,false),null);
});

test('settled reconstruction consumes every action consent without erasing the snapshot outcome or recovery cause',()=>{
  const receipt={path:'/backups/app.config.42',generation:'g1',files:1,bytes:20};
  let ui=reduceBootstrap(INITIAL_BOOTSTRAP,{type:'snapshotSettled',outcome:{kind:'receipt',receipt}});
  ui=reduceBootstrap(ui,{type:'restore',draft:{archivePath:'/a',confirm:true,discard:true,recoverConfirm:true,recoverDiscard:true,retryDiscard:true}});
  ui=reduceBootstrap(ui,{type:'pending',action:'restoring'});
  ui=reduceBootstrap(ui,{type:'status',status:status({state:'recovery',fault,pending_restore:true,application_available:false})});
  ui=reduceBootstrap(ui,{type:'pending',action:null});
  assert.deepEqual(ui.restore,{archivePath:'/a',confirm:false,discard:false,recoverConfirm:false,recoverDiscard:false,retryDiscard:false});
  assert.equal(ui.receiptGeneration,'g1');
  ui=reduceBootstrap(ui,{type:'snapshotSettled',outcome:{kind:'fault',fault}});
  assert.equal(ui.receiptGeneration,'g1');
  ui=reduceBootstrap(ui,{type:'snapshotSettled',outcome:{kind:'receipt',receipt}});
  ui=reduceBootstrap(ui,{type:'receiptConsumed'});
  assert.equal(ui.receiptGeneration,null);
  assert.deepEqual(ui.snapshotOutcome,{kind:'receipt',receipt});
  assert.deepEqual(ui.status.fault,fault);
  ui=reduceBootstrap(ui,{type:'snapshotSettled',outcome:{kind:'receipt',receipt}});
  assert.equal(ui.receiptGeneration,'g1');
});

// Fault shapes as the host emits them: restore::finish annotates its own cleanup fault, an automatic rollback nests the
// cleanup or rollback fault it hit, and the bootstrap annotates a failed reconstruction after a verified transaction.
const installFault={category:'Storage',message:'install staged configuration: permission denied',context:null};
const reconstructionFault={category:'Application',message:'log sink unavailable',context:null};
const failed=(context,cause=installFault)=>({...cause,context});
function finishFault(rollback,pending){
  return failed({configuration_installed:!rollback,rolled_back:rollback,cleanup_incomplete:true,pending_restore:pending,
    retained_staging:'/data/.restore-journal',completion_marker:'/data/.restore-completion'},{category:'Storage',message:'remove completed restore marker: permission denied'});
}
function recovering(fault,pending,stage='restore'){
  return status({state:'recovery',stage,fault,application_available:false,pending_restore:pending});
}
const rollbackFailure={rollback_failure:failed(null,{category:'Storage',message:'retain displaced configuration: disk full'}),pending_restore:true};

for (const {scenario,current,outcome} of [
  {scenario:'an unrelated settings fault',current:recovery,outcome:null},
  {scenario:'a refused installation that only retained its unpublished staging',current:recovering(failed({staging_cleanup:{path:'/data/.restore-stage-1',fault:failed(null)}}),false),outcome:null},
  {scenario:'an interrupted restore found at startup',current:recovering({category:'RestorePending',message:'An interrupted restore must be completed or rolled back',context:null},true),outcome:'unfinished'},
  {scenario:'a recovery that failed before cleanup',current:recovering(failed({pending_restore:true}),true),outcome:'unfinished'},
  {scenario:'a recovery failure reported unresolved after the host no longer reports it',current:recovering(failed({pending_restore:true}),false),outcome:'unfinished'},
  {scenario:'a failed installation whose automatic rollback also failed',current:recovering(failed(rollbackFailure),true),outcome:'rollbackFailed'},
  {scenario:'a double failure even without a host pending flag',current:recovering(failed(rollbackFailure),false),outcome:'rollbackFailed'},
  {scenario:'a failed installation rolled back automatically',current:recovering(failed({rolled_back:true}),false),outcome:'rolledBackAutomatically'},
  {scenario:'installation cleanup incomplete while the restore is pending',current:recovering(finishFault(false,true),true),outcome:'installedCleanupPending'},
  {scenario:'installation cleanup error after the transaction is gone',current:recovering(finishFault(false,false),false),outcome:'installedCleanupUnconfirmed'},
  {scenario:'recovery rollback cleanup incomplete while the restore is pending',current:recovering(finishFault(true,true),true),outcome:'rolledBackCleanupPending'},
  {scenario:'recovery rollback cleanup error after the transaction is gone',current:recovering(finishFault(true,false),false),outcome:'rolledBackCleanupUnconfirmed'},
  {scenario:'an automatic rollback whose nested cleanup is incomplete',current:recovering(failed({rolled_back:true,rollback_cleanup:finishFault(true,true)}),true),outcome:'rolledBackCleanupPending'},
  {scenario:'an automatic rollback whose nested cleanup erred after the transaction is gone',current:recovering(failed({rolled_back:true,rollback_cleanup:finishFault(true,false)}),false),outcome:'rolledBackCleanupUnconfirmed'},
  {scenario:'the host pending flag outranks a cleanup context that reported none',current:recovering(finishFault(false,false),true),outcome:'installedCleanupPending'},
  {scenario:'the host pending flag outranks a cleanup context that reported one',current:recovering(finishFault(false,true),false),outcome:'installedCleanupUnconfirmed'},
  {scenario:'an installed restore whose reconstruction failed',current:recovering(failed({configuration_installed:true},reconstructionFault),false,'application'),outcome:'installedNotReconstructed'},
  {scenario:'a completed recovery whose reconstruction failed',current:recovering(failed({configuration_installed:true,rolled_back:false},reconstructionFault),false,'application'),outcome:'installedNotReconstructed'},
  {scenario:'a rolled-back recovery whose reconstruction failed',current:recovering(failed({configuration_installed:true,rolled_back:true},reconstructionFault),false,'application'),outcome:'rolledBackNotReconstructed'},
  {scenario:'a pending restore outranks a reconstruction annotation',current:recovering(failed({configuration_installed:true},reconstructionFault),true,'application'),outcome:'unfinished'},
  {scenario:'a non-boolean installation flag',current:recovering(failed({configuration_installed:'true'},reconstructionFault),false,'application'),outcome:null},
  {scenario:'an array context',current:recovering(failed(['configuration_installed'],reconstructionFault),false,'application'),outcome:null},
  {scenario:'a cleanup flag without a verified direction',current:recovering(failed({cleanup_incomplete:true}),false),outcome:'unfinished'},
  {scenario:'a nested cleanup fault with a malformed context',current:recovering(failed({rolled_back:true,rollback_cleanup:failed(['cleanup_incomplete'])}),false),outcome:'unfinished'},
]) {
  test(`restore outcome: ${scenario}`,()=>{
    assert.equal(restoreOutcome(current),outcome);
  });
}

test('a constructing action waits for the poll in flight, refuses new polls until it settles, and drops nothing',async()=>{
  const gate=new PollGate();
  const events=[];
  // A poll is already in flight when the action starts; its response has not been ingested yet.
  assert.equal(gate.claim(),true);
  let respond;
  const poll=new Promise(resolve=>{respond=resolve;}).then(()=>{events.push('ingested');gate.release();});
  const action=gate.hold().then(()=>{events.push('construct');});
  // From the hold on, no new poll is dispatched and the constructing call has not started.
  assert.equal(gate.claim(),false);
  await Promise.resolve();
  assert.deepEqual(events,[]);
  respond();
  await Promise.all([poll,action]);
  assert.deepEqual(events,['ingested','construct']);
  // Polling stays refused while the action's status is being applied and resumes once it settles.
  assert.equal(gate.claim(),false);
  gate.resume();
  assert.equal(gate.claim(),true);
  gate.release();
  // With nothing in flight the hold resolves immediately; a refused action resumes the retained session unchanged.
  let constructed=false;
  const refusal=gate.hold().then(()=>{constructed=true;});
  assert.equal(gate.claim(),false);
  await refusal;
  assert.equal(constructed,true);
  gate.resume();
  assert.equal(gate.claim(),true);
  gate.release();
});
