import {test} from 'node:test';
import assert from 'node:assert/strict';
import {AuthoringController} from './authoring-controller.ts';
import {applySave, beginComposition, beginPending, endComposition, openSession, replaceFile, saveTicket, selectFile, undoFile} from './authoring.ts';
import {applyTrialRecovery, currentTrialSource, functionKey, trialArguments, trialContext, trialDiagnosticLocation, trialDiagnostics, trialRunIntent, trialRunRequest, trialSourceTicket} from './edit-trial.ts';
import {approveNative, chooseLane, commandValues, deriveBound, editDraft, editNativeReview, newDraft, selectProfile} from './workspace.ts';
import {nativeOutcome} from './state.ts';
import {authoringActivity} from './status.ts';
import {editRecovery, recoveryTicket} from './recovery.ts';
import {editTarget, readTarget, targetReadFailed} from './target.ts';

const owner={workspace:{workspace_id:'trial-owner',revision:3},token:'trial-lease'};
const source='function battle(turn: number) { return turn; }\n';
const candidate={module:'battle.ts',function:'battle',signature:'battle(turn: number): number',line:1,column:1,required_arguments:1};
const packageInfo={package_id:'example.trial',inventory_identity:'inventory-1',schema_identity:'schema-1',runtime:'typescript',
  schema:{type:'object',properties:{speed:{type:'integer',default:1}}},profiles:{fast:{options:{speed:2}}},effective_defaults:{speed:1},target:null,target_identity:null};
const profile={version:1,id:'profile-one',name:'Reviewed',package_id:'example.trial',schema_identity:'schema-1',values:{speed:3}};
function view(revision='saved-1',lease=owner){
  return {owner:lease,revision,package_path:'/fixture/package',package_id:'example.trial',files:[
    {path:'battle.ts',kind:'source',text:source,bytes:Buffer.byteLength(source)},
    {path:'other.ts',kind:'source',text:'export const value = 1;\n',bytes:24},
  ]};
}
function controller(){
  const result=new AuthoringController(()=>{});
  result.update(()=>openSession(view()));
  return result;
}
function context(revision='saved-1',extra={}){
  return {owner,revision,package:packageInfo,profiles:[profile],profiles_error:null,target:null,target_error:null,functions:[candidate],recovery:null,...extra};
}

test('the packaged default preset is usable immediately and wins over schema defaults and saved profiles',()=>{
  const packaged={...packageInfo,schema:{type:'object',properties:{
    speed:{type:'integer',default:1},settings:{type:'object',properties:{retries:{type:'integer'}},default:{retries:1}},
    schemaOnly:{type:'integer',default:9},
  }},profiles:{...packageInfo.profiles,default:{options:{speed:2,settings:{retries:4}}}}};
  const initial=trialContext(context('saved-1',{package:packaged}),openSession(view()),'fixture',null);
  const facts=deriveBound(initial.bound,null);
  assert.equal(initial.profileChoice,'preset:default');
  assert.equal(initial.bound.selectedId,null);
  assert.equal(initial.bound.preset,'default');
  assert.equal(initial.bound.touched,false);
  assert.equal(initial.bound.lane,'controlled');
  assert.equal(facts.bound,true);
  assert.equal(facts.numericErrors,false);
  assert.equal(facts.startBlock,null);
  assert.deepEqual(commandValues({bound:initial.bound},facts),{speed:2,settings:{retries:4}});
  assert.notEqual(initial.bound.draft,packaged.profiles.default.options);
  assert.notEqual(initial.bound.draft.settings,packaged.profiles.default.options.settings);
  initial.bound.draft.settings.retries=8;
  assert.equal(packaged.profiles.default.options.settings.retries,4);
  assert.deepEqual(packaged.schema.properties.settings.default,{retries:1});
  assert.deepEqual(profile.values,{speed:3});
});

for(const {scenario,profiles} of [
  {scenario:'no packaged default preset',profiles:packageInfo.profiles},
  {scenario:'an inherited rather than package-owned default preset',profiles:Object.assign(Object.create({default:{options:{speed:8}}}),packageInfo.profiles)},
]) test(`schema defaults are usable immediately with ${scenario}`,()=>{
  const initial=trialContext(context('saved-1',{package:{...packageInfo,profiles}}),openSession(view()),'fixture',null);
  const facts=deriveBound(initial.bound,null);
  assert.equal(initial.profileChoice,'values');
  assert.equal(initial.bound.selectedId,null);
  assert.equal(initial.bound.preset,'');
  assert.equal(initial.bound.touched,false);
  assert.equal(facts.bound,true);
  assert.equal(facts.numericErrors,false);
  assert.equal(facts.startBlock,null);
  assert.deepEqual(commandValues({bound:initial.bound},facts),{speed:1});
});

test('an invalid packaged default remains invalid instead of falling back to schema or saved profile values',()=>{
  const packaged={...packageInfo,profiles:{...packageInfo.profiles,default:{options:{speed:'not-a-number'}}}};
  const initial=trialContext(context('saved-1',{package:packaged}),openSession(view()),'fixture',null);
  const facts=deriveBound(initial.bound,null);
  assert.equal(initial.profileChoice,'preset:default');
  assert.deepEqual(facts.parsed.values,{speed:'not-a-number'});
  assert.equal(facts.numericErrors,true);
  assert.ok(facts.parsed.errors['$.speed']);
  assert.throws(()=>commandValues({bound:initial.bound},facts),error=>error.presentation.key==='numericFields');
});

