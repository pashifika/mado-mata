import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {messages,renderMessage,LocalFault} from './i18n.ts';
import {interpolate} from './i18n-format.ts';
import {readDraft,readSettingsDraft,environmentDraft,DEFAULT_NOTIFICATIONS,SUPPORTED_PROFILES} from './state.ts';
import {applyIfCurrent,bindSelection,editDraft,updateBound,viewLogs,workspaceFromView} from './workspace.ts';
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

const adapterArguments = {
  'app.workspaceLimit': [[8]], 'app.closeConfirm': [['profile-A']],
  'app.workspaceAria': [['profile-A']], 'app.activity': [['profile-A']], 'app.closedLabel': [['profile-A']],
  'app.unknownOrigin': [['workspace-A']], 'app.retainedOutcome': [[2]], 'app.attention': [['reason-A']],
  'app.ready': [['profile-A']], 'app.descriptorLimit': [[4096]], 'app.profileSaved': [['profile-A','P']],
  'app.profileRenamed': [['profile-A']], 'app.deletedElsewhere': [['profile-A']],
  'app.profilesImported': [[0,0],[2,1]], 'app.profilesImportPartial': [[1,0]],
  'app.updatedElsewhere': [['profile-A']], 'app.renamedElsewhere': [['profile-A']],
  'app.stopFailed': [['diagnostic-A']], 'app.settingsSaved': [['profile-A']],
  'app.waitForCommand': [['savingSettings'],['creatingWorkspace'],['importingProfiles'],['repairingProfile']],
  'app.recoverySaved': [['profile-A','P']], 'app.recoveryEarlierSaved': [['profile-A','P']],
  'app.inspectionOutcomes': [[0,1],[2,0]],
  'app.editOwner': [['workspace-A']], 'app.authoringWait': [['savingFile'],['exitingEdit']],
  'app.authoringDuplicated': [['/pkg-B','pkg-B']], 'app.authoringSaved': [['src/main.ts','rev-A']],
  'app.authoringEarlierSaved': [['src/main.ts','rev-A']], 'app.authoringSavedRefreshFailed': [['src/main.ts','rev-A']],
  'app.authoringCatalogSaved': [['rev-A']], 'app.authoringCatalogRefreshFailed': [['rev-A']], 'app.authoringRefreshed': [['rev-A']],
  'app.authoringDiskChanged': [[1],[2]], 'app.authoringValidated': [['rev-A']], 'app.authoringInvalid': [['rev-A',1],['rev-A',2]],
  'app.authoringEarlierValidated': [['rev-A']], 'app.authoringDiscarded': [['src/main.ts']],
  'ui.phase': [['idle']], 'ui.operation': [['run']], 'ui.lane': [['controlled']],
  'ui.severity': [['ERROR']], 'ui.entryOutcome': [['Returned']],
  'ui.common.revision': [['package-A',2]],
  'ui.settings.invalid': [[1,''],[2,''],[1,'Logs'],[2,'Logs']],
  'ui.settings.invalidCount': [[2]], 'ui.settings.wait': [['reason-A']],
  'ui.settings.cards': [[1],[2]], 'ui.settings.seconds': [[1],[2]],
  'ui.settings.logLimitHelp': [[null],[1000]],
  'ui.environment.truncated': [[1],[2]], 'ui.environment.staleHelp': [[['reason-A','reason-B']]],
  'ui.environment.wait': [['reason-A']], 'ui.environment.unsupported': [['profile-A']],
  'ui.environment.profileLabel': SUPPORTED_PROFILES.map(({profile})=>[profile]),
  'ui.environment.mismatch': [['model-A','language-A','provider-A','runtime-A']],
  'ui.run.heading': [['profile-A']], 'ui.run.owned': [[null],['run-A']],
  'ui.run.settled': [[null],['run-A']], 'ui.run.inspected': [[2]],
  'ui.run.descriptorHelp': [[4096]],
  'ui.run.startUses': [[null,false,null,null],['profile-A',false,null,null],[null,true,null,null],['profile-A',true,'environment-A','corpus-A']],
  'ui.run.olderRevision': [[1,2]], 'ui.run.stopTarget': [[null],['run-A']],
  'ui.run.runKind': [['controlled']],
  'ui.target.argument': [[1],[32]], 'ui.target.revision': [[0],[2]],
  'ui.result.truncated': [[1],[2]], 'ui.result.disclosureHelp': [[false,512],[true,512]],
  'ui.schema.fieldAction': [[false,'$.field'],[true,'$.field']],
  'ui.schema.omittedDefault': [['default-A']], 'ui.schema.unknown': [['$.field','value-A']],
  'ui.schema.ordered': [1,2].flatMap(count=>[[count,undefined,undefined],[count,0,undefined],[count,undefined,4],[count,0,4]]),
  'ui.schema.moveUp': [['$.field']], 'ui.schema.moveDown': [['$.field']],
  'ui.schema.removeItem': [['$.field']], 'ui.schema.invalid': [['value-A']],
  'ui.schema.length': [[0,undefined],[0,4]],
  'ui.schema.numeric': [['number',undefined,undefined],['number',0,undefined],['number',undefined,4],['number',0,4]],
  'ui.schema.mismatch': [['number','value-A']], 'ui.schema.replace': [['number']],
  'ui.logs.missing': [[42]], 'ui.logs.filtered': [[1],[2]], 'ui.logs.stats': [[2,3,4,1000]],
  'ui.logs.fileError': [['diagnostic-A']], 'ui.logs.lossHelp': [[false],[true]],
  'ui.notifications.dismiss': [[42]], 'ui.notifications.event': [[42]],
  'ui.workspaces.attention': [[1],[2]], 'ui.workspaces.label': [['workspace-A']],
  'ui.workspaces.open': [[2]], 'ui.workspaces.closeLabel': [['workspace-A']],
  'ui.workspaces.closeTitle': [['workspace-A']],
  'ui.bootstrap.state': [['loading'],['setup'],['ready'],['recovery'],['future-state']],
  'ui.bootstrap.legacyHelp': [['/Users/example/Library/Application Support/dev.madomata.desktop']],
  'ui.bootstrap.block': ['pendingRestore','active','authoring','command','archivePath','confirm','discard'].map(kind=>[kind]),
  'ui.create.count': [[0,64],[80,80]], 'ui.create.openLimit': [[8]], 'ui.create.savedLimit': [[64]],
  'ui.create.errors': ['internalEmpty','internalLong','internalChars','displayBlank','displayLong','displayControl'].map(kind=>[kind]),
  'ui.reopen.directory': [['pkg-A']], 'ui.reopen.archive': [['pkg-A']], 'ui.reopen.references': [[2]],
  'ui.reopen.saved': [[3,64]], 'ui.reopen.limit': [[8]], 'ui.reopen.reopenLabel': [['workspace-A']],
  'ui.guidance.scope': [['workspace-A']],
  'ui.recognition.frame': [[1920,1080]], 'ui.recognition.limits': [[32,'16,777,216',16,'4,194,304']],
  'ui.recognition.count': [[9,256]], 'ui.recognition.undoHelp': [[3,64]], 'ui.recognition.trialFor': [['HP']], 'ui.recognition.cropFor': [['HP']],
  'ui.recognition.savedFacts': [['recognition/crops/r1.png',40,12]], 'ui.recognition.threshold': [['0.9',8]], 'ui.recognition.bytes': [[12,4096]],
  'ui.recognition.issue': ['name','region','search','searchSmall'].map(code=>[code]),
  'ui.recognition.notice': ['definitionLimit','documentLimit','expectedLimit','nameLimit','invalidGeometry','staleEdit'].map(code=>[code]),
  'ui.recognition.block': ['noDocument','noFrame','unconfirmed','confirmed','running','noCapability','empty','overLimit','mixedKinds','templateSingle','noSample','invalid','noChanges','rights','cropFrame','kind','waitText','templateUnsaved'].map(code=>[code]),
  'ui.recognition.engineLimit': [[8]], 'ui.recognition.selection': [[3,8],[3,null]],
  'ui.recognition.freshness': ['fresh','stale','historical'].map(value=>[value]), 'ui.recognition.freshnessHelp': ['fresh','stale','historical'].map(value=>[value]),
  'ui.recognition.regions': [[1],[2]], 'ui.recognition.rect': [[10,20,300,40]], 'ui.recognition.templateThreshold': [['0.9']], 'ui.recognition.score': [['0.97']],
  'ui.recognition.cleanupState': [[true,false],[true,true],[false,false]], 'ui.recognition.crops': [[1],[2]],
  'ui.recognition.basis': [[1920,1080,0,60,1920,960]], 'ui.recognition.defaultName': [[3]], 'ui.recognition.percent': [[150]],
  'ui.recognition.scale': [[1920,1080,50]], 'ui.recognition.searchLabel': [['HP']],
  'ui.authoring.scope': [['workspace-A']], 'ui.authoring.kind': [['source'],['source_map'],['future-kind']],
  'ui.authoring.unsavedFiles': [[1],[2]], 'ui.authoring.binary': [[42]], 'ui.authoring.editorLabel': [['src/main.ts']],
  'ui.authoring.position': [[1,1]], 'ui.authoring.lines': [[1],[2]],
  'ui.authoring.matches': [[0,null,false],[3,null,false],[3,2,false],[10000,null,true]],
  'ui.authoring.valid': [['rev-A']], 'ui.authoring.invalid': [['rev-A',1],['rev-A',2]], 'ui.authoring.staleValidation': [['rev-A']],
  'ui.authoring.location': [['src/main.ts',3,7]], 'ui.authoring.goTo': [['src/main.ts:3:7']],
  'ui.authoring.renameHeading': [['src/main.ts']], 'ui.authoring.confirmRemove': [['src/main.ts']],
  'ui.authoring.fileActions': [['src/main.ts'],['src/lib/']],
  'ui.authoring.block': ['pending','refresh','missing','clean','binary','manifestDirty','fileDirty'].map(kind=>[kind]),
  'ui.authoring.blockedOther': [['workspace-A']], 'ui.authoring.startBlocked': [['workspace-A']], 'ui.authoring.stripKind': [['pkg-A']],
  'ui.authoring.dirtyHeading': ['exit','duplicate','close','closeTab'].map(kind=>[kind]),
  'ui.authoring.presetLabel': [['default']], 'ui.authoring.sourceMapLabel': [['main.ts']],
  'ui.authoring.jsonLocation': ['unexpectedCharacter','unexpectedEnd','invalidString','invalidNumber','trailingContent','depth','future-problem'].map(problem=>[problem,3,7]),
  'ui.authoring.normalizedNumbers': [['1.0, 1e3']], 'ui.authoring.normalizedDuplicates': [['$.a, $.b[0].c']],
  'ui.authoring.runtimeLabel': [['typescript'],['javascript'],['lua']], 'ui.authoring.runtimeUnsupported': [['lua']],
  'ui.authoring.entryName': [['readiness'],['workflow']], 'ui.authoring.entryModule': [['Readiness']], 'ui.authoring.entryFunction': [['Workflow']],
  'ui.authoring.moduleUndeclared': [['src/main.ts']], 'ui.authoring.helper': [['@mado/helper','1.0.0']],
  'ui.authoring.assetDimensions': [['0','0'],['640','480']], 'ui.authoring.addFieldHeading': [['$.recognition']],
  'ui.authoring.schemaType': ['object','array','string','number','integer','boolean','future-type'].map(type=>[type]),
  'ui.authoring.unsupportedType': [['"date"']],
  'ui.authoring.boundLabel': ['minLength','maxLength','minimum','maximum','minItems','maxItems'].map(key=>[key]),
  'ui.authoring.enumValue': [[1]], 'ui.authoring.enumRemove': [[2]], 'ui.authoring.nestedDefault': [['0.5']],
  'ui.authoring.schemaProblems': [[1],[2]],
  'ui.authoring.schemaIssue': ['node','type','version','rootType','additionalProperties','properties','required','items','reversed','enum','depth']
    .map(code=>[{code}]).concat([[{code:'keyword',key:'format'}],[{code:'bound',key:'minimum'}]]),
  'ui.authoring.schemaRepair': ['node','type','version','rootType','additionalProperties','properties','required','items','reversed','enum','depth']
    .map(code=>[{code}]).concat([[{code:'keyword',key:'format'}],[{code:'bound',key:'minimum'}]]),
  'ui.authoring.presetIssue': ['root','packageId','schemaVersion','options'].map(code=>[{code}]).concat([[{code:'member',key:'extra'}]]),
  'ui.authoring.presetRepair': ['root','packageId','schemaVersion','options'].map(code=>[{code}]).concat([[{code:'member',key:'extra'}]]),
  'ui.authoring.characters': [[1],[2]],
  'ui.authoring.sourceMapIssue': ['root','version','sources','mappings','sourceRoot'].map(code=>[{code}]).concat([[{code:'source',index:0}]]),
  'ui.recovery.outcomeStatus': [['saved'],['repair_required'],['storage_failed'],['future-status']],
  'ui.recovery.confirmReset': [['profile-A','P','workspace-A','pkg-A']],
};

