import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, mkdir, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {AuthoringController} from './authoring-controller.ts';
import {beginComposition, fileDirty, openSession, replaceFile, undoFile} from './authoring.ts';
import {saveFileDraft, saveFiles} from './authoring-publication.ts';

const owner={workspace:{workspace_id:'publication',revision:1},token:'publication-owner'};
const source={
  'main.ts':'export function readiness() { return "Ready"; }\nexport function workflow() {}\n',
  'helper.ts':'export const limit = 1;\n',
  'other.ts':'export const name = "original";\n',
};
function view(texts,revision='rev-1',lease=owner){
  return {owner:lease,revision,package_path:'/fixture/package',package_id:'example.publication',
    files:Object.entries(texts).map(([path,text])=>({path,text,kind:'source',bytes:Buffer.byteLength(text)}))};
}
function setup(){
  const controller=new AuthoringController(()=>{});
  controller.update(()=>openSession(view(source)));
  return controller;
}
function targets(controller,paths){
  const resources=controller.describe().resources;
  return paths.map(path=>{
    const item=resources.find(resource=>resource.path===path);
    return {resource:item.id,version:item.version};
  });
}
function ports(save){
  return {save,available:()=>true,refreshRecognition:async()=>true,
    fault:error=>({category:'Io',message:String(error),context:null})};
}

test('a late Save commits its captured text without erasing later input or its Undo step',async()=>{
  const controller=setup();
  const saved=source['main.ts']+'// agent change\n';
  const later=saved+'// human after Save started\n';
  controller.update(state=>replaceFile(state,'main.ts',saved));
  const [target]=targets(controller,['main.ts']);
  let release;
  const held=new Promise(resolve=>{release=resolve;});
  const pending=saveFileDraft(controller,'main.ts',ports(async()=>held),target);
  controller.update(state=>replaceFile(state,'main.ts',later));
  const [newer]=targets(controller,['main.ts']);
  release({owner,committed_revision:'rev-2',view:view({...source,'main.ts':saved},'rev-2'),refresh_error:null});
  const receipt=await pending;
  assert.equal(receipt.committedRevision,'rev-2');
  assert.equal(receipt.error,null);
  assert.deepEqual([controller.current().drafts.get('main.ts').base,controller.current().drafts.get('main.ts').text],[saved,later]);
  assert.equal(fileDirty(controller.current().drafts.get('main.ts')),true);
  assert.deepEqual(targets(controller,['main.ts']),[newer]);
  controller.update(state=>undoFile(state,'main.ts'));
  assert.equal(controller.current().drafts.get('main.ts').text,saved);
  assert.equal(fileDirty(controller.current().drafts.get('main.ts')),false);
});

test('Save all reports the actual persisted prefix and stops at a filesystem failure',async()=>{
  const root=await mkdtemp(join(tmpdir(),'mado-publication-'));
  try {
    const controller=setup();
    const texts={...source};
    await Promise.all(Object.entries(texts).map(([path,text])=>writeFile(join(root,path),text)));
    for(const path of Object.keys(source)) controller.update(state=>replaceFile(state,path,source[path]+'// unsaved\n'));
    const requested=targets(controller,Object.keys(source));
    // An ordinary external path change after the view was opened makes the second file unwritable.
    await rm(join(root,'helper.ts'));
    await mkdir(join(root,'helper.ts'));
    let revision=1;
    const receipt=await saveFiles(controller,requested,ports(async(_,ticket)=>{
      await writeFile(join(root,ticket.path),ticket.text);
      texts[ticket.path]=ticket.text;
      const committed_revision=`rev-${++revision}`;
      return {owner,committed_revision,view:view(texts,committed_revision),refresh_error:null};
    }));
    assert.deepEqual(receipt.committed,[{path:'main.ts',revision:'rev-2'}]);
    assert.deepEqual(receipt.remaining,requested.slice(1));
    assert.equal(receipt.failure.path,'helper.ts');
    assert.equal(receipt.failure.committedRevision,null);
    assert.equal(await readFile(join(root,'main.ts'),'utf8'),source['main.ts']+'// unsaved\n');
    assert.equal(await readFile(join(root,'other.ts'),'utf8'),source['other.ts']);
    assert.equal(fileDirty(controller.current().drafts.get('main.ts')),false);
    assert.equal(fileDirty(controller.current().drafts.get('helper.ts')),true);
    assert.equal(fileDirty(controller.current().drafts.get('other.ts')),true);
    assert.equal(controller.current().pending,null);
  } finally {await rm(root,{recursive:true,force:true});}
});

test('a later target changed during Save all is retained, not published under its old token',async()=>{
  const controller=setup();
  controller.update(state=>replaceFile(state,'main.ts',source['main.ts']+'// first\n'));
  controller.update(state=>replaceFile(state,'helper.ts','export const limit = 2;\n'));
  const requested=targets(controller,['main.ts','helper.ts']);
  let calls=0;
  const receipt=await saveFiles(controller,requested,ports(async(_,ticket)=>{
    calls++;
    controller.update(state=>replaceFile(state,'helper.ts','export const limit = 3;\n'));
    return {owner,committed_revision:'rev-2',view:view({...source,'main.ts':ticket.text},'rev-2'),refresh_error:null};
  }));
  assert.equal(calls,1);
  assert.deepEqual(receipt.committed,[{path:'main.ts',revision:'rev-2'}]);
  assert.equal(receipt.failure.error.category,'AuthoringDraftChanged');
  assert.deepEqual(receipt.remaining,[requested[1]]);
  assert.deepEqual([controller.current().drafts.get('helper.ts').base,controller.current().drafts.get('helper.ts').text],
    [source['helper.ts'],'export const limit = 3;\n']);
});

test('composition refuses Save without clearing composition or entering host work',async()=>{
  const controller=setup();
  controller.update(state=>replaceFile(state,'helper.ts','export const limit = 2;\n'));
  controller.update(state=>beginComposition(state,'helper.ts',{start:0,end:0}));
  const before=controller.current();
  const receipt=await saveFileDraft(controller,'helper.ts',ports(async()=>assert.fail('composition must not reach persistence')),
    targets(controller,['helper.ts'])[0]);
  assert.equal(receipt.error.category,'AuthoringComposing');
  assert.equal(receipt.committedRevision,null);
  assert.equal(controller.current(),before);
});

test('a committed Save with a different refreshed owner requires reconciliation and stops the sequence',async()=>{
  const controller=setup();
  controller.update(state=>replaceFile(state,'main.ts',source['main.ts']+'// first\n'));
  controller.update(state=>replaceFile(state,'helper.ts','export const limit = 2;\n'));
  const requested=targets(controller,['main.ts','helper.ts']);
  let calls=0;
  const receipt=await saveFiles(controller,requested,ports(async(_,ticket)=>{
    calls++;
    return {owner,committed_revision:'rev-2',view:view({...source,'main.ts':ticket.text},'rev-2',{...owner,token:'another-owner'}),refresh_error:null};
  }));
  assert.equal(calls,1);
  assert.deepEqual(receipt.committed,[{path:'main.ts',revision:'rev-2'}]);
  assert.deepEqual(receipt.remaining,[requested[1]]);
  assert.equal(receipt.failure.refreshRequired,true);
  assert.equal(controller.current().refreshRequired,true);
  assert.equal(controller.current().owner.token,owner.token);
  assert.equal(controller.current().drafts.get('main.ts').base,source['main.ts']+'// first\n');
});
