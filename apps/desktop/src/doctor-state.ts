import type {DoctorReport} from './doctor-types.ts';
import type {Fault, WorkspaceRef} from './types.ts';
import {sameWorkspace} from './distribution.ts';

export interface DoctorOwner {internalName: string; ref: WorkspaceRef}
export interface DoctorState {
  request: number;
  origin: 'application' | 'library';
  owners: DoctorOwner[];
  package: {package_id: string; artifact_id: string} | null;
  pending: boolean;
  report: DoctorReport | null;
  error: Fault | null;
}
export type DoctorEvent =
  | {type: 'request'; state: DoctorState}
  | {type: 'report'; request: number; report: DoctorReport}
  | {type: 'failed'; request: number; error: Fault}
  | {type: 'close'};

export function reduceDoctor(state: DoctorState | null, event: DoctorEvent): DoctorState | null {
  if (event.type === 'close') return null;
  if (event.type === 'request') return event.state;
  if (!state || !state.pending || state.request !== event.request) return state;
  return event.type === 'report' ? {...state, pending: false, report: event.report, error: null}
    : {...state, pending: false, report: null, error: event.error};
}

// A report names storage owners, not mutation authority. Navigation retains the observed live session.
export function currentDoctorOwner(state: DoctorState, internalName: string | null, live: DoctorOwner[]): WorkspaceRef | null {
  if (internalName === null) return null;
  const observed = state.owners.find(owner => owner.internalName === internalName);
  return observed && live.some(owner => owner.internalName === internalName && sameWorkspace(owner.ref, observed.ref)) ? observed.ref : null;
}
