<script lang="ts">
  import { onMount } from "svelte";
  import { AlertTriangle, RefreshCw, WifiOff } from "lucide-svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import Mascot from "./Mascot.svelte";
  import {
    COMMANDS,
    EVENTS,
    type CaptureStatus,
    type DatabaseRecoveryPayload,
    type DatabaseRecoveryReason,
    type HealthChangedPayload,
    type HealthState
  } from "$lib/contracts/ipc";
  import { invokeCommand, listenEvent } from "$lib/contracts/bridge";

  let { state: healthState = "offline" }: { state?: HealthState } = $props();
  let retrying = $state(false);
  let currentState = $state<HealthState>("offline");
  let databaseRecovery = $state<DatabaseRecoveryReason | null>(null);

  $effect(() => {
    currentState = healthState;
  });

  const copy = {
    healthy: ["Everything is ready", "Woof’s local service and memory are healthy."],
    starting: ["Starting local service", "The local service is starting. This usually takes a moment."],
    degraded: ["Local service needs attention", "The background service is running with reduced availability."],
    offline: ["The local service is offline", "woof couldn’t recover its background service automatically."]
  } as const;

  const databaseRecoveryCopy: Record<DatabaseRecoveryReason, string> = {
    corrupt: "The previous database did not pass its integrity checks.",
    "incompatible-schema": "The previous database did not match this version of woof.",
    "unsupported-version": "The previous database was created by an unsupported version."
  };

  function validDatabaseRecoveryReason(value: unknown): value is DatabaseRecoveryReason {
    return value === "corrupt" || value === "incompatible-schema" || value === "unsupported-version";
  }

  async function refreshDatabaseRecovery(): Promise<void> {
    const status = await invokeCommand<CaptureStatus>(COMMANDS.captureStatus).catch(() => null);
    const recovery = status?.database_recovery;
    if (recovery?.occurred === true && validDatabaseRecoveryReason(recovery.reason)) {
      databaseRecovery = recovery.reason;
    }
  }

  async function dismissDatabaseRecovery(): Promise<void> {
    databaseRecovery = null;
    await getCurrentWindow().hide().catch(() => undefined);
  }

  async function retry(): Promise<void> {
    retrying = true;
    currentState = "starting";
    try {
      const result = await invokeCommand<{
        healthy: boolean;
        status: string;
        capture?: string;
      }>(COMMANDS.daemonHealth, { restart: true });
      currentState = result.healthy
        ? result.capture === "permission-revoked" || result.capture === "error"
          ? "degraded"
          : "healthy"
        : result.status === "starting" || result.status === "restarting"
          ? "starting"
          : "offline";
    } catch {
      currentState = "offline";
    } finally {
      retrying = false;
    }
  }

  onMount(() => {
    void refreshDatabaseRecovery();
    const unlisteners: Array<() => void> = [];
    void listenEvent<HealthChangedPayload>(EVENTS.healthChanged, ({ state }) => {
      currentState = state;
    }).then((listener) => {
      unlisteners.push(listener);
    });
    void listenEvent<DatabaseRecoveryPayload>(EVENTS.databaseReset, ({ reason }) => {
      if (validDatabaseRecoveryReason(reason)) databaseRecovery = reason;
    }).then((listener) => {
      unlisteners.push(listener);
    });
    return () => unlisteners.forEach((unlisten) => unlisten());
  });
</script>

<svelte:window onfocus={() => void refreshDatabaseRecovery()} />

<main class="health glass">
  <div class="mascot"><Mascot size={114} mood={currentState === "starting" ? "thinking" : "sleeping"} /></div>
  <div class:error={currentState !== "healthy" && currentState !== "starting"} class="state-icon">
    {#if currentState === "offline"}<WifiOff size={22} />
    {:else}<AlertTriangle size={22} />{/if}
  </div>
  {#if databaseRecovery}
    <h1>Local memory started fresh</h1>
    <p>
      Woof isolated the unusable database and initialized fresh local storage.
      {databaseRecoveryCopy[databaseRecovery]} Any isolated copy follows your retention setting and
      stays in woof’s private data directory. No captured content was written to logs.
    </p>
    <div class="actions">
      <button class="primary" onclick={dismissDatabaseRecovery}>Got it</button>
    </div>
    <small>The recovery notice contains no captured content or file paths.</small>
  {:else}
    <h1>{copy[currentState][0]}</h1>
    <p>{copy[currentState][1]}</p>
    <div class="actions">
      <button class="primary" onclick={retry} disabled={retrying}>
        <RefreshCw class={retrying ? "spin" : ""} size={14} /> {retrying ? "Checking…" : "Try again"}
      </button>
    </div>
    <small>Recovery checks stay on this Mac.</small>
  {/if}
</main>

<style>
  .health {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    overflow: hidden;
    border-radius: 24px;
    text-align: center;
    background:
      radial-gradient(circle at 50% 25%, rgba(231, 173, 117, 0.17), transparent 32%),
      var(--glass-strong);
  }

  .mascot {
    height: 108px;
  }

  .state-icon {
    width: 42px;
    height: 42px;
    display: grid;
    place-items: center;
    margin-top: -12px;
    border: 5px solid var(--cream);
    border-radius: 15px;
    color: var(--amber);
    background: color-mix(in srgb, var(--amber) 12%, var(--cream-solid));
  }

  .state-icon.error {
    color: var(--rose);
    background: color-mix(in srgb, var(--rose) 11%, var(--cream-solid));
  }

  h1 {
    margin: 13px 0 8px;
    font-size: 20px;
    letter-spacing: -0.04em;
  }

  p {
    max-width: 300px;
    margin: 0;
    color: var(--ink-muted);
    font-size: 10px;
    line-height: 1.5;
  }

  .actions {
    display: flex;
    gap: 8px;
    margin-top: 18px;
  }

  .actions button {
    height: 35px;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 12px;
    border: 1px solid var(--line);
    border-radius: 11px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 70%, transparent);
    font-size: 9px;
    font-weight: 650;
    cursor: pointer;
  }

  .actions .primary {
    color: #fff8ef;
    border-color: transparent;
    background: var(--brown);
  }

  small {
    margin-top: 17px;
    color: var(--ink-faint);
    font-size: 7px;
  }

  :global(.spin) {
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
