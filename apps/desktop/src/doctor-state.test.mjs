import {test} from 'node:test';
import assert from 'node:assert/strict';
import {currentDoctorOwner, reduceDoctor} from './doctor-state.ts';

const owner = {internalName: 'saved-a', ref: {workspace_id: 'session-a', revision: 2}};
const request = id => ({type: 'request', state: {request: id, origin: 'application', owners: [owner], package: null, pending: true, report: null, error: null}});
const report = {root: '/isolated', checks: [], truncated: false};

test('dismissal and newer observations fence late reports and errors', () => {
  const first = reduceDoctor(null, request(1));
  const closed = reduceDoctor(first, {type: 'close'});
  assert.equal(reduceDoctor(closed, {type: 'report', request: 1, report}), null);
  const second = reduceDoctor(closed, request(2));
  assert.equal(reduceDoctor(second, {type: 'report', request: 1, report}), second);
  const settled = reduceDoctor(second, {type: 'report', request: 2, report});
  assert.equal(settled.report, report);
  assert.equal(settled.pending, false);
  assert.equal(reduceDoctor(settled, {type: 'failed', request: 1, error: {category: 'Old', message: 'old', context: null}}), settled);
});

test('a stored owner cannot retarget navigation to a reopened or changed session', () => {
  const state = reduceDoctor(null, request(1));
  assert.deepEqual(currentDoctorOwner(state, 'saved-a', [owner]), owner.ref);
  assert.equal(currentDoctorOwner(state, 'saved-a', []), null);
  assert.equal(currentDoctorOwner(state, 'saved-a', [{...owner, ref: {...owner.ref, revision: 3}}]), null);
  assert.equal(currentDoctorOwner(state, 'saved-a', [{...owner, ref: {workspace_id: 'reopened-a', revision: 2}}]), null);
  assert.equal(currentDoctorOwner(state, null, [owner]), null);
});
