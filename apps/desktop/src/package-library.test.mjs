import {test} from 'node:test';
import assert from 'node:assert/strict';
import {buildLibrary, filterLibrary, groupSummary, initialArtifact, libraryOutcome, locate, resolveHighlight, savedArtifact} from './package-library.ts';

const KEY_A='a'.repeat(64), KEY_B='b'.repeat(64);
function verified(packageId,artifact,{key=KEY_A,version='1.0.0',name='Sample Tool',author='Package Author',games={}}={}){
  return {package_id:packageId,artifact_id:artifact,fault:null,facts:{package_id:packageId,release_version:version,name,description:'',author_name:author,
    game_names:games,key_id:key,revision:'rev-1',artifact_id:artifact,files:1,bytes:10}};
}
function broken(packageId,artifact){
  return {package_id:packageId,artifact_id:artifact,facts:null,fault:{category:'Integrity',message:'archive bytes differ',context:null}};
}
const versionsOf=group=>group.versions.map(item=>[item.facts.release_version,item.release.artifact_id]);

test('versions of one signer and package form one group ordered by authored label without choosing a latest',()=>{
  const releases=[verified('example.a','c3',{version:'1.9.0'}),verified('example.a','b2',{version:'1.10.0'}),verified('example.a','a1',{version:'1.10.0'})];
  const library=buildLibrary(releases);
  assert.equal(library.groups.length,1);
  assert.deepEqual(versionsOf(library.groups[0]),[['1.10.0','a1'],['1.10.0','b2'],['1.9.0','c3']]);
  assert.deepEqual(buildLibrary([...releases].reverse()),library);
  assert.equal(initialArtifact(library.groups[0],null),null);
  assert.equal(initialArtifact(library.groups[0],{package_id:'example.a',artifact_id:'c3'}),'c3');
  assert.equal(initialArtifact(buildLibrary([releases[0]]).groups[0],null),'c3');
});

test('signing keys separate groups that share a name, author and package ID; declared metadata never merges or splits them',()=>{
  const library=buildLibrary([verified('example.a','a1'),verified('example.a','b1',{key:KEY_B}),
    verified('example.a','a2',{name:'Renamed Tool',author:'Another Author',version:'2.0.0'})]);
  assert.deepEqual(library.groups.map(group=>[group.key_id,group.package_id,versionsOf(group).map(([,artifact])=>artifact)]),
    [[KEY_A,'example.a',['a1','a2']],[KEY_B,'example.a',['b1']]]);
  assert.deepEqual(library.groups[0].names,['Sample Tool','Renamed Tool']);
});

test('row metadata follows the saved exact artifact or a browsed candidate, not a matching version label or latest',()=>{
  const candidate=verified('example.a','a1',{author:'Candidate Author'});
  const saved=verified('example.a','z1',{author:'Saved Author'});
  const newer=verified('example.a','n2',{version:'2.0.0',author:'Newer Author'});
  const group=buildLibrary([candidate,newer,saved]).groups[0];
  const savedRef={package_id:'example.a',artifact_id:'z1'};
  assert.deepEqual(groupSummary(group,savedRef,null),{authors:['Saved Author'],version:group.versions[1]});
  const highlight={kind:'group',id:group.id,artifact:'a1'};
  assert.deepEqual(groupSummary(group,savedRef,highlight),{authors:['Candidate Author'],version:group.versions[0]});
  assert.equal(initialArtifact(group,savedRef),'z1','browsing does not change the saved exact version');
  assert.equal(groupSummary(group,savedRef,{kind:'group',id:'another-group',artifact:'a1'}).version.release,saved);
  assert.equal(groupSummary(group,savedRef,{kind:'group',id:group.id,artifact:'removed'}).version,null,
    'refresh does not substitute another exact artifact for a vanished candidate');
});

test('an unchosen multi-version row summarizes authors deterministically and leaves every exact version accessible',()=>{
  const releases=[verified('example.a','c3',{version:'1.9.0',author:'Team A'}),
    verified('example.a','b2',{version:'1.10.0',author:'Team B'}),verified('example.a','a1',{version:'1.10.0',author:'Team B'})];
  const group=buildLibrary(releases).groups[0];
  const summary=groupSummary(group,null,null);
  assert.deepEqual(summary,{authors:['Team B','Team A'],version:null});
  assert.deepEqual(groupSummary(buildLibrary([...releases].reverse()).groups[0],null,null),summary);
  assert.deepEqual(versionsOf(group),[['1.10.0','a1'],['1.10.0','b2'],['1.9.0','c3']]);
  assert.equal(initialArtifact(group,null),null);
  assert.equal(groupSummary(group,null,{kind:'group',id:group.id,artifact:'c3'}).version.release,releases[0]);
  const sole=buildLibrary([releases[0]]).groups[0];
  assert.deepEqual(groupSummary(sole,null,null),{authors:['Team A'],version:sole.versions[0]});
});

