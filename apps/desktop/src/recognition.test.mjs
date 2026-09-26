import {test} from 'node:test';
import assert from 'node:assert/strict';
import {
  MAX_DEFINITIONS, UNDO_BYTES, UNDO_ENTRIES, applyConfirm, applyCopy, applyDiscard, applyPreviewEdit, applySave, applySync, applyTrial, applyView,
  clientToFrame, confirmTicket, copyBlock, copyFreshness, copyTicket, createDefinition, deleteDefinition, displayScale, dragEdges, geometryConfirmed,
  hitHandle, mapRegion, openRecognition, previewSnapshot, regionFromEdges, renameDefinition, saveBlock, saveTicket, setContent, setExpected, setRegion,
  spanEdges, syncTicket, toggleCrop, toggleTrial, trialBlock, trialFreshness, trialTicket, undoRecognition,
} from './recognition.ts';

const MIB=1_048_576;
const owner={workspace:{workspace_id:'a',revision:1},token:'lease-1'};
const policy={input_bytes:32*MIB,input_pixels:16_777_216,crop_bytes:16*MIB,crop_pixels:4_194_304,package_image_bytes:64*MIB,package_non_image_bytes:MIB,
  package_bytes:65*MIB,package_decoded_bytes:128*MIB,replay_decoded_bytes:128*MIB,payload_bytes:512*MIB,preview_pixels:4_194_304};
const FRAME={id:'f1',width:1920,height:1080,revision:1,confirmed:true};
function hostView(overrides={}){
  return {owner,revision:'rev-1',document:null,saved_document:null,document_revision:1,frame:null,
    capabilities:{max_ocr_zones:8,diagnostic_regions:256,diagnostic_bytes:262144,expected_bytes:4096,image_policy:policy},
    configuration_revision:'cfg-1',trial:null,...overrides};
}
const edges=(left,top,right,bottom)=>({left,top,right,bottom});
// The host accepts exactly the local draft, as recognition_update does; a changed basis needs confirmation again.
function sync(state){
  const ticket=syncTicket(state);
  if (!ticket) return state;
  const basisChanged=JSON.stringify(ticket.document.basis)!==JSON.stringify(state.view.document?.basis);
  const frame=state.view.frame&&{...state.view.frame,confirmed:state.view.frame.confirmed&&!basisChanged};
  return applySync(state,ticket,{...state.view,document:ticket.document,document_revision:state.view.document_revision+1,frame});
}
function confirm(state){
  const ticket=confirmTicket(state);
  assert.ok(ticket,'geometry should be confirmable');
  return applyConfirm(state,ticket,{...state.view,frame:{...state.view.frame,confirmed:true}});
}
// A confirmed frame whose default full-image document the host already holds.
function loaded(frame=FRAME){
  const state=openRecognition(hostView({frame}));
  return confirm(sync(state));
}
function create(state,box,name='zone'){
  return createDefinition(state,regionFromEdges(box,state.document.basis),name);
}
function trialResult(ticket,zones,overrides={}){
  const envelope={version:1,operation:'recognition_trial',environment_identity:'env',primary:null,cleanup:{clean:true},child_reaped:true,forced:false,exit_code:0,
    result:{kind:'ocr',text_contract:'facade',zones}};
  return {owner,revision:ticket.revision,document_revision:ticket.document_revision,frame_id:ticket.frame_id,frame_revision:1,configuration_revision:'cfg-1',
    sample_id:ticket.sample_id,stale:false,
    controller:{run:'run-1',state:'finished',operation:'recognition_trial',result:envelope,error:null,progress:[],dropped_logs:0,workspace_id:'a',workspace_revision:1},
    ...overrides};
}
const zone=(id,text)=>({id,outcome:text===null?'no_match':'recognized',regions:text===null?[]:[{text,confidence:0.9,bounds:{x:1,y:2,width:3,height:4},geometry:[]}]});

