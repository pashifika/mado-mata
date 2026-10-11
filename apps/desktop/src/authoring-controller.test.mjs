import {test} from 'node:test';
import assert from 'node:assert/strict';
import {AuthoringController, EDIT_RANGES, NOTICE_HISTORY, PAGE_UNITS} from './authoring-controller.ts';
import {AUTHORING_NON_IMAGE_BYTES, AUTHORING_SOURCE_BYTES, applyExternalEdits, applySave, applyValidation, beginComposition, beginPending, editFile, endComposition, openSession,
  recordRange, replaceFile, saveTicket, selectFile, undoFile} from './authoring.ts';
import {editValues, formatJson, loadValues, typedText} from './metadata.ts';
import {applyView, mapRegion, openRecognition, regionFromEdges, renameDefinition, selectDefinition, setContent, setRegion, toggleCrop, toggleTrial,
  undoRecognition} from './recognition.ts';

const ID=/^[!-~]{1,256}$/;
const CODE=/^[a-z0-9_]{1,64}$/;
const owner={workspace:{workspace_id:'a',revision:1},token:'authoring-1-1'};
// CRLF and a non-BMP character must survive every read and edit unchanged.
const MAIN='export const a = 1;\r\nconst face = "😀";\n';
const HELPER='export const b = 2;\n';
function file(path,kind,text){
  return {path,kind,text,bytes:text===null?4:new TextEncoder().encode(text).length};
}
const files=[file('package.json','manifest','{}\n'),file('main.ts','source',MAIN),file('helper.ts','source',HELPER),file('schema.json','schema','{}\n'),file('images/logo.png','asset',null)];
function packageView(list=files,token='authoring-1-1',path='/private/packages/a'){
  return {owner:{...owner,token},package_path:path,package_id:'example.a',revision:'rev-1',files:list};
}
function setup(view=packageView()){
  const published=[];
  const controller=new AuthoringController(session=>published.push(session));
  controller.update(()=>openSession(view));
  return {controller,published};
}
function describe(controller){
  const response=controller.handle({id:'describe',owner:null,package:null,operation:{kind:'describe'}});
  assert.equal(response.ok,true);
  return response.result;
}
const resource=(description,path)=>description.resources.find(item=>item.path===path);
const request=(description,operation)=>({id:'request',owner:description.owner,package:description.package,operation});
const draft=(controller,path)=>controller.current().drafts.get(path);
// Human typing at the draft's caret, as the editor reports it.
function type(controller,path,inserted){
  controller.update(session=>{
    const current=session.drafts.get(path);
    const at=current.range;
    const caret=at.start+inserted.length;
    return editFile(session,path,{text:current.text.slice(0,at.start)+inserted+current.text.slice(at.end),start:caret,end:caret},at,{type:'insertText',data:inserted,composing:false});
  });
}
function refused(response,code,resourceId){
  assert.equal(response.ok,false);
  assert.equal(response.error.code,code);
  assert.match(response.error.code,CODE);
  assert.equal(response.error.outcome,'not_applied');
  assert.ok(response.error.message.length<=1024);
  if(resourceId!==undefined) assert.equal(response.error.resource,resourceId);
}

test('UI edits and external reads share one live session and publication',()=>{
  const {controller,published}=setup();
  type(controller,'main.ts','// unsaved\n');
  assert.equal(published.at(-1),controller.current());
  const description=describe(controller);
  const main=resource(description,'main.ts');
  assert.equal(main.dirty,true);
  const page=controller.handle(request(description,{kind:'read',resource:main.id}));
  assert.equal(page.ok,true);
  assert.equal(page.owner,'authoring-1-1');
  assert.deepEqual(page.result,{resource:main.id,path:'main.ts',version:main.version,text:'// unsaved\n'+MAIN,offset:0,nextOffset:null,
    totalUnits:('// unsaved\n'+MAIN).length,savedRevision:'rev-1'});
});

test('describe without an Edit owner is usable while read and edit refuse',()=>{
  const controller=new AuthoringController(()=>{});
  const description=describe(controller);
  assert.equal(description.protocol,1);
  assert.equal(description.sdk,'mado-host-v1');
  assert.deepEqual([description.owner,description.package,description.packageId,description.savedRevision,description.selected,description.resources],
    [null,null,null,null,null,[]]);
  assert.equal(description.limits.pageUnits,PAGE_UNITS);
  const read=controller.handle({id:'r',owner:'authoring-1-1',package:'package',operation:{kind:'read',resource:'resource'}});
  refused(read,'no_edit_owner');
  assert.equal(read.owner,null);
  refused(controller.handle({id:'e',owner:'authoring-1-1',package:'package',operation:{kind:'edit',edits:[{resource:'resource',version:'v',text:''}]}}),'no_edit_owner');
});

test('describe identifies owner, opaque package and resources without host paths',()=>{
  const {controller}=setup();
  const description=describe(controller);
  assert.equal(description.owner,'authoring-1-1');
  assert.equal(description.packageId,'example.a');
  assert.equal(description.savedRevision,'rev-1');
  assert.match(description.package,ID);
  assert.equal(JSON.stringify(description).includes('/private/packages'),false);
  assert.deepEqual(description.resources.map(item=>[item.path,item.kind,item.dirty,item.composing,item.missing]),
    [['package.json','manifest',false,false,false],['main.ts','source',false,false,false],['helper.ts','source',false,false,false],
      ['schema.json','schema',false,false,false],['images/logo.png','asset',false,false,false]]);
  for(const item of description.resources){
    assert.match(item.id,ID);
    assert.match(item.version,ID);
    assert.notEqual(item.id,item.path);
  }
  assert.equal(new Set(description.resources.map(item=>item.id)).size,description.resources.length);
  assert.equal(description.selected,resource(description,'main.ts').id);
  controller.update(session=>({...session,destination:'recognition'}));
  assert.equal(describe(controller).selected,null);
});