test('whitespace-only declared authors remain explicit without rewriting signed metadata',()=>{
  const blank=verified('example.a','a1',{author:' \t '});
  const named=verified('example.a','b2',{author:'  Declared Author  '});
  const group=buildLibrary([blank,named]).groups[0];
  assert.deepEqual(groupSummary(group,null,null).authors,[null,'Declared Author']);
  assert.deepEqual(groupSummary(group,{package_id:'example.a',artifact_id:'a1'},null).authors,[null]);
  assert.deepEqual(groupSummary(group,null,{kind:'group',id:group.id,artifact:'b2'}).authors,['Declared Author']);
  assert.equal(blank.facts.author_name,' \t ');
  assert.equal(named.facts.author_name,'  Declared Author  ');
});

test('an entry without verified facts stays an attributable problem outside every trusted group',()=>{
  const library=buildLibrary([verified('example.a','a1'),broken('example.a','f9')]);
  assert.deepEqual(versionsOf(library.groups[0]),[['1.0.0','a1']]);
  assert.deepEqual(library.problems.map(item=>item.artifact_id),['f9']);
  const byId=filterLibrary(library,'  EXAMPLE.A ','all');
  assert.equal(byId.groups.length,1);
  assert.deepEqual(byId.problems.map(item=>item.artifact_id),['f9']);
  assert.deepEqual(filterLibrary(library,'sample','all').problems,[]);
  const problems=filterLibrary(library,'','problems');
  assert.deepEqual(problems.groups,[]);
  assert.deepEqual(problems.problems.map(item=>item.artifact_id),['f9']);
});

test('a localized game name on one version reveals the whole group regardless of case or presentation language',()=>{
  const library=buildLibrary([verified('example.a','a1',{games:{ja:'放置ゲーム',en:'Idle Game'}}),verified('example.a','a2',{version:'2.0.0'}),verified('other.b','b1',{name:'Other'})]);
  for (const query of ['放置','idle GAME']) {
    const shown=filterLibrary(library,query,'all');
    assert.deepEqual(shown.groups.map(group=>[group.package_id,versionsOf(group).length]),[['example.a',2]],query);
  }
  assert.deepEqual(filterLibrary(library,'Idle','problems').groups,[]);
});

test('list failure, an unread list, an empty installation and an empty search stay distinct',()=>{
  const fault={category:'Io',message:'denied',context:null};
  const empty={groups:[],problems:[]};
  assert.equal(libraryOutcome(null,fault,false,empty,''),'failed');
  assert.equal(libraryOutcome(null,fault,true,empty,''),'loading');
  assert.equal(libraryOutcome(null,null,false,empty,''),'unread');
  assert.equal(libraryOutcome([],null,false,empty,'x'),'empty');
  const releases=[verified('example.a','a1')];
  const library=buildLibrary(releases);
  assert.equal(libraryOutcome(releases,null,false,filterLibrary(library,'missing','all'),'missing'),'noMatches');
  assert.equal(libraryOutcome(releases,null,false,filterLibrary(library,'','problems'),''),'noProblems');
  assert.equal(libraryOutcome(releases,null,false,filterLibrary(library,'sample','all'),'sample'),'shown');
});

test('a highlight hidden by search or removed by refresh never resolves to another release',()=>{
  const first=buildLibrary([verified('example.a','a1'),verified('example.a','a2',{version:'2.0.0'}),broken('example.a','f9')]);
  const highlight={kind:'group',id:first.groups[0].id,artifact:'a2'};
  assert.equal(resolveHighlight(filterLibrary(first,'other','all'),highlight),null);
  assert.equal(resolveHighlight(filterLibrary(first,'','all'),highlight).version.release.artifact_id,'a2');
  const refreshed=buildLibrary([verified('example.a','a1')]);
  const resolved=resolveHighlight(refreshed,highlight);
  assert.equal(resolved.group.id,highlight.id);
  assert.equal(resolved.version,null);
  const problem={kind:'problem',package_id:'example.a',artifact_id:'f9'};
  assert.equal(resolveHighlight(first,problem).release.artifact_id,'f9');
  assert.equal(resolveHighlight(refreshed,problem),null);
});

test('only an installed reference locates a release; a directory under the packages folder stays a directory',()=>{
  const library=buildLibrary([verified('example.a','a1'),broken('example.b','f9')]);
  assert.equal(savedArtifact({kind:'directory',path:`/data/pkgs/${KEY_A}/example.a`}),null);
  assert.equal(savedArtifact({kind:'custom_archive',path:'/data/example.zip'}),null);
  assert.equal(locate(library,savedArtifact({kind:'installed',package_id:'example.a',artifact_id:'a1'})).version.release.artifact_id,'a1');
  assert.equal(locate(library,{package_id:'example.b',artifact_id:'f9'}).kind,'problem');
  assert.equal(locate(library,{package_id:'example.a',artifact_id:'gone'}),null);
});
