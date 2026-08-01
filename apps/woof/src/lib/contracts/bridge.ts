import { invoke, isTauri as isNativeRuntime } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CommandName, EventName } from "./ipc";

export function isTauri(): boolean {
  const internals = Reflect.get(globalThis, "__TAURI_INTERNALS__") as unknown;
  return (
    isNativeRuntime() &&
    typeof internals === "object" &&
    internals !== null &&
    typeof Reflect.get(internals, "invoke") === "function"
  );
}

export async function invokeCommand<T>(
  command: CommandName,
  args: Record<string, unknown> = {}
): Promise<T> {
  if (!isTauri()) {
    throw new Error("the native woof bridge is unavailable");
  }
  return invoke<T>(command, args);
}

export async function listenEvent<T>(
  event: EventName,
  handler: (payload: T) => void
): Promise<UnlistenFn> {
  if (!isTauri()) {
    throw new Error("the native woof bridge is unavailable");
  }
  return listen<T>(event, ({ payload }) => handler(payload));
}