test('every integer pixel edge survives normalized storage and floor/ceil remapping for any content size',()=>{
  for (let size=1; size<=300; size++) {
    const basis={frame_width:size+37,frame_height:size+11,content:{x:37,y:11,width:size,height:size}};
    for (let k=0; k<size; k++) {
      // Low edges map with floor, high edges with ceil; neither may drift a pixel after f64 rounding.
      const low=mapRegion(regionFromEdges(edges(37+k,11+k,37+size,11+size),basis),basis);
      assert.deepEqual(low,edges(37+k,11+k,37+size,11+size),`low edge ${k} of ${size}`);
      const high=mapRegion(regionFromEdges(edges(37,11,37+k+1,11+k+1),basis),basis);
      assert.deepEqual(high,edges(37,11,37+k+1,11+k+1),`high edge ${k+1} of ${size}`);
    }
  }
});

test('regions follow each image\'s own confirmed black bars and round outward at fractional edges',()=>{
  const letterbox={frame_width:1920,frame_height:1080,content:{x:0,y:140,width:1920,height:800}};
  const region=regionFromEdges(edges(100,200,300,260),letterbox);
  assert.deepEqual(mapRegion(region,letterbox),edges(100,200,300,260));
  // The same normalized zone on another capture with thinner bars uses that capture's content origin and size.
  const thinner={frame_width:1920,frame_height:1080,content:{x:0,y:60,width:1920,height:960}};
  assert.deepEqual(mapRegion(region,thinner),edges(100,60+72,300,60+144));
  // Fractional edges: left/top floor, right/bottom ceil.
  const small={frame_width:20,frame_height:20,content:{x:5,y:5,width:10,height:10}};
  assert.deepEqual(mapRegion({u0:0.25,v0:0.125,u1:0.625,v1:0.875},small),edges(5+2,5+1,5+7,5+9));
  // Invalid or out-of-content geometry is refused, never clipped.
  for (const invalid of [{u0:0,v0:0,u1:1.25,v1:1},{u0:Number.NaN,v0:0,u1:1,v1:1},{u0:0.5,v0:0,u1:0.5,v1:1},{u0:-0.1,v0:0,u1:1,v1:1}]) {
    assert.equal(mapRegion(invalid,letterbox),null);
  }
  assert.equal(regionFromEdges(edges(100,100,300,200),letterbox),null,'a rectangle inside the top bar is outside the content');
  assert.equal(regionFromEdges(edges(100,200,100,260),letterbox),null);
});

test('drawing the same rectangle at Fit and at 200% with scroll stores identical metadata',()=>{
  const frame={width:3840,height:2160};
  const basis={frame_width:3840,frame_height:2160,content:{x:0,y:0,width:3840,height:2160}};
  const fit=displayScale('fit',frame.width,frame.height,{width:960,height:600});
  assert.equal(fit,0.25);
  assert.equal(displayScale(200,frame.width,frame.height,{width:960,height:600}),2);
  // Rendered client rectangles in CSS pixels: letterboxed at Fit, scrolled far left/up at 200%.
  const rects=[{left:12,top:50,width:3840*fit,height:2160*fit},{left:-3000.5,top:-1200.25,width:3840*2,height:2160*2}];
  const regions=rects.map(rect=>{
    const scale=rect.width/frame.width;
    const client=point=>clientToFrame(rect.left+point.x*scale,rect.top+point.y*scale,rect,frame.width,frame.height);
    return regionFromEdges(spanEdges(client({x:1000.2,y:500.4}),client({x:1500.4,y:800.2}),edges(0,0,3840,2160)),basis);
  });
  assert.deepEqual(regions[0],regions[1]);
  assert.deepEqual(mapRegion(regions[0],basis),edges(1000,500,1500,800));
});

