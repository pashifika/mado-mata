import type {Fault} from '../types.ts';

// Wire types of the main-window `plugin_inspect` and `plugin_apply` host commands.
export type PluginAction = 'install' | 'update' | 'uninstall' | 'migrate';
export type PluginInstallState = 'missing' | 'unsupported' | 'incompatible' | 'absent' | 'current' | 'update_available' | 'legacy' | 'conflict';
export interface PluginTarget {executable: string; version: string; user_root: string}
export interface PluginInstallation {version: string; source: string; content: string | null; managed: boolean}
export interface PluginOutcome {action: PluginAction; target: PluginTarget | null; status: 'verified' | 'unknown' | 'failed'; issue: Fault | null}
export interface PluginView {
  included_version: string; included_content: string; minimum_omp_version: string;
  target: PluginTarget | null; installed: PluginInstallation | null; state: PluginInstallState;
  actions: PluginAction[]; observation: string | null; issue: Fault | null; outcome: PluginOutcome | null;
}

export interface PluginClient {
  inspect: (executable: string | null) => Promise<PluginView>;
  apply: (observation: string, action: PluginAction) => Promise<PluginView>;
}

// Presentation order of the actions the host offers; availability is always the host's.
export const ACTION_ORDER: readonly PluginAction[] = ['install', 'update', 'migrate', 'uninstall'];

// An action bound to the exact observation it was reviewed against.
export interface PluginReview {action: PluginAction; view: PluginView}
// Attribution of the latest admitted action: the host's read-back outcome, or the rejection of the request.
export interface PluginResult {action: PluginAction; target: PluginTarget | null; outcome: PluginOutcome | null; error: unknown}

export interface PluginManagementState {
  // Executable field text; blank selects automatic discovery.
  executable: string;
  // Executable argument of the current observation, or of the inspection in flight.
  selection: string | null;
  view: PluginView | null;
  inspecting: boolean;
  inspectError: unknown;
  confirmation: PluginReview | null;
  // A shown confirmation was withdrawn by a newer observation or an executable edit.
  withdrawn: boolean;
  applying: PluginReview | null;
  // A rejected request consumed the reviewed observation; only a fresh read admits another action.
  stale: boolean;
  result: PluginResult | null;
}

export const INITIAL_PLUGIN_STATE: PluginManagementState = {
  executable: '', selection: null, view: null, inspecting: false, inspectError: null,
  confirmation: null, withdrawn: false, applying: null, stale: false, result: null,
};

export function selectedExecutable(text: string): string | null {
  const path = text.trim();
  return path === '' ? null : path;
}

// The edited executable differs from the one the shown observation describes.
export function executableChanged(state: PluginManagementState): boolean {
  return selectedExecutable(state.executable) !== state.selection;
}

export function pluginBusy(state: PluginManagementState): boolean {
  return state.inspecting || state.applying !== null;
}

export function canReview(state: PluginManagementState, action: PluginAction): boolean {
  return !pluginBusy(state) && !state.stale && state.confirmation === null && !executableChanged(state)
    && state.view !== null && state.view.observation !== null && state.view.actions.includes(action);
}

// Application-owned plugin management. It outlives the dialog: closing the dialog withdraws only an unconfirmed
// review, never an admitted inspection or mutation. Inspection and mutation are serialized; success is only the
// host's read-back, never assumed locally.
export class PluginManagement {
  #state: PluginManagementState = INITIAL_PLUGIN_STATE;
  readonly #client: PluginClient;
  readonly #publish: (state: PluginManagementState) => void;

  constructor(client: PluginClient, publish: (state: PluginManagementState) => void) {
    this.#client = client;
    this.#publish = publish;
  }

  readonly current = (): PluginManagementState => this.#state;

  #update(next: Partial<PluginManagementState>): void {
    this.#state = {...this.#state, ...next};
    this.#publish(this.#state);
  }

  // Reads the target of the current observation again.
  readonly refresh = (): Promise<void> => this.#inspect(this.#state.selection);

  // Reads the target named by the executable field.
  readonly check = (): Promise<void> => this.#inspect(selectedExecutable(this.#state.executable));

  async #inspect(selection: string | null): Promise<void> {
    if (pluginBusy(this.#state)) return;
    this.#update({selection, inspecting: true, inspectError: null, confirmation: null, withdrawn: this.#state.confirmation !== null});
    try {
      const view = await this.#client.inspect(selection);
      // A host-retained outcome (for example one left unknown by shutdown) supersedes the local attribution.
      const result = view.outcome ? {action: view.outcome.action, target: view.outcome.target, outcome: view.outcome, error: null} : this.#state.result;
      this.#update({view, inspecting: false, stale: false, result});
    } catch (error) {
      // A failed read leaves no current observation; an earlier one may describe another executable.
      this.#update({view: null, inspecting: false, inspectError: error});
    }
  }

  readonly edit = (executable: string): void => {
    const confirmation = this.#state.confirmation;
    this.#update({executable, confirmation: null, withdrawn: this.#state.withdrawn || confirmation !== null});
  };

  readonly review = (action: PluginAction): void => {
    const view = this.#state.view;
    if (!view || !canReview(this.#state, action)) return;
    this.#update({confirmation: {action, view}, withdrawn: false});
  };

  // Withdraws an unconfirmed review; an admitted action is unaffected.
  readonly dismiss = (): void => {
    if (this.#state.confirmation !== null || this.#state.withdrawn) this.#update({confirmation: null, withdrawn: false});
  };

  readonly confirm = async (): Promise<void> => {
    const review = this.#state.confirmation;
    // Only the reviewed observation that is still shown may be applied.
    if (!review || review.view !== this.#state.view || review.view.observation === null || pluginBusy(this.#state)
      || this.#state.stale || executableChanged(this.#state) || !review.view.actions.includes(review.action)) return;
    const observation = review.view.observation;
    this.#update({confirmation: null, withdrawn: false, applying: review, result: null});
    try {
      const view = await this.#client.apply(observation, review.action);
      this.#update({view, applying: null, stale: false,
        result: {action: review.action, target: view.outcome?.target ?? review.view.target, outcome: view.outcome, error: null}});
    } catch (error) {
      // No read-back arrived: the outcome is not claimed, and the request is never replayed.
      this.#update({applying: null, stale: true, result: {action: review.action, target: review.view.target, outcome: null, error}});
    }
  };
}
