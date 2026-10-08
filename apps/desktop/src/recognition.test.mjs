import {test} from 'node:test';
import assert from 'node:assert/strict';
import {
  MAX_DEFINITIONS, UNDO_BYTES, UNDO_ENTRIES, applyConfirm, applyCopy, applyDiscard, applyPreviewEdit, applySave, applySync, applyTrial, applyView,
  clientToFrame, confirmTicket, copyBlock, copyFreshness, copyTicket, createDefinition, deleteDefinition, displayScale, dragEdges, geometryConfirmed,
  hitHandle, hitRegion, mapRegion, openRecognition, previewSnapshot, recognitionDirty, regionFromEdges, renameDefinition, saveBlock, saveTicket,
  selectDefinition, setContent, setDisplay, setExpected, setKind, setRegion, setRights, spanEdges, syncTicket, toggleCrop, toggleTrial, trialBlock,
  trialFreshness, trialTicket, undoRecognition, captureDocuments, aggregateDefinitions, canUndoRecognition, discardPixelCrops, canReleaseImage, rebaseFrame, nativePrimaryAction,
} from './recognition.ts';
import {PixelInspection, clientToPixel, pixelCenter, rgbHex} from './recognitionInspection.ts';
import {PreviewImageLoad, previewActionAvailable} from './recognitionPreviewLoad.ts';

const MIB=1_048_576;
const owner={workspace:{workspace_id:'a',revision:1},token:'lease-1'};
const policy={input_bytes:32*MIB,input_pixels:16_777_216,crop_bytes:16*MIB,crop_pixels:4_194_304,package_image_bytes:64*MIB,package_non_image_bytes:MIB,
  package_bytes:65*MIB,package_decoded_bytes:128*MIB,replay_decoded_bytes:128*MIB,payload_bytes:512*MIB,preview_pixels:4_194_304};
const FRAME={id:'f1',width:1920,height:1080,revision:1,confirmed:true};
const CAPTURE_A='00000000000000000000';
const CAPTURE_B='0000000000000000000g';
function hostView(overrides={}){
  const view={owner,revision:'rev-1',document:null,saved_document:null,document_revision:1,basis_confirmed:false,frame:null,
    capture_id:null,captures:[],saved_captures:[],migration_required:false,other_bases_confirmed:true,
    staged_crop_ids:[],stale_crop_ids:[],staged_crop_sources:{},
    capabilities:{max_ocr_zones:8,diagnostic_regions:256,diagnostic_bytes:262144,expected_bytes:4096,image_policy:policy},
    configuration_revision:'cfg-1',trial:null,...overrides};
  if (view.document && !Object.hasOwn(overrides,'capture_id')) view.capture_id=CAPTURE_A;
  if (view.document && !Object.hasOwn(overrides,'captures')) view.captures=[{capture_id:view.capture_id,document:view.document}];
  if (view.saved_document && !Object.hasOwn(overrides,'saved_captures')) view.saved_captures=[{capture_id:view.capture_id,document:view.saved_document}];
  return view;
}
const edges=(left,top,right,bottom)=>({left,top,right,bottom});
// The host accepts exactly the local draft, as recognition_update does; a changed basis needs confirmation again.
function sync(state){
  const ticket=syncTicket(state);
  if (!ticket) return state;
  const basisChanged=JSON.stringify(ticket.document.basis)!==JSON.stringify(state.view.document?.basis);
  const frame=state.view.frame&&{...state.view.frame,confirmed:state.view.frame.confirmed&&!basisChanged};
  return applySync(state,ticket,{...state.view,document:ticket.document,captures:captureDocuments(state),document_revision:state.view.document_revision+1,
    basis_confirmed:state.view.basis_confirmed&&!basisChanged,frame});
}
function confirm(state){
  const ticket=confirmTicket(state);
  assert.ok(ticket,'geometry should be confirmable');
  return applyConfirm(state,ticket,{...state.view,basis_confirmed:true,frame:{...state.view.frame,confirmed:true}});
}
// A confirmed frame whose default full-image document the host already holds.
function loaded(frame=FRAME){
  const document={version:1,rounding:1,basis:{frame_width:frame.width,frame_height:frame.height,
    content:{x:0,y:0,width:frame.width,height:frame.height}},definitions:[],template_rights:null};
  const state=openRecognition(hostView({frame:{...frame,confirmed:false},document}));
  return confirm(sync(state));
}
function savedView(state,document=state.document,overrides={}){
  const captures=captureDocuments(state,document);
  return {...state.view,document,saved_document:document,captures,saved_captures:structuredClone(captures),migration_required:false,...overrides};
}
function create(state,box,name='zone'){
  return createDefinition(state,regionFromEdges(box,state.document.basis),name);
}
// Host capture selection/New capture reports all drafts, but staged originals only for the active capture.
function captureView(state,capture_id,document,overrides={}){
  const captures=captureDocuments(state);
  const existing=captures.some(capture=>capture.capture_id===capture_id);
  return {...state.view,capture_id,document,
    captures:existing?captures.map(capture=>capture.capture_id===capture_id?{capture_id,document}:capture):[...captures,{capture_id,document}],
    saved_document:state.view.saved_captures.find(capture=>capture.capture_id===capture_id)?.document??null,
    document_revision:state.view.document_revision+1,frame:null,staged_crop_ids:[],stale_crop_ids:[],staged_crop_sources:{},...overrides};
}
function stagedOriginal(){
  const state=toggleCrop(sync(create(loaded(),edges(10,10,50,50))),'r1');
  return applyView(state,{...state.view,staged_crop_ids:['r1'],staged_crop_sources:{r1:'f1'},frame:{...FRAME,id:'f2',revision:2}});
}
function trialResult(ticket,zones,overrides={}){
  const envelope={version:1,operation:'recognition_trial',environment_identity:'env',primary:null,cleanup:{clean:true},child_reaped:true,forced:false,exit_code:0,
    result:{kind:'ocr',text_contract:'facade',zones}};
  return {owner,capture_id:ticket.capture_id,revision:ticket.revision,document_revision:ticket.document_revision,frame_id:ticket.frame_id,frame_revision:1,configuration_revision:'cfg-1',
    sample_id:ticket.sample_id,stale:false,
    controller:{run:'run-1',state:'finished',operation:'recognition_trial',result:envelope,error:null,progress:[],dropped_logs:0,workspace_id:'a',workspace_revision:1},
    ...overrides};
}
const zone=(id,text)=>({id,outcome:text===null?'no_match':'recognized',regions:text===null?[]:[{text,confidence:0.9,bounds:{x:1,y:2,width:3,height:4},geometry:[]}]});

test('native capture action follows saved-target and held Engine state without inventing authority',()=>{
  assert.equal(nativePrimaryAction(null),'select');
  const unselected={status:'unselected',selected_id:null,has_saved_target:true};
  assert.equal(nativePrimaryAction(unselected),'start');
  assert.equal(nativePrimaryAction({...unselected,status:'discovering'}),'start');
  const selected={...unselected,status:'selected',selected_id:'candidate-1'};
  assert.equal(nativePrimaryAction(selected),'capture');
  assert.equal(nativePrimaryAction({...selected,status:'capturing'}),'start');
  assert.equal(nativePrimaryAction({...selected,status:'selected',selected_id:null}),'start');
  assert.equal(nativePrimaryAction({...unselected,has_saved_target:false}),'select');
});

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

test('a selected template search area grabs only its edges, so regions and empty content inside it stay reachable',()=>{
  let state=setContent(loaded(),{x:0,y:140,width:1920,height:800});
  state=create(create(state,edges(100,200,200,260),'A'),edges(400,300,500,400),'C');
  state=setKind(selectDefinition(state,'r1'),'r1','template');
  const snapshot=previewSnapshot(state,'en',true,null);
  assert.equal(snapshot.selected,'r1');
  const shown=(definition,part)=>mapRegion(part==='region'?definition.region:definition.search,snapshot.basis);
  assert.deepEqual(shown(snapshot.definitions[0],'search'),edges(0,140,1920,940),'a new search area is the whole content');
  const hit=(x,y,tolerance)=>{
    const found=hitRegion(snapshot.definitions,snapshot.selected,{x,y},tolerance,shown);
    return found&&[found.definition.id,found.part,found.handle];
  };
  // The same 6 CSS pixel grab radius at Fit (0.25 CSS px per frame pixel) and at 200%.
  for (const tolerance of [24,3]) {
    assert.deepEqual(hit(450,350,tolerance),['r2','region','move'],'another region inside the search area is selectable');
    assert.equal(hit(1000,600,tolerance),null,'empty content inside the search area creates a region');
    assert.deepEqual(hit(150,230,tolerance),['r1','region','move'],'the pattern stays movable');
    assert.deepEqual(hit(1919,600,tolerance),['r1','search','e']);
    assert.deepEqual(hit(1,141,tolerance),['r1','search','nw'],'edges follow the content offset');
  }
  assert.deepEqual(hit(1900,600,24),['r1','search','e']);
  assert.equal(hit(1900,600,3),null);
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
  assert.equal(copyBlock(state,state.trialIds,'ocr_recognize'),'overLimit','grouped Copy is one request under the same engine bound');
  state=toggleTrial(state,'r9');
  const ticket=trialTicket(state,'frame',state.trialIds);
  assert.deepEqual(ticket.selected_ids,['r1','r2','r3','r4','r5','r6','r7','r8']);
  assert.deepEqual(copyTicket(state,state.trialIds,'ocr_recognize').definition_ids,ticket.selected_ids);
  // Without the engine's reported bound there is no fallback number.
  const unknown={...state,view:{...state.view,capabilities:{...state.view.capabilities,max_ocr_zones:null}}};
  assert.equal(trialBlock(unknown,'frame',['r1']),'noCapability');
});

test('refresh retains the Game content ROI, unsaved definitions and selected original crops; resized frames require explicit rebase',()=>{
  let state=create(loaded(),edges(100,100,200,200));
  state=setContent(state,{x:0,y:60,width:1920,height:960});
  state=confirm(sync(state));
  state=applyView(state,savedView(state));
  state=sync(create(state,edges(400,200,500,300),'unsaved region'));
  state=selectDefinition(state,'r1');
  const ticket=trialTicket(state,'frame',['r1']);
  state=applyTrial(state,ticket,trialResult(ticket,[zone('r1','scene A')]));
  const copied=copyTicket(state,[],'game_content');
  state=applyCopy(state,copied,{capture_id:state.view.capture_id,source:'x',basis:state.document.basis,verified:false,document_revision:state.view.document_revision,definition_ids:[]},null);
  const definitions=state.document.definitions;
  state=toggleCrop(state,'r1');
  const region=state.document.definitions[0].region;
  state=applyView(state,{...state.view,staged_crop_ids:['r1'],staged_crop_sources:{r1:'f1'},frame:{id:'f2',width:1920,height:1080,revision:2,confirmed:true}});
  assert.deepEqual(state.document.definitions,definitions);
  assert.deepEqual(state.document.basis.content,{x:0,y:60,width:1920,height:960});
  assert.deepEqual(state.cropIds,['r1']);
  assert.equal(state.cropSources.r1,'f1');
  assert.deepEqual(saveTicket(state).crop_sources,{r1:'f1'});
  assert.equal(trialFreshness(state,'r1'),'stale');
  assert.equal(copyFreshness(state,'game_content'),'current','unchanged Game content setup remains reusable');
  // The previous basis and selected original are retained even when the new frame is differently sized.
  state=applyView(state,{...state.view,basis_confirmed:false,frame:{id:'f3',width:1280,height:720,revision:3,confirmed:false}});
  assert.deepEqual(state.document.basis,{frame_width:1920,frame_height:1080,content:{x:0,y:60,width:1920,height:960}});
  assert.equal(state.cropSources.r1,'f1');
  assert.equal(previewSnapshot(state,'en',true,null).frameGeometryReady,false);
  assert.equal(trialBlock(state,'frame',['r1']),'unconfirmed');
  state=rebaseFrame(state);
  assert.deepEqual(state.document.basis,{frame_width:1280,frame_height:720,content:{x:0,y:0,width:1280,height:720}});
  assert.deepEqual(state.document.definitions[0].region,region);
  assert.deepEqual(state.cropIds,['r1'],'explicit rebase keeps pending intent so invalid original must be resolved visibly');
  assert.equal(previewSnapshot(state,'en',true,null).frameGeometryReady,true);
});
test('a reselected crop uses the current confirmed frame instead of a stale staged original',()=>{
  let state=sync(create(loaded(),edges(10,10,50,50)));
  state=toggleCrop(state,'r1');
  state=applyView(state,{...state.view,staged_crop_ids:['r1'],staged_crop_sources:{r1:'f1'},
    frame:{id:'f2',width:1920,height:1080,revision:2,confirmed:true}});
  assert.equal(saveTicket(state).crop_sources.r1,'f1');
  state=toggleCrop(toggleCrop(state,'r1'),'r1');
  assert.equal(state.cropSources.r1,'f2');
  assert.equal(saveTicket(state).crop_sources.r1,'f2');
  state=applyView(state,{...state.view,stale_crop_ids:['r1'],frame:{id:'f3',width:1920,height:1080,revision:3,confirmed:true}});
  assert.equal(saveBlock(state),'cropFrame','stale stage cannot silently recrop from a later frame');
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
  assert.notEqual(trialFreshness(edited,'r1'),'fresh');
  const restored=sync(undoRecognition(edited));
  assert.deepEqual(restored.document.definitions[0].region,state.document.definitions[0].region);
  assert.notEqual(trialFreshness(restored,'r1'),'fresh');
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
  const message=edit=>({token:owner.token,capture_id:snapshot.capture_id,revision:snapshot.revision,frameId:snapshot.frame.id,basisRevision:snapshot.basisRevision,edit});
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
  const refreshed=applyView(state,{...state.view,frame:{...FRAME,id:'next-frame',revision:2,confirmed:true}});
  const staleFrame=applyPreviewEdit(refreshed,message({kind:'create',region:moved}),n=>`Region ${n}`);
  assert.equal(staleFrame.notice,'staleEdit','a delayed edit from the prior frame cannot appear on the new image');
  assert.equal(staleFrame.document.definitions.length,2);
});