test('mouse moves stay inside the content and resized edges never cross',()=>{
  const content=edges(0,140,1920,940);
  assert.deepEqual(dragEdges(edges(100,200,300,260),'move',-500,-500,content),edges(0,140,200,200));
  assert.deepEqual(dragEdges(edges(100,200,300,260),'move',5000,5000,content),edges(1720,880,1920,940));
  assert.deepEqual(dragEdges(edges(100,200,300,260),'nw',400,400,content),edges(299,259,300,260));
  assert.deepEqual(dragEdges(edges(100,200,300,260),'se',-400,-400,content),edges(100,200,101,201));
  assert.deepEqual(dragEdges(edges(100,200,300,260),'w',-500,0,content),edges(0,200,300,260));
  assert.equal(spanEdges({x:10,y:10},{x:10.2,y:300},content),null,'a click is not a rectangle');
  assert.deepEqual(spanEdges({x:50,y:100},{x:10,y:2000},content),edges(10,140,50,940));
  // A narrow region keeps a reachable body; nearby outside points grab its edges.
  assert.equal(hitHandle(edges(10,10,13,40),{x:11.5,y:25},6),'move');
  assert.equal(hitHandle(edges(10,10,13,40),{x:6,y:25},6),'w');
  assert.equal(hitHandle(edges(10,10,13,40),{x:30,y:25},6),null);
});

test('overlay and list share stable IDs through create, delete and Undo; IDs are never reused',()=>{
  let state=loaded();
  state=create(state,edges(10,10,50,50),'a');
  state=create(state,edges(60,10,90,50),'b');
  state=create(state,edges(100,10,150,50),'c');
  assert.deepEqual(state.document.definitions.map(item=>item.id),['r1','r2','r3']);
  assert.equal(state.selected,'r3');
  state=deleteDefinition({...state,selected:'r2'},'r2');
  assert.equal(state.selected,'r3');
  state=undoRecognition(state);
  assert.deepEqual(state.document.definitions.map(item=>item.name),['a','b','c']);
  assert.equal(state.selected,'r2');
  assert.equal(previewSnapshot(state,'en',true,null).selected,'r2');
  state=create(state,edges(200,10,250,50),'d');
  assert.equal(state.selected,'r4');
});

test('metadata Undo keeps at most 64 actions and 1 MiB, dropping the oldest',()=>{
  let state=create(loaded(),edges(10,10,50,50));
  for (let step=1; step<=70; step++) state=setRegion(state,'r1','region',regionFromEdges(edges(10+step,10,50+step,50),state.document.basis));
  assert.equal(state.undo.length,UNDO_ENTRIES);
  for (let step=0; step<UNDO_ENTRIES; step++) state=undoRecognition(state);
  assert.deepEqual(mapRegion(state.document.definitions[0].region,state.document.basis),edges(16,10,56,50),'the six oldest steps were dropped');
  assert.equal(undoRecognition(state),state);

  // Large metadata: each snapshot is about 200 KB, so the byte bound applies before the action count.
  let large=loaded();
  for (let index=0; index<50; index++) large=setExpected(create(large,edges(index,0,index+1,1)),`r${index+1}`,'x'.repeat(4000));
  for (let step=0; step<10; step++) large=setRegion(large,'r1','region',regionFromEdges(edges(0,step+1,1,step+2),large.document.basis));
  assert.ok(large.undoBytes<=UNDO_BYTES);
  assert.ok(large.undo.length>0 && large.undo.length<10);
});

test('definition and metadata budgets refuse edits without changing the draft',()=>{
  let state=loaded();
  const tooLong='あ'.repeat(1366);
  state=create(state,edges(0,0,10,10));
  const before=state.document;
  state=setExpected(state,'r1',tooLong);
  assert.equal(state.notice,'expectedLimit');
  assert.equal(state.document,before);
  for (let index=1; index<MAX_DEFINITIONS; index++) state=create(state,edges(index,0,index+1,1),'z');
  assert.equal(state.document.definitions.length,MAX_DEFINITIONS);
  const full=state.document;
  state=create(state,edges(0,20,5,25));
  assert.equal(state.notice,'definitionLimit');
  assert.equal(state.document,full);
});