test('a resource changed and restored never reuses its version',()=>{
  const {controller}=setup();
  const before=describe(controller);
  const main=resource(before,'main.ts');
  type(controller,'main.ts','X');
  controller.update(session=>undoFile(session,'main.ts'));
  assert.equal(draft(controller,'main.ts').text,MAIN);
  const after=describe(controller);
  assert.notEqual(resource(after,'main.ts').version,main.version);
  assert.equal(resource(after,'helper.ts').version,resource(before,'helper.ts').version);
  const session=controller.current();
  refused(controller.handle(request(after,{kind:'edit',edits:[{resource:main.id,version:main.version,text:'stale'}]})),'stale_version',main.id);
  refused(controller.handle(request(after,{kind:'read',resource:main.id,version:main.version})),'stale_version',main.id);
  assert.equal(controller.current(),session);
});

test('caret moves, file selection and a save of the same text keep resource versions',()=>{
  const {controller}=setup();
  type(controller,'main.ts','A');
  const version=resource(describe(controller),'main.ts').version;
  controller.update(session=>recordRange(session,'main.ts',{start:3,end:5}));
  controller.update(session=>selectFile(session,'helper.ts',{start:3,end:5}));
  controller.update(session=>selectFile(session,'main.ts',null));
  const ticket=saveTicket(controller.current(),'main.ts');
  controller.update(session=>beginPending(session,{kind:'save',path:'main.ts'}));
  controller.update(session=>applySave(session,ticket,{owner,committed_revision:'rev-2',view:null,refresh_error:null}));
  const description=describe(controller);
  assert.equal(resource(description,'main.ts').version,version);
  assert.equal(resource(description,'main.ts').dirty,false);
  assert.equal(description.savedRevision,'rev-2');
});

test('owner replacement or recreation refuses old identities and issues fresh versions',()=>{
  const {controller}=setup();
  const old=describe(controller);
  controller.update(()=>openSession(packageView(files,'authoring-1-2')));
  const fresh=describe(controller);
  assert.equal(fresh.owner,'authoring-1-2');
  assert.equal(fresh.package,old.package);
  assert.notEqual(resource(fresh,'main.ts').id,resource(old,'main.ts').id);
  assert.notEqual(resource(fresh,'main.ts').version,resource(old,'main.ts').version);
  const stale=controller.handle(request(old,{kind:'read',resource:resource(old,'main.ts').id}));
  refused(stale,'stale_owner');
  assert.equal(stale.owner,'authoring-1-2');
  refused(controller.handle({...request(fresh,{kind:'read',resource:resource(fresh,'main.ts').id}),package:old.package+'x'}),'stale_package');
  // The same lease reopened after its session closed is still a new lineage.
  controller.update(()=>null);
  controller.update(()=>openSession(packageView(files,'authoring-1-2')));
  const recreated=describe(controller);
  assert.notEqual(resource(recreated,'main.ts').version,resource(fresh,'main.ts').version);
  refused(controller.handle(request(recreated,{kind:'read',resource:resource(fresh,'main.ts').id})),'unknown_resource',resource(fresh,'main.ts').id);
  controller.update(()=>openSession(packageView(files,'authoring-1-3','/private/packages/b')));
  assert.notEqual(describe(controller).package,old.package);
});

test('an agent edit applies once as a separate undo group between human edits',()=>{
  const {controller,published}=setup();
  type(controller,'main.ts','A');
  controller.update(session=>selectFile(session,'helper.ts',{start:1,end:1}));
  type(controller,'helper.ts','H');
  controller.update(session=>selectFile(session,'main.ts',null));
  const helper=draft(controller,'helper.ts');
  const before=controller.current();
  const description=describe(controller);
  const main=resource(description,'main.ts');
  const publications=published.length;
  const response=controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,ranges:[{from:0,to:0,text:'// agent\n'}]}]}));
  assert.equal(response.ok,true);
  assert.equal(published.length,publications+1);
  const after=controller.current();
  assert.equal(after.selected,'main.ts');
  assert.equal(after.destination,before.destination);
  assert.equal(after.reveal,before.reveal);
  assert.equal(draft(controller,'main.ts').text,'// agent\nA'+MAIN);
  // The caret after the human's "A" moves with its text, not to the agent's insertion.
  assert.deepEqual(draft(controller,'main.ts').range,{start:10,end:10});
  assert.equal(draft(controller,'main.ts').undo.length,2);
  assert.equal(after.drafts.get('helper.ts'),helper);
  const version=resource(describe(controller),'main.ts').version;
  assert.notEqual(version,main.version);
  assert.deepEqual(response.result,{resources:[{resource:main.id,version}],created:[],savedRevision:'rev-1'});
  // Later human typing is undone first, then the agent group, then the earlier human group.
  type(controller,'main.ts','B');
  controller.update(session=>undoFile(session,'main.ts'));
  assert.equal(draft(controller,'main.ts').text,'// agent\nA'+MAIN);
  controller.update(session=>undoFile(session,'main.ts'));
  assert.equal(draft(controller,'main.ts').text,'A'+MAIN);
  assert.deepEqual(draft(controller,'main.ts').range,{start:1,end:1});
  controller.update(session=>undoFile(session,'main.ts'));
  assert.equal(draft(controller,'main.ts').text,MAIN);
  assert.equal(draft(controller,'helper.ts'),helper);
});

test('editing a nonselected file leaves the selected file and its caret untouched',()=>{
  const {controller}=setup();
  controller.update(session=>recordRange(session,'main.ts',{start:4,end:6}));
  const main=draft(controller,'main.ts');
  const description=describe(controller);
  const helper=resource(description,'helper.ts');
  const response=controller.handle(request(description,{kind:'edit',edits:[{resource:helper.id,version:helper.version,text:'export const b = 3;\n'}]}));
  assert.equal(response.ok,true);
  assert.equal(controller.current().selected,'main.ts');
  assert.equal(draft(controller,'main.ts'),main);
  assert.equal(draft(controller,'helper.ts').text,'export const b = 3;\n');
  assert.equal(draft(controller,'helper.ts').undo.length,1);
  const after=describe(controller);
  assert.equal(resource(after,'helper.ts').dirty,true);
  assert.equal(resource(after,'main.ts').version,resource(description,'main.ts').version);
});