test('Save keeps a crop chosen again or changed while the save was pending',()=>{
  let state=sync(toggleCrop(toggleCrop(create(create(loaded(),edges(10,10,50,50)),edges(60,10,90,50)),'r1'),'r2'));
  const ticket=saveTicket(state);
  assert.deepEqual(ticket.crop_ids,['r1','r2']);
  state=toggleCrop(toggleCrop(state,'r1'),'r1');
  state=applySave(state,ticket,null);
  assert.deepEqual(state.cropIds,['r1']);
});

test('Delete and Undo recover the selected F1 original while F2 is active, including a synchronized deletion',()=>{
  let state=stagedOriginal();
  const originalMark=state.cropMarks.r1;
  state=sync(deleteDefinition(state,'r1'));
  state=applyView(state,{...state.view,staged_crop_ids:[],stale_crop_ids:['r1']});
  state=undoRecognition(state);
  assert.equal(state.view.frame.id,'f2');
  assert.equal(state.selected,'r1');
  assert.deepEqual(state.cropIds,['r1']);
  assert.equal(state.cropMarks.r1,originalMark);
  assert.deepEqual(state.cropSources,{r1:'f1'});
  state=sync(state);
  state=applyView(state,{...state.view,staged_crop_ids:['r1'],stale_crop_ids:[]});
  assert.deepEqual(saveTicket(state).crop_sources,{r1:'f1'});

  const current=toggleCrop(sync(create(loaded(),edges(10,10,50,50))),'r1');
  assert.deepEqual(saveTicket(sync(undoRecognition(deleteDefinition(current,'r1')))).crop_sources,{r1:'f1'},
    'an unstaged original is recoverable while its live frame is still held');
});

test('Undo never substitutes a missing or wrong-frame stage for a deleted original',()=>{
  const deleted=deleteDefinition(stagedOriginal(),'r1');
  const missing=applyView(deleted,{...deleted.view,staged_crop_ids:[],staged_crop_sources:{}});
  const restored=undoRecognition(missing);
  assert.equal(restored.document.definitions[0].id,'r1');
  assert.deepEqual(restored.cropIds,[]);
  assert.deepEqual(restored.cropSources,{});
  const wrong=applyView(deleted,{...deleted.view,staged_crop_sources:{r1:'f3'}});
  assert.deepEqual(undoRecognition(wrong).cropIds,[],'a retained stage from another frame is not the deleted choice');
});

test('a Save reply invalidates deleted crop Undo even when its recognition follow-up read fails',()=>{
  const state=stagedOriginal();
  const ticket=saveTicket(state);
  const deleted=deleteDefinition(state,'r1');
  const restored=undoRecognition(applySave(deleted,ticket,null));
  assert.equal(restored.document.definitions[0].id,'r1');
  assert.deepEqual(restored.cropIds,[]);
  assert.deepEqual(restored.cropSources,{},'the old host view must not authorize released F1 pixels');

  const saved={...ticket.document,definitions:ticket.document.definitions.map(item=>({...item,
    saved:{asset:`captures/${CAPTURE_A}/r1.png`,sha256:'a'.repeat(64),width:40,height:40}}))};
  const committed=applySave(deleted,ticket,savedView(state,saved,{revision:'rev-2',
    document_revision:state.view.document_revision+1,staged_crop_ids:[],staged_crop_sources:{}}));
  const afterUndo=undoRecognition(committed);
  assert.equal(afterUndo.document.definitions[0].saved.sha256,'a'.repeat(64));
  assert.deepEqual(afterUndo.cropIds,[]);
  assert.deepEqual(afterUndo.cropSources,{});
});

test('a committed Save cannot lend stale staging metadata to a later Delete and Undo',()=>{
  const state=stagedOriginal();
  const ticket=saveTicket(state);
  const edited=setRegion(state,'r1','region',regionFromEdges(edges(20,10,60,50),state.document.basis));
  const committed=applySave(edited,ticket,null);
  assert.deepEqual(committed.cropIds,['r1'],'newer crop intent remains explicit after the commit');
  assert.equal(saveBlock(committed),'cropFrame','the consumed F1 original is no longer saveable');
  const restored=undoRecognition(deleteDefinition(committed,'r1'));
  assert.deepEqual(restored.document.definitions[0].region,edited.document.definitions[0].region);
  assert.deepEqual(restored.cropIds,[]);
  assert.deepEqual(restored.cropSources,{});
});

test('metadata-only Save and explicit pixel discard invalidate hidden deleted-crop Undo',()=>{
  const deleted=sync(deleteDefinition(stagedOriginal(),'r1'));
  const ticket=saveTicket(deleted);
  assert.deepEqual(ticket.crop_ids,[]);
  const saved=applySave(deleted,ticket,savedView(deleted,deleted.document,{revision:'rev-2',
    document_revision:deleted.view.document_revision+1,staged_crop_ids:[],staged_crop_sources:{}}));
  const restored=undoRecognition(saved);
  assert.equal(restored.document.definitions[0].id,'r1');
  assert.deepEqual(restored.cropSources,{});
  assert.deepEqual(restored.cropIds,[],'Save releases every stage owned by this capture, not just submitted ids');
  const discarded=undoRecognition(discardPixelCrops(deleted));
  assert.equal(discarded.document.definitions[0].id,'r1');
  assert.deepEqual(discarded.cropIds,[],'acknowledged pixel discard cannot be undone into abandoned pixels');
  assert.deepEqual(discarded.cropSources,{});
});

test('Discard removes obsolete pending captures and authoring can create and save crops again',()=>{
  let state=stagedOriginal();
  const empty=loaded().document;
  state=applyView(state,captureView(state,CAPTURE_B,empty,{frame:{...FRAME,id:'f3',revision:3},basis_confirmed:true}));
  state=toggleCrop(sync(create(state,edges(60,10,90,50))),'r1');
  state=applyDiscard(state,hostView({document_revision:state.view.document_revision+1}));
  assert.equal(recognitionDirty(state),false);
  assert.deepEqual(state.cropIds,[]);
  assert.deepEqual(state.cropMarks,{});
  assert.deepEqual(state.cropSources,{});
  assert.deepEqual(state.pendingCrops,{});
  assert.equal(canUndoRecognition(state),false);

  const captureId='00000000000000000100';
  state=applyView(state,captureView(state,captureId,empty,{frame:{...FRAME,id:'f4',revision:4},basis_confirmed:true}));
  state=toggleCrop(sync(create(state,edges(100,10,150,50))),'r1');
  assert.equal(saveBlock(state),null);
  const ticket=saveTicket(state);
  assert.deepEqual(ticket.crop_sources,{r1:'f4'});
  state=applySave(state,ticket,savedView(state,state.document,{revision:'rev-2',document_revision:state.view.document_revision+1}));
  assert.equal(recognitionDirty(state),false);
});

test('Discard keeps newer metadata on a retained capture but clears all pixel intent and deleted-crop Undo',()=>{
  let state=sync(create(create(loaded(),edges(10,10,50,50)),edges(60,10,90,50)));
  state=applyView(state,savedView(state));
  const saved=state.document;
  state=toggleCrop(toggleCrop(state,'r1'),'r2');
  const savedCaptures=[...state.view.saved_captures,{capture_id:CAPTURE_B,document:saved}];
  state=applyView(state,captureView(state,CAPTURE_B,saved,{saved_captures:savedCaptures,saved_document:saved,
    frame:{...FRAME,id:'f2',revision:2},basis_confirmed:true}));
  state=toggleCrop(toggleCrop(state,'r1'),'r2');
  state=applyView(state,{...state.view,staged_crop_ids:['r1','r2'],staged_crop_sources:{r1:'f2',r2:'f2'},
    frame:{...FRAME,id:'f3',revision:3}});
  state=sync(deleteDefinition(state,'r1'));
  const sent=state;
  state=renameDefinition(state,'r2','newer metadata');
  const reply={...state.view,document:saved,captures:state.view.saved_captures,frame:null,
    document_revision:state.view.document_revision+1,staged_crop_ids:[],stale_crop_ids:[],staged_crop_sources:{}};
  assert.equal(applyDiscard(state,{...reply,owner:{...owner,token:'other'}},sent),state);
  assert.equal(applyDiscard(state,{...reply,document_revision:sent.view.document_revision-1},sent),state);
  assert.deepEqual(state.pendingCrops[CAPTURE_A].sources,{r1:'f1',r2:'f1'});
  state=applyDiscard(state,reply,sent);
  assert.equal(state.document.definitions[0].name,'newer metadata');
  assert.deepEqual(state.cropIds,[]);
  assert.deepEqual(state.cropMarks,{});
  assert.deepEqual(state.cropSources,{});
  assert.deepEqual(state.pendingCrops,{});
  assert.equal(recognitionDirty(state),true,'the later rename is still an unsaved metadata edit');
  state=undoRecognition(undoRecognition(state));
  assert.deepEqual(state.document.definitions.map(item=>item.id),['r1','r2']);
  assert.deepEqual(state.cropIds,[]);
  assert.deepEqual(state.cropSources,{},'metadata Undo cannot restore any globally discarded original');
});