test('schema defaults do not invent a required value when no default is declared',()=>{
  const packaged={...packageInfo,schema:{type:'object',required:['speed'],properties:{speed:{type:'integer'}}},effective_defaults:{}};
  const initial=trialContext(context('saved-1',{package:packaged}),openSession(view()),'fixture',null);
  assert.equal(initial.profileChoice,'values');
  assert.equal(initial.bound.selectedId,null);
  assert.deepEqual(initial.bound.draft,{});
  assert.deepEqual(deriveBound(initial.bound,null).parsed.values,{});
  assert.equal(Object.hasOwn(initial.bound.draft,'speed'),false);
  assert.deepEqual(initial.context.package.schema.required,['speed']);
});

test('a declared target chooses Native without inventing intent, consent or a supported-host fallback',()=>{
  const packaged={...packageInfo,target:{id:'game',window_title:'Exact title'},target_identity:'target-1'};
  const initial=trialContext(context('saved-1',{package:packaged}),openSession(view()),'fixture',null);
  const facts=deriveBound(initial.bound,null,'en',null);
  assert.equal(initial.bound.lane,'native');
  assert.equal(initial.profileChoice,'values');
  assert.deepEqual(initial.bound.native,{operation:'',postcondition:'',workflowSeconds:null,capture:false,input:false,launch:false,recovery:false,key:null});
  assert.equal(facts.native.block,'nativeUnavailable');
  assert.equal(facts.native.intent,null);
  assert.ok(facts.startBlock);
  assert.equal(initial.bound.target.declaration,packaged.target);
});

test('same-owner reload retains explicitly edited invalid values and the chosen lane instead of new defaults',()=>{
  const session=openSession(view());
  const initial=trialContext(context(),session,'fixture',null);
  const configured={...initial,profileChoice:'values',bound:chooseLane(editDraft(initial.bound,{speed:'unfinished'}),'controlled')};
  const packaged={...packageInfo,profiles:{default:{options:{speed:7}}},target:{id:'game',window_title:'Exact title'},target_identity:'target-1'};
  const next=trialContext(context('saved-2',{package:packaged}),{...session,revision:'saved-2'},'fixture',configured);
  const facts=deriveBound(next.bound,null);
  assert.equal(next.profileChoice,'values');
  assert.equal(next.bound.draft,configured.bound.draft);
  assert.equal(next.bound.draftRevision,configured.bound.draftRevision);
  assert.equal(next.bound.lane,'controlled');
  assert.equal(facts.numericErrors,true);
  assert.deepEqual(facts.parsed.values,{speed:'unfinished'});
  assert.throws(()=>commandValues({bound:next.bound},facts),error=>error.presentation.key==='numericFields');
});

test('another Edit owner receives independent package defaults and no previous Native choices',()=>{
  const drafts=controller();
  const initial=trialContext(context(),drafts.current(),'fixture',null);
  const configured={...initial,unit:'function',selected:functionKey(candidate),arguments:'[8]',profileChoice:'profile:profile-one',
    bound:{...selectProfile(initial.bound,profile.id),lane:'replay',descriptorPath:'/fixture/replay',
      native:{operation:'Press once',postcondition:'Dialog is shown',workflowSeconds:'60',capture:true,input:true,launch:true,recovery:true,key:'previous-review'}}};
  const otherOwner={...owner,token:'another-edit-lease'};
  const packaged={...packageInfo,profiles:{default:{options:{speed:6}}},target:{id:'game',window_title:'Other title'},target_identity:'target-2'};
  const next=trialContext(context('saved-1',{owner:otherOwner,package:packaged}),openSession(view('saved-1',otherOwner)),'fixture',configured);
  assert.equal(next.profileChoice,'preset:default');
  assert.equal(next.bound.selectedId,null);
  assert.deepEqual(deriveBound(next.bound,null).parsed.values,{speed:6});
  assert.notEqual(next.bound.draft,configured.bound.draft);
  assert.equal(next.bound.lane,'native');
  assert.equal(next.bound.descriptorPath,'');
  assert.equal(next.unit,'package');
  assert.equal(next.selected,null);
  assert.equal(next.arguments,'[]');
  assert.deepEqual(next.bound.native,{operation:'',postcondition:'',workflowSeconds:null,capture:false,input:false,launch:false,recovery:false,key:null});
  assert.equal(deriveBound(next.bound,null).native.intent,null);
});

for(const {scenario,input,required,error} of [
  {scenario:'expression arguments are not evaluated',input:'[(() => 1)()]',required:0,error:'json'},
  {scenario:'an object is not a positional argument array',input:'{}',required:0,error:'array'},
  {scenario:'argument count is bounded',input:JSON.stringify(Array(33).fill(null)),required:0,error:'count'},
  {scenario:'argument bytes use UTF-8 rather than UTF-16',input:JSON.stringify(['界'.repeat(22_000)]),required:0,error:'bytes'},
  {scenario:'nested argument depth is bounded',input:'['.repeat(34)+'0'+']'.repeat(34),required:0,error:'depth'},
  {scenario:'statically required arguments cannot be invented',input:'[]',required:1,error:'required'},
]) test(scenario,()=>assert.equal(trialArguments(input,required).error,error));

test('JSON arguments preserve positional data, empty optional arguments and literal object keys',()=>{
  assert.deepEqual(trialArguments('[]'),{values:[],error:null});
  const result=trialArguments('[7,{"__proto__":{"value":2},"text":"host.input()"},null]',1);
  assert.equal(result.error,null);
  assert.deepEqual(result.values[0],7);
  assert.equal(Object.hasOwn(result.values[1],'__proto__'),true);
  assert.equal(result.values[1].text,'host.input()');
});

