import type { CodexRuntimeModeState } from "../types/codexLocalAccess";

export type CodexLocalAccessPrimaryActionKind = "activate" | "deactivate";

export function isCodexLocalAccessRuntimeActive(
  localAccessLaunchCurrent: boolean,
  runtimeMode: CodexRuntimeModeState | null | undefined,
): boolean {
  return Boolean(
    localAccessLaunchCurrent || runtimeMode?.mode === "cockpit_api_service",
  );
}

export function getCodexLocalAccessPrimaryActionKind(
  localAccessLaunchCurrent: boolean,
  runtimeMode: CodexRuntimeModeState | null | undefined,
): CodexLocalAccessPrimaryActionKind {
  return isCodexLocalAccessRuntimeActive(localAccessLaunchCurrent, runtimeMode)
    ? "deactivate"
    : "activate";
}