test('saving B then A preserves both original sources without reviving the saved B choice',()=>{
  let state=toggleCrop(sync(create(loaded(),edges(10,10,50,50))),'r1');
  state=applyView(state,captureView(state,CAPTURE_B,loaded().document,{frame:{...FRAME,id:'f2',revision:2},basis_confirmed:true}));
  state=toggleCrop(sync(create(state,edges(60,10,90,50))),'r1');
  state=applyView(state,{...state.view,staged_crop_ids:['r1'],staged_crop_sources:{r1:'f2'},
    frame:{...FRAME,id:'f3',revision:3}});
  assert.equal(saveBlock(state),null);
  const bTicket=saveTicket(state);
  assert.deepEqual(bTicket.crop_sources,{r1:'f2'});
  const b={...bTicket.document,definitions:bTicket.document.definitions.map(item=>({...item,
    saved:{asset:`captures/${CAPTURE_B}/r1.png`,sha256:'b'.repeat(64),width:30,height:40}}))};
  state=applySave(state,bTicket,savedView(state,b,{revision:'rev-2',document_revision:state.view.document_revision+1,
    staged_crop_ids:[],staged_crop_sources:{}}));
  assert.deepEqual(state.cropIds,[]);
  assert.deepEqual(state.cropSources,{});
  assert.deepEqual(state.pendingCrops[CAPTURE_A].sources,{r1:'f1'});
  assert.equal(recognitionDirty(state),true);
  assert.equal(saveBlock(state),'otherCapture','pending pixels on the inactive capture are a distinct reason, not "nothing to save"');
  const a=state.view.captures.find(capture=>capture.capture_id===CAPTURE_A).document;
  state=applyView(state,captureView(state,CAPTURE_A,a,{staged_crop_ids:['r1'],staged_crop_sources:{r1:'f1'}}));
  const aTicket=saveTicket(state);
  assert.deepEqual(aTicket.crop_sources,{r1:'f1'});
  const savedA={...a,definitions:a.definitions.map(item=>({...item,
    saved:{asset:`captures/${CAPTURE_A}/r1.png`,sha256:'a'.repeat(64),width:40,height:40}}))};
  state=applySave(state,aTicket,savedView(state,savedA,{revision:'rev-3',document_revision:state.view.document_revision+1,
    staged_crop_ids:[],staged_crop_sources:{}}));
  assert.equal(recognitionDirty(state),false);
  assert.equal(saveBlock(state),'noChanges','once every capture is saved nothing remains');
  state=applyView(state,captureView(state,CAPTURE_B,b));
  assert.deepEqual(state.cropIds,[]);
  assert.deepEqual(state.cropMarks,{});
  assert.deepEqual(state.cropSources,{});
  assert.deepEqual(state.pendingCrops,{});
  assert.equal(recognitionDirty(state),false);
});

test('saving another capture retains deleted-crop Undo for its still-staged original',()=>{
  let state=sync(deleteDefinition(stagedOriginal(),'r1'));
  const a=state.document;
  state=applyView(state,captureView(state,CAPTURE_B,loaded().document,{frame:{...FRAME,id:'f3',revision:3},basis_confirmed:true}));
  state=toggleCrop(sync(create(state,edges(60,10,90,50))),'r1');
  const ticket=saveTicket(state);
  state=applySave(state,ticket,savedView(state,state.document,{revision:'rev-2',document_revision:state.view.document_revision+1}));
  state=applyView(state,captureView(state,CAPTURE_A,a,{stale_crop_ids:['r1'],staged_crop_sources:{r1:'f1'}}));
  state=undoRecognition(state);
  assert.deepEqual(state.cropIds,['r1']);
  assert.deepEqual(state.cropSources,{r1:'f1'});
  state=sync(state);
  state=applyView(state,{...state.view,staged_crop_ids:['r1'],stale_crop_ids:[]});
  assert.deepEqual(saveTicket(state).crop_sources,{r1:'f1'});
});

test('a host view removing an inactive capture prunes its pending crops and Undo history',()=>{
  let state=stagedOriginal();
  const b=loaded().document;
  state=applyView(state,captureView(state,CAPTURE_B,b,{basis_confirmed:true}));
  const captures=[{capture_id:CAPTURE_B,document:b}];
  state=applyView(state,{...state.view,captures,saved_captures:captures,saved_document:b,document_revision:state.view.document_revision+1});
  assert.deepEqual(state.pendingCrops,{});
  assert.equal(recognitionDirty(state),false);
  assert.equal(state.undo.some(entry=>entry.capture_id===CAPTURE_A),false);
  assert.equal(state.undoBytes,0);
});

test('discarding an unsaved capture releases its image and does not offer cross-capture Undo',()=>{
  const state=sync(create(loaded(),edges(10,10,50,50)));
  const discarded=applyDiscard(state,hostView({document_revision:state.view.document_revision+1}));
  assert.equal(discarded.document,null);
  assert.equal(discarded.view.frame,null);
  assert.equal(canUndoRecognition(discarded),false);
  assert.equal(undoRecognition(discarded),discarded);
});

test('Discard restores exact saved metadata after a differently sized image, then a new image can be confirmed and edited',()=>{
  let state=setContent(loaded(),{x:0,y:140,width:1920,height:800});
  state=setExpected(create(state,edges(100,200,300,260),'saved zone'),'Script query text');
  state=confirm(sync(state));
  const saved=state.document;
  state=applyView(state,savedView(state,saved));
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
  assert.equal(copyBlock(discarded, ['r1'], 'ocr_recognize'),'noFrame');
  assert.equal(copyTicket(discarded, ['r1'], 'ocr_recognize'),null);

  state=applyView(discarded,{...discarded.view,document:replacement,document_revision:discarded.view.document_revision+1,
    frame:{id:'f3',width:1280,height:720,revision:3,confirmed:false}});
  assert.equal(copyBlock(state, ['r1'], 'ocr_recognize'),'unconfirmed');
  state=confirm(sync(state));
  state=sync(create(state,edges(20,30,80,90),'after discard'));
  assert.equal(geometryConfirmed(state),true);
  assert.deepEqual(state.document.definitions.map(item=>item.name),['saved zone','after discard']);
  assert.deepEqual(mapRegion(state.document.definitions.find(item=>item.id===state.selected).region,state.document.basis),edges(20,30,80,90));
  assert.deepEqual(copyTicket(toggleTrial(state,state.selected),[state.selected],'ocr_recognize').definition_ids,[state.selected]);
  assert.deepEqual(state.view.saved_document,saved);
});

test('reopened metadata requires a frame, then reuses saved content on a compatible image',()=>{
  const saved=sync(setExpected(create(loaded(),edges(100,100,200,200)),'r1','Script reference text')).document;
  let state=openRecognition(hostView({document:saved,saved_document:saved}));
  assert.deepEqual(state.document,saved);
  state=toggleTrial(state,'r1');
  assert.equal(copyBlock(state, ['r1'], 'ocr_recognize'),'noFrame');
  assert.equal(copyTicket(state, ['r1'], 'ocr_recognize'),null);
  assert.equal(copyBlock(state, [], 'game_content'),'noFrame');
  assert.equal(copyTicket(state, [], 'game_content'),null);

  state=applyView(state,{...state.view,frame:{...FRAME,id:'reopened-frame',confirmed:true}});
  assert.equal(geometryConfirmed(state),true);
  assert.equal(state.document.definitions[0].expected,'Script reference text','saved reference text survives unchanged');
  assert.equal(copyBlock(state, ['r1'], 'ocr_recognize'),null);
  assert.equal(copyTicket(state, ['r1'], 'ocr_recognize').kind,'ocr_recognize');
  assert.equal(copyBlock(state, [], 'game_content'),null);
  assert.equal(copyTicket(state, [], 'game_content').kind,'game_content');
});

test('grouped OCR Copy becomes obsolete after a zone edit, even when undone',()=>{
  let state=toggleTrial(sync(create(loaded(),edges(10,10,50,50))),'r1');
  const ticket=copyTicket(state, ['r1'], 'ocr_recognize');
  state=applyCopy(state,ticket,{capture_id:state.view.capture_id,source:'x',basis:state.document.basis,verified:false,document_revision:state.view.document_revision,definition_ids:['r1']},null);
  assert.equal(copyFreshness(state, 'ocr_recognize'),'current');
  const moved=setRegion(state,'r1','region',regionFromEdges(edges(11,10,51,50),state.document.basis));
  assert.equal(copyFreshness(moved, 'ocr_recognize'),'obsolete');
  assert.equal(copyFreshness(undoRecognition(moved), 'ocr_recognize'),'obsolete');
  const late=applyCopy(moved,ticket,{capture_id:state.view.capture_id,source:'x',basis:state.document.basis,verified:false,document_revision:state.view.document_revision,definition_ids:['r1']},null);
  assert.equal(copyFreshness(late, 'ocr_recognize'),'obsolete','a completed native Copy remains published, not failed, after a concurrent edit');
  const failed=applyCopy(state,ticket,null,{category:'Clipboard',message:'denied',context:null});
  assert.equal(copyFreshness(failed, 'ocr_recognize'),'failed');
});

test('saving a diagnostic OCR crop keeps grouped Copy current; zone, name and checked-set changes still obsolete it',()=>{
  let state=toggleTrial(sync(create(loaded(),edges(10,10,50,50),'HP')),'r1');
  const copied=copyTicket(state,['r1'],'ocr_recognize');
  state=applyCopy(state,copied,{capture_id:state.view.capture_id,source:'x',basis:state.document.basis,verified:false,document_revision:state.view.document_revision,definition_ids:['r1']},null);
  state=toggleCrop(state,'r1');
  const ticket=saveTicket(state);
  const saved={...ticket.document,definitions:ticket.document.definitions.map(item=>({...item,saved:{asset:'recognition/r1.png',sha256:'a'.repeat(64),width:40,height:40}}))};
  state=applySave(state,ticket,savedView(state,saved,{revision:'rev-2',document_revision:state.view.document_revision+1}));
  assert.equal(state.document.definitions[0].saved.sha256,'a'.repeat(64));
  assert.equal(recognitionDirty(state),false);
  assert.equal(copyFreshness(state,'ocr_recognize'),'current','grouped source carries no saved crop');
  const moved=setRegion(state,'r1','region',regionFromEdges(edges(11,10,51,50),state.document.basis));
  assert.equal(copyFreshness(moved,'ocr_recognize'),'obsolete');
  assert.equal(copyFreshness(undoRecognition(moved),'ocr_recognize'),'obsolete','Undo does not revive it');
  assert.equal(copyFreshness(renameDefinition(state,'r1','MP'),'ocr_recognize'),'obsolete');
  assert.equal(copyFreshness(toggleTrial(toggleTrial(state,'r1'),'r1'),'ocr_recognize'),'obsolete');
});

test('hand-restored saved metadata is clean again without reusing an old definition revision',()=>{
  let state=sync(create(loaded(),edges(10,10,50,50),'HP'));
  state=applyView(state,savedView(state));
  const savedRevision=state.document.definitions[0].revision;
  const snapshot=previewSnapshot(state,'en',true,null);
  let restored=renameDefinition(state,'r1','HPx');
  assert.equal(recognitionDirty(restored),true);
  restored=renameDefinition(restored,'r1','HP');
  assert.equal(recognitionDirty(restored),false);
  assert.equal(saveBlock(restored),'noChanges');
  assert.ok(restored.document.definitions[0].revision>savedRevision,'revisions stay monotonic');
  const late={token:owner.token,capture_id:snapshot.capture_id,revision:snapshot.revision,frameId:snapshot.frame.id,basisRevision:snapshot.basisRevision,
    edit:{kind:'region',id:'r1',part:'region',revision:snapshot.definitions[0].revision,region:regionFromEdges(edges(20,10,60,50),state.document.basis)}};
  assert.equal(applyPreviewEdit(restored,late,n=>`Region ${n}`).notice,'staleEdit','an edit on the pre-rename snapshot stays fenced');
  assert.equal(recognitionDirty(setKind(setKind(state,'r1','template'),'r1','ocr')),false);
  const typed=setRights(state,{license:'M',created_by:'',created_for:null,reviewed:false});
  assert.equal(recognitionDirty(typed),true);
  const cleared=setRights(typed,{license:'',created_by:'',created_for:null,reviewed:false});
  assert.equal(cleared.document.template_rights,null);
  assert.equal(recognitionDirty(cleared),false);
  assert.deepEqual(setRights(typed,{license:'',created_by:'',created_for:null,reviewed:true}).document.template_rights,
    {license:'',created_by:'',created_for:null,reviewed:true},'a meaningful invalid rights draft is kept');

  // Template Copy follows the host's whole saved-document check.
  let template=sync(setRights(setKind(state,'r1','template'),{license:'CC0',created_by:'author',created_for:null,reviewed:true}));
  const png={asset:'recognition/r1.png',sha256:'b'.repeat(64),width:40,height:40};
  const savedTemplate={...template.document,definitions:template.document.definitions.map(item=>({...item,saved:png}))};
  template=applyView(template,savedView(template,savedTemplate,{revision:'rev-2',document_revision:template.view.document_revision+1}));
  assert.equal(copyBlock(template,['r1'],'template_recognize'),null);
  const reverted=sync(renameDefinition(renameDefinition(template,'r1','HPx'),'r1','HP'));
  assert.equal(copyBlock(reverted,['r1'],'template_recognize'),null);
  assert.ok(copyTicket(reverted,['r1'],'template_recognize'));
  assert.equal(copyBlock(create(template,edges(60,10,90,50),'MP'),['r1'],'template_recognize'),'templateUnsaved');
});