test('a captured Run source survives navigation and non-mutating work, but not composition',()=>{
  const drafts=controller();
  const ticket=trialSourceTicket(drafts);
  assert.ok(ticket);
  drafts.update(state=>selectFile(state,'other.ts',null));
  drafts.update(state=>beginPending(state,{kind:'script_trial'}));
  assert.equal(currentTrialSource(drafts,ticket),true);
  assert.equal(trialSourceTicket(drafts),null);
  drafts.update(state=>({...state,pending:null}));
  drafts.update(state=>beginComposition(state,'battle.ts',{start:0,end:0}));
  assert.equal(trialSourceTicket(drafts),null);
  assert.equal(currentTrialSource(drafts,ticket),false);
  drafts.update(state=>endComposition(state,'battle.ts'));
  assert.ok(trialSourceTicket(drafts));
});

test('human and OMP edits invalidate captured source even after Undo returns to saved bytes',()=>{
  const drafts=controller();
  const before=trialSourceTicket(drafts);
  drafts.update(state=>replaceFile(state,'battle.ts',source+'// human\n'));
  assert.equal(trialSourceTicket(drafts),null);
  drafts.update(state=>undoFile(state,'battle.ts'));
  assert.equal(drafts.current().drafts.get('battle.ts').text,source);
  assert.equal(currentTrialSource(drafts,before),false);
  const reviewed=trialSourceTicket(drafts);
  const description=drafts.describe();
  const resource=description.resources.find(item=>item.path==='other.ts');
  const result=drafts.handle({id:'edit',owner:description.owner,package:description.package,operation:{kind:'edit',edits:[
    {resource:resource.id,version:resource.version,text:'export const value = 2;\n'},
  ],dependencies:[]}});
  assert.equal(result.ok,true);
  assert.equal(currentTrialSource(drafts,reviewed),false);
  assert.equal(trialSourceTicket(drafts),null);
});

test('a late save preserves newer text and cannot capture old disk as current source',()=>{
  const drafts=controller();
  drafts.update(state=>replaceFile(state,'battle.ts',source+'// saved\n'));
  const ticket=saveTicket(drafts.current(),'battle.ts');
  drafts.update(state=>beginPending(state,{kind:'save',path:'battle.ts'}));
  drafts.update(state=>replaceFile(state,'battle.ts',source+'// newer\n'));
  const saved=view('saved-2');
  saved.files[0].text=ticket.text;
  drafts.update(state=>applySave(state,ticket,{owner,committed_revision:'saved-2',view:saved,refresh_error:null}));
  assert.equal(drafts.current().drafts.get('battle.ts').base,ticket.text);
  assert.equal(drafts.current().drafts.get('battle.ts').text,source+'// newer\n');
  assert.equal(trialSourceTicket(drafts),null);
  assert.equal(drafts.current().drafts.get('battle.ts').undo.length>0,true);
});

for(const {scenario,update} of [
  {scenario:'a committed save awaiting refresh cannot run',update:state=>({...state,refreshRequired:true})},
  {scenario:'an external conflict cannot run',update:state=>({...state,conflict:true})},
  {scenario:'a retained missing source cannot run',update:state=>({...state,drafts:new Map([...state.drafts].map(([path,draft])=>[path,{...draft,missing:true}]))})},
  {scenario:'an external disk change cannot run',update:state=>({...state,drafts:new Map([...state.drafts].map(([path,draft])=>[path,{...draft,diskChanged:true}]))})},
]) test(scenario,()=>{
  const drafts=controller();
  const ticket=trialSourceTicket(drafts);
  drafts.update(update);
  assert.equal(trialSourceTicket(drafts),null);
  assert.equal(currentTrialSource(drafts,ticket),false);
});

test('rerun resolves the same declaration on current saved source and refuses a renamed entry without fallback',()=>{
  const session=openSession(view());
  const initial=trialContext(context(),session,'fixture',null);
  const configured={...initial,unit:'function',selected:functionKey(candidate),arguments:'[8]',profileChoice:'profile:profile-one',
    bound:{...chooseLane(editDraft(selectProfile(initial.bound,profile.id),{speed:'4'}),'replay'),descriptorPath:'/fixture/replay'}};
  const moved={...candidate,line:12,column:7,signature:'battle(turn: number, optional?: string): number'};
  const refreshed=trialContext(context('saved-2',{functions:[moved],package:{...packageInfo,inventory_identity:'inventory-2'}}),
    {...session,revision:'saved-2'},'fixture',configured);
  assert.equal(refreshed.selected,functionKey(candidate));
  assert.equal(refreshed.arguments,'[8]');
  assert.deepEqual(refreshed.bound.draft,{speed:'4'});
  assert.equal(refreshed.bound.selectedId,profile.id);
  assert.equal(refreshed.profileChoice,'profile:profile-one');
  assert.equal(refreshed.unit,'function');
  assert.equal(refreshed.bound.lane,'replay');
  assert.equal(refreshed.bound.descriptorPath,'/fixture/replay');
  assert.equal(refreshed.bound.draftRevision,configured.bound.draftRevision);
  assert.deepEqual(deriveBound(refreshed.bound,null).parsed.values,{speed:4});
  const renamed=trialContext(context('saved-3',{functions:[{...candidate,function:'newBattle'}]}),{...session,revision:'saved-3'},'fixture',refreshed);
  assert.equal(renamed.unit,'function');
  assert.equal(renamed.selected,null);
  assert.equal(renamed.arguments,'[8]');
  assert.equal(trialContext({...context(),owner:{...owner,token:'foreign'}},session,'fixture',configured),null);
});

