import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {messages,renderMessage,LocalFault} from './i18n.ts';
import {interpolate} from './i18n-format.ts';
import {readDraft,readSettingsDraft,environmentDraft,DEFAULT_NOTIFICATIONS} from './state.ts';
import {applyIfCurrent,editDraft,openWorkspace,viewLogs} from './workspace.ts';
import {emptyStack,ingestCards,interactCard,tickCards} from './notifications.ts';

function resourceContract(value,path='') {
  if (typeof value === 'string') {
    const parameters=[...value.matchAll(/\{([A-Za-z][A-Za-z0-9]*)\}/g)].map(match=>match[1]);
    return [[path,[...new Set(parameters)].sort()]];
  }
  assert.ok(value !== null && typeof value === 'object' && !Array.isArray(value),path);
  return Object.keys(value).sort().flatMap(key=>resourceContract(value[key],`${path}.${key}`));
}

for (const namespace of ['app','ui']) {
  test(`${namespace} language resources have identical keys and named parameters`,async()=>{
    const [en,ja]=await Promise.all(['en','ja'].map(async locale=>JSON.parse(await readFile(new URL(`./locales/${namespace}.${locale}.json`,import.meta.url),'utf8'))));
    assert.deepEqual(resourceContract(ja),resourceContract(en));
  });
}

test('interpolation keeps parameter-like and markup-like user values literal',()=>{
  assert.equal(interpolate('{name} / {id} / {name}',{name:'{id}<b>利用者</b>',id:7}),'{id}<b>利用者</b> / 7 / {id}<b>利用者</b>');
  assert.throws(()=>interpolate('{missing}',{}),/Missing message parameter/);
});

test('a retained asynchronous notice renders in the current language without changing attribution or later drafts',()=>{
  const selection=id=>({workspace_id:id,revision:1,package_path:`/packages/${id}`,package:{package_id:id,schema_identity:'schema',inventory_identity:'inventory',schema:{type:'object',properties:{count:{type:'integer'}}},profiles:{}},profiles:[],profiles_error:null});
  let workspaces=openWorkspace(openWorkspace([],selection('a')),selection('b'));
  workspaces=workspaces.map(item=>editDraft(item,{count:item.id === 'a' ? '7' : '9'}));
  const before=structuredClone(workspaces);
  const name='{id}<profile>';
  const completed=applyIfCurrent(workspaces,{id:'a',revision:1},item=>({...item,notice:{key:'profileSaved',args:[name,'P']}}));
  const notice=completed[0].notice;
  const english=renderMessage('en',notice);
  const japanese=renderMessage('ja',notice);
  assert.notEqual(japanese,english);
  for (const text of [english,japanese]) {assert.ok(text.includes(name));assert.ok(text.includes('P'));}
  assert.deepEqual(completed[0].draft,before[0].draft);
  assert.equal(completed[0].draftRevision,before[0].draftRevision);
  assert.equal(completed[1],workspaces[1]);
  assert.deepEqual(workspaces,before);
  const error=new LocalFault({key:'numericFields'});
  assert.notEqual(renderMessage('en',error.presentation),renderMessage('ja',error.presentation));
});

for (const {scenario,input,value,invalid} of [
  {scenario:'decimal points remain numeric',input:'1.25',value:1.25,invalid:false},
  {scenario:'comma separators remain invalid',input:'1,25',value:'1,25',invalid:true},
  {scenario:'full-width digits remain invalid',input:'１２',value:'１２',invalid:true},
]) {
  test(`localized validation: ${scenario}`,()=>{
    const schema={type:'object',properties:{amount:{type:'number'}}};
    const en=readDraft(schema,{amount:input},'en');
    const ja=readDraft(schema,{amount:input},'ja');
    assert.equal(ja.values.amount,value);
    assert.deepEqual(ja.values,en.values);
    assert.deepEqual(Object.keys(ja.errors),invalid ? ['$.amount'] : []);
    assert.deepEqual(Object.keys(ja.errors),Object.keys(en.errors));
  });
}

test('draft locale and validation presentation do not implicitly change each other',()=>{
  const draft={locale:'ja',logLimit:'invalid',notifications:{...DEFAULT_NOTIFICATIONS},environment:environmentDraft(null)};
  const original=structuredClone(draft);
  const en=readSettingsDraft(draft,'en');
  const ja=readSettingsDraft(draft,'ja');
  assert.equal(en.settings,null);
  assert.equal(ja.settings,null);
  assert.deepEqual(Object.keys(ja.errors),['logLimit']);
  assert.notEqual(ja.errors.logLimit,en.errors.logLimit);
  assert.deepEqual(draft,original);
  const saved=readSettingsDraft({...draft,logLimit:'123'},'en').settings;
  assert.equal(saved.locale,'ja');
  assert.equal(saved.gui_log_limit,123);
});

test('unknown diagnostic identifiers stay raw and localized controls do not replace log or card bodies',()=>{
  const entry={sequence:1,time_ms:0,source:'Script',level:'ERROR',run:'run-a',workspace_id:'a',code:'command.failed',message:'Unknown SDK: {name} <private>',fields:{category:'UnknownSdkCategory',hidden:'not-searchable'}};
  const stack=ingestCards(emptyStack,[entry],DEFAULT_NOTIFICATIONS);
  const card=stack.cards[0];
  const paused=interactCard(stack,card.id,{hovered:true});
  const elapsed=tickCards(paused,1000);
  for (const locale of ['en','ja']) {
    assert.equal(messages[locale].ui.phase('future-phase'),'future-phase');
    assert.equal(messages[locale].ui.operation('future-operation'),'future-operation');
    assert.equal(messages[locale].ui.lane('future-lane'),'future-lane');
    assert.equal(elapsed.cards[0].message,entry.message);
    assert.equal(elapsed.cards[0].id,card.id);
    assert.equal(elapsed.cards[0].remainingMs,card.remainingMs);
    assert.deepEqual(viewLogs([entry],{kind:'workspace',id:'a'},{text:'Unknown SDK',level:'error'}).shown,[entry]);
    assert.deepEqual(viewLogs([entry],{kind:'workspace',id:'a'},{text:'not-searchable',level:''}).shown,[]);
  }
});