for (const locale of ['en','ja']) {
  test(`${locale} presentation messages resolve every adapter and grammar branch`,()=>{
    function check(value,path='') {
      if (typeof value === 'function') {
        assert.ok(Object.hasOwn(adapterArguments,path),`Missing consumer inputs for ${path}`);
        for (const args of adapterArguments[path]) {
          assert.doesNotMatch(value(...args),/\{[A-Za-z][A-Za-z0-9]*\}/,`${path}: ${JSON.stringify(args)}`);
        }
      } else if (typeof value === 'string') {
        assert.doesNotMatch(value,/\{[A-Za-z][A-Za-z0-9]*\}/,`Unformatted consumer message: ${path}`);
      } else {
        for (const [key,entry] of Object.entries(value)) check(entry,path ? `${path}.${key}` : key);
      }
    }
    check(messages[locale]);
  });
}

test('known runner entry outcomes are localized while future outcomes remain distinguishable',()=>{
  for (const outcome of ['Returned','NotStarted','FailedOrNotStarted','NotExecuted','Unobserved']) {
    assert.notEqual(messages.ja.ui.entryOutcome(outcome),outcome);
  }
  assert.equal(messages.ja.ui.entryOutcome('FutureOutcome'),'FutureOutcome');
});

test('interpolation keeps parameter-like and markup-like user values literal',()=>{
  assert.equal(interpolate('{name} / {id} / {name}',{name:'{id}<b>利用者</b>',id:7}),'{id}<b>利用者</b> / 7 / {id}<b>利用者</b>');
  assert.throws(()=>interpolate('{missing}',{}),/Missing message parameter/);
});