test('grouped OCR Copy captures every checked region independently of Selected definition; setup follows Game content',()=>{
  let state=loaded();
  for (let index=0; index<3; index++) {
    state=create(state,edges(index*100,0,index*100+50,50));
    state=setExpected(state,state.selected,`reference ${index}`);
  }
  state=sync(toggleTrial(toggleTrial(state,'r2'),'r1'));
  assert.equal(state.selected,'r3','the selected region is not checked');
  const ticket=copyTicket(state,state.trialIds,'ocr_recognize');
  assert.deepEqual(ticket.definition_ids,['r1','r2']);
  assert.equal(copyTicket(state,['r3'],'ocr_recognize'),null,'the editor row is not the checked set');
  const result={capture_id:state.view.capture_id,source:'x',basis:state.document.basis,verified:false,document_revision:state.view.document_revision,definition_ids:['r1','r2']};
  state=applyCopy(state,ticket,result,null);
  const setup=copyTicket(state,[],'game_content');
  state=applyCopy(state,setup,{...result,definition_ids:[]},null);
  assert.equal(copyFreshness(state,'ocr_recognize'),'current');
  assert.equal(copyFreshness(selectDefinition(state,null),'ocr_recognize'),'current');
  assert.equal(copyFreshness(setExpected(state,'r3','unchecked edit'),'ocr_recognize'),'current');
  assert.equal(copyFreshness(setExpected(state,'r2','new reference'),'ocr_recognize'),'obsolete','reference text is part of the copied comments');
  // Grouped source reads the pasted recognitionBasis, so only the setup must be copied again.
  const recropped=setContent(state,{x:0,y:60,width:1920,height:960});
  assert.equal(copyFreshness(recropped,'game_content'),'obsolete');
  assert.equal(copyFreshness(recropped,'ocr_recognize'),'current');
  assert.equal(copyFreshness(undoRecognition(recropped),'game_content'),'current','setup source is the basis value itself');
  const changed=toggleTrial(state,'r2');
  assert.equal(copyFreshness(changed,'ocr_recognize'),'obsolete');
  assert.equal(copyFreshness(toggleTrial(changed,'r2'),'ocr_recognize'),'obsolete');
  assert.equal(copyFreshness(applyCopy(changed,ticket,result,null),'ocr_recognize'),'obsolete','late clipboard success keeps the captured set');
  assert.equal(copyTicket(changed,ticket.definition_ids,'ocr_recognize'),null,'selection changed while synchronizing');
  assert.equal(copyFreshness(deleteDefinition(state,'r2'),'ocr_recognize'),'obsolete');
  assert.equal(copyFreshness(changed,'game_content'),'current','checking rows never changes the setup');
});

test('Copy refuses invalid whole requests and unknown or exceeded engine limits, and never needs reference text',()=>{
  let state=sync(setExpected(create(create(loaded(),edges(0,0,50,50)),edges(100,0,150,50)),'r1','first'));
  state=toggleTrial(toggleTrial(state,'r1'),'r2');
  assert.equal(copyBlock(state,[],'ocr_recognize'),'empty');
  assert.equal(copyBlock(state,['r1','r1'],'ocr_recognize'),'invalid');
  assert.equal(copyBlock(state,['r1','missing'],'ocr_recognize'),'invalid');
  assert.equal(copyBlock(state,['r1'],'game_content'),'invalid','setup names no definitions');
  assert.equal(copyBlock(state,['r1','r2'],'template_recognize'),'invalid');
  assert.equal(copyBlock(state,['r1','r2'],'ocr_recognize'),null,'r2 has no reference text');
  assert.equal(copyBlock(state,[],'game_content'),null);
  const limit=max_ocr_zones=>({...state,view:{...state.view,capabilities:{...state.view.capabilities,max_ocr_zones}}});
  assert.equal(copyBlock(limit(null),['r1','r2'],'ocr_recognize'),'noCapability');
  assert.equal(copyTicket(limit(null),state.trialIds,'ocr_recognize'),null);
  assert.equal(copyBlock(limit(null),[],'game_content'),null,'setup does not need the engine report');
  assert.equal(copyBlock(limit(1),['r1','r2'],'ocr_recognize'),'overLimit');
  assert.equal(copyTicket(limit(1),state.trialIds,'ocr_recognize'),null);
  state=sync(setExpected(state,'r2','second'));
  const template=setKind(state,'r2','template');
  assert.equal(copyBlock(template,['r1','r2'],'ocr_recognize'),'kind');
  assert.deepEqual(template.trialIds,['r1']);
  assert.equal(template.document.definitions[1].expected,'second','changing kind preserves author reference text');
  assert.equal(setKind(template,'r2','ocr').document.definitions[1].expected,'second','switching back restores the same reference');
  assert.equal(copyBlock(template,['r2'],'template_recognize'),'templateUnsaved');
  assert.equal(undoRecognition(template).document.definitions[1].expected,'second');
  const invalid={...state,document:{...state.document,definitions:state.document.definitions.map(item=>item.id==='r2'?{...item,region:{u0:0,v0:0,u1:2,v1:1}}:item)}};
  assert.equal(copyBlock(invalid,['r1'],'ocr_recognize'),'invalid','the host validates the whole document');
  assert.equal(copyBlock(invalid,[],'game_content'),'invalid');
  const changed=setContent(state,{x:0,y:10,width:1920,height:1000});
  assert.equal(copyBlock(changed,['r1','r2'],'ocr_recognize'),'unconfirmed');
  assert.equal(copyBlock(changed,[],'game_content'),'unconfirmed','unconfirmed geometry is never copied as setup');
  assert.equal(saveBlock(changed),'unconfirmed','Save cannot persist unconfirmed geometry for later implicit reuse');
  const saved=applyView(state,savedView(state,state.document,{frame:null}));
  assert.equal(saveBlock(renameDefinition(saved,'r1','metadata edit')),null,'saved geometry remains editable without a frame');
});

for (const savedSetup of [false,true]) test(`confirmed ${savedSetup?'changed saved':'first'} setup remains saveable after a failed image load`,()=>{
  let state=sync(create(loaded(),edges(100,100,200,200)));
  if (savedSetup) {
    state=applyView(state,savedView(state));
    state=confirm(sync(setContent(state,{x:0,y:60,width:1920,height:960})));
  }
  const changed=sync(setContent(state,{x:0,y:80,width:1920,height:900}));
  assert.equal(saveBlock(applyView(changed,{...changed.view,frame:null})),'unconfirmed','failed loading must not approve an unconfirmed edit');
  const document=state.document;
  state=applyView(state,{...state.view,frame:null});
  assert.equal(geometryConfirmed(state),false,'frame-dependent operations still need pixels');
  assert.equal(copyBlock(state,['r1'],'ocr_recognize'),'noFrame');
  assert.equal(saveBlock(toggleCrop(state,'r1')),'cropFrame');
  assert.equal(saveBlock(state),null);
  assert.deepEqual(saveTicket(state).document,document,'Save retains every confirmed region without reloading pixels');
});

test('content setup returns to repeat region drawing only after a valid commit',()=>{
  let state=setDisplay(loaded(),{zoom:200,tool:'content'});
  const send=edit=>({token:owner.token,capture_id:state.view.capture_id,revision:state.view.revision,frameId:state.view.frame.id,basisRevision:state.basis,edit});
  state=applyPreviewEdit(state,send({kind:'content',content:{x:0,y:0,width:0,height:1080}}),n=>`Region ${n}`);
  assert.equal(state.display.tool,'content','refused setup remains adjustable');
  state=applyPreviewEdit(state,send({kind:'content',content:{x:0,y:60,width:1920,height:960}}),n=>`Region ${n}`);
  assert.deepEqual(state.display,{zoom:200,tool:'zones'});
  assert.equal(geometryConfirmed(state),false);
  for (const box of [edges(10,70,60,120),edges(100,70,160,120)]) {
    state=applyPreviewEdit(state,send({kind:'create',region:regionFromEdges(box,state.document.basis)}),n=>`Region ${n}`);
    assert.equal(state.display.tool,'zones');
  }
  assert.deepEqual(state.document.definitions.map(item=>mapRegion(item.region,state.document.basis)),[edges(10,70,60,120),edges(100,70,160,120)]);
  state=setDisplay(state,{zoom:200,tool:'content'});
  state=applyView(state,{...state.view,frame:{...FRAME,id:'next',revision:2,confirmed:false}});
  assert.equal(state.display.tool,'zones','each scene starts with region authoring');
});

test('capture switches isolate identical r1 names, Undo and every delayed receipt',()=>{
  let state=sync(create(loaded(),edges(10,10,50,50),'same'));
  state=applyView(state,savedView(state));
  const storedA=state.document;
  const b={...structuredClone(storedA),basis:{frame_width:640,frame_height:480,content:{x:0,y:20,width:640,height:440}}};
  state=renameDefinition(state,'r1','A edited');
  state=toggleCrop(state,'r1');
  const delayedSync=syncTicket(state);
  state=sync(state);
  const delayedSave=saveTicket(state);
  const delayedCopy=copyTicket(state,[],'game_content');
  const delayedTrial=trialTicket(state,'frame',['r1']);
  const preview=previewSnapshot(state,'en',true,null);
  const a=state.document;
  const captures=[{capture_id:CAPTURE_A,document:a},{capture_id:CAPTURE_B,document:b}];
  const saved=[{capture_id:CAPTURE_A,document:storedA},{capture_id:CAPTURE_B,document:b}];
  state=applyView(state,{...state.view,capture_id:CAPTURE_B,document:b,saved_document:b,captures,saved_captures:saved,
    document_revision:state.view.document_revision+1,frame:null});
  assert.deepEqual(state.document.basis,b.basis);
  assert.equal(state.view.frame,null);
  assert.equal(recognitionDirty(state),true,'pending crops on another capture still block a silent Edit exit');
  assert.equal(saveBlock(state),null,'metadata on B can be saved without consuming A staged pixels');
  assert.equal(copyBlock(state,[],'game_content'),'noFrame');
  assert.equal(canUndoRecognition(state),false,'A metadata actions do not undo B r1');
  assert.equal(undoRecognition(state),state);
  assert.equal(applyCopy(state,delayedCopy,null,{category:'late',message:'late',context:null}),state);
  assert.equal(applyTrial(state,delayedTrial,trialResult(delayedTrial,[zone('r1','A')])),state);
  const selectedA={token:owner.token,capture_id:CAPTURE_A,revision:preview.revision,frameId:preview.frame.id,
    basisRevision:preview.basisRevision,edit:{kind:'select',id:null}};
  assert.equal(applyPreviewEdit(state,selectedA,n=>`Region ${n}`),state);
  state=renameDefinition(state,'r1','B edited');
  const all=captureDocuments(state);
  state=applyView(state,{...state.view,capture_id:CAPTURE_A,document:a,saved_document:storedA,captures:all,
    staged_crop_ids:['r1'],staged_crop_sources:{r1:'f1'},document_revision:state.view.document_revision+1,frame:null});
  assert.deepEqual(state.document,a);
  assert.deepEqual(state.cropIds,['r1'],'switching captures retains pending original crop selection');
  assert.deepEqual(saveTicket(state)?.crop_sources,{r1:'f1'},'Save names the original frame after returning to the first capture');
  assert.equal(state.view.frame,null,'A is metadata-only until an explicit image load');
  assert.equal(state.trial,null);
  assert.equal(state.copies.game_content,undefined);
  const before=state.document;
  assert.equal(applySync(state,delayedSync,{...state.view,document:delayedSync.document}),state);
  assert.equal(applySave(state,delayedSave,savedView(state)),state);
  assert.equal(applyCopy(state,delayedCopy,null,null),state);
  assert.equal(applyTrial(state,delayedTrial,trialResult(delayedTrial,[zone('r1','A')])),state);
  assert.equal(applyPreviewEdit(state,{...selectedA,edit:{kind:'undo',localRevision:preview.localRevision}},n=>`Region ${n}`).document,before);
  state=undoRecognition(state);
  assert.equal(state.document.definitions[0].name,'same');
  assert.equal(state.view.captures.find(capture=>capture.capture_id===CAPTURE_B).document.definitions[0].name,'B edited');
});