test('nine saved definitions stay valid while a grouped trial is bounded by the engine limit',()=>{
  let state=loaded();
  for (let index=0; index<9; index++) state=create(state,edges(index*20,0,index*20+10,10),`zone ${index}`);
  state=sync(state);
  assert.equal(saveBlock(state),null);
  for (const {id} of state.document.definitions) state=toggleTrial(state,id);
  assert.equal(state.trialIds.length,9);
  assert.equal(trialBlock(state,'frame',state.trialIds),'overLimit');
  assert.equal(trialTicket(state,'frame',state.trialIds),null);
  state=toggleTrial(state,'r9');
  const ticket=trialTicket(state,'frame',state.trialIds);
  assert.deepEqual(ticket.selected_ids,['r1','r2','r3','r4','r5','r6','r7','r8']);
  // Without the engine's reported bound there is no fallback number.
  const unknown={...state,view:{...state.view,capabilities:{...state.view.capabilities,max_ocr_zones:null}}};
  assert.equal(trialBlock(unknown,'frame',['r1']),'noCapability');
});

test('a replacement frame keeps normalized definitions but requires confirmation and drops chosen crops',()=>{
  let state=create(loaded(),edges(100,100,200,200));
  state=setContent(state,{x:0,y:60,width:1920,height:960});
  state=confirm(sync(state));
  state=toggleCrop(state,'r1');
  const region=state.document.definitions[0].region;
  assert.equal(geometryConfirmed(state),true);
  state=applyView(state,{...state.view,frame:{id:'f2',width:1920,height:1080,revision:2,confirmed:false}});
  assert.deepEqual(state.document.definitions[0].region,region);
  assert.deepEqual(state.document.basis.content,{x:0,y:60,width:1920,height:960});
  assert.equal(geometryConfirmed(state),false);
  assert.deepEqual(state.cropIds,[]);
  assert.equal(trialBlock(state,'frame',['r1']),'unconfirmed');
  assert.equal(copyBlock(state,'r1','ocr_recognize'),'unconfirmed');
  // Content that does not fit a smaller replacement restarts at the full image.
  state=applyView(state,{...state.view,frame:{id:'f3',width:1280,height:720,revision:3,confirmed:false}});
  assert.deepEqual(state.document.basis,{frame_width:1280,frame_height:720,content:{x:0,y:0,width:1280,height:720}});
  // A failed replacement leaves no frame while geometry metadata stays.
  state=applyView(state,{...state.view,frame:null});
  assert.equal(state.view.frame,null);
  assert.deepEqual(state.document.definitions[0].region,region);
});

test('a late result for edited inputs never marks the edited zone current, and Undo does not revive it',()=>{
  let state=create(create(loaded(),edges(10,10,50,50)),edges(60,10,90,50));
  state=sync(state);
  const ticket=trialTicket(state,'frame',['r1','r2']);
  const fresh=applyTrial(state,ticket,trialResult(ticket,[zone('r1','HP'),zone('r2',null)]));
  assert.equal(trialFreshness(fresh,'r1'),'fresh');
  assert.equal(trialFreshness(fresh,'r2'),'fresh');
  // The zone moves while the trial is outstanding; the result settles afterwards.
  let edited=setRegion(state,'r1','region',regionFromEdges(edges(12,10,52,50),state.document.basis));
  edited=applyTrial(sync(edited),ticket,trialResult(ticket,[zone('r1','HP'),zone('r2',null)]));
  assert.equal(trialFreshness(edited,'r1'),'stale');
  const restored=sync(undoRecognition(edited));
  assert.deepEqual(restored.document.definitions[0].region,state.document.definitions[0].region);
  assert.equal(trialFreshness(restored,'r1'),'stale');
  // A result that does not carry the ticket's captured inputs is historical only.
  const foreign=applyTrial(state,ticket,trialResult(ticket,[zone('r1','HP')],{document_revision:ticket.document_revision+5}));
  assert.equal(trialFreshness(foreign,'r1'),'historical');
  // A changed App OCR configuration makes it stale.
  const reconfigured=applyView(fresh,{...fresh.view,configuration_revision:'cfg-2'});
  assert.equal(trialFreshness(reconfigured,'r1'),'stale');
});

