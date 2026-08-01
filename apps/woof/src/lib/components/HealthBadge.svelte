<script lang="ts">
  import { AlertTriangle, Check, LoaderCircle, WifiOff } from "lucide-svelte";
  import type { HealthState } from "$lib/contracts/ipc";

  interface Props {
    state?: HealthState;
    compact?: boolean;
  }

  let { state = "healthy", compact = false }: Props = $props();

  const labels: Record<HealthState, string> = {
    healthy: "All local systems ready",
    starting: "Starting local service",
    degraded: "Local service needs attention",
    offline: "Local service offline"
  };
</script>

<div class:compact class:healthy={state === "healthy"} class="badge" title={labels[state]}>
  {#if state === "healthy"}
    <Check size={13} strokeWidth={2.7} />
  {:else if state === "starting"}
    <LoaderCircle class="spin" size={13} />
  {:else if state === "offline"}
    <WifiOff size={13} />
  {:else}
    <AlertTriangle size={13} />
  {/if}
  {#if !compact}<span>{labels[state]}</span>{/if}
</div>

<style>
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    width: max-content;
    min-height: 28px;
    padding: 5px 10px;
    border: 1px solid color-mix(in srgb, var(--amber) 28%, transparent);
    border-radius: 999px;
    color: var(--amber);
    background: color-mix(in srgb, var(--amber) 10%, transparent);
    font-size: 11px;
    font-weight: 650;
  }

  .healthy {
    color: var(--sage);
    border-color: color-mix(in srgb, var(--sage) 26%, transparent);
    background: color-mix(in srgb, var(--sage) 9%, transparent);
  }

  .compact {
    width: 25px;
    min-height: 25px;
    padding: 0;
    justify-content: center;
  }

  :global(.spin) {
    animation: spin 0.9s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