test('metadata and Undo ceilings are aggregate and Undo cannot overflow another capture',()=>{
  const seed=create(loaded(),edges(10,10,50,50)).document;
  const makeDocument=count=>({...structuredClone(seed),definitions:Array.from({length:count},(_,index)=>({...seed.definitions[0],id:`r${index+1}`}))});
  const a=makeDocument(128), b=makeDocument(128);
  let state=openRecognition(hostView({capture_id:CAPTURE_A,document:a,saved_document:a,
    captures:[{capture_id:CAPTURE_A,document:a},{capture_id:CAPTURE_B,document:b}],
    saved_captures:[{capture_id:CAPTURE_A,document:a},{capture_id:CAPTURE_B,document:b}],basis_confirmed:true}));
  assert.equal(aggregateDefinitions(state),256);
  assert.equal(create(state,edges(100,100,110,110)).notice,'definitionLimit');
  state=deleteDefinition(state,'r128');
  let captures=captureDocuments(state);
  state=applyView(state,{...state.view,capture_id:CAPTURE_B,document:b,saved_document:b,captures,document_revision:state.view.document_revision+1});
  state=create(state,edges(100,100,110,110));
  captures=captureDocuments(state);
  const aAfter=captures.find(capture=>capture.capture_id===CAPTURE_A).document;
  state=applyView(state,{...state.view,capture_id:CAPTURE_A,document:aAfter,saved_document:a,captures,document_revision:state.view.document_revision+1});
  assert.equal(undoRecognition(state).notice,'definitionLimit');
  assert.equal(aggregateDefinitions(state),256);

  for(let index=0;index<70;index++){
    const capture_id=index%2===0?CAPTURE_B:CAPTURE_A;
    const captures=captureDocuments(state);
    const document=captures.find(capture=>capture.capture_id===capture_id).document;
    state=applyView(state,{...state.view,capture_id,document,captures,document_revision:state.view.document_revision+1});
    state=renameDefinition(state,'r1',`edit ${index}`);
  }
  assert.ok(state.undo.length<=UNDO_ENTRIES);
  assert.ok(state.undoBytes<=UNDO_BYTES);
  assert.ok(state.undo.some(entry=>entry.capture_id===CAPTURE_A));
  assert.ok(state.undo.some(entry=>entry.capture_id===CAPTURE_B));

  const large=makeDocument(40);
  large.definitions=large.definitions.map(item=>({...item,expected:'x'.repeat(4096)}));
  const empty=makeDocument(0);
  state=openRecognition(hostView({capture_id:CAPTURE_B,document:empty,
    captures:[{capture_id:CAPTURE_A,document:large},{capture_id:CAPTURE_B,document:empty}]}));
  for(let index=0;index<40;index++){
    state=create(state,edges(index,0,index+1,1));
    state=setExpected(state,state.selected,'x'.repeat(4096));
    if(state.notice==='documentLimit') break;
  }
  assert.equal(state.notice,'documentLimit');
  assert.equal(state.view.captures[0].document,large);
  assert.ok(state.document.definitions.length<40);
});

test('legacy migration stays explicitly dirty and crop discard retains all metadata',()=>{
  const document=create(loaded(),edges(10,10,50,50)).document;
  let state=openRecognition(hostView({document,saved_document:document,basis_confirmed:true,migration_required:true}));
  assert.equal(recognitionDirty(state),true);
  assert.equal(saveBlock(state),null,'migration is saveable without original pixels');
  const ticket=saveTicket(state);
  state=applySave(state,ticket,savedView(state,document,{document_revision:state.view.document_revision+1}));
  assert.equal(recognitionDirty(state),false);
  const chosen=toggleCrop(state,'r1');
  const discarded=discardPixelCrops(chosen);
  assert.deepEqual(discarded.cropIds,[]);
  assert.equal(discarded.document,chosen.document);
  assert.equal(discarded.view.captures,chosen.view.captures);
});

test('unconfirmed native geometry retains pixels until confirm or discard, while missing-image recovery remains possible',()=>{
  const document=loaded().document;
  let state=openRecognition(hostView({document,frame:{...FRAME,confirmed:false}}));
  assert.equal(canReleaseImage(state,true),false);
  assert.equal(canReleaseImage(state,false),false);
  state=confirm(state);
  assert.equal(canReleaseImage(state,true),true);
  state=setContent(state,{x:0,y:10,width:1920,height:1000});
  assert.equal(canReleaseImage(state,true),false,'local unsynchronized geometry is guarded too');
  state=applyView(state,{...state.view,frame:null});
  assert.equal(canReleaseImage(state,false),true,'a lost image can be explicitly reloaded for confirmation');
  assert.equal(canReleaseImage(state,true),false,'missing pixels do not approve abandoning changed geometry');
});

test('saving an active confirmed capture cannot hide another unconfirmed capture basis',()=>{
  let state=create(loaded(),edges(10,10,50,50));
  state={...state,view:{...state.view,other_bases_confirmed:false}};
  assert.equal(saveBlock(state),'unconfirmed');
  assert.equal(saveTicket(state),null);
  state={...state,view:{...state.view,other_bases_confirmed:true}};
  assert.equal(saveBlock(state),null);
});

// Literal expected points are independent of the conversion; no DPR or display-raster pixels enter the read.
const pixelMappings=[
  {scenario:'offset image at a noninteger scale',rect:{left:17.25,top:43.5,width:11.25,height:8.75},client:[20.375,47.875],expected:{x:2,y:3}},
  {scenario:'scrolled image at 200 percent',rect:{left:-13.5,top:-8.25,width:18,height:14},client:[3.75,4.5],expected:{x:8,y:6}},
  {scenario:'the first pixel at the exact top-left',rect:{left:17.25,top:43.5,width:11.25,height:8.75},client:[17.25,43.5],expected:{x:0,y:0}},
  {scenario:'the last pixel just inside both edges',rect:{left:17.25,top:43.5,width:11.25,height:8.75},client:[28.499,52.249],expected:{x:8,y:6}},
  {scenario:'the exact right edge is excluded',rect:{left:17.25,top:43.5,width:11.25,height:8.75},client:[28.5,47],expected:null},
  {scenario:'the exact bottom edge is excluded',rect:{left:17.25,top:43.5,width:11.25,height:8.75},client:[20,52.25],expected:null},
  {scenario:'right-edge subtraction rounding must not become the last pixel',rect:{left:0.3,top:0,width:0.6,height:7},client:[0.3+0.6,3],expected:null},
  {scenario:'bottom-edge subtraction rounding must not become the last pixel',rect:{left:0,top:0.3,width:9,height:0.6},client:[2,0.3+0.6],expected:null},
  {scenario:'negative frame coordinates are excluded before flooring',rect:{left:17.25,top:43.5,width:11.25,height:8.75},client:[17.249,47],expected:null},
  {scenario:'the empty viewport outside the image is excluded',rect:{left:17.25,top:43.5,width:11.25,height:8.75},client:[40,60],expected:null},
  {scenario:'zero rendered width is refused',rect:{left:0,top:0,width:0,height:7},client:[0,0],expected:null},
  {scenario:'negative rendered height is refused',rect:{left:0,top:0,width:9,height:-7},client:[0,0],expected:null},
  {scenario:'a nonfinite pointer is refused',rect:{left:0,top:0,width:9,height:7},client:[Infinity,0],expected:null},
  {scenario:'a nonfinite image offset is refused',rect:{left:NaN,top:0,width:9,height:7},client:[0,0],expected:null},
];
for (const {scenario,rect,client,expected} of pixelMappings) {
  test(`Inspect mapping: ${scenario}`,()=>{
    assert.deepEqual(clientToPixel(...client,rect,9,7),expected);
  });
}

test('Inspect mapping uses CSS pixels once, with finite integer original dimensions and pixel-center markers',()=>{
  const prior=Object.getOwnPropertyDescriptor(globalThis,'devicePixelRatio');
  Object.defineProperty(globalThis,'devicePixelRatio',{value:3,configurable:true});
  try {
    assert.deepEqual(clientToPixel(20.375,47.875,{left:17.25,top:43.5,width:11.25,height:8.75},9,7),{x:2,y:3});
  } finally {
    if (prior) Object.defineProperty(globalThis,'devicePixelRatio',prior);
    else delete globalThis.devicePixelRatio;
  }
  for (const [width,height] of [[0,7],[9,0],[9.5,7],[9,Infinity]]) {
    assert.equal(clientToPixel(0,0,{left:0,top:0,width:9,height:7},width,height),null);
  }
  assert.deepEqual(pixelCenter({x:0,y:0}),{x:0.5,y:0.5});
  assert.deepEqual(pixelCenter({x:8,y:6}),{x:8.5,y:6.5});
  assert.equal(rgbHex([13,201,77,0]),'#0DC94D','alpha zero preserves RGB');
  assert.equal(rgbHex([13,201,77,128]),'#0DC94D','RGB-only hex excludes partial alpha');
  assert.equal(rgbHex([255,0,16,255]),'#FF0010');
});

const INSPECT_FRAME={...FRAME,width:9,height:7};
function inspectionSnapshot(overrides={}){
  return {...previewSnapshot(setDisplay(loaded(INSPECT_FRAME),{tool:'inspect',zoom:'fit'}),'en',true,null),...overrides};
}
// Transport promises are controlled, but responses are independent fixed original-pixel values, not request echoes.
function inspectionSample(overrides={}){
  return {owner,capture_id:CAPTURE_A,frame_id:'f1',frame_revision:1,x:2,y:3,rgba:[13,201,77,128],...overrides};
}
function inspectionHarness(snapshot=inspectionSnapshot()){
  const calls=[],published=[];
  const inspection=new PixelInspection(request=>new Promise((resolve,reject)=>calls.push({request,resolve,reject})),
    value=>published.push(value),cause=>cause);
  inspection.incoming(snapshot);
  inspection.setRaster(inspection.sourceEpoch,'blob:original');
  inspection.rasterLoaded(inspection.sourceEpoch,'blob:original');
  return {inspection,calls,published,snapshot};
}
function inspectionSettled(){
  return new Promise(resolve=>setImmediate(resolve));
}

test('Inspect works on unconfirmed raw geometry and pixels outside Game content without changing draft state',async()=>{
  let state=toggleCrop(create(loaded(INSPECT_FRAME),edges(1,1,4,4)),'r1');
  state=setContent(state,{x:1,y:1,width:7,height:5});
  state=setDisplay(state,{tool:'inspect',zoom:'fit'});
  const saved=structuredClone(state);
  const snapshot=previewSnapshot(state,'en',true,null);
  const h=inspectionHarness(snapshot);
  assert.equal(h.inspection.select({x:0,y:0}),true,'top/left bars are inside the original frame');
  await inspectionSettled();
  h.calls[0].resolve(inspectionSample({x:0,y:0,rgba:[231,19,47,0]}));
  await inspectionSettled();
  assert.deepEqual(h.inspection.value.sample.rgba,[231,19,47,0]);
  assert.deepEqual(state,saved,'selection cannot change geometry, crops, trial stamps, dirty state or Undo');

  const raw=applyView(state,{...state.view,frame:{...INSPECT_FRAME,width:11,height:5,confirmed:false}});
  const rawSaved=structuredClone(raw);
  const rawSnapshot=previewSnapshot(raw,'en',true,null);
  assert.equal(rawSnapshot.frameGeometryReady,false);
  assert.equal(rawSnapshot.confirmed,false);
  assert.equal(rawSnapshot.basis.frame_width,9,'inspection does not rebase the retained basis');
  h.inspection.incoming(rawSnapshot);
  h.inspection.setRaster(h.inspection.sourceEpoch,'blob:raw');
  h.inspection.rasterLoaded(h.inspection.sourceEpoch,'blob:raw');
  assert.equal(h.inspection.pointer(10.9,4.9,{left:0,top:0,width:11,height:5}),true);
  await inspectionSettled();
  assert.deepEqual(h.calls[1].request,{owner,captureId:CAPTURE_A,frameId:'f1',frameRevision:1,x:10,y:4});
  h.calls[1].resolve(inspectionSample({x:10,y:4,rgba:[4,88,173,255]}));
  await inspectionSettled();
  assert.deepEqual(h.inspection.value.sample.rgba,[4,88,173,255]);
  assert.deepEqual(raw,rawSaved);
});