test('a batch is refused whole for any stale, unknown, unsupported or invalid part',()=>{
  const {controller}=setup();
  const description=describe(controller);
  const main=resource(description,'main.ts'), helper=resource(description,'helper.ts'), schema=resource(description,'schema.json');
  controller.update(session=>selectFile(session,'helper.ts',null));
  type(controller,'helper.ts','x');
  controller.update(session=>selectFile(session,'main.ts',null));
  const session=controller.current();
  const replaceMain={resource:main.id,version:main.version,text:'replaced'};
  const cases=[
    [{edits:[replaceMain,{resource:helper.id,version:helper.version,text:'replaced'}]},'stale_version',helper.id],
    [{edits:[replaceMain],dependencies:[{resource:helper.id,version:helper.version}]},'stale_version',helper.id],
    [{edits:[replaceMain],dependencies:[{resource:'gone',version:helper.version}]},'unknown_resource','gone'],
    [{edits:[replaceMain,{resource:schema.id,version:schema.version,text:'{}'}]},'unsupported_resource',schema.id],
    [{edits:[replaceMain,{resource:'gone',version:main.version,text:''}]},'unknown_resource','gone'],
    [{edits:[replaceMain,{resource:helper.id,version:resource(describe(controller),'helper.ts').version,ranges:[{from:0,to:0,text:'a'},{from:5,to:4,text:''}]}]},'invalid_range',helper.id],
  ];
  for(const [operation,code,resourceId] of cases){
    refused(controller.handle(request(description,{kind:'edit',...operation})),code,resourceId);
    assert.equal(controller.current(),session);
  }
  // An independent change elsewhere does not reject an edit that did not declare it.
  assert.equal(controller.handle(request(description,{kind:'edit',edits:[replaceMain]})).ok,true);
  assert.equal(draft(controller,'main.ts').text,'replaced');
  assert.equal(draft(controller,'helper.ts').text,'x'+HELPER);
});

test('active IME composition refuses the edit without committing or queuing it',()=>{
  const {controller}=setup();
  controller.update(session=>beginComposition(session,'main.ts',{start:0,end:0}));
  controller.update(session=>editFile(session,'main.ts',{text:'か'+MAIN,start:1,end:1},{start:0,end:0},{type:'insertCompositionText',data:'か',composing:true}));
  const description=describe(controller);
  const main=resource(description,'main.ts');
  assert.equal(main.composing,true);
  const session=controller.current();
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,ranges:[{from:0,to:0,text:'x'}]}]})),'composing',main.id);
  assert.equal(controller.current(),session);
  controller.update(current=>endComposition(current,'main.ts'));
  assert.equal(draft(controller,'main.ts').text,'か'+MAIN);
  assert.equal(draft(controller,'main.ts').undo.length,1);
});

test('UTF-16 ranges preserve line endings and non-BMP text without normalization',()=>{
  const {controller}=setup();
  const description=describe(controller);
  const main=resource(description,'main.ts');
  const face=MAIN.indexOf('😀');
  const crlf=MAIN.indexOf('\r\n');
  // Any order; insertions at the same offset keep request order.
  const response=controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,ranges:[
    {from:face,to:face+2,text:'🎉'},{from:crlf,to:crlf,text:' // one\r\n'},{from:0,to:0,text:'a'},{from:0,to:0,text:'b'},
  ]}]}));
  assert.equal(response.ok,true);
  const expected='ab'+MAIN.slice(0,crlf)+' // one\r\n'+MAIN.slice(crlf,face)+'🎉'+MAIN.slice(face+2);
  assert.equal(draft(controller,'main.ts').text,expected);
  assert.equal(draft(controller,'main.ts').undo.length,1);
  const page=controller.handle(request(describe(controller),{kind:'read',resource:main.id}));
  assert.equal(page.result.text,expected);
  // Whole-document replacement is taken verbatim as well.
  const whole='line one\r\nline two 𝄞\rline three\n';
  const latest=resource(describe(controller),'main.ts');
  assert.equal(controller.handle(request(describe(controller),{kind:'edit',edits:[{resource:main.id,version:latest.version,text:whole}]})).ok,true);
  assert.equal(draft(controller,'main.ts').text,whole);
});

test('overlapping, out-of-range, character-splitting and malformed ranges are refused',()=>{
  const {controller}=setup();
  const description=describe(controller);
  const main=resource(description,'main.ts');
  const face=MAIN.indexOf('😀');
  const session=controller.current();
  const edit=ranges=>request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,ranges}]});
  refused(controller.handle(edit([{from:0,to:5,text:''},{from:4,to:6,text:''}])),'invalid_range',main.id);
  refused(controller.handle(edit([{from:0,to:MAIN.length+1,text:''}])),'invalid_range',main.id);
  refused(controller.handle(edit([{from:face+1,to:face+1,text:'x'}])),'invalid_range',main.id);
  refused(controller.handle(edit([{from:face,to:face+1,text:''}])),'invalid_range',main.id);
  refused(controller.handle(edit([])),'invalid_request');
  refused(controller.handle(edit([{from:0,to:0,text:'x',extra:true}])),'invalid_request');
  refused(controller.handle(edit([{from:-1,to:0,text:'x'}])),'invalid_request');
  refused(controller.handle(edit([{from:0.5,to:1,text:'x'}])),'invalid_request');
  refused(controller.handle(edit(Array.from({length:EDIT_RANGES+1},()=>({from:0,to:0,text:''})))),'invalid_request');
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,text:'a',ranges:[{from:0,to:0,text:''}]}]})),'invalid_request');
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,text:'a'},{resource:main.id,version:main.version,text:'b'}]})),'invalid_request');
  assert.equal(controller.current(),session);
});

test('edits beyond the existing source or package UTF-8 byte budgets are refused',()=>{
  const {controller}=setup();
  const description=describe(controller);
  const main=resource(description,'main.ts'), helper=resource(description,'helper.ts');
  const session=controller.current();
  // Fewer UTF-16 units than the allowance, but more UTF-8 bytes.
  const wide='😀'.repeat(AUTHORING_SOURCE_BYTES/4)+'x';
  assert.ok(wide.length<AUTHORING_SOURCE_BYTES);
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,text:wide}]})),'source_too_large',main.id);
  const half='x'.repeat(AUTHORING_NON_IMAGE_BYTES/2);
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,text:half},{resource:helper.id,version:helper.version,text:half}]})),'package_too_large');
  assert.equal(controller.current(),session);
  assert.equal(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,text:half}]})).ok,true);
});

