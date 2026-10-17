import {test} from 'node:test';
import assert from 'node:assert/strict';
import {AuthoringController} from './authoring-controller.ts';
import {applySave, beginComposition, beginPending, endComposition, openSession, replaceFile, saveTicket, selectFile, undoFile} from './authoring.ts';
import {applyTrialRecovery, currentTrialSource, functionKey, trialArguments, trialContext, trialDiagnosticLocation, trialDiagnostics, trialSourceTicket} from './edit-trial.ts';
import {deriveBound, editDraft, selectProfile} from './workspace.ts';
import {nativeOutcome} from './state.ts';
import {authoringActivity} from './status.ts';
import {editRecovery, recoveryTicket} from './recovery.ts';

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

test('a captured source review survives navigation and non-mutating work, but not composition',()=>{
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
    bound:editDraft(selectProfile(initial.bound,profile.id),{speed:'4'})};
  const moved={...candidate,line:12,column:7,signature:'battle(turn: number, optional?: string): number'};
  const refreshed=trialContext(context('saved-2',{functions:[moved],package:{...packageInfo,inventory_identity:'inventory-2'}}),
    {...session,revision:'saved-2'},'fixture',configured);
  assert.equal(refreshed.selected,functionKey(candidate));
  assert.equal(refreshed.arguments,'[8]');
  assert.deepEqual(refreshed.bound.draft,{speed:'4'});
  assert.equal(refreshed.bound.selectedId,profile.id);
  assert.equal(refreshed.review,null);
  const renamed=trialContext(context('saved-3',{functions:[{...candidate,function:'newBattle'}]}),{...session,revision:'saved-3'},'fixture',refreshed);
  assert.equal(renamed.unit,'function');
  assert.equal(renamed.selected,null);
  assert.equal(renamed.arguments,'[8]');
  assert.equal(trialContext({...context(),owner:{...owner,token:'foreign'}},session,'fixture',configured),null);
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
  const next=trialContext(context('saved-2',{profiles:[],profiles_error:{category:'ProfileRejected',message:'schema changed',context:null}}),
    {...session,revision:'saved-2'},'fixture',selected);
  assert.equal(next.profileChoice,'');
  assert.equal(next.bound.selectedId,profile.id);
  assert.deepEqual(next.bound.draft,{speed:3});
  assert.equal(next.bound.profilesError.category,'ProfileRejected');
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

test('an unchanged saved-context read retains review, while a newer saved revision consumes it',()=>{
  const drafts=controller();
  const initial=trialContext(context(),drafts.current(),'fixture',null);
  const sourceTicket=trialSourceTicket(drafts);
  const reviewed={...initial,review:sourceTicket,bound:{...initial.bound,native:{...initial.bound.native,capture:true,input:true,key:'reviewed'}}};
  const same=trialContext(context(),drafts.current(),'fixture',reviewed);
  assert.equal(same.review,sourceTicket);
  assert.equal(same.bound.native.capture,true);
  const newer=trialContext(context('saved-2'),{...drafts.current(),revision:'saved-2'},'fixture',reviewed);
  assert.equal(newer.review,null);
  assert.equal(newer.bound.native.capture,false);
  assert.equal(newer.bound.native.input,false);
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