test('main applies Regions edits after a dropped Preview display edit left its own display at Inspect',()=>{
  // Main acknowledged Inspect, then dropped the Preview's Regions switch (unsaved-changes choice, capture transition,
  // closing). Its later snapshots still carry `inspect`, but the Preview's effective tool is Regions and its send guard
  // lets geometry through; main must apply that geometry instead of discarding it without a notice.
  let state=create(loaded(INSPECT_FRAME),edges(1,1,4,4));
  state=setDisplay(state,{tool:'inspect',zoom:'fit'});
  const h=inspectionHarness(previewSnapshot(state,'en',false,'pending',false,null,null,true));
  assert.equal(h.inspection.inspecting,true,'the Preview send guard refuses geometry while Inspect is effective');
  h.inspection.setDisplay({tool:'zones',zoom:'fit'});
  const reopened=previewSnapshot(state,'en',true,null);
  assert.equal(reopened.display.tool,'inspect');
  assert.deepEqual(h.inspection.incoming(reopened),{tool:'zones',zoom:'fit'});
  assert.equal(h.inspection.inspecting,false,'the Preview send guard now emits Regions edits');
  const message=edit=>({token:owner.token,capture_id:CAPTURE_A,revision:reopened.revision,frameId:'f1',basisRevision:reopened.basisRevision,edit});
  const apply=(current,edit)=>applyPreviewEdit(current,message(edit),n=>`Region ${n}`);
  const moved=apply(state,{kind:'region',id:'r1',part:'region',revision:state.document.definitions[0].revision,
    region:regionFromEdges(edges(2,2,5,5),state.document.basis)});
  assert.deepEqual(mapRegion(moved.document.definitions[0].region,moved.document.basis),edges(2,2,5,5));
  assert.equal(moved.notice,null);
  const created=apply(moved,{kind:'create',region:regionFromEdges(edges(5,0,8,3),moved.document.basis)});
  assert.deepEqual(created.document.definitions.map(item=>item.id),['r1','r2']);
  assert.equal(created.selected,'r2');
  const chosen=apply(created,{kind:'select',id:'r1'});
  assert.equal(chosen.selected,'r1');
  const deleted=apply(chosen,{kind:'delete',id:'r1',revision:chosen.document.definitions[0].revision});
  assert.deepEqual(deleted.document.definitions.map(item=>item.id),['r2']);
  const undone=apply(deleted,{kind:'undo',localRevision:deleted.localRevision});
  assert.deepEqual(undone.document.definitions.map(item=>item.id),['r1','r2']);
  // Main only records the last acknowledged Preview display; it stays `inspect` until the next display edit arrives.
  assert.equal(undone.display.tool,'inspect');
  assert.deepEqual(apply(undone,{kind:'display',display:{tool:'zones',zoom:'fit'}}).display,{tool:'zones',zoom:'fit'});
});

test('display edits change neither the draft nor Undo, and Regions/Game content edits remain ordinary afterwards',()=>{
  const state=create(loaded(INSPECT_FRAME),edges(1,1,4,4));
  const snapshot=previewSnapshot(state,'en',true,null);
  const message=edit=>({token:owner.token,capture_id:CAPTURE_A,revision:snapshot.revision,frameId:'f1',basisRevision:snapshot.basisRevision,edit});
  const inspecting=applyPreviewEdit(state,message({kind:'display',display:{tool:'inspect',zoom:200}}),n=>`Region ${n}`);
  assert.equal(inspecting.document,state.document);
  assert.equal(inspecting.undo,state.undo);
  assert.equal(inspecting.localRevision,state.localRevision);
  assert.deepEqual(inspecting.display,{tool:'inspect',zoom:200});
  const regions=applyPreviewEdit(inspecting,message({kind:'display',display:{tool:'zones',zoom:200}}),n=>`Region ${n}`);
  assert.equal(applyPreviewEdit(regions,message({kind:'delete',id:'r1',revision:regions.document.definitions[0].revision}),n=>`Region ${n}`).document.definitions.length,0);
  const content=applyPreviewEdit(regions,message({kind:'display',display:{tool:'content',zoom:200}}),n=>`Region ${n}`);
  assert.deepEqual(applyPreviewEdit(content,message({kind:'content',content:{x:0,y:1,width:9,height:6}}),n=>`Region ${n}`).document.basis.content,
    {x:0,y:1,width:9,height:6});
});

test('focused pixel-step keys never invent a selection, consume foreign focus or turn boundary arrows into scroll',async()=>{
  const h=inspectionHarness();
  assert.equal(h.inspection.key('ArrowRight',true),false);
  assert.equal(h.inspection.value.point,null);
  h.inspection.select({x:0,y:0});
  await inspectionSettled();
  h.calls[0].resolve(inspectionSample({x:0,y:0}));
  await inspectionSettled();
  const unchanged=h.inspection.value;
  assert.equal(h.inspection.key('ArrowRight',false),false);
  assert.equal(h.inspection.key('Escape',false),false);
  assert.equal(h.inspection.key('Delete',true),false);
  assert.equal(h.inspection.key('z',true),false);
  assert.equal(h.inspection.value,unchanged);
  assert.equal(h.inspection.key('ArrowLeft',true),true);
  assert.equal(h.inspection.key('ArrowUp',true),true);
  assert.equal(h.inspection.value,unchanged,'an edge no-op preserves selected bytes and dispatches nothing');
  assert.equal(h.calls.length,1);
  assert.equal(h.inspection.key('ArrowRight',true),true);
  assert.deepEqual(h.inspection.value.point,{x:1,y:0});
  assert.equal(h.inspection.value.sample,null);
  assert.equal(h.inspection.key('ArrowDown',true),true);
  assert.deepEqual(h.inspection.value.point,{x:1,y:1});
  await inspectionSettled();
  assert.equal(h.calls.length,2);
  assert.deepEqual([h.calls[1].request.x,h.calls[1].request.y],[1,1]);
  h.calls[1].resolve(inspectionSample({x:1,y:1}));
  await inspectionSettled();
  assert.equal(h.inspection.key('Escape',true),true);
  assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
});

test('same-source zoom, scroll-only redisplay and package revision changes preserve selected original bytes',async()=>{
  const h=inspectionHarness();
  h.inspection.select({x:2,y:3});
  await inspectionSettled();
  h.calls[0].resolve(inspectionSample());
  await inspectionSettled();
  const value=h.inspection.value,epoch=h.inspection.sourceEpoch;
  h.inspection.incoming({...h.snapshot,revision:'saved-next',display:{tool:'inspect',zoom:200}});
  h.inspection.incoming({...h.snapshot,display:{tool:'inspect',zoom:'fit'}});
  assert.equal(h.inspection.sourceEpoch,epoch);
  assert.equal(h.inspection.value,value);
  assert.deepEqual(value.sample.rgba,[13,201,77,128]);
  assert.equal(h.calls.length,1);
});

for (const outcome of ['success','failure']) {
  test(`single-flight selection coalesces to the latest point after late ${outcome}`,async()=>{
    const h=inspectionHarness();
    h.inspection.select({x:2,y:3});
    await inspectionSettled();
    h.inspection.select({x:1,y:1});
    h.inspection.select({x:8,y:6});
    assert.equal(h.calls.length,1);
    assert.deepEqual(h.inspection.value,{point:{x:8,y:6},sample:null,pending:true,error:null});
    if (outcome==='success') h.calls[0].resolve(inspectionSample());
    else h.calls[0].reject({category:'Busy',message:'old read refused',context:null});
    await inspectionSettled();
    assert.equal(h.calls.length,2);
    assert.deepEqual(h.calls[1].request,{owner,captureId:CAPTURE_A,frameId:'f1',frameRevision:1,x:8,y:6});
    assert.equal(h.inspection.value.error,null);
    assert.equal(h.inspection.value.sample,null);
    h.calls[1].resolve(inspectionSample({x:8,y:6,rgba:[9,72,240,255]}));
    await inspectionSettled();
    assert.deepEqual(h.inspection.value.sample.rgba,[9,72,240,255]);
    assert.equal(h.calls.length,2);
  });
}

test('a new selection immediately clears previous channels and hex, and a failed read never automatically retries',async()=>{
  const h=inspectionHarness();
  h.inspection.select({x:2,y:3});
  await inspectionSettled();
  h.calls[0].resolve(inspectionSample());
  await inspectionSettled();
  assert.equal(rgbHex(h.inspection.value.sample.rgba),'#0DC94D');
  h.inspection.select({x:8,y:6});
  assert.equal(h.inspection.value.sample,null);
  assert.equal(h.inspection.value.pending,true);
  await inspectionSettled();
  const error={category:'CommandBusy',message:'explicitly refused',context:null};
  h.calls[1].reject(error);
  await inspectionSettled();
  assert.deepEqual(h.inspection.value,{point:{x:8,y:6},sample:null,pending:false,error});
  h.inspection.incoming(h.snapshot);
  await inspectionSettled();
  assert.equal(h.calls.length,2);
  h.inspection.select({x:8,y:6});
  await inspectionSettled();
  assert.equal(h.calls.length,3,'the same point can be explicitly selected again after failure');
  h.calls[2].resolve(inspectionSample({x:8,y:6}));
  await inspectionSettled();
});

const inspectionInvalidations=[
  {scenario:'equal-size frame replacement',change:snapshot=>({...snapshot,frame:{...snapshot.frame,id:'f2'}})},
  {scenario:'frame revision replacement',change:snapshot=>({...snapshot,frame:{...snapshot.frame,revision:2}})},
  {scenario:'different dimensions with the same local frame ID',change:snapshot=>({...snapshot,frame:{...snapshot.frame,width:11,height:5}})},
  {scenario:'another capture with the same local frame identity',change:snapshot=>({...snapshot,capture_id:CAPTURE_B})},
  {scenario:'another owner token',change:snapshot=>({...snapshot,owner:{...owner,token:'lease-2'}})},
  {scenario:'another workspace revision with the same owner token',change:snapshot=>({...snapshot,owner:{...owner,workspace:{...owner.workspace,revision:2}}})},
  {scenario:'another workspace ID with the same owner token and revision',change:snapshot=>({...snapshot,owner:{...owner,workspace:{...owner.workspace,workspace_id:'b'}}})},
  {scenario:'native acquisition starts',change:snapshot=>({...snapshot,nativeSelection:{busy:true}})},
  {scenario:'the original frame disappears',change:snapshot=>({...snapshot,frame:null})},
  {scenario:'the Edit owner disappears',change:()=>null},
  {scenario:'Inspect is left in an incoming snapshot',change:snapshot=>({...snapshot,display:{zoom:'fit',tool:'zones'}})},
];
for (const {scenario,change} of inspectionInvalidations) {
  for (const outcome of ['success','failure']) {
    test(`Inspect fences ${outcome} and drops queued coordinates when ${scenario}`,async()=>{
      const h=inspectionHarness();
      h.inspection.select({x:2,y:3});
      await inspectionSettled();
      h.inspection.select({x:8,y:6});
      h.inspection.incoming(change(h.snapshot));
      assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
      const publications=h.published.length;
      if (outcome==='success') h.calls[0].resolve(inspectionSample());
      else h.calls[0].reject({category:'Stale',message:'obsolete failure',context:null});
      await inspectionSettled();
      assert.equal(h.published.length,publications,'late settlement cannot publish success or failure');
      assert.equal(h.calls.length,1,'cleared desired work never dispatches');
      assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
    });
  }
}