test('maximum-sized source is retrievable through stable pages that never split characters',()=>{
  const manifest=file('package.json','manifest','{}\n');
  // The largest source the package's aggregate non-image allowance admits beside its manifest.
  const budget=Math.min(AUTHORING_SOURCE_BYTES,AUTHORING_NON_IMAGE_BYTES-manifest.bytes);
  const head='a'.repeat(PAGE_UNITS-1)+'😀';
  const source=head+'c'.repeat(budget-(PAGE_UNITS-1)-4);
  assert.equal(new TextEncoder().encode(source).length,budget);
  const {controller}=setup(packageView([manifest,file('main.ts','source',source)]));
  const description=describe(controller);
  const main=resource(description,'main.ts');
  const pages=[];
  let offset=0;
  for(;;){
    const page=controller.handle(request(description,{kind:'read',resource:main.id,version:main.version,offset}));
    assert.equal(page.ok,true);
    assert.equal(page.result.totalUnits,source.length);
    assert.ok(page.result.text.length<=PAGE_UNITS);
    pages.push(page.result.text);
    if(page.result.nextOffset===null) break;
    offset=page.result.nextOffset;
  }
  assert.equal(pages[0].length,PAGE_UNITS-1);
  assert.equal(pages.join(''),source);
  refused(controller.handle(request(description,{kind:'read',resource:main.id,offset:PAGE_UNITS})),'invalid_range',main.id);
  refused(controller.handle(request(description,{kind:'read',resource:main.id,offset:PAGE_UNITS-1,limit:1})),'invalid_range',main.id);
  refused(controller.handle(request(description,{kind:'read',resource:main.id,offset:source.length+1})),'invalid_range',main.id);
  const end=controller.handle(request(description,{kind:'read',resource:main.id,offset:source.length}));
  assert.deepEqual([end.result.text,end.result.nextOffset],['',null]);
  refused(controller.handle(request(description,{kind:'read',resource:main.id,limit:0})),'invalid_request');
  refused(controller.handle(request(description,{kind:'read',resource:main.id,limit:PAGE_UNITS+1})),'invalid_request');
  // One more byte exceeds the aggregate allowance; a same-size human edit stays within it and stales the continuation.
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,ranges:[{from:source.length,to:source.length,text:'d'}]}]})),
    'package_too_large');
  controller.update(session=>editFile(session,'main.ts',{text:source.slice(0,-1)+'d',start:source.length,end:source.length},
    {start:source.length-1,end:source.length},{type:'insertReplacementText',data:'d',composing:false}));
  assert.equal(new TextEncoder().encode(draft(controller,'main.ts').text).length,budget);
  refused(controller.handle(request(description,{kind:'read',resource:main.id,version:main.version,offset})),'stale_version',main.id);
});

test('replacement states and App blocks refuse edits while an in-flight Save keeps admitting them',()=>{
  const {controller}=setup();
  for(const kind of ['catalog','refresh','exit','duplicate']){
    controller.update(session=>beginPending(session,{kind}));
    const description=describe(controller);
    assert.equal(description.pending,kind);
    const main=resource(description,'main.ts');
    const session=controller.current();
    refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,text:'x'}]})),'ineligible');
    assert.equal(controller.current(),session);
    assert.equal(controller.handle(request(description,{kind:'read',resource:main.id})).ok,true);
    controller.update(current=>({...current,pending:null}));
  }
  type(controller,'main.ts','A');
  const ticket=saveTicket(controller.current(),'main.ts');
  controller.update(session=>beginPending(session,{kind:'save',path:'main.ts'}));
  const description=describe(controller);
  const main=resource(description,'main.ts');
  const blocked=controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,text:'x'}]}),()=>'The host no longer reports this Edit lease');
  refused(blocked,'ineligible');
  assert.equal(blocked.error.message,'The host no longer reports this Edit lease');
  assert.equal(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,ranges:[{from:0,to:0,text:'B'}]}]}),()=>null).ok,true);
  controller.update(session=>applySave(session,ticket,{owner,committed_revision:'rev-2',view:null,refresh_error:null}));
  const after=describe(controller);
  assert.equal(after.savedRevision,'rev-2');
  assert.equal(resource(after,'main.ts').dirty,true);
  assert.equal(draft(controller,'main.ts').text,'BA'+MAIN);
  assert.equal(draft(controller,'main.ts').base,'A'+MAIN);
});

test('malformed or unsupported requests are refused before any state changes',()=>{
  const {controller}=setup();
  const description=describe(controller);
  const main=resource(description,'main.ts');
  const session=controller.current();
  refused(controller.handle(request(description,{kind:'save'})),'unsupported_operation');
  refused(controller.handle(request(description,{kind:'describe',extra:1})),'invalid_request');
  refused(controller.handle(request(description,{kind:'read',resource:main.id,extra:1})),'invalid_request');
  refused(controller.handle(request(description,{kind:'read',resource:'has space'})),'invalid_request');
  refused(controller.handle(request(description,{kind:'edit',edits:[]})),'invalid_request');
  refused(controller.handle({...request(description,{kind:'describe'}),owner:'has space'}),'invalid_request');
  refused(controller.handle({id:'x',package:null,operation:{kind:'describe'}}),'invalid_request');
  refused(controller.handle(null),'invalid_request');
  refused(controller.handle(request(description,{kind:'notices',limit:0})),'invalid_request');
  refused(controller.handle(request(description,{kind:'notices',cursor:'has space'})),'invalid_request');
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,fields:[]}]})),'invalid_request');
  assert.equal(controller.current(),session);
});