test('preview edits apply only to the snapshot they were made on',()=>{
  let state=sync(create(create(loaded(),edges(10,10,50,50),'a'),edges(60,10,90,50),'b'));
  const snapshot=previewSnapshot(state,'en',true,null);
  const message=edit=>({token:owner.token,frameId:snapshot.frame.id,basisRevision:snapshot.basisRevision,edit});
  const target=snapshot.definitions[0];
  const moved=regionFromEdges(edges(20,10,60,50),state.document.basis);
  // A main-window rename of another definition does not block the relayed move.
  state=renameDefinition(state,'r2','renamed');
  state=applyPreviewEdit(state,message({kind:'region',id:'r1',part:'region',revision:target.revision,region:moved}),n=>`Region ${n}`);
  assert.deepEqual(mapRegion(state.document.definitions[0].region,state.document.basis),edges(20,10,60,50));
  assert.equal(state.document.definitions[1].name,'renamed');
  // A second edit made on the same, now older, snapshot is refused.
  const again=applyPreviewEdit(state,message({kind:'region',id:'r1',part:'region',revision:target.revision,region:regionFromEdges(edges(30,10,70,50),state.document.basis)}),n=>`Region ${n}`);
  assert.equal(again.notice,'staleEdit');
  assert.equal(again.document,state.document);
  assert.equal(applyPreviewEdit(state,message({kind:'undo',localRevision:snapshot.localRevision}),n=>`Region ${n}`).notice,'staleEdit');
  // Geometry drawn before the content basis changed never applies to the new basis.
  const recropped=setContent(state,{x:0,y:100,width:1920,height:880});
  const late=applyPreviewEdit(recropped,{...message({kind:'create',region:moved}),basisRevision:snapshot.basisRevision},n=>`Region ${n}`);
  assert.equal(late.notice,'staleEdit');
  assert.equal(late.document.definitions.length,2);
  const created=applyPreviewEdit(state,{...message({kind:'create',region:moved}),basisRevision:state.basis},n=>`Region ${n}`);
  assert.equal(created.document.definitions.at(-1).name,'Region 3');
});

test('Save keeps a crop chosen again or changed while the save was pending',()=>{
  let state=sync(toggleCrop(toggleCrop(create(create(loaded(),edges(10,10,50,50)),edges(60,10,90,50)),'r1'),'r2'));
  const ticket=saveTicket(state);
  assert.deepEqual(ticket.crop_ids,['r1','r2']);
  state=toggleCrop(toggleCrop(state,'r1'),'r1');
  state=applySave(state,ticket,null);
  assert.deepEqual(state.cropIds,['r1']);
});

test('Discard to a package without metadata keeps the frame and stays one Undo away',()=>{
  let state=sync(create(loaded(),edges(10,10,50,50)));
  const discarded=applyDiscard(state,{...state.view,document:null,document_revision:state.view.document_revision+1,frame:{...state.view.frame,confirmed:false}});
  assert.deepEqual(discarded.document.definitions,[]);
  assert.deepEqual(discarded.document.basis,state.document.basis);
  assert.equal(discarded.view.frame.id,'f1');
  assert.deepEqual(undoRecognition(discarded).document.definitions.map(item=>item.id),['r1']);
});

