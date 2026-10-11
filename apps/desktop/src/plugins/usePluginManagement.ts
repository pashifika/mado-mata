import {useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {INITIAL_PLUGIN_STATE, PluginManagement} from './plugin-management.ts';
import type {PluginClient, PluginManagementState, PluginView} from './plugin-management.ts';

const client: PluginClient = {
  inspect: executable => invoke<PluginView>('plugin_inspect', {executable}),
  apply: (observation, action) => invoke<PluginView>('plugin_apply', {observation, action}),
};

export interface PluginManagementControls {state: PluginManagementState; session: PluginManagement}

// Owned by the application root, not the dialog, so closing and reopening Plugins keeps pending work and outcomes.
export function usePluginManagement(): PluginManagementControls {
  const [state, setState] = useState(INITIAL_PLUGIN_STATE);
  const [session] = useState(() => new PluginManagement(client, setState));
  return {state, session};
}