test('coordinator seams check the exact scope and expose identities without a writable draft',()=>{
  const empty=new AuthoringController(()=>{});
  assert.equal(empty.checkScope(null,null),null);
  assert.equal(empty.checkScope('authoring-1-1',null).code,'no_edit_owner');
  const {controller}=setup();
  const description=controller.describe();
  assert.equal(controller.checkScope(description.owner,description.package),null);
  assert.equal(controller.checkScope(null,null).code,'stale_owner');
  assert.equal(controller.checkScope(description.owner,description.package+'x').code,'stale_package');
  const main=resource(description,'main.ts');
  assert.deepEqual(controller.resource(main.id),{path:'main.ts',kind:'source',version:main.version});
  assert.equal(controller.resource('unknown'),null);
  controller.update(()=>null);
  assert.equal(controller.resource(main.id),null);
});

test('notices coalesce human and agent changes after a cursor and report gaps instead of adopting owners',()=>{
  const {controller}=setup();
  const start=describe(controller);
  const notices=(description,cursor,limit)=>controller.handle(request(description,{kind:'notices',
    ...(cursor===undefined?{}:{cursor}),...(limit===undefined?{}:{limit})})).result;
  for(const text of ['x','y','z']) type(controller,'main.ts',text);
  const typed=notices(start,start.cursor);
  const main=resource(describe(controller),'main.ts');
  assert.deepEqual(typed,{cursor:describe(controller).cursor,gap:null,changes:[{resource:main.id,path:'main.ts',kind:'source',version:main.version}],more:false,
    savedRevision:'rev-1'});
  assert.equal(JSON.stringify(typed).includes('xyz'),false,'notices never carry text');
  assert.deepEqual(notices(start,typed.cursor).changes,[]);
  // An agent edit and later typing arrive in change order; a limit pages them.
  const description=describe(controller);
  const helper=resource(description,'helper.ts');
  assert.equal(controller.handle(request(description,{kind:'edit',edits:[{resource:helper.id,version:helper.version,text:'export const b = 3;\n'}]})).ok,true);
  type(controller,'main.ts','!');
  const first=notices(start,typed.cursor,1);
  assert.deepEqual([first.changes.map(change=>change.path),first.more],[['helper.ts'],true]);
  const second=notices(start,first.cursor,1);
  assert.deepEqual([second.changes.map(change=>change.path),second.more,second.cursor],[['main.ts'],false,describe(controller).cursor]);
  assert.deepEqual([notices(start).gap,notices(start).changes],['start',[]]);
  // A successor owner: the old scope gets a gap without a cursor, and the old cursor never resumes on the successor.
  controller.update(()=>openSession(packageView(files,'authoring-1-2')));
  const stale=notices(start,second.cursor);
  assert.deepEqual([stale.gap,stale.cursor,stale.changes],['owner',null,[]]);
  const successor=describe(controller);
  assert.equal(notices(successor,second.cursor).gap,'expired');
  assert.deepEqual(notices(successor,successor.cursor),{cursor:successor.cursor,gap:null,changes:[],more:false,savedRevision:'rev-1'});
});

test('notice history beyond its bound reports an expired cursor rather than a partial stream',()=>{
  const sources=Array.from({length:NOTICE_HISTORY+1},(_,index)=>file(`src/f${index}.ts`,'source','x\n'));
  const {controller}=setup(packageView([file('package.json','manifest','{}\n'),...sources]));
  const start=describe(controller);
  controller.update(session=>applyExternalEdits(session,sources.map(item=>({path:item.path,text:'y\n',range:{start:0,end:0}}))));
  const head=describe(controller).cursor;
  assert.deepEqual(controller.handle(request(start,{kind:'notices',cursor:start.cursor})).result,
    {cursor:head,gap:'expired',changes:[],more:false,savedRevision:'rev-1'});
  assert.deepEqual(controller.handle(request(start,{kind:'notices',cursor:head})).result.gap,null);
});

test('validation diagnostics name the checked saved revision and the unsaved drafts it excluded',()=>{
  const {controller}=setup();
  type(controller,'main.ts','A');
  controller.update(session=>applyValidation(session,{token:'authoring-1-1',revision:'rev-1'},{owner,revision:'rev-1',valid:false,diagnostics:[
    {category:'TypeScript',message:'compile failed',context:{diagnostics:[{message:"Cannot find name 'x' in /private/packages/a/main.ts",module:'main.ts',line:2,column:3,code:2304}]}},
    {category:'AuthoringRecoveryRequired',message:'interrupted',context:{package_path:'/private/packages/a'}}]}));
  const description=describe(controller);
  const main=resource(description,'main.ts');
  assert.deepEqual(description.validation,{revision:'rev-1',valid:false,current:true,diagnostics:2,excluded:[main.id]});
  const report=controller.handle(request(description,{kind:'read',resource:resource(description,'validation').id})).result;
  assert.equal(report.text.includes('/private'),false);
  assert.deepEqual(JSON.parse(report.text),{revision:'rev-1',valid:false,diagnostics:[
    {category:'TypeScript',message:"Cannot find name 'x' in <package>/main.ts",path:'main.ts',line:2,column:3,code:2304},
    {category:'AuthoringRecoveryRequired',message:'interrupted',path:null,line:null,column:null,code:null}]});
});

const RATIO_SCHEMA='{"version":1,"type":"object","additionalProperties":false,"required":[],"properties":{"ratio":{"type":"number"},"label":{"type":"string"}}}';
const PRESET='{"package_id":"example.a","schema_version":1,"options":{},"note":"kept"}';
const PRESET_PATH='profiles/default.json';
function metadataView(manifest='{"version":1,"package_id":"example.a","sources":["main.ts"]}\n'){
  return packageView([file('package.json','manifest',manifest),file('main.ts','source',MAIN),file('schema.json','schema',RATIO_SCHEMA),file(PRESET_PATH,'profile',PRESET)]);
}
// What the preset form does with typed numeric input.
function typeRatio(controller,text){
  controller.update(session=>{
    const current=session.drafts.get(PRESET_PATH);
    const preset=JSON.parse(current.text);
    const form=editValues(loadValues(JSON.parse(RATIO_SCHEMA),preset.options,current.typed),{...preset.options,ratio:text},{kind:'replace',path:'$.ratio'});
    return replaceFile(session,PRESET_PATH,formatJson(current.base,{...preset,options:form.committed}),typedText(form));
  });
}
function shownRatio(controller){
  const current=draft(controller,PRESET_PATH);
  const form=loadValues(JSON.parse(RATIO_SCHEMA),JSON.parse(current.text).options,current.typed);
  return [form.stored.length,form.draft.ratio];
}
const options=controller=>JSON.parse(draft(controller,PRESET_PATH).text).options;