test('same-owner reload retains the chosen preset values and lane when package defaults change',()=>{
  const session=openSession(view());
  const initial=trialContext(context(),session,'fixture',null);
  const selected={...initial,profileChoice:'preset:fast',bound:chooseLane(newDraft(initial.bound,'fast'),'controlled')};
  const packaged={...packageInfo,profiles:{fast:{options:{speed:9}},default:{options:{speed:7}}},
    target:{id:'game',window_title:'Exact title'},target_identity:'target-1'};
  const next=trialContext(context('saved-2',{package:packaged}),{...session,revision:'saved-2'},'fixture',selected);
  assert.equal(next.profileChoice,'preset:fast');
  assert.equal(next.bound.preset,'fast');
  assert.equal(next.bound.selectedId,null);
  assert.equal(next.bound.draft,selected.bound.draft);
  assert.equal(next.bound.lane,'controlled');
  const facts=deriveBound(next.bound,null);
  assert.equal(facts.numericErrors,false);
  assert.equal(facts.startBlock,null);
  assert.deepEqual(commandValues({bound:next.bound},facts),{speed:2});
});

test('a disappeared preset stays unselected across later reloads instead of adopting the packaged default',()=>{
  const session=openSession(view());
  const initial=trialContext(context(),session,'fixture',null);
  const selected={...initial,profileChoice:'preset:fast',bound:newDraft(initial.bound,'fast')};
  const packaged={...packageInfo,profiles:{default:{options:{speed:7}}}};
  const next=trialContext(context('saved-2',{package:packaged}),{...session,revision:'saved-2'},'fixture',selected);
  assert.equal(next.profileChoice,'');
  assert.equal(next.bound.preset,'fast');
  assert.equal(next.bound.draft,selected.bound.draft);
  assert.deepEqual(deriveBound(next.bound,null).parsed.values,{speed:2});
  const reloaded=trialContext(context('saved-3',{package:packaged}),{...session,revision:'saved-3'},'fixture',next);
  assert.equal(reloaded.profileChoice,'');
  assert.equal(reloaded.bound.draft,selected.bound.draft);
  assert.deepEqual(deriveBound(reloaded.bound,null).parsed.values,{speed:2});
});

test('schema changes preserve incompatible profile values for correction rather than inventing defaults',()=>{
  const session=openSession(view());
  const initial=trialContext(context(),session,'fixture',null);
  const selected={...initial,bound:selectProfile(initial.bound,profile.id),profileChoice:'profile:profile-one'};
  const next=trialContext(context('saved-2',{package:{...packageInfo,schema_identity:'schema-2'}}),{...session,revision:'saved-2'},'fixture',selected);
  assert.deepEqual(next.bound.draft,{speed:3});
  assert.equal(next.bound.selectedId,profile.id);
  assert.equal(deriveBound(next.bound,null).bound,false);
  assert.equal(profile.schema_identity,'schema-1');
});

test('a removed or rejected saved profile requires a deliberate new values choice',()=>{
  const session=openSession(view());
  const initial=trialContext(context(),session,'fixture',null);
  const selected={...initial,bound:selectProfile(initial.bound,profile.id),profileChoice:'profile:profile-one'};
  const packaged={...packageInfo,profiles:{default:{options:{speed:7}}}};
  const next=trialContext(context('saved-2',{package:packaged,profiles:[],profiles_error:{category:'ProfileRejected',message:'schema changed',context:null}}),
    {...session,revision:'saved-2'},'fixture',selected);
  assert.equal(next.profileChoice,'');
  assert.equal(next.bound.selectedId,profile.id);
  assert.deepEqual(next.bound.draft,{speed:3});
  assert.equal(next.bound.profilesError.category,'ProfileRejected');
  const reloaded=trialContext(context('saved-3',{package:packaged,profiles:[]}),{...session,revision:'saved-3'},'fixture',next);
  assert.equal(reloaded.profileChoice,'');
  assert.equal(reloaded.bound.selectedId,profile.id);
  assert.deepEqual(reloaded.bound.draft,{speed:3});
});

function rejectedProfileView(token='repair-one'){
  return {context:{workspace:owner.workspace,token},relocation:false,binding_required:false,package_path:'/fixture/package',
    package:packageInfo,profiles:[{profile:{...profile,schema_identity:'older-schema'},
      issue:{category:'ProfileRejected',message:'Schema changed',context:null}}],profiles_error:null};
}

test('a committed Edit repair survives catalog failure without erasing newer repair or trial values',()=>{
  const recovery=rejectedProfileView();
  const initial=trialContext(context('saved-1',{profiles:[],recovery}),openSession(view()),'fixture',null);
  const ticket=recoveryTicket(initial.recovery);
  const current={...initial,profileChoice:'values',bound:editDraft(initial.bound,{speed:'7'}),
    recovery:editRecovery(initial.recovery,{speed:'8'},{kind:'replace',path:'$.speed'})};
  const saved={...profile,values:{speed:4}};
  const refresh_error={category:'Storage',message:'Catalog read failed',context:null};
  const next=applyTrialRecovery(current,ticket,{saved,draft:null,issue:null,recovery:{...recovery,profiles:[]},catalog:null,refresh_error});
  assert.deepEqual(next.recoveryOutcomes,[{profile_id:profile.id,name:profile.name,status:'saved',issue:null}]);
  assert.deepEqual(next.recovery.draft,{speed:'8'});
  assert.equal(next.recovery.touched,true);
  assert.equal(next.recovery.refreshError,refresh_error);
  assert.deepEqual(next.bound.draft,{speed:'7'});
  assert.equal(next.bound.selectedId,null);
  assert.equal(next.profileChoice,'values');
  assert.deepEqual(next.bound.profiles,[]);
  assert.equal(next.bound.profilesError,refresh_error);
});

