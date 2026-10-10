import {test} from 'node:test';
import assert from 'node:assert/strict';
import {COLLABORATION_REQUEST, CollaborationBridge} from './collaboration-bridge.ts';
import {AuthoringController} from './authoring-controller.ts';
import {editFile, openSession} from './authoring.ts';

const DESCRIBE={id:'1',owner:null,package:null,operation:{kind:'describe'}};
const settle=()=>new Promise(resolve=>setImmediate(resolve));
function deferred(){
  let resolve;
  const promise=new Promise(done=>{resolve=done;});
  return {promise,resolve};
}
// The Tauri seam: records every command and delivers events to the listeners still registered.
function fakeHost({ready=()=>({instance:'instance-1',protocol:1}),claim=ticket=>({ticket,request:DESCRIBE}),listening=null}={}){
  const calls=[];
  const listeners=new Set();
  return {
    calls,listeners,
    names:()=>calls.map(([command])=>command),
    async invoke(command,args){
      calls.push([command,args]);
      if(command==='collaboration_ready') return ready();
      if(command==='collaboration_claim') return claim(args.ticket);
      return null;
    },
    async listen(event,handler){
      if(listening) await listening.promise;
      const listener={event,handler};
      listeners.add(listener);
      return ()=>listeners.delete(listener);
    },
    emit(payload){
      for(const listener of [...this.listeners]) if(listener.event===COLLABORATION_REQUEST) listener.handler({event:COLLABORATION_REQUEST,id:1,payload});
    },
  };
}


test('unclaimable, malformed and invalid tickets are never handled',async()=>{
  const host=fakeHost({claim:ticket=>ticket==='expired'?null:{ticket:'other',request:DESCRIBE}});
  let handled=0;
  const stop=new CollaborationBridge(host).start(()=>{handled+=1; return {owner:null,ok:true,result:{}};});
  await settle();
  host.emit({ticket:'has space'});
  host.emit({ticket:42});
  host.emit(null);
  host.emit({ticket:'expired'});
  host.emit({ticket:'mismatched'});
  await settle();
  assert.equal(handled,0);
  assert.deepEqual(host.calls.slice(1).map(([command,args])=>[command,args.ticket]),
    [['collaboration_claim','expired'],['collaboration_claim','mismatched'],['collaboration_reply','mismatched']]);
  assert.equal(host.calls.at(-1)[1].response.error.code,'invalid_request');
  stop();
});

test('teardown during a claim replies unavailable without handling it',async()=>{
  const claimed=deferred();
  const host=fakeHost({claim:()=>claimed.promise});
  let handled=0;
  const stop=new CollaborationBridge(host).start(()=>{handled+=1; return {owner:null,ok:true,result:{}};});
  await settle();
  host.emit({ticket:'ticket-1'});
  await settle();
  stop();
  claimed.resolve({ticket:'ticket-1',request:DESCRIBE});
  await settle();
  assert.equal(handled,0);
  const reply=host.calls.find(([command])=>command==='collaboration_reply');
  assert.deepEqual(reply[1].response,{owner:null,ok:false,error:{code:'unavailable',message:reply[1].response.error.message,outcome:'not_applied'}});
});

test('a failure while handling reports an unknown outcome instead of a rollback',async()=>{
  const host=fakeHost();
  const stop=new CollaborationBridge(host).start(()=>{throw new Error('render failed');});
  await settle();
  host.emit({ticket:'ticket-1'});
  await settle();
  const reply=host.calls.find(([command])=>command==='collaboration_reply');
  assert.equal(reply[1].response.ok,false);
  assert.equal(reply[1].response.error.code,'internal');
  assert.equal(reply[1].response.error.outcome,'unknown');
  stop();
});

test('a quick stop and restart keeps registration and invalidation in call order',async()=>{
  const first=deferred();
  let readies=0;
  const host=fakeHost({ready:()=>++readies===1?first.promise:{instance:'instance-1',protocol:1}});
  const bridge=new CollaborationBridge(host);
  const handler=()=>({owner:null,ok:true,result:{}});
  const stopFirst=bridge.start(handler);
  await settle();
  stopFirst();
  const stopSecond=bridge.start(handler);
  await settle();
  assert.deepEqual(host.names(),['collaboration_ready']);
  first.resolve({instance:'instance-1',protocol:1});
  await settle();
  assert.deepEqual(host.names(),['collaboration_ready','collaboration_unavailable','collaboration_ready']);
  assert.equal(host.listeners.size,1);
  stopSecond();
  await settle();
  assert.deepEqual(host.names().at(-1),'collaboration_unavailable');
  assert.equal(host.listeners.size,0);
});

test('stopping before the listener exists never registers the window',async()=>{
  const listening=deferred();
  const host=fakeHost({listening});
  const stop=new CollaborationBridge(host).start(()=>({owner:null,ok:true,result:{}}));
  stop();
  listening.resolve();
  await settle();
  assert.deepEqual(host.names(),['collaboration_unavailable']);
  assert.equal(host.listeners.size,0);
});

for (const [name, ready] of [
  ['incompatible', () => ({instance:'instance-1', protocol:2})],
  ['failed', () => {throw new Error('CollaborationUnavailable');}],
]) test(`${name} registration prevents subsequent draft handling`, async () => {
  const host = fakeHost({ready});
  let handled = 0;
  const stop = new CollaborationBridge(host).start(() => {handled += 1; return {owner:null, ok:true, result:{}};});
  await settle();
  host.emit({ticket:'ticket-1'});
  await settle();
  assert.equal(handled, 0);
  assert.equal(host.listeners.size, 0);
  stop();
});

test('a claimed external edit reaches the same live draft the UI edits',async()=>{
  const published=[];
  const controller=new AuthoringController(session=>published.push(session));
  controller.update(()=>openSession({owner:{workspace:{workspace_id:'a',revision:1},token:'authoring-1-1'},package_path:'/private/packages/a',
    package_id:'example.a',revision:'rev-1',files:[{path:'main.ts',kind:'source',text:'one\n',bytes:4}]}));
  controller.update(session=>editFile(session,'main.ts',{text:'zero\none\n',start:5,end:5},{start:0,end:0},{type:'insertFromPaste',data:null,composing:false}));
  const description=controller.handle(DESCRIBE).result;
  const main=description.resources[0];
  const operations=[{kind:'read',resource:main.id},{kind:'edit',edits:[{resource:main.id,version:main.version,ranges:[{from:9,to:9,text:'two\n'}]}]}];
  const host=fakeHost({claim:ticket=>({ticket,request:{id:ticket,owner:description.owner,package:description.package,operation:operations[Number(ticket.slice(-1))]}})});
  const stop=new CollaborationBridge(host).start(request=>controller.handle(request));
  await settle();
  host.emit({ticket:'ticket-0'});
  await settle();
  host.emit({ticket:'ticket-1'});
  await settle();
  const replies=host.calls.filter(([command])=>command==='collaboration_reply').map(([,args])=>args.response);
  assert.equal(replies[0].result.text,'zero\none\n');
  assert.equal(replies[1].ok,true);
  assert.equal(controller.current().drafts.get('main.ts').text,'zero\none\ntwo\n');
  assert.deepEqual(controller.current().drafts.get('main.ts').range,{start:5,end:5});
  assert.equal(published.at(-1),controller.current());
  stop();
});