test('structured metadata edits keep incomplete form input and take one chronological undo step per file',()=>{
  const {controller}=setup(metadataView());
  typeRatio(controller,'1.');
  const description=describe(controller);
  const preset=resource(description,PRESET_PATH), manifest=resource(description,'package.json');
  const page=controller.handle(request(description,{kind:'read',resource:preset.id})).result;
  assert.deepEqual([page.typed,JSON.parse(page.text).options.ratio],[[{path:'$.ratio',text:'1.'}],'1.']);
  const response=controller.handle(request(description,{kind:'edit',edits:[
    {resource:preset.id,version:preset.version,fields:[{field:'option',name:'label',value:'fast'}]},
    {resource:manifest.id,version:manifest.version,fields:[{field:'helper',enabled:true}]}]}));
  assert.equal(response.ok,true);
  assert.deepEqual(JSON.parse(draft(controller,PRESET_PATH).text),{package_id:'example.a',schema_version:1,options:{ratio:'1.',label:'fast'},note:'kept'});
  assert.deepEqual(shownRatio(controller),[0,'1.'],'the untouched partial number is still the person\'s editable text');
  assert.deepEqual(JSON.parse(draft(controller,'package.json').text),{version:1,package_id:'example.a',sources:['main.ts'],dependencies:{'@mado/helper':'1.0.0'}});
  assert.deepEqual([draft(controller,'package.json').undo.length,draft(controller,'main.ts').undo.length],[1,0]);
  // Later human form input is undone first; then the agent step, each restoring the partial number as editable text.
  typeRatio(controller,'1.5');
  assert.deepEqual(shownRatio(controller),[0,1.5]);
  controller.update(session=>undoFile(session,PRESET_PATH));
  assert.deepEqual([shownRatio(controller),options(controller).label],[[0,'1.'],'fast']);
  controller.update(session=>undoFile(session,PRESET_PATH));
  assert.deepEqual([shownRatio(controller),options(controller).label],[[0,'1.'],undefined]);
});

test('malformed metadata is never rewritten and a refused field or composing form rejects the whole batch',()=>{
  const {controller}=setup(metadataView('{"version":1,"package_id":"example.a",\n'));
  const description=describe(controller);
  const manifest=resource(description,'package.json'), main=resource(description,'main.ts'), schema=resource(description,'schema.json');
  const preset=resource(description,PRESET_PATH);
  const session=controller.current();
  const insert={resource:main.id,version:main.version,ranges:[{from:0,to:0,text:'x'}]};
  for(const [edit,code,id] of [
    [{resource:manifest.id,version:manifest.version,fields:[{field:'helper',enabled:true}]},'malformed_document',manifest.id],
    [{resource:schema.id,version:schema.version,fields:[{field:'helper',enabled:true}]},'unsupported_field',schema.id],
    [{resource:schema.id,version:schema.version,fields:[{field:'default',name:'missing',value:1}]},'invalid_field',schema.id],
    [{resource:preset.id,version:preset.version,fields:[{field:'option',name:'ratio',value:1},{field:'repair',issue:{code:'root'}}]},'invalid_field',preset.id],
    [{resource:schema.id,version:schema.version,text:'{}'},'unsupported_resource',schema.id],
    [{resource:main.id+'x',version:main.version,fields:[{field:'helper',enabled:true}]},'unknown_resource',main.id+'x'],
  ]){
    refused(controller.handle(request(description,{kind:'edit',edits:[insert,edit]})),code,id);
    assert.equal(controller.current(),session);
  }
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,fields:[{field:'helper',enabled:true}]}]})),'unsupported_resource',main.id);
  controller.update(current=>beginComposition(current,PRESET_PATH,{start:0,end:0}));
  refused(controller.handle(request(describe(controller),{kind:'edit',edits:[{resource:preset.id,version:preset.version,fields:[{field:'option',name:'label',value:'x'}]}]})),
    'composing',preset.id);
  assert.equal(draft(controller,PRESET_PATH).text,PRESET);
  assert.equal(draft(controller,'package.json').text,'{"version":1,"package_id":"example.a",\n');
});

test('a late metadata Save advances the saved base while a newer agent field edit stays dirty and current',()=>{
  const {controller}=setup(metadataView());
  typeRatio(controller,'2');
  const ticket=saveTicket(controller.current(),PRESET_PATH);
  controller.update(session=>beginPending(session,{kind:'save',path:PRESET_PATH}));
  const preset=resource(describe(controller),PRESET_PATH);
  const response=controller.handle(request(describe(controller),{kind:'edit',edits:[{resource:preset.id,version:preset.version,fields:[{field:'option',name:'label',value:'late'}]}]}));
  assert.equal(response.ok,true);
  const version=response.result.resources[0].version;
  controller.update(session=>applySave(session,ticket,{owner,committed_revision:'rev-2',view:null,refresh_error:null}));
  const description=describe(controller);
  const after=resource(description,PRESET_PATH);
  assert.deepEqual([after.version,after.dirty,description.savedRevision,draft(controller,PRESET_PATH).base],[version,true,'rev-2',ticket.text]);
  assert.deepEqual(options(controller),{ratio:2,label:'late'});
  assert.equal(controller.handle(request(description,{kind:'edit',edits:[{resource:after.id,version,fields:[{field:'option',name:'label',value:'later'}]}]})).ok,true);
});