test('an earlier repair receipt cannot replace the current context or profile catalog',()=>{
  const session=openSession(view());
  const initial=trialContext(context('saved-1',{recovery:rejectedProfileView()}),session,'fixture',null);
  const ticket=recoveryTicket(initial.recovery);
  const current=trialContext(context('saved-1',{recovery:rejectedProfileView('repair-two')}),session,'fixture',initial);
  const next=applyTrialRecovery(current,ticket,{saved:{...profile,values:{speed:4}},draft:null,issue:null,recovery:null,
    catalog:{profiles:[],profiles_error:null},refresh_error:null});
  assert.equal(next.recovery,current.recovery);
  assert.equal(next.bound,current.bound);
  assert.equal(next.recoveryOutcomes[0].status,'saved');
  assert.deepEqual(next.bound.profiles,[profile]);
});

test('Script preparation and stopping retain the real Edit owner and shared cancellation',()=>{
  const session=openSession(view());
  const idle={state:'idle',workspace_id:null,workspace_revision:null,operation:'run',run:null};
  const preparing=authoringActivity({...session,pending:{kind:'script_trial'}},owner,idle,null,'en');
  assert.equal(preparing.owner,owner);
  assert.deepEqual({phase:preparing.worker.phase,run:preparing.worker.run,cancellable:preparing.worker.cancellable},
    {phase:'preparing',run:null,cancellable:true});
  const live={state:'stopping',workspace_id:owner.workspace.workspace_id,workspace_revision:owner.workspace.revision,operation:'authoring_trial',run:'trial-run'};
  const stopping=authoringActivity(session,owner,live,null,'ja');
  assert.equal(stopping.owner,owner);
  assert.equal(stopping.worker.phase,'stopping');
  assert.equal(stopping.worker.run,'trial-run');
  assert.equal(stopping.worker.cancellable,true);
  assert.equal(authoringActivity(session,owner,{...live,workspace_revision:99},null,'en').worker,null);
});

test('compiler preparation diagnostics retain each original source position and only navigable source facts',()=>{
  const failure={category:'TypeScript',message:'compilation failed',context:{compiler:{identity:'compiler'},diagnostics:[
    {module:'battle.ts',line:3,column:7,code:2322,category:'Error',message:'incompatible value',function:'battle',private:{recognized:'private runtime text'}},
    {module:'other.ts',line:2,column:4,code:2339,message:'unknown property'},
    {code:1000,message:'unlocated compiler option error'},
    {module:'battle.ts',line:1,column:1,message:42},
  ]}};
  const diagnostics=trialDiagnostics(failure);
  assert.deepEqual(diagnostics,[
    {category:'TypeScript',message:'incompatible value',context:{path:'battle.ts',line:3,column:7,code:2322}},
    {category:'TypeScript',message:'unknown property',context:{path:'other.ts',line:2,column:4,code:2339}},
    {category:'TypeScript',message:'unlocated compiler option error',context:{path:null,line:null,column:null,code:1000}},
  ]);
  assert.deepEqual(diagnostics.map(trialDiagnosticLocation),[
    {path:'battle.ts',line:3,column:7},{path:'other.ts',line:2,column:4},null,
  ]);
  assert.deepEqual(trialDiagnosticLocation(failure),{path:'battle.ts',line:3,column:7});
});

test('compiler diagnostics and their message projection are bounded independently of the full fault context',()=>{
  const failure={category:'TypeScript',message:'compilation failed',context:{diagnostics:Array.from({length:105},(_,index)=>({
    module:'battle.ts',line:index+1,column:1,code:2322,message:'x'.repeat(5000),
  }))}};
  const diagnostics=trialDiagnostics(failure);
  assert.equal(diagnostics.length,100);
  assert.equal(diagnostics[0].message.length,4097);
  assert.equal(diagnostics[0].message.endsWith('…'),true);
  assert.deepEqual(trialDiagnosticLocation(diagnostics[99]),{path:'battle.ts',line:100,column:1});
  assert.equal(failure.context.diagnostics.length,105);
  assert.equal(failure.context.diagnostics[0].message.length,5000);
});

for(const {scenario,position} of [
  {scenario:'empty original module',position:{module:'',line:1,column:1}},
  {scenario:'absent original module',position:{line:1,column:1}},
  {scenario:'absent original line',position:{module:'battle.ts',column:1}},
  {scenario:'zero original line',position:{module:'battle.ts',line:0,column:1}},
  {scenario:'fractional original line',position:{module:'battle.ts',line:1.5,column:1}},
  {scenario:'unsafe original line',position:{module:'battle.ts',line:Number.MAX_SAFE_INTEGER+1,column:1}},
  {scenario:'non-finite original line',position:{module:'battle.ts',line:Infinity,column:1}},
  {scenario:'string original line',position:{module:'battle.ts',line:'3',column:1}},
  {scenario:'negative original column',position:{module:'battle.ts',line:1,column:-2}},
  {scenario:'zero original column',position:{module:'battle.ts',line:1,column:0}},
  {scenario:'fractional original column',position:{module:'battle.ts',line:1,column:1.5}},
  {scenario:'unsafe original column',position:{module:'battle.ts',line:1,column:Number.MAX_SAFE_INTEGER+1}},
  {scenario:'string original column',position:{module:'battle.ts',line:1,column:'7'}},
]) test(`${scenario} cannot become an exact source action`,()=>{
  const failure={category:'TypeScript',message:'compilation failed',context:{diagnostics:[{...position,message:'invalid position'}]}};
  assert.equal(trialDiagnosticLocation(failure),null);
  assert.equal(trialDiagnosticLocation(trialDiagnostics(failure)[0]),null);
  assert.equal(trialDiagnosticLocation({category:'Script',message:'failed',context:{typescript:{frames:[{original:position}]}}}),null);
});