test('Discard restores exact saved metadata after a differently sized image, then a new image can be confirmed and edited',()=>{
  let state=setContent(loaded(),{x:0,y:140,width:1920,height:800});
  state=setExpected(create(state,edges(100,200,300,260),'saved zone'),'Script query text');
  state=confirm(sync(state));
  const saved=state.document;
  state=applyView(state,{...state.view,saved_document:saved});
  const replacement={...saved,basis:{frame_width:1280,frame_height:720,content:{x:0,y:0,width:1280,height:720}}};
  state=applyView(state,{...state.view,document:replacement,document_revision:state.view.document_revision+1,
    frame:{id:'f2',width:1280,height:720,revision:2,confirmed:false}});
  state=sync(renameDefinition(state,'r1','unsaved rename'));
  state=create(state,edges(10,10,50,50),'unsent zone');

  const discarded=applyDiscard(state,{...state.view,document:saved,saved_document:saved,
    document_revision:state.view.document_revision+1,frame:null});
  assert.deepEqual(discarded.document,saved);
  assert.equal(discarded.view.frame,null);
  assert.equal(syncTicket(discarded),null,'Discard must not create another draft from the discarded image dimensions');
  assert.equal(copyBlock(discarded,'r1','ocr_recognize'),'noFrame');
  assert.equal(copyTicket(discarded,'r1','ocr_recognize'),null);

  state=applyView(discarded,{...discarded.view,document:replacement,document_revision:discarded.view.document_revision+1,
    frame:{id:'f3',width:1280,height:720,revision:3,confirmed:false}});
  assert.equal(copyBlock(state,'r1','ocr_recognize'),'unconfirmed');
  state=confirm(sync(state));
  state=sync(create(state,edges(20,30,80,90),'after discard'));
  assert.equal(geometryConfirmed(state),true);
  assert.deepEqual(state.document.definitions.map(item=>item.name),['saved zone','after discard']);
  assert.deepEqual(mapRegion(state.document.definitions.find(item=>item.id===state.selected).region,state.document.basis),edges(20,30,80,90));
  assert.equal(copyTicket(state,state.selected,'ocr_recognize').definition_id,state.selected);
  assert.deepEqual(state.view.saved_document,saved);
});

test('reopened saved metadata cannot issue Copy until a loaded frame is confirmed',()=>{
  const saved=sync(setExpected(create(loaded(),edges(100,100,200,200)),'r1','Script query text')).document;
  let state=openRecognition(hostView({document:saved,saved_document:saved}));
  assert.deepEqual(state.document,saved);
  assert.equal(copyBlock(state,'r1','ocr_recognize'),'noFrame');
  assert.equal(copyTicket(state,'r1','ocr_recognize'),null);
  assert.equal(copyBlock(state,'r1','ocr_wait'),'noFrame');
  assert.equal(copyTicket(state,'r1','ocr_wait'),null);

  state=applyView(state,{...state.view,frame:{...FRAME,id:'reopened-frame',confirmed:false}});
  assert.equal(copyBlock(state,'r1','ocr_recognize'),'unconfirmed');
  assert.equal(copyTicket(state,'r1','ocr_recognize'),null);
  state=confirm(state);
  assert.equal(copyBlock(state,'r1','ocr_recognize'),null);
  assert.equal(copyTicket(state,'r1','ocr_recognize').kind,'ocr_recognize');
  assert.equal(copyBlock(state,'r1','ocr_wait'),null);
  assert.equal(copyTicket(state,'r1','ocr_wait').kind,'ocr_wait');
});

test('Copy is recorded against its basis and becomes obsolete after a geometry edit, even when undone',()=>{
  let state=sync(create(loaded(),edges(10,10,50,50)));
  const ticket=copyTicket(state,'r1','ocr_recognize');
  state=applyCopy(state,ticket,{source:'x',basis:state.document.basis,verified:false,document_revision:state.view.document_revision},null);
  assert.equal(copyFreshness(state,'r1'),'current');
  const moved=setRegion(state,'r1','region',regionFromEdges(edges(11,10,51,50),state.document.basis));
  assert.equal(copyFreshness(moved,'r1'),'obsolete');
  assert.equal(copyFreshness(undoRecognition(moved),'r1'),'obsolete');
  const late=applyCopy(moved,ticket,{source:'x',basis:state.document.basis,verified:false,document_revision:state.view.document_revision},null);
  assert.equal(copyFreshness(late,'r1'),'obsolete','a completed native Copy remains published, not failed, after a concurrent edit');
  assert.equal(copyBlock(state,'r1','ocr_wait'),'waitText');
  const failed=applyCopy(state,ticket,null,{category:'Clipboard',message:'denied',context:null});
  assert.equal(copyFreshness(failed,'r1'),'failed');
});
