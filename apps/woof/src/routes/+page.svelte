<script lang="ts">
  import { onMount } from "svelte";
  import Onboarding from "$lib/components/Onboarding.svelte";
  import PermissionScreen from "$lib/components/PermissionScreen.svelte";
  import MemoryHub from "$lib/components/MemoryHub.svelte";
  import Companion from "$lib/components/Companion.svelte";
  import CaretOverlay from "$lib/components/CaretOverlay.svelte";
  import EditOverlay from "$lib/components/EditOverlay.svelte";
  import HealthRecovery from "$lib/components/HealthRecovery.svelte";
  import {
    COMMANDS,
    EVENTS,
    type HealthState,
    type PreferencesChangedPayload
  } from "$lib/contracts/ipc";
  import { invokeCommand, listenEvent } from "$lib/contracts/bridge";

  type View =
    | "onboarding"
    | "permission"
    | "memory-hub"
    | "companion"
    | "caret"
    | "edit"
    | "health";

  let view = $state<View>("memory-hub");
  let health = $state<HealthState>("offline");
  const views: readonly View[] = [
    "onboarding",
    "permission",
    "memory-hub",
    "companion",
    "caret",
    "edit",
    "health"
  ];
  const healthStates: readonly HealthState[] = [
    "healthy",
    "starting",
    "degraded",
    "offline"
  ];

  onMount(() => {
    const params = new URLSearchParams(window.location.search);
    const requested = params.get("view");
    if (views.includes(requested as View)) view = requested as View;
    const healthParam = params.get("state");
    if (healthStates.includes(healthParam as HealthState)) health = healthParam as HealthState;
    document.body.dataset.view = view;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const applyReducedEffects = (enabled: boolean): void => {
      if (enabled) document.documentElement.dataset.reduceEffects = "true";
      else delete document.documentElement.dataset.reduceEffects;
    };
    void invokeCommand<boolean>(COMMANDS.getReduceVisualEffects)
      .then(applyReducedEffects)
      .catch(() => applyReducedEffects(false));
    void listenEvent<PreferencesChangedPayload>(EVENTS.preferencesChanged, (payload) => {
      if (typeof payload.reduceVisualEffects === "boolean") {
        applyReducedEffects(payload.reduceVisualEffects);
      }
    }).then((cleanup) => {
      if (disposed) void cleanup();
      else unlisten = cleanup;
    });
    return () => {
      disposed = true;
      void unlisten?.();
    };
  });
</script>

<svelte:head>
  <title>woof</title>
</svelte:head>

{#if view === "onboarding"}
  <Onboarding />
{:else if view === "permission"}
  <PermissionScreen />
{:else if view === "memory-hub"}
  <MemoryHub />
{:else if view === "companion"}
  <Companion mode="collapsed" />
{:else if view === "caret"}
  <CaretOverlay />
{:else if view === "edit"}
  <EditOverlay />
{:else}
  <HealthRecovery state={health} />
{/if}