test('structured compiler diagnostics never guess locations from messages or unrelated nested runtime contexts',()=>{
  const fault={category:'Script',message:'battle.ts:3:7 failed',context:{cause:{category:'TypeScript',context:{
    diagnostics:[{module:'battle.ts',line:3,column:7,message:'private runtime cause'}],
  }},diagnostics:[{module:'battle.ts',line:3,column:7,message:'not a compiler envelope'}]}};
  assert.deepEqual(trialDiagnostics(fault),[]);
  assert.equal(trialDiagnosticLocation(fault),null);
  assert.deepEqual(trialDiagnostics({...fault,category:'TypeScript',context:{diagnostics:[null,42,[],{message:7}]}}),[]);
  assert.equal(trialDiagnosticLocation({...fault,category:'TypeScript',context:{diagnostics:[
    {message:'battle.ts:3:7 failed'},
  ]}}),null);
});

test('normalized compiler source locations and missing columns use the existing one-based navigation convention',()=>{
  assert.deepEqual(trialDiagnosticLocation({category:'TypeScript',message:'failed',context:{path:'battle.ts',line:3,column:7}}),
    {path:'battle.ts',line:3,column:7});
  const fault={category:'TypeScript',message:'failed',context:{diagnostics:[{module:'battle.ts',line:3,message:'failed'}]}};
  assert.deepEqual(trialDiagnosticLocation(trialDiagnostics(fault)[0]),{path:'battle.ts',line:3,column:1});
});

test('a malformed original runtime frame does not hide a later valid original mapping',()=>{
  const fault={category:'Script',message:'failed',context:{module:'generated.js',line:99,column:10,typescript:{frames:[
    {original:{module:'battle.ts',line:-1,column:1}},
    {original:{module:'other.ts',line:4,column:8}},
  ]}}};
  assert.deepEqual(trialDiagnosticLocation(fault),{path:'other.ts',line:4,column:8});
  assert.deepEqual(trialDiagnostics(fault),[]);
});

test('source navigation prefers original TypeScript frames and never links generated coordinates when mapping is unavailable',()=>{
  const fault={category:'Script',message:'failed',context:{module:'battle.js',line:99,column:10,typescript:{mapping:'available',frames:[
    {generated:{module:'battle.js',line:99,column:10},original:{module:'battle.ts',line:3,column:7,function:'battle'}},
  ]}}};
  assert.deepEqual(trialDiagnosticLocation(fault),{path:'battle.ts',line:3,column:7});
  assert.equal(trialDiagnosticLocation({...fault,context:{...fault.context,typescript:{mapping:'unavailable',frames:[]}}}),null);
  assert.deepEqual(trialDiagnosticLocation({...fault,context:{module:'battle.js',line:4,column:null}}),{path:'battle.js',line:4,column:1});
});

for(const phase of ['host_attachment','instantiation','function']) test(`${phase} failure never claims package readiness was invoked`,()=>{
  const result=nativeOutcome({state:'terminal',native_preparation:{attempt:1,status:'pending',phase,launch:'not_requested'},
    error:{category:'Script',message:'failed',context:null},result:null});
  assert.equal(result.failure,phase);
  assert.equal(result.launch,null);
});

const trialLimits={startup_ms:60000,readiness_ms:30000,workflow_ms:30000,max_frames:300,wait_ms:1000,interval_ms:100,max_actions:64,cleanup_ms:1000,containment_ms:2000};
const trialCapability={default_limits:trialLimits,max_workflow_ms:900000};
const trialEnvironment={profile:'fixture',model:'fixture-model',language:'en',provider:'cpu',runtime_profile:'fixture-runtime',
  model_root:'/fixture/models',runtime_path:'/fixture/runtime',native_library_paths:[]};

function nativeRunContext(){
  const drafts=controller();
  const packaged={...packageInfo,target:{id:'game',window_title:'Exact window'},target_identity:'target-declaration'};
  const initial=trialContext(context('saved-1',{package:packaged}),drafts.current(),'fixture',null);
  const configuration={platform:'macos',game:{kind:'bundle',path:'/fixture/Game.app'},launcher:null,arguments:['--fixture'],
    working_directory:null,window_title:'Exact window',input:{route:'process_directed',focus:'preserve',pointer_mode:'appkit_background',click_hold_ms:50}};
  const target={platform:'macos',context:initial.bound.target.context,compatible:true,
    record:{version:1,internal_name:'fixture',package_id:packageInfo.package_id,revision:1,binding:{
      id:'saved-target',package_id:packageInfo.package_id,target_id:'game',declaration_identity:'target-declaration',
      configuration,resolution:{game:{path:'/fixture/Game.app',executable:'/fixture/Game.app/Contents/MacOS/Game'},launcher:null,working_directory:null},
    }}};
  const state=trialContext({...initial.context,target},drafts.current(),'fixture',initial);
  return {controller:drafts,state,environment:structuredClone(trialEnvironment),capability:structuredClone(trialCapability)};
}

// The final request one explicit Run submits after rereading `state` for the current saved source.
function runRequest(input,state=input.state,intent=trialRunIntent(input.state,input.environment,input.capability)){
  return trialRunRequest(intent,state,trialSourceTicket(input.controller),input.environment,input.capability);
}

function chooseStartup(input){
  const launch=approveNative(input.state.bound,'launch',true,input.environment,input.capability,'script');
  return {...input.state,bound:approveNative(launch,'recovery',true,input.environment,input.capability,'script')};
}

