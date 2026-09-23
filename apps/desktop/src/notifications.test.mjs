import {test} from 'node:test';
import assert from 'node:assert/strict';
import {dismissCard,emptyStack,ingestCards,interactCard,severityOf,tickCards,trimCards} from './notifications.ts';

const preferences={visible_count:2,timeout_seconds:8,show_success:true};
function entry(sequence,code,level='INFO',extra={}){
  return {sequence,time_ms:0,source:'Rust',level,run:null,workspace_id:'a',code,message:`${code} #${sequence}`,fields:null,...extra};
}

test('only outcome codes become cards and repeated delivery of a sequence is ignored',()=>{
  const batch=[entry(1,'application.ready'),entry(2,'script.log','WARN'),entry(3,'profile.saved'),entry(4,'run.state'),entry(5,'run.terminal','ERROR')];
  const stack=ingestCards(emptyStack,batch,preferences);
  assert.deepEqual(stack.cards.map(card=>card.id),[3,5]);
  assert.equal(stack.lastSequence,5);
  const again=ingestCards(stack,[entry(5,'run.terminal','ERROR'),entry(3,'profile.saved')],preferences);
  assert.equal(again,stack);
});

test('severity follows the log level so a failed terminal run is never a success card',()=>{
  assert.equal(severityOf('ERROR'),'error');
  assert.equal(severityOf('WARN'),'warning');
  assert.equal(severityOf('INFO'),'success');
  const stack=ingestCards(emptyStack,[entry(1,'run.terminal','WARN'),entry(2,'command.failed','ERROR',{fields:{category:'Draft',action:'save_profile',path:'/private/root'}})],preferences);
  assert.equal(stack.cards[0].severity,'warning');
  assert.equal(stack.cards[1].severity,'error');
  assert.equal(stack.cards[1].category,'Draft');
  assert.equal(stack.cards[1].action,'save_profile');
  assert.equal(Object.hasOwn(stack.cards[1],'path'),false);
});

test('success suppression never hides warnings or errors',()=>{
  const quiet={...preferences,show_success:false};
  const stack=ingestCards(emptyStack,[entry(1,'profile.saved'),entry(2,'workspace.opened','INFO'),entry(3,'run.terminal','WARN'),entry(4,'command.failed','ERROR')],quiet);
  assert.deepEqual(stack.cards.map(card=>card.id),[3,4]);
  assert.equal(stack.lastSequence,4);
});

test('overflow displaces the oldest visible card and a lower limit trims immediately',()=>{
  const stack=ingestCards(emptyStack,[entry(1,'profile.saved'),entry(2,'profile.renamed'),entry(3,'profile.deleted')],preferences);
  assert.deepEqual(stack.cards.map(card=>card.id),[2,3]);
  const trimmed=trimCards(stack,1);
  assert.deepEqual(trimmed.cards.map(card=>card.id),[3]);
  assert.equal(trimCards(trimmed,1),trimmed);
});

test('each card keeps the timeout captured at creation while later cards use the new preference',()=>{
  const first=ingestCards(emptyStack,[entry(1,'profile.saved')],preferences);
  const second=ingestCards(first,[entry(2,'profile.saved')],{...preferences,timeout_seconds:12});
  assert.equal(second.cards[0].timeoutMs,8000);
  assert.equal(second.cards[1].timeoutMs,12000);
  const ticked=tickCards(second,8000);
  assert.deepEqual(ticked.cards.map(card=>card.id),[2]);
  assert.equal(ticked.cards[0].remainingMs,4000);
});

test('hover or focus pauses only the interacted card and expiry resumes after interaction ends',()=>{
  const stack=ingestCards(emptyStack,[entry(1,'profile.saved'),entry(2,'run.terminal','ERROR')],preferences);
  const hovered=interactCard(stack,2,{hovered:true});
  const ticked=tickCards(hovered,8000);
  assert.deepEqual(ticked.cards.map(card=>card.id),[2]);
  assert.equal(ticked.cards[0].remainingMs,8000);
  const focused=interactCard(interactCard(ticked,2,{hovered:false}),2,{focused:true});
  assert.equal(tickCards(focused,5000).cards[0].remainingMs,8000);
  const released=interactCard(focused,2,{focused:false});
  assert.equal(tickCards(released,5000).cards[0].remainingMs,3000);
  assert.equal(tickCards(released,9000).cards.length,0);
  assert.equal(interactCard(stack,99,{hovered:true}),stack);
});

test('dismissal removes exactly one card and expiry of the stack keeps the sequence high-water mark',()=>{
  const stack=ingestCards(emptyStack,[entry(1,'profile.saved'),entry(2,'run.terminal','ERROR')],preferences);
  const dismissed=dismissCard(stack,1);
  assert.deepEqual(dismissed.cards.map(card=>card.id),[2]);
  assert.equal(dismissCard(dismissed,1),dismissed);
  const expired=tickCards(dismissed,10000);
  assert.equal(expired.cards.length,0);
  assert.equal(expired.lastSequence,2);
  assert.equal(ingestCards(expired,[entry(2,'run.terminal','ERROR')],preferences),expired);
});