// Owned recognition metadata only: no pixels. The retained trial carries an observation that must never be disclosed.
const BASIS={frame_width:200,frame_height:100,content:{x:0,y:0,width:200,height:100}};
const box=(left,top,right,bottom)=>({left,top,right,bottom});
function zone(id,name,edges,expected=null){
  return {id,name,revision:1,kind:'ocr',region:regionFromEdges(edges,BASIS),expected,template:null,saved:null};
}
const CAPTURE_A={version:1,rounding:1,basis:BASIS,definitions:[zone('r1','HP',box(10,10,30,30),'100'),zone('r2','MP',box(50,10,70,30))],template_rights:null};
const CAPTURE_B={...CAPTURE_A,definitions:[zone('r1','Other',box(20,20,40,40))]};
function recognitionView(){
  const captures=[{capture_id:'capture-a',document:CAPTURE_A},{capture_id:'capture-b',document:CAPTURE_B}];
  const observation={kind:'ocr',text_contract:'facade',zones:[{id:'r1',outcome:'recognized',regions:[{text:'RAW-OCR-OBSERVATION',confidence:0.9,
    bounds:{x:10,y:10,width:20,height:20},geometry:[]}]}]};
  const trial={owner,revision:'rev-1',document_revision:1,frame_id:'frame-1',frame_revision:1,capture_id:'capture-a',configuration_revision:'cfg-1',sample_id:null,
    stale:false,controller:{run:'run-1',state:'finished',operation:'recognition_trial',error:null,progress:[],dropped_logs:0,workspace_id:'a',workspace_revision:1,
      result:{version:1,operation:'recognition_trial',environment_identity:'env',primary:null,cleanup:{},child_reaped:true,forced:false,exit_code:0,result:observation}}};
  return {owner,revision:'rev-1',document:CAPTURE_A,saved_document:CAPTURE_A,capture_id:'capture-a',captures,saved_captures:captures,migration_required:false,
    document_revision:1,basis_confirmed:true,other_bases_confirmed:true,
    frame:{id:'frame-1',width:200,height:100,revision:1,confirmed:true,historical:false,historical_capture_at_ms:null},
    capabilities:{max_ocr_zones:8,diagnostic_regions:256,diagnostic_bytes:262144,expected_bytes:4096,image_policy:{}},
    staged_crop_ids:[],stale_crop_ids:[],staged_crop_sources:{},configuration_revision:'cfg-1',trial};
}
function withRecognition(){
  const opened=setup();
  opened.controller.update(session=>({...session,recognition:openRecognition(recognitionView())}));
  return opened;
}
const recognize=(controller,change)=>controller.update(session=>({...session,recognition:change(session.recognition)}));
function part(description,role,capture='capture-a',definition){
  return description.resources.find(item=>item.role===role&&(role==='context'||item.capture===capture)
    &&(definition===undefined||item.path===`recognition/${capture}/definitions/${definition}`));
}

test('recognition context is the current draft metadata without pixels, observations or machine paths',()=>{
  const {controller}=withRecognition();
  recognize(controller,state=>renameDefinition(state,'r1','Health'));
  const description=describe(controller);
  const context=part(description,'context');
  assert.deepEqual([context.path,context.kind,context.capture,context.dirty],['recognition','recognition','capture-a',true]);
  assert.deepEqual(description.recognition,{capture:'capture-a',frame:{width:200,height:100},confirmed:true,maxOcrZones:8});
  const page=controller.handle(request(description,{kind:'read',resource:context.id,version:context.version})).result;
  const value=JSON.parse(page.text);
  assert.deepEqual(value.captures.map(capture=>[capture.capture,capture.active,capture.definitions.map(item=>item.name)]),
    [['capture-a',true,['Health','MP']],['capture-b',false,['Other']]]);
  assert.deepEqual([value.captures[0].definitions[0].edges,value.captures[0].definitions[0].expected],[box(10,10,30,30),'100']);
  for(const hidden of ['RAW-OCR-OBSERVATION','frame-1','run-1','cfg-1','/private/packages']) assert.equal(JSON.stringify(page).includes(hidden),false,hidden);
  const r1=part(description,'definition','capture-a','r1');
  assert.deepEqual(JSON.parse(controller.handle(request(description,{kind:'read',resource:r1.id})).result.text),
    {capture:'capture-a',id:'r1',name:'Health',kind:'ocr',region:CAPTURE_A.definitions[0].region,expected:'100',template:null});
  assert.deepEqual([r1.dirty,part(description,'definition','capture-a','r2').dirty,part(description,'basis').dirty],[true,false,false]);
});

test('the savable recognition aggregate versions draft documents, not frames, checked rows or unrelated saves',()=>{
  const {controller}=withRecognition();
  const version=()=>part(describe(controller),'context').version;
  const before=version();
  recognize(controller,state=>toggleTrial(selectDefinition(state,'r2'),'r1'));
  // A Script save re-reads recognition with a new package revision and a newer frame of the same size.
  recognize(controller,state=>applyView(state,{...state.view,revision:'rev-2',frame:{...state.view.frame,id:'frame-2',revision:2}}));
  assert.equal(version(),before);
  // A pending pixel choice changes what recognition Save publishes; restoring it is a new state.
  recognize(controller,state=>toggleCrop(state,'r1'));
  const cropped=version();
  recognize(controller,state=>toggleCrop(state,'r1'));
  assert.equal(new Set([before,cropped,version()]).size,3);
});