test('batched A-B-A source snapshots invalidate both pixel replies and old raster-load events',async()=>{
  const h=inspectionHarness();
  const oldEpoch=h.inspection.sourceEpoch;
  h.inspection.select({x:2,y:3});
  await inspectionSettled();
  h.inspection.incoming({...h.snapshot,capture_id:CAPTURE_B});
  h.inspection.incoming(h.snapshot);
  assert.equal(h.inspection.sourceEpoch,oldEpoch+2);
  assert.equal(h.inspection.rasterLoaded(oldEpoch,'blob:original'),false);
  assert.equal(h.inspection.select({x:2,y:3}),false);
  const epoch=h.inspection.sourceEpoch;
  h.inspection.setRaster(epoch,'blob:returned-a');
  assert.equal(h.inspection.rasterLoaded(oldEpoch,'blob:returned-a'),false);
  assert.equal(h.inspection.rasterLoaded(epoch,'blob:returned-a'),true);
  h.inspection.select({x:2,y:3});
  assert.equal(h.calls.length,1,'the obsolete invocation still owns its slot');
  h.calls[0].resolve(inspectionSample());
  await inspectionSettled();
  assert.equal(h.inspection.value.sample,null,'old A cannot revive by returning to the same source and coordinate');
  assert.equal(h.calls.length,2);
  h.calls[1].resolve(inspectionSample({rgba:[67,8,249,0]}));
  await inspectionSettled();
  assert.deepEqual(h.inspection.value.sample.rgba,[67,8,249,0]);
});

test('a missing raster or replaced blob clears inspection and only the matching image load can enable reads',async()=>{
  const h=inspectionHarness(),epoch=h.inspection.sourceEpoch;
  h.inspection.select({x:2,y:3});
  await inspectionSettled();
  h.inspection.select({x:8,y:6});
  h.inspection.setRaster(epoch,null);
  assert.equal(h.inspection.value.point,null);
  assert.equal(h.inspection.select({x:2,y:3}),false);
  h.inspection.setRaster(epoch,'blob:replacement');
  assert.equal(h.inspection.rasterLoaded(epoch,'blob:original'),false);
  assert.equal(h.inspection.available,false);
  assert.equal(h.inspection.rasterLoaded(epoch,'blob:replacement'),true);
  h.calls[0].reject({category:'Image',message:'old image failed',context:null});
  await inspectionSettled();
  assert.equal(h.calls.length,1);
  assert.equal(h.inspection.value.error,null);
  h.inspection.select({x:8,y:6});
  await inspectionSettled();
  h.calls[1].resolve(inspectionSample({x:8,y:6}));
  await inspectionSettled();
});

for (const cause of ['Escape','tool departure','close','source transition']) {
  test(`a ${cause} before the dispatch microtask prevents even the obsolete transport invocation`,async()=>{
    const h=inspectionHarness();
    h.inspection.select({x:2,y:3});
    if (cause==='Escape') h.inspection.key('Escape',true);
    else if (cause==='tool departure') h.inspection.setDisplay({tool:'content',zoom:'fit'});
    else if (cause==='close') h.inspection.close();
    else h.inspection.incoming({...h.snapshot,capture_id:CAPTURE_B});
    await inspectionSettled();
    assert.equal(h.calls.length,0);
    assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
  });
}

for (const outcome of ['success','failure']) {
  test(`close/reopen keeps the occupied slot until late ${outcome} settles without publishing into the new instance`,async()=>{
    const h=inspectionHarness();
    const oldEpoch=h.inspection.sourceEpoch;
    h.inspection.select({x:2,y:3});
    await inspectionSettled();
    h.inspection.select({x:8,y:6});
    h.inspection.close();
    const closedPublications=h.published.length;
    assert.equal(h.inspection.select({x:2,y:3}),false);
    assert.equal(h.inspection.rasterLoaded(oldEpoch,'blob:original'),false);
    h.inspection.incoming(h.snapshot);
    assert.equal(h.published.length,closedPublications,'closed Preview cannot publish incoming invalidations');
    h.inspection.open();
    assert.equal(h.inspection.select({x:2,y:3}),false,'reopening requires a matching newly loaded raster');
    h.inspection.setRaster(h.inspection.sourceEpoch,'blob:reopened');
    h.inspection.rasterLoaded(h.inspection.sourceEpoch,'blob:reopened');
    h.inspection.select({x:2,y:3});
    assert.equal(h.calls.length,1);
    if (outcome==='success') h.calls[0].resolve(inspectionSample());
    else h.calls[0].reject({category:'Busy',message:'closed read failed',context:null});
    await inspectionSettled();
    assert.equal(h.calls.length,2);
    assert.equal(h.inspection.value.sample,null);
    assert.equal(h.inspection.value.error,null);
    h.calls[1].resolve(inspectionSample({rgba:[211,18,5,255]}));
    await inspectionSettled();
    assert.deepEqual(h.inspection.value.sample.rgba,[211,18,5,255]);
  });
}

test('Escape discards both queued selection and in-flight failure, and reselection cannot resurrect its reply',async()=>{
  const h=inspectionHarness();
  h.inspection.select({x:2,y:3});
  await inspectionSettled();
  h.inspection.select({x:8,y:6});
  assert.equal(h.inspection.key('Escape',true),true);
  h.inspection.select({x:2,y:3});
  h.calls[0].reject({category:'Busy',message:'old selection failed',context:null});
  await inspectionSettled();
  assert.equal(h.calls.length,2);
  assert.equal(h.inspection.value.error,null);
  assert.equal(h.inspection.value.sample,null);
  h.calls[1].resolve(inspectionSample({rgba:[0,255,16,0]}));
  await inspectionSettled();
  assert.equal(rgbHex(h.inspection.value.sample.rgba),'#00FF10');
});

const mismatchedPixels=[
  {scenario:'owner',override:{owner:{...owner,token:'foreign'}}},
  {scenario:'owner workspace ID',override:{owner:{...owner,workspace:{...owner.workspace,workspace_id:'b'}}}},
  {scenario:'capture',override:{capture_id:CAPTURE_B}},
  {scenario:'frame',override:{frame_id:'foreign'}},
  {scenario:'frame revision',override:{frame_revision:2}},
  {scenario:'X coordinate',override:{x:3}},
  {scenario:'Y coordinate',override:{y:4}},
];
for (const {scenario,override} of mismatchedPixels) {
  test(`a mismatched pixel response ${scenario} reports an attributable failure without a default sample or retry`,async()=>{
    const h=inspectionHarness();
    h.inspection.select({x:2,y:3});
    await inspectionSettled();
    h.calls[0].resolve(inspectionSample(override));
    await inspectionSettled();
    assert.equal(h.inspection.value.pending,false);
    assert.equal(h.inspection.value.sample,null);
    assert.equal(h.inspection.value.error.category,'Stale');
    assert.equal(h.calls.length,1);
  });
}

test('synchronous transport failures settle the slot normally and require another explicit selection',async()=>{
  const error={category:'Transport',message:'synchronous refusal',context:null};
  let calls=0;
  const inspection=new PixelInspection(()=>{calls++;throw error;},()=>{},cause=>cause);
  inspection.incoming(inspectionSnapshot());
  inspection.setRaster(inspection.sourceEpoch,'blob:source');
  inspection.rasterLoaded(inspection.sourceEpoch,'blob:source');
  inspection.select({x:2,y:3});
  await inspectionSettled();
  assert.deepEqual(inspection.value,{point:{x:2,y:3},sample:null,pending:false,error});
  assert.equal(calls,1);
  inspection.select({x:8,y:6});
  await inspectionSettled();
  assert.equal(calls,2);
});

test('bottom/right pixel-step boundaries and invalid selections preserve the last successful sample without another read',async()=>{
  const h=inspectionHarness();
  h.inspection.select({x:8,y:6});
  await inspectionSettled();
  h.calls[0].resolve(inspectionSample({x:8,y:6,rgba:[25,219,6,255]}));
  await inspectionSettled();
  const value=h.inspection.value;
  assert.equal(h.inspection.key('ArrowRight',true),true);
  assert.equal(h.inspection.key('ArrowDown',true),true);
  for (const point of [{x:-1,y:0},{x:9,y:0},{x:0,y:7},{x:0.5,y:0},{x:NaN,y:0},{x:0,y:Infinity}]) {
    assert.equal(h.inspection.select(point),false);
    assert.equal(h.inspection.value,value);
  }
  assert.equal(h.inspection.pointer(9,7,{left:0,top:0,width:9,height:7}),false);
  assert.equal(h.inspection.value,value);
  await inspectionSettled();
  assert.equal(h.calls.length,1);
});

test('a failed obsolete blob cannot clear the currently loaded image at the same source epoch',()=>{
  const h=inspectionHarness(),epoch=h.inspection.sourceEpoch;
  h.inspection.setRaster(epoch,'blob:new-display');
  h.inspection.rasterLoaded(epoch,'blob:new-display');
  assert.equal(h.inspection.rasterFailed(epoch,'blob:original'),false);
  assert.equal(h.inspection.available,true);
  assert.equal(h.inspection.rasterFailed(epoch,'blob:new-display'),true);
  assert.equal(h.inspection.available,false);
  assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
});

test('acquisition A-busy-A does not revive a pending read even when the source identity is unchanged',async()=>{
  const h=inspectionHarness();
  h.inspection.select({x:2,y:3});
  await inspectionSettled();
  h.inspection.incoming({...h.snapshot,nativeSelection:{busy:true}});
  assert.equal(h.inspection.select({x:8,y:6}),false);
  h.inspection.incoming(h.snapshot);
  assert.equal(h.inspection.available,true);
  assert.equal(h.inspection.value.point,null,'acquisition ending does not restore the old point');
  h.inspection.select({x:2,y:3});
  h.calls[0].resolve(inspectionSample());
  await inspectionSettled();
  assert.equal(h.calls.length,2);
  assert.equal(h.inspection.value.sample,null);
  h.calls[1].resolve(inspectionSample({rgba:[184,7,222,128]}));
  await inspectionSettled();
  assert.deepEqual(h.inspection.value.sample.rgba,[184,7,222,128]);
});

for (const outcome of ['success','failure']) {
  test(`a closed Preview never dispatches its queued point or publishes a late ${outcome}`,async()=>{
    const old=inspectionHarness();
    old.inspection.select({x:2,y:3});
    await inspectionSettled();
    old.inspection.select({x:8,y:6});
    old.inspection.close();
    const publications=old.published.length;
    const reopened=inspectionHarness();
    reopened.inspection.select({x:8,y:6});
    await inspectionSettled();
    if (outcome==='success') old.calls[0].resolve(inspectionSample());
    else old.calls[0].reject({category:'Stale',message:'closed instance',context:null});
    await inspectionSettled();
    assert.equal(old.published.length,publications);
    assert.equal(old.calls.length,1);
    assert.equal(old.inspection.value.sample,null);
    assert.deepEqual(reopened.inspection.value,{point:{x:8,y:6},sample:null,pending:true,error:null});
    reopened.calls[0].resolve(inspectionSample({x:8,y:6,rgba:[127,16,201,255]}));
    await inspectionSettled();
    assert.deepEqual(reopened.inspection.value.sample.rgba,[127,16,201,255]);
  });
}

test('an owner workspace mismatch cannot publish a pixel even when its token and frame identity match',async()=>{
  const h=inspectionHarness();
  h.inspection.select({x:2,y:3});
  await inspectionSettled();
  h.calls[0].resolve(inspectionSample({owner:{...owner,workspace:{...owner.workspace,revision:99}}}));
  await inspectionSettled();
  assert.equal(h.inspection.value.sample,null);
  assert.equal(h.inspection.value.error.category,'Stale');
  assert.equal(h.calls.length,1);
});