test('configured Script Native is ready without prose or checkboxes, while ordinary Run still requires operation review',()=>{
  const input=nativeRunContext();
  const facts=deriveBound(input.state.bound,input.environment,'en',input.capability,'script');
  assert.equal(facts.startBlock,null);
  assert.equal(facts.native.block,null);
  assert.deepEqual(facts.native.intent.review,{kind:'script'});
  assert.equal(facts.native.intent.capture_approved,false);
  assert.equal(facts.native.intent.input_approved,false);
  assert.equal(facts.native.intent.launch_approved,false);
  assert.equal(facts.native.intent.max_exit_recoveries,0);
  assert.equal(deriveBound(input.state.bound,input.environment,'en',input.capability).native.block,'nativeText');
  assert.equal(deriveBound(input.state.bound,input.environment,'en',input.capability).native.intent,null);
});

for(const {scenario,change,block} of [
  {scenario:'missing OCR environment',change:input=>({...input,environment:null}),block:'nativeEnvironment'},
  {scenario:'unsupported Native capability',change:input=>({...input,capability:null}),block:'nativeUnavailable'},
  {scenario:'invalid duration',change:input=>({...input,state:{...input.state,bound:editNativeReview(input.state.bound,'workflowSeconds','0')}}),block:'nativeWorkflow'},
  {scenario:'unreadable saved target',change:input=>({...input,state:{...input.state,bound:{...input.state.bound,
    target:targetReadFailed(input.state.bound.target,{category:'Storage',message:'Unavailable',context:null})}}}),block:'nativeTarget'},
  {scenario:'dirty target configuration',change:input=>({...input,state:{...input.state,bound:{...input.state.bound,
    target:editTarget(input.state.bound.target,{...input.state.bound.target.draft,windowTitle:'Unsaved title'})}}}),block:'nativeTargetDirty'},
]) test(`Script Run with ${scenario} submits nothing`,()=>{
  const input=change(nativeRunContext());
  assert.equal(deriveBound(input.state.bound,input.environment,'en',input.capability,'script').native.block,block);
  assert.equal(runRequest(input),'setupIncomplete');
});

test('setup and its rereads grant no capture/input authority; only the request of one explicit Run carries it',()=>{
  const input=nativeRunContext();
  const prepared=runRequest(input);
  assert.deepEqual(prepared.request.native_intent,{target_revision:1,target_binding_id:'saved-target',target_declaration_identity:'target-declaration',
    capture_approved:true,input_approved:true,launch_approved:false,max_exit_recoveries:0,review:{kind:'script'},limits:trialLimits});
  assert.deepEqual([input.state.bound.native.capture,input.state.bound.native.input],[false,false]);
  const reread=trialContext(input.state.context,input.controller.current(),'fixture',input.state);
  assert.equal(deriveBound(reread.bound,input.environment,'en',input.capability,'script').native.intent.capture_approved,false);
  // A repeated Run needs no reopened setup and builds its own request instead of reusing the previous one.
  const repeated=runRequest(input,reread);
  assert.notEqual(repeated.request.native_intent,prepared.request.native_intent);
  assert.deepEqual(repeated.request,prepared.request);
});

test('explicit Run submits the selected function, arguments, profile and target after an unchanged reread',()=>{
  const input=nativeRunContext();
  input.state={...input.state,unit:'function',selected:functionKey(candidate),arguments:'[8, {"nested":[1]}]',
    profileChoice:'profile:profile-one',bound:selectProfile(input.state.bound,profile.id)};
  const reread=trialContext(input.state.context,input.controller.current(),'fixture',input.state);
  const prepared=runRequest(input,reread);
  assert.equal(prepared.profileName,'Reviewed');
  assert.deepEqual(prepared.request.function,{module:'battle.ts',function:'battle',arguments:[8,{nested:[1]}]});
  assert.deepEqual(prepared.request.values,{speed:3});
  assert.equal(prepared.request.profile_id,'profile-one');
  assert.equal(prepared.request.native_intent.target_binding_id,'saved-target');
  assert.deepEqual([prepared.request.native_intent.launch_approved,prepared.request.native_intent.max_exit_recoveries],[false,0]);
});

for(const {scenario,change} of [
  {scenario:'another Edit owner',change:state=>({...state,context:{...state.context,owner:{...owner,token:'other-owner'}}})},
  {scenario:'another execution unit',change:state=>({...state,unit:'function',selected:functionKey(candidate),arguments:'[7]'})},
  {scenario:'another destination',change:state=>({...state,bound:chooseLane(state.bound,'controlled')})},
  {scenario:'a saved revision other than the captured source',change:state=>({...state,context:{...state.context,revision:'saved-2'}})},
  {scenario:'a newer saved target revision under the same binding ID',change:state=>{
    const target=state.bound.target;
    return {...state,bound:{...state.bound,target:readTarget(target,{...target.view,record:{...target.view.record,revision:2}})}};
  }},
]) test(`a reread with ${scenario} refuses the Run instead of submitting newer data`,()=>{
  const input=nativeRunContext();
  assert.equal(runRequest(input,change(input.state)),'changed');
});

test('a target replaced during Run preparation is refused rather than replacing the target shown at Run',()=>{
  const input=nativeRunContext();
  const intent=trialRunIntent(input.state,input.environment,input.capability);
  assert.deepEqual(intent.target,{id:'saved-target',revision:1});
  const original=input.state.context.target;
  const binding=original.record.binding;
  const reread=trialContext({...input.state.context,target:{...original,record:{...original.record,revision:2,
    binding:{...binding,id:'replacement-target',configuration:{...binding.configuration,window_title:'Replacement window'}}}}},
    input.controller.current(),'fixture',input.state);
  assert.equal(reread.bound.target.view.record.binding.id,'replacement-target');
  assert.equal(runRequest(input,reread,intent),'changed');
  // The next explicit Run names the replacement before it may submit it.
  assert.equal(runRequest(input,reread,trialRunIntent(reread,input.environment,input.capability)).request.native_intent.target_binding_id,'replacement-target');
});