test('snippet dependencies follow only contributing recognition inputs and never reuse a restored version',()=>{
  const {controller}=withRecognition();
  const ocr=controller.recognitionDependencies('capture-a',['r1'],'ocr_recognize');
  const setupInputs=controller.recognitionDependencies('capture-a',[],'game_content');
  const template=controller.recognitionDependencies('capture-a',['r2'],'template_recognize');
  assert.deepEqual([ocr,setupInputs,template].map(list=>list.map(item=>controller.resource(item.resource).role)),
    [['definition'],['basis'],['definition','basis','template']]);
  for(const [capture,ids,flavor,code] of [['capture-a',['r1'],'game_content','invalid_request'],['capture-a',[],'ocr_recognize','invalid_request'],
    ['capture-a',['r1','r1'],'ocr_recognize','invalid_request'],['capture-a',['r9'],'ocr_recognize','unknown_resource'],['capture-z',[],'game_content','unknown_resource'],
    ['capture-a',['r1'],'pixels','invalid_request']]) assert.equal(controller.recognitionDependencies(capture,ids,flavor).code,code);
  const insert=dependencies=>{
    const description=describe(controller);
    const main=resource(description,'main.ts');
    return controller.handle(request(description,{kind:'edit',edits:[{resource:main.id,version:main.version,ranges:[{from:0,to:0,text:'// snippet\n'}]}],dependencies}));
  };
  // Checked rows, the selected row, another definition and a newer frame with the same confirmed basis are not inputs.
  recognize(controller,state=>renameDefinition(toggleTrial(selectDefinition(state,'r2'),'r2'),'r2','Mana'));
  recognize(controller,state=>applyView(state,{...state.view,frame:{...state.view.frame,id:'frame-2',revision:2}}));
  assert.equal(insert([...ocr,...setupInputs]).ok,true);
  // A contributing region changed and restored by Undo is stale, so known-stale source is never inserted.
  recognize(controller,state=>setRegion(state,'r1','region',regionFromEdges(box(12,10,30,30),BASIS)));
  recognize(controller,undoRecognition);
  assert.deepEqual(controller.current().recognition.document.definitions[0].region,CAPTURE_A.definitions[0].region);
  const script=draft(controller,'main.ts').text;
  refused(insert(ocr),'stale_version',ocr[0].resource);
  assert.equal(draft(controller,'main.ts').text,script);
  // A basis change stales setup source but not grouped OCR source, which reads the pasted basis.
  const fresh=controller.recognitionDependencies('capture-a',['r1'],'ocr_recognize');
  recognize(controller,state=>setContent(state,{x:10,y:0,width:190,height:100}));
  refused(insert(setupInputs),'stale_version',setupInputs[0].resource);
  assert.equal(insert(fresh).ok,true);
  // A Script edit without recognition dependencies is independent of all of this.
  assert.equal(insert([]).ok,true);
});

test('one batch edits Script and recognition fields with separate histories and keeps the person\'s view',()=>{
  const {controller}=withRecognition();
  recognize(controller,state=>toggleTrial(selectDefinition(state,'r2'),'r1'));
  const before=controller.current();
  const description=describe(controller);
  const main=resource(description,'main.ts');
  const r1=part(description,'definition','capture-a','r1'), context=part(description,'context');
  const blocks=[];
  const block=(session,recognition)=>{
    blocks.push(recognition);
    return null;
  };
  const response=controller.handle(request(description,{kind:'edit',edits:[
    {resource:main.id,version:main.version,ranges:[{from:0,to:0,text:'// zones\n'}]},
    {resource:r1.id,version:r1.version,fields:[{field:'name',value:'Health'},{field:'region',edges:box(12,10,40,32)}]},
    {resource:context.id,version:context.version,fields:[{field:'create',name:'Gold',edges:box(100,50,140,70)}]}]}),block);
  assert.equal(response.ok,true);
  assert.deepEqual(blocks,[true]);
  const after=controller.current();
  const state=after.recognition;
  assert.deepEqual(state.document.definitions.map(item=>item.name),['Health','MP','Gold']);
  assert.deepEqual(mapRegion(state.document.definitions[0].region,BASIS),box(12,10,40,32));
  assert.deepEqual([state.selected,state.trialIds,after.selected,after.destination,after.reveal],
    [before.recognition.selected,before.recognition.trialIds,before.selected,before.destination,before.reveal]);
  assert.deepEqual([state.undo.length-before.recognition.undo.length,draft(controller,'main.ts').undo.length],[3,1]);
  const created=response.result.created[0];
  assert.equal(created.path,'recognition/capture-a/definitions/r3');
  assert.equal(controller.resource(created.resource).version,created.version);
  // Recognition Undo reverses recognition only; the removed definition is a notice with no version.
  const cursor=describe(controller).cursor;
  recognize(controller,undoRecognition);
  assert.deepEqual(controller.current().recognition.document.definitions.map(item=>item.name),['Health','MP']);
  assert.equal(draft(controller,'main.ts').text,'// zones\n'+MAIN);
  const changes=controller.handle(request(description,{kind:'notices',cursor})).result.changes;
  assert.deepEqual(changes.find(change=>change.resource===created.resource),
    {resource:created.resource,path:created.path,kind:'recognition_definition',version:null,capture:'capture-a',role:'definition'});
  assert.ok(changes.some(change=>change.role==='context'));
});

test('recognition edits refuse inactive captures, read-only provenance, invalid geometry and saved-asset writes as a whole',()=>{
  const {controller}=withRecognition();
  const description=describe(controller);
  const main=resource(description,'main.ts');
  const insert={resource:main.id,version:main.version,ranges:[{from:0,to:0,text:'x'}]};
  const other=part(description,'definition','capture-b','r1'), template=part(description,'template'), basis=part(description,'basis');
  const r1=part(description,'definition','capture-a','r1'), context=part(description,'context');
  const session=controller.current();
  for(const [edit,code,id] of [
    [{resource:other.id,version:other.version,fields:[{field:'name',value:'x'}]},'ineligible',other.id],
    [{resource:template.id,version:template.version,fields:[{field:'name',value:'x'}]},'unsupported_resource',template.id],
    [{resource:basis.id,version:basis.version,fields:[{field:'name',value:'x'}]},'unsupported_field',basis.id],
    [{resource:r1.id,version:r1.version,fields:[{field:'region',edges:box(190,10,210,30)}]},'invalid_geometry',r1.id],
    [{resource:context.id,version:context.version,text:'{}'},'unsupported_resource',context.id],
    [{resource:r1.id,version:r1.version,fields:[{field:'saved',value:{asset:'other',sha256:'0',width:1,height:1}}]},'invalid_request',undefined],
  ]){
    refused(controller.handle(request(description,{kind:'edit',edits:[insert,edit]})),code,id);
    assert.equal(controller.current(),session);
  }
  // The App can refuse recognition targets (for example during a capture change) while Script edits continue.
  const capturing=(current,recognition)=>recognition?'The capture is changing':null;
  refused(controller.handle(request(description,{kind:'edit',edits:[{resource:r1.id,version:r1.version,fields:[{field:'name',value:'x'}]}]}),capturing),'ineligible');
  assert.equal(controller.handle(request(description,{kind:'edit',edits:[insert]}),capturing).ok,true);
});