test('a raw frame loads once before Inspect and stays ready across tool changes and zoom without rebasing geometry',async()=>{
  const before=create(loaded(INSPECT_FRAME),edges(1,1,4,4));
  let state=applyView(before,{...before.view,frame:{...INSPECT_FRAME,id:'raw',revision:2,width:11,height:5,confirmed:false}});
  const retained=structuredClone(state);
  const snapshot=previewSnapshot(state,'ja',true,null);
  assert.equal(snapshot.display.tool,'zones');
  assert.equal(snapshot.frameGeometryReady,false);
  assert.equal(snapshot.confirmed,false);
  const h=inspectionHarness(snapshot),epoch=h.inspection.sourceEpoch;
  assert.equal(h.inspection.available,false,'loading the raw raster does not activate Inspect');
  state=setDisplay(state,{tool:'inspect',zoom:'fit'});
  h.inspection.setDisplay(state.display);
  h.inspection.incoming(previewSnapshot(state,'ja',true,null));
  assert.equal(h.inspection.available,true,'the already loaded raw raster stays available on tool entry');
  assert.equal(h.inspection.sourceEpoch,epoch);
  assert.equal(h.inspection.pointer(10.9,4.9,{left:0,top:0,width:11,height:5}),true);
  await inspectionSettled();
  h.calls[0].resolve(inspectionSample({frame_id:'raw',frame_revision:2,x:10,y:4,rgba:[171,5,244,0]}));
  await inspectionSettled();
  const value=h.inspection.value;
  state=setDisplay(state,{tool:'inspect',zoom:150});
  h.inspection.setDisplay(state.display);
  h.inspection.incoming(previewSnapshot(state,'ja',true,null));
  assert.equal(h.inspection.value,value,'a display-only zoom preserves the original point and bytes');
  assert.equal(h.inspection.sourceEpoch,epoch);
  assert.equal(h.inspection.available,true);
  state=setDisplay(state,{tool:'zones',zoom:150});
  h.inspection.setDisplay(state.display);
  h.inspection.incoming(previewSnapshot(state,'ja',true,null));
  assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
  state=setDisplay(state,{tool:'inspect',zoom:150});
  h.inspection.setDisplay(state.display);
  h.inspection.incoming(previewSnapshot(state,'ja',true,null));
  assert.equal(h.inspection.available,true,'reentry needs no second image load for the unchanged raster');
  assert.equal(h.inspection.value.point,null,'reentry does not restore the old selection');
  assert.equal(h.inspection.sourceEpoch,epoch);
  assert.equal(h.calls.length,1);
  assert.deepEqual({...state,display:retained.display},retained,'geometry, crops, trials, dirtiness and Undo remain unchanged');
});

for (const outcome of ['success','failure']) {
  test(`incoming Inspect-Regions-Inspect snapshots fence late ${outcome} while retaining the same loaded raster`,async()=>{
    const initial=setDisplay(loaded(INSPECT_FRAME),{tool:'inspect',zoom:'fit'});
    const h=inspectionHarness(previewSnapshot(initial,'en',true,null)),epoch=h.inspection.sourceEpoch;
    h.inspection.select({x:2,y:3});
    await inspectionSettled();
    h.inspection.select({x:8,y:6});
    h.inspection.incoming(previewSnapshot(setDisplay(initial,{tool:'zones',zoom:'fit'}),'en',true,null));
    h.inspection.incoming(previewSnapshot(initial,'en',true,null));
    assert.equal(h.inspection.sourceEpoch,epoch);
    assert.equal(h.inspection.available,true);
    assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
    h.inspection.select({x:2,y:3});
    assert.equal(h.calls.length,1,'tool invalidation cannot release an occupied invocation');
    if (outcome==='success') h.calls[0].resolve(inspectionSample());
    else h.calls[0].reject({category:'CommandBusy',message:'departed tool read',context:null});
    await inspectionSettled();
    assert.equal(h.calls.length,2);
    assert.deepEqual(h.inspection.value,{point:{x:2,y:3},sample:null,pending:true,error:null});
    h.calls[1].resolve(inspectionSample({rgba:[203,49,7,128]}));
    await inspectionSettled();
    assert.deepEqual(h.inspection.value.sample.rgba,[203,49,7,128]);
    assert.equal(h.calls.length,2);
  });
}

const inspectionToolTransitions=[
  {scenario:'incoming snapshot',update:(inspection,snapshot,tool)=>inspection.incoming({...snapshot,display:{...snapshot.display,tool}})},
  {scenario:'local tool change',update:(inspection,snapshot,tool)=>inspection.setDisplay({...snapshot.display,tool})},
];
for (const {scenario,update} of inspectionToolTransitions) {
  test(`${scenario} exposes the synchronous Inspect guard before the next render and clears it on departure`,async()=>{
    const snapshot=inspectionSnapshot({display:{tool:'zones',zoom:'fit'}});
    const h=inspectionHarness(snapshot),epoch=h.inspection.sourceEpoch;
    assert.equal(h.inspection.inspecting,false);
    assert.equal(h.inspection.available,false);
    update(h.inspection,snapshot,'inspect');
    assert.equal(snapshot.display.tool,'zones','a handler can still hold the previous rendered snapshot');
    assert.equal(h.inspection.inspecting,true,'pointer/key/edit routing reads the synchronous guard instead');
    assert.equal(h.inspection.available,true);
    assert.equal(h.inspection.key('ArrowRight',true),false,'tool entry still cannot invent a selection');
    h.inspection.select({x:2,y:3});
    await inspectionSettled();
    update(h.inspection,snapshot,'zones');
    assert.equal(h.inspection.inspecting,false);
    assert.equal(h.inspection.available,false);
    assert.equal(h.inspection.sourceEpoch,epoch,'tool changes keep the loaded raster');
    assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
    const publications=h.published.length;
    h.calls[0].resolve(inspectionSample());
    await inspectionSettled();
    assert.equal(h.published.length,publications);
    assert.equal(h.calls.length,1);
  });
}

const rasterFailureSettlements=[
  {scenario:'success',settle:call=>call.resolve(inspectionSample())},
  {scenario:'failure',settle:call=>call.reject({category:'Image',message:'obsolete raster read',context:null})},
];
for (const {scenario,settle} of rasterFailureSettlements) {
  test(`a current raster failure clears queued selection and fences late ${scenario} until explicit reselection`,async()=>{
    const h=inspectionHarness(),epoch=h.inspection.sourceEpoch;
    h.inspection.select({x:2,y:3});
    await inspectionSettled();
    h.inspection.select({x:8,y:6});
    assert.equal(h.inspection.rasterFailed(epoch,'blob:original'),true);
    assert.equal(h.inspection.available,false);
    assert.deepEqual(h.inspection.value,{point:null,sample:null,pending:false,error:null});
    h.inspection.setRaster(epoch,'blob:recovered');
    assert.equal(h.inspection.rasterLoaded(epoch,'blob:original'),false);
    assert.equal(h.inspection.rasterLoaded(epoch,'blob:recovered'),true);
    assert.equal(h.inspection.value.point,null,'a successful image load never restores the old selection');
    h.inspection.select({x:2,y:3});
    assert.equal(h.calls.length,1,'image failure cannot free an occupied invocation');
    settle(h.calls[0]);
    await inspectionSettled();
    assert.equal(h.calls.length,2);
    assert.deepEqual(h.inspection.value,{point:{x:2,y:3},sample:null,pending:true,error:null});
    h.calls[1].resolve(inspectionSample({rgba:[52,213,6,0]}));
    await inspectionSettled();
    assert.deepEqual(h.inspection.value.sample.rgba,[52,213,6,0]);
    assert.equal(rgbHex(h.inspection.value.sample.rgba),'#34D506');
  });
}

test('an unchanged pre-edit snapshot cannot discard a local Inspect selection or its queued pixel',async()=>{
  const snapshot=inspectionSnapshot({display:{tool:'zones',zoom:'fit'}});
  const h=inspectionHarness(snapshot);
  h.inspection.setDisplay({tool:'inspect',zoom:'fit'});
  h.inspection.select({x:2,y:3});
  await inspectionSettled();
  h.inspection.select({x:8,y:6});
  h.inspection.incoming({...snapshot,commandBusy:true});
  assert.equal(h.inspection.inspecting,true);
  assert.deepEqual(h.inspection.value,{point:{x:8,y:6},sample:null,pending:true,error:null});
  h.inspection.incoming({...snapshot,display:{tool:'inspect',zoom:'fit'}});
  h.calls[0].resolve(inspectionSample());
  await inspectionSettled();
  assert.equal(h.calls.length,2);
  h.calls[1].resolve(inspectionSample({x:8,y:6,rgba:[25,219,6,255]}));
  await inspectionSettled();
  assert.deepEqual(h.inspection.value.sample.rgba,[25,219,6,255]);
});

test('display synchronization retains local zoom through repeated polls but accepts authoritative display and source changes',()=>{
  const snapshot=inspectionSnapshot();
  const h=inspectionHarness(snapshot);
  h.inspection.setDisplay({tool:'inspect',zoom:150});
  assert.deepEqual(h.inspection.incoming({...snapshot,commandBusy:true}),{tool:'inspect',zoom:150});
  const acknowledged={...snapshot,display:{tool:'inspect',zoom:150}};
  assert.deepEqual(h.inspection.incoming(acknowledged),{tool:'inspect',zoom:150});
  assert.deepEqual(h.inspection.incoming({...acknowledged,display:{tool:'content',zoom:200}}),{tool:'content',zoom:200});
  h.inspection.setDisplay({tool:'inspect',zoom:300});
  const replaced={...acknowledged,display:{tool:'content',zoom:200},frame:{...snapshot.frame,id:'replacement'}};
  assert.deepEqual(h.inspection.incoming(replaced),{tool:'content',zoom:200});
  assert.equal(h.inspection.inspecting,false);
  assert.equal(h.inspection.available,false);
});

test('Preview loading refuses every ordinary target action before admission and while outstanding',()=>{
  const load=new PreviewImageLoad();
  const snapshot=inspectionSnapshot();
  assert.equal(previewActionAvailable({...snapshot,frame:null},false,load.pending(null),'select'),true);
  for (const action of ['select','start','capture','newCapture']) {
    assert.equal(previewActionAvailable(snapshot,false,load.pending(1),action),false,action);
  }
  const request=load.begin(1);
  assert.equal(previewActionAvailable(snapshot,false,load.pending(1),'select'),false);
  assert.equal(previewActionAvailable(snapshot,false,load.pending(1),'cancel'),true);
  assert.equal(previewActionAvailable(snapshot,true,load.pending(1),'cancel'),false);
  request.settle();
  assert.equal(previewActionAvailable(snapshot,false,load.pending(1),'select'),true);
});

test('Preview settlement needs no raster and preserves independent host restrictions',()=>{
  const load=new PreviewImageLoad();
  const request=load.begin(1);
  request.settle();
  const snapshot=inspectionSnapshot({error:{category:'Image',message:'preview encoding failed',context:null}});
  assert.equal(previewActionAvailable(snapshot,false,load.pending(1),'start'),true);
  for (const restriction of [{editable:false},{commandBusy:true},{running:true},{nativeSelection:{busy:true}},{nativeSelection:{platform:'unsupported'}}]) {
    assert.equal(previewActionAvailable({...snapshot,...restriction},false,load.pending(1),'start'),false);
  }
  assert.equal(previewActionAvailable(null,false,load.pending(null),'select'),false);
});

test('Preview invalidation cannot release an outstanding request even when the successor is empty',()=>{
  const load=new PreviewImageLoad();
  const request=load.begin(1);
  request.invalidate();
  assert.equal(load.pending(null),true);
  request.settle();
  assert.equal(load.pending(null),false);
  assert.equal(load.pending(1),true,'reopening the same source must await a fresh load');
  const reopened=load.begin(1);
  request.invalidate();
  request.settle();
  assert.equal(load.pending(1),true);
  reopened.settle();
  assert.equal(load.pending(1),false);
});

for (const {scenario,first} of [
  {scenario:'old completion precedes the successor',first:'old'},
  {scenario:'successor completion precedes the old request',first:'new'},
]) test(`Preview overlapping settlement: ${scenario}`,()=>{
  const load=new PreviewImageLoad();
  const old=load.begin(1);
  old.invalidate();
  const current=load.begin(2);
  const requests=first==='old'?[old,current]:[current,old];
  requests[0].settle();
  assert.equal(load.pending(2),true);
  requests[1].settle();
  assert.equal(load.pending(2),false);
  old.invalidate();
  old.settle();
  assert.equal(load.pending(2),false,'old cleanup cannot erase successor settlement');
  assert.equal(load.pending(3),true,'a new source is blocked before its effect starts');
});
