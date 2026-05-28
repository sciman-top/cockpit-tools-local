import { invoke } from '@tauri-apps/api/core';
import type {
  CodexLocalAccessPortCleanupResult,
  CodexLocalAccessRoutingStrategy,
  CodexLocalAccessState,
  CodexLocalApiSafetyPresetId,
  CodexRuntimeIntegrationMode,
  CodexRuntimeModeState,
} from '../types/codexLocalAccess';

export async function getCodexLocalAccessState(): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_get_state');
}

export async function saveCodexLocalAccessAccounts(
  accountIds: string[],
  restrictFreeAccounts: boolean,
): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_save_accounts', {
    accountIds,
    restrictFreeAccounts,
  });
}

export async function removeCodexLocalAccessAccount(
  accountId: string,
): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_remove_account', { accountId });
}

export async function rotateCodexLocalAccessApiKey(): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_rotate_api_key');
}

export async function clearCodexLocalAccessStats(): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_clear_stats');
}

export async function recoverCodexLocalAccessHealth(
  accountId: string,
  model?: string | null,
): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_recover_health', { accountId, model });
}

export async function pauseCodexLocalAccessHealth(
  accountId: string,
): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_pause_health', { accountId });
}

export async function prepareCodexLocalAccessForRestart(): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_prepare_restart');
}

export async function killCodexLocalAccessPort(): Promise<CodexLocalAccessPortCleanupResult> {
  return await invoke('codex_local_access_kill_port');
}

export async function updateCodexLocalAccessPort(
  port: number,
): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_update_port', { port });
}

export async function updateCodexLocalAccessRoutingStrategy(
  strategy: CodexLocalAccessRoutingStrategy,
): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_update_routing_strategy', { strategy });
}

export async function applyCodexLocalAccessSafetyPreset(
  preset: CodexLocalApiSafetyPresetId,
): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_apply_safety_preset', { preset });
}

export async function setCodexLocalAccessEnabled(
  enabled: boolean,
  options?: { force?: boolean },
): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_set_enabled', {
    enabled,
    force: options?.force ?? false,
  });
}

export async function getCodexRuntimeMode(): Promise<CodexRuntimeModeState> {
  return await invoke('codex_runtime_mode_get');
}

export async function setCodexRuntimeMode(
  mode: CodexRuntimeIntegrationMode,
  options?: { force?: boolean },
): Promise<CodexRuntimeModeState> {
  return await invoke('codex_runtime_mode_set', {
    mode,
    force: options?.force ?? false,
  });
}

export async function activateCodexLocalAccess(): Promise<CodexLocalAccessState> {
  return await invoke('codex_local_access_activate');
}