test('a function missing from the newer saved source asks for a new selection and is never substituted',()=>{
  const input=nativeRunContext();
  const other={...candidate,function:'other',line:5};
  input.state={...input.state,context:{...input.state.context,functions:[candidate,other]},unit:'function',selected:functionKey(candidate),arguments:'[1]'};
  const intent=trialRunIntent(input.state,input.environment,input.capability);
  const reread=trialContext({...input.state.context,functions:[other]},input.controller.current(),'fixture',input.state);
  assert.equal(reread.selected,null);
  assert.equal(runRequest(input,reread,intent),'selectAgain');
  assert.equal(runRequest(input,{...input.state,selected:functionKey(other)},intent),'changed');
});

test('dirty Native Save all and run continues to the newly saved revision with the same setup and no review step',()=>{
  const input=nativeRunContext();
  input.state={...chooseStartup(input),unit:'package'};
  const intent=trialRunIntent(input.state,input.environment,input.capability);
  input.controller.update(state=>replaceFile(state,'battle.ts',source+'// saved later\n'));
  assert.equal(trialSourceTicket(input.controller),null);
  const ticket=saveTicket(input.controller.current(),'battle.ts');
  const saved=view('saved-2');
  saved.files[0].text=ticket.text;
  input.controller.update(state=>applySave(state,ticket,{owner,committed_revision:'saved-2',view:saved,refresh_error:null}));
  const current=trialSourceTicket(input.controller);
  assert.equal(current.revision,'saved-2');
  // The setup read for the earlier revision cannot run the newer source; its reread does, with the same choices.
  assert.equal(runRequest(input),'changed');
  const reread=trialContext({...input.state.context,revision:'saved-2',package:{...input.state.context.package,inventory_identity:'inventory-2'}},
    input.controller.current(),'fixture',input.state);
  const prepared=runRequest(input,reread,intent);
  assert.equal(prepared.request.inventory_identity,'inventory-2');
  assert.equal(prepared.request.function,null);
  assert.deepEqual([prepared.request.native_intent.capture_approved,prepared.request.native_intent.input_approved],[true,true]);
  assert.deepEqual([prepared.request.native_intent.launch_approved,prepared.request.native_intent.max_exit_recoveries],[true,1]);
});

test('whole-package launch/recovery remain separate choices, Function never borrows them and a new target withdraws them',()=>{
  const input=nativeRunContext();
  input.state=chooseStartup(input);
  const whole=runRequest(input);
  assert.deepEqual([whole.request.native_intent.launch_approved,whole.request.native_intent.max_exit_recoveries],[true,1]);
  const fn=runRequest(input,{...input.state,unit:'function',selected:functionKey(candidate),arguments:'[1]'},
    trialRunIntent({...input.state,unit:'function',selected:functionKey(candidate)},input.environment,input.capability));
  assert.deepEqual([fn.request.native_intent.launch_approved,fn.request.native_intent.max_exit_recoveries],[false,0]);
  assert.deepEqual([fn.request.native_intent.capture_approved,fn.request.native_intent.input_approved],[true,true]);
  const target=input.state.bound.target;
  const retargeted={...input.state,bound:{...input.state.bound,target:readTarget(target,{...target.view,record:{...target.view.record,revision:2}})}};
  const native=deriveBound(retargeted.bound,input.environment,'en',input.capability,'script').native;
  assert.deepEqual([native.launch,native.recovery,native.intent.launch_approved],[false,false,false]);
});

test('a schema reread cannot silently drop the startup choices named by Run',()=>{
  const input=nativeRunContext();
  input.state=chooseStartup(input);
  const intent=trialRunIntent(input.state,input.environment,input.capability);
  const reread=trialContext({...input.state.context,package:{...input.state.context.package,schema_identity:'schema-2'}},
    input.controller.current(),'fixture',input.state);
  assert.equal(runRequest(input,reread,intent),'changed');
});

test('Controlled and replay Runs keep their values and invocation without Native authority',()=>{
  const input=nativeRunContext();
  input.state={...input.state,unit:'function',selected:functionKey(candidate),arguments:'[4]',bound:chooseLane(input.state.bound,'controlled')};
  const controlled=runRequest({...input,environment:null,capability:null});
  assert.equal(controlled.request.native_intent,null);
  assert.deepEqual(controlled.request.function,{module:'battle.ts',function:'battle',arguments:[4]});
  const replay={...input.state,bound:{...input.state.bound,lane:'replay',descriptorPath:'/fixture/corpus.json'}};
  const prepared=runRequest({...input,state:replay,capability:null});
  assert.equal(prepared.request.replay_descriptor_path,'/fixture/corpus.json');
  assert.equal(prepared.request.native_intent,null);
});

test('a saved profile changed under the same ID before Run submits the selected values as a draft, not that profile identity',()=>{
  const input=nativeRunContext();
  input.state={...input.state,profileChoice:'profile:profile-one',bound:selectProfile(input.state.bound,profile.id)};
  const reread=trialContext({...input.state.context,profiles:[{...profile,name:'Changed profile',values:{speed:7}}]},
    input.controller.current(),'fixture',input.state);
  const prepared=runRequest(input,reread);
  assert.deepEqual(prepared.request.values,{speed:3});
  assert.equal(prepared.request.profile_id,'draft');
});