test('a retained asynchronous notice renders in the current language without changing attribution or later drafts',()=>{
  const selection=id=>({workspace_id:id,revision:1,internal_name:id,display_name:'Tab '+id,package_path:`/packages/${id}`,package:{package_id:id,schema_identity:'schema',inventory_identity:'inventory',schema:{type:'object',properties:{count:{type:'integer'}}},profiles:{},target:null,target_identity:null},profiles:[],profiles_error:null});
  const view=id=>({workspace_id:id,revision:0,internal_name:id,display_name:'Tab '+id,selection:null,source_error:null,saved_package:null});
  let workspaces=[bindSelection(workspaceFromView(view('a')),selection('a')),bindSelection(workspaceFromView(view('b')),selection('b'))];
  workspaces=workspaces.map(item=>updateBound(item,bound=>editDraft(bound,{count:item.id === 'a' ? '7' : '9'})));
  const before=structuredClone(workspaces);
  const name='{id}<profile>';
  const completed=applyIfCurrent(workspaces,{id:'a',revision:1},item=>({...item,notice:{key:'profileSaved',args:[name,'P']}}));
  const notice=completed[0].notice;
  const english=renderMessage('en',notice);
  const japanese=renderMessage('ja',notice);
  assert.notEqual(japanese,english);
  for (const text of [english,japanese]) {assert.ok(text.includes(name));assert.ok(text.includes('P'));}
  assert.deepEqual(completed[0].bound.draft,before[0].bound.draft);
  assert.equal(completed[0].bound.draftRevision,before[0].bound.draftRevision);
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
  const draft={locale:'ja',logLimit:'invalid',notifications:{...DEFAULT_NOTIFICATIONS},environment:environmentDraft(null),backupDirectory:'',packagesRoot:''};
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
