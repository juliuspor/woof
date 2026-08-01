<script lang="ts">
  import { onMount } from "svelte";
  import {
    Activity,
    AlertCircle,
    Bell,
    BookOpen,
    Brain,
    Briefcase,
    ChevronRight,
    Clock3,
    FolderKanban,
    Home,
    MoreHorizontal,
    Pause,
    Play,
    RefreshCw,
    Search,
    Settings,
    Sparkles,
    Users
  } from "lucide-svelte";
  import Mascot from "./Mascot.svelte";
  import HealthBadge from "./HealthBadge.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import { appState, updateState } from "$lib/state/app";
  import {
    COMMANDS,
    EVENTS,
    type CaptureState,
    type CaptureStatus,
    type FollowupItem,
    type HealthChangedPayload,
    type MemoryHubNavigatePayload,
    type MemoryHubRoute,
    type RecentActivityItem,
    type TimeReport,
    type TimeRule,
    type WikiPage,
    type WikiSummary,
    type WorkPatternStatus,
    type WorkingMemoryItem
  } from "$lib/contracts/ipc";
  import { invokeCommand, listenEvent } from "$lib/contracts/bridge";

  let query = $state("");
  let selectedMemory = $state<WikiSummary | null>(null);
  let selectedPage = $state<WikiPage | null>(null);
  let recentActivity = $state<RecentActivityItem[]>([]);
  let workingMemory = $state<WorkingMemoryItem[]>([]);
  let wikiPages = $state<WikiSummary[]>([]);
  let timeReport = $state<TimeReport | null>(null);
  let timeRules = $state<TimeRule[]>([]);
  let followups = $state<FollowupItem[]>([]);
  let followupUpdating = $state<Record<number, boolean>>({});
  let followupErrors = $state<Record<number, string>>({});
  type WorkflowCard = WorkPatternStatus["recent"][number] & {
    excerpt?: string | null;
    apps?: string[];
    frequency_label?: string | null;
    observations?: unknown[];
    first_detected_at?: number | null;
  };
  type WorkPatternView = Omit<WorkPatternStatus, "recent"> & {
    recent: WorkflowCard[];
  };
  let workPatterns = $state<WorkPatternView | null>(null);
  let workflowUpdating = $state<Record<string, boolean>>({});
  let workflowErrors = $state<Record<string, string>>({});
  let routedView = $state<MemoryHubRoute | null>(null);
  let routedStatus = $state<"idle" | "loading" | "ready" | "error">("idle");
  let routedError = $state("");
  let captureStatus = $state<CaptureStatus | null>(null);
  let captureObserved = $state(false);
  let homeStatus = $state<"loading" | "ready" | "error">("loading");
  let memoryStatus = $state<"idle" | "loading" | "ready" | "error">("idle");
  let detailStatus = $state<"idle" | "loading" | "ready" | "error">("idle");
  let timeStatus = $state<"idle" | "loading" | "ready" | "error">("idle");
  let homeError = $state("");
  let memoryError = $state("");
  let timeError = $state("");
  let contactName = $state("");
  let searchSequence = 0;

  const icons = {
    person: Users,
    project: FolderKanban,
    topic: BookOpen,
    tool: Sparkles,
    org: Briefcase
  } as const;

  const primaryMemory = $derived(workingMemory[0] ?? null);
  const workingApps = $derived(
    [...new Set(workingMemory.map((item) => item.app).filter(Boolean))].slice(0, 3)
  );
  const focusPercent = $derived(
    timeReport ? Math.min(100, Math.round((timeReport.total_seconds / (8 * 3600)) * 100)) : 0
  );
  const profileName = $derived(contactName || "Local user");
  const profileInitial = $derived(
    Array.from(profileName)[0]?.toLocaleUpperCase() ?? "L"
  );
  const displayedCaptureState = $derived(
    captureObserved || $appState.capture === "paused" ? $appState.capture : "starting"
  );
  const currentHour = new Date().getHours();
  const greeting = currentHour < 12
    ? "Good morning."
    : currentHour < 18
      ? "Good afternoon."
      : "Good evening.";

  async function loadProfile(): Promise<void> {
    try {
      const contact = await invokeCommand<{ name?: unknown }>(COMMANDS.loadContactInfo);
      if (typeof contact?.name !== "string") {
        contactName = "";
        return;
      }
      const name = contact.name.trim();
      contactName = /[\u0000-\u001f\u007f-\u009f]/u.test(name)
        ? ""
        : Array.from(name).slice(0, 160).join("");
    } catch {
      contactName = "";
    }
  }

  function captureStateFromStatus(status: CaptureStatus): CaptureState {
    const permission =
      typeof status.runtime.permission === "string" ? status.runtime.permission : null;
    const lastError =
      typeof status.runtime.last_error === "string" ? status.runtime.last_error : null;
    if (permission === "denied" || lastError === "permission_denied") {
      return "permission-revoked";
    }
    if (status.runtime.running !== true) {
      return permission === "unknown" ? "starting" : "error";
    }
    if (status.paused) return "paused";
    if (permission === "unknown") return "starting";
    if (permission !== "granted" || !status.capturing) return "error";
    if (
      lastError !== null &&
      !["secure_input", "no_focused_application", "semantic_index"].includes(lastError)
    ) {
      return "error";
    }
    return "active";
  }

  function applyCaptureState(state: CaptureState): void {
    captureObserved = true;
    if (state === "permission-revoked" || state === "error") {
      updateState({
        capture: state,
        health: $appState.health === "offline" ? "offline" : "degraded"
      });
    } else if (state === "starting" && $appState.health === "healthy") {
      updateState({ capture: state, health: "starting" });
    } else {
      updateState({ capture: state });
    }
  }

  async function refreshCapture(): Promise<void> {
    try {
      captureStatus = await invokeCommand<CaptureStatus>(COMMANDS.captureStatus);
      applyCaptureState(captureStateFromStatus(captureStatus));
    } catch {
      captureStatus = null;
      applyCaptureState("error");
    }
  }

  async function refreshHome(): Promise<void> {
    homeStatus = "loading";
    homeError = "";
    try {
      const [activityResponse, memoryResponse] = await Promise.all([
        invokeCommand<{ activity: RecentActivityItem[] }>(COMMANDS.memoryRecentActivity, {
          minutes: 60,
          limit: 12
        }),
        invokeCommand<{ items: WorkingMemoryItem[] }>(COMMANDS.memoryWorkingMemory, {
          limit: 40
        })
      ]);
      recentActivity = activityResponse.activity ?? [];
      workingMemory = memoryResponse.items ?? [];
      homeStatus = "ready";
    } catch (error) {
      homeError = error instanceof Error ? error.message : "The local memory service is unavailable.";
      homeStatus = "error";
    }
  }

  async function loadWiki(): Promise<void> {
    memoryStatus = "loading";
    memoryError = "";
    try {
      const response = await invokeCommand<{ pages: WikiSummary[] }>(COMMANDS.memoryWikiList, {
        pageType: null,
        limit: 50
      });
      wikiPages = response.pages ?? [];
      memoryStatus = "ready";
    } catch (error) {
      memoryError = error instanceof Error ? error.message : "Wiki pages could not be loaded.";
      memoryStatus = "error";
    }
  }

  async function searchWiki(value: string): Promise<void> {
    const sequence = ++searchSequence;
    memoryStatus = "loading";
    try {
      const response = value
        ? await invokeCommand<{ pages: WikiSummary[] }>(COMMANDS.memoryWikiSearch, {
            query: value,
            limit: 50
          })
        : await invokeCommand<{ pages: WikiSummary[] }>(COMMANDS.memoryWikiList, {
            pageType: null,
            limit: 50
          });
      if (sequence !== searchSequence) return;
      wikiPages = response.pages ?? [];
      memoryStatus = "ready";
      memoryError = "";
    } catch (error) {
      if (sequence !== searchSequence) return;
      memoryError = error instanceof Error ? error.message : "Wiki search is unavailable.";
      memoryStatus = "error";
    }
  }

  async function selectMemory(item: WikiSummary): Promise<void> {
    selectedMemory = item;
    selectedPage = null;
    if (!item.slug) {
      detailStatus = "error";
      return;
    }
    detailStatus = "loading";
    try {
      const response = await invokeCommand<{ page: WikiPage }>(COMMANDS.memoryWikiPage, {
        slug: item.slug
      });
      selectedPage = response.page;
      detailStatus = "ready";
    } catch {
      detailStatus = "error";
    }
  }

  async function loadTime(): Promise<void> {
    timeStatus = "loading";
    timeError = "";
    try {
      const [report, rules] = await Promise.all([
        invokeCommand<TimeReport>(COMMANDS.memoryTimeReport, {
          period: "today",
          from: null,
          to: null
        }),
        invokeCommand<{ rules: TimeRule[] }>(COMMANDS.memoryTimeRules)
      ]);
      timeReport = report;
      timeRules = rules.rules ?? [];
      timeStatus = "ready";
    } catch (error) {
      timeError = error instanceof Error ? error.message : "Time data could not be loaded.";
      timeStatus = "error";
    }
  }

  async function loadRoutedView(route: MemoryHubRoute): Promise<void> {
    routedStatus = "loading";
    routedError = "";
    try {
      if (route === "followups") {
        const response = await invokeCommand<{ followups: FollowupItem[] }>(
          COMMANDS.memoryFollowups
        );
        if (routedView !== route) return;
        followups = response.followups ?? [];
      } else {
        const response = await invokeCommand<{ status: WorkPatternView }>(
          COMMANDS.memoryWorkPatterns
        );
        if (routedView !== route) return;
        workPatterns = response.status;
      }
      routedStatus = "ready";
    } catch (error) {
      if (routedView !== route) return;
      routedError = error instanceof Error ? error.message : "This memory view is unavailable.";
      routedStatus = "error";
    }
  }

  async function setFollowupStatus(
    followup: FollowupItem,
    status: "resolved" | "dismissed"
  ): Promise<void> {
    if (followupUpdating[followup.flag_id]) return;
    followupUpdating = { ...followupUpdating, [followup.flag_id]: true };
    const nextErrors = { ...followupErrors };
    delete nextErrors[followup.flag_id];
    followupErrors = nextErrors;
    try {
      const result = await invokeCommand<{ updated: boolean }>(
        COMMANDS.memoryFollowupSetStatus,
        { flagId: followup.flag_id, status }
      );
      if (result.updated !== true) {
        followupErrors = {
          ...followupErrors,
          [followup.flag_id]: "This follow-up could not be updated."
        };
        return;
      }
      followups = followups.filter((item) => item.flag_id !== followup.flag_id);
    } catch {
      followupErrors = {
        ...followupErrors,
        [followup.flag_id]: "This follow-up could not be updated."
      };
    } finally {
      const nextUpdating = { ...followupUpdating };
      delete nextUpdating[followup.flag_id];
      followupUpdating = nextUpdating;
    }
  }

  async function setWorkflowStatus(
    workflow: WorkflowCard,
    status: "accepted" | "dismissed"
  ): Promise<void> {
    const workflowId = workflow.workflow_id;
    if (!workflowId || workflowUpdating[workflowId]) return;
    workflowUpdating = { ...workflowUpdating, [workflowId]: true };
    const nextErrors = { ...workflowErrors };
    delete nextErrors[workflowId];
    workflowErrors = nextErrors;
    try {
      const result = await invokeCommand<{ updated: boolean }>(
        COMMANDS.memoryWorkPatternSetStatus,
        { workflowId, status }
      );
      if (result.updated !== true) {
        workflowErrors = {
          ...workflowErrors,
          [workflowId]: "This pattern could not be updated."
        };
        return;
      }
      if (!workPatterns) return;
      workPatterns = {
        ...workPatterns,
        recent:
          status === "dismissed"
            ? workPatterns.recent.filter((item) => item.workflow_id !== workflowId)
            : workPatterns.recent.map((item) =>
                item.workflow_id === workflowId ? { ...item, status: "accepted" } : item
              )
      };
    } catch {
      workflowErrors = {
        ...workflowErrors,
        [workflowId]: "This pattern could not be updated."
      };
    } finally {
      const nextUpdating = { ...workflowUpdating };
      delete nextUpdating[workflowId];
      workflowUpdating = nextUpdating;
    }
  }

  function navigateRoutedView(route: MemoryHubRoute): void {
    routedView = route;
    updateState({ activeNav: "home" });
    void loadRoutedView(route);
  }

  function activate(nav: "home" | "memory" | "time" | "settings"): void {
    routedView = null;
    updateState({ activeNav: nav });
    if (nav === "home") void refreshHome();
    if (nav === "memory" && memoryStatus === "idle") void loadWiki();
    if (nav === "time" && timeStatus === "idle") void loadTime();
  }

  async function toggleCapture(): Promise<void> {
    const paused = $appState.capture === "paused";
    const previous = $appState.capture;
    applyCaptureState(paused ? "starting" : "paused");
    await invokeCommand(paused ? COMMANDS.captureResume : COMMANDS.capturePause).catch(() => {
      applyCaptureState(previous);
    });
    await refreshCapture();
  }

  function formatClock(timestamp: number): string {
    return new Intl.DateTimeFormat(undefined, {
      hour: "numeric",
      minute: "2-digit"
    }).format(new Date(timestamp * 1000));
  }

  function formatDuration(seconds: number): string {
    if (seconds < 60) return `${Math.max(0, Math.round(seconds))}s`;
    if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.round((seconds % 3600) / 60);
    return minutes ? `${hours}h ${minutes}m` : `${hours}h`;
  }

  function relativeTime(timestamp: number): string {
    const seconds = Math.max(0, Math.round(Date.now() / 1000 - timestamp));
    if (seconds < 60) return "Just now";
    if (seconds < 3600) return `${Math.round(seconds / 60)}m ago`;
    if (seconds < 86_400) return `${Math.round(seconds / 3600)}h ago`;
    return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(
      new Date(timestamp * 1000)
    );
  }

  function countSnapshotIds(value: string): number {
    try {
      const ids = JSON.parse(value);
      return Array.isArray(ids) ? ids.length : 0;
    } catch {
      return value.split(",").filter(Boolean).length;
    }
  }

  $effect(() => {
    const normalized = query.trim();
    if ($appState.activeNav !== "memory") return;
    const timer = window.setTimeout(() => void searchWiki(normalized), 240);
    return () => window.clearTimeout(timer);
  });

  onMount(() => {
    void loadProfile();
    void refreshCapture();
    void refreshHome();
    const unlisteners: Array<() => void> = [];
    void listenEvent(EVENTS.memoryHubRefreshRequested, () => {
      void refreshCapture();
      if (routedView) void loadRoutedView(routedView);
      if ($appState.activeNav === "home") void refreshHome();
      if ($appState.activeNav === "time") void loadTime();
    }).then((unlisten) => unlisteners.push(unlisten));
    void listenEvent<MemoryHubNavigatePayload>(EVENTS.memoryHubNavigate, ({ route }) => {
      navigateRoutedView(route);
    }).then((unlisten) => unlisteners.push(unlisten));
    void listenEvent<{ state: CaptureState }>(EVENTS.captureChanged, ({ state }) => {
      if (!["active", "paused", "starting", "permission-revoked", "error"].includes(state)) {
        return;
      }
      applyCaptureState(state === "active" ? "starting" : state);
      void refreshCapture();
    }).then((unlisten) => unlisteners.push(unlisten));
    void listenEvent<HealthChangedPayload>(EVENTS.healthChanged, ({ state }) => {
      updateState({ health: state });
    }).then((unlisten) => unlisteners.push(unlisten));
    return () => unlisteners.forEach((unlisten) => unlisten());
  });
</script>

<main class="memory-hub glass">
  <aside class="drag-region">
    <div class="brand"><span></span><b>woof</b></div>
    <nav class="no-drag">
      <button class:active={$appState.activeNav === "home"} onclick={() => activate("home")}>
        <Home size={16} /> Home
      </button>
      <button class:active={$appState.activeNav === "memory"} onclick={() => activate("memory")}>
        <Brain size={16} /> Memory
      </button>
      <button class:active={$appState.activeNav === "time"} onclick={() => activate("time")}>
        <Clock3 size={16} /> Time
      </button>
    </nav>
    <div class="aside-spacer"></div>
    <div
      class="capture-card no-drag"
      class:paused={displayedCaptureState === "paused"}
      class:attention={displayedCaptureState === "permission-revoked" || displayedCaptureState === "error"}
    >
      <div class="capture-heading">
        <span class="status-dot"></span>
        <b>
          {displayedCaptureState === "paused"
            ? "Capture paused"
            : displayedCaptureState === "active"
              ? "Noticing locally"
              : displayedCaptureState === "starting"
                ? "Capture starting"
                : displayedCaptureState === "permission-revoked"
                  ? "Accessibility needed"
                  : "Capture unavailable"}
        </b>
      </div>
      <p>
        {displayedCaptureState === "paused"
          ? "Capture is paused."
          : displayedCaptureState === "active"
            ? "Visible text, never screenshots."
            : displayedCaptureState === "starting"
              ? "Waiting for the local capture runtime."
              : displayedCaptureState === "permission-revoked"
                ? "Allow Accessibility access to resume capture."
                : "The local capture runtime needs attention."}
      </p>
      {#if displayedCaptureState === "paused"}
        <button onclick={toggleCapture}><Play size={12} /> Resume</button>
      {:else if displayedCaptureState === "active"}
        <button onclick={toggleCapture}><Pause size={12} /> Pause</button>
      {:else if displayedCaptureState === "permission-revoked"}
        <button onclick={() => void invokeCommand(COMMANDS.openAccessibilitySettings).catch(() => undefined)}>
          <Settings size={12} /> Open settings
        </button>
      {:else}
        <button onclick={refreshCapture}><RefreshCw size={12} /> Refresh status</button>
      {/if}
    </div>
    <button class:active={$appState.activeNav === "settings"} class="settings-link no-drag" onclick={() => activate("settings")}>
      <Settings size={16} /> Settings
    </button>
    <div class="profile no-drag">
      <div class="avatar">{profileInitial}</div>
      <div><b>{profileName}</b><span>Local profile</span></div>
      <MoreHorizontal size={15} />
    </div>
  </aside>

  <section class="surface">
    {#if $appState.activeNav === "settings"}
      <SettingsPanel embedded />
    {:else}
      <header class="drag-region">
        <div>
          <div class="eyebrow">
            {routedView
              ? "Memory hub"
              : $appState.activeNav === "home"
              ? "Today"
              : $appState.activeNav === "memory"
                ? "Knowledge"
                : "Tracked focus"}
          </div>
          <h1>
            {routedView === "followups"
              ? "Follow-ups"
                : routedView === "workflows"
                  ? "Workflows"
                  : $appState.activeNav === "home"
              ? greeting
              : $appState.activeNav === "memory"
                ? "Your memory"
                : "Time in focus"}
          </h1>
        </div>
        <div class="header-actions no-drag">
          <HealthBadge state={$appState.health} />
          <button aria-label="Notifications"><Bell size={16} /><i></i></button>
          <button class="ask" onclick={() => invokeCommand(COMMANDS.companionOpenFocused)}>
            <Sparkles size={14} /> Ask woof
          </button>
        </div>
      </header>

      {#if routedView}
        <div class="scroll routed-view">
          <div class="routed-heading">
            <div>
              <span class="eyebrow">Local memory</span>
              <h2>{routedView === "followups" ? "Open follow-ups" : "Detected workflows"}</h2>
              <p>
                {routedView === "followups"
                  ? "Questions and commitments surfaced from your local chronicles."
                  : "Recurring work patterns detected in your local memory."}
              </p>
            </div>
            <button onclick={() => activate("home")}>Back to overview</button>
          </div>
          {#if routedStatus === "loading" || routedStatus === "idle"}
            <div class="load-state routed-load"><span class="loader"></span><p>Loading local memory…</p></div>
          {:else if routedStatus === "error"}
            <div class="load-state routed-load error-state">
              <AlertCircle size={19} /><p>{routedError}</p>
              <button onclick={() => loadRoutedView(routedView!)}><RefreshCw size={12} /> Retry</button>
            </div>
          {:else if routedView === "followups"}
            <div class="routed-list" aria-label="Open follow-ups">
              {#each followups as followup (followup.flag_id)}
                <article>
                  <span class="route-icon"><Bell size={16} /></span>
                  <div class="followup-copy">
                    <b>{followup.text}</b>
                    <small>{followup.kind} · {relativeTime(followup.created_at)}</small>
                    {#if followupErrors[followup.flag_id]}
                      <em role="alert">{followupErrors[followup.flag_id]}</em>
                    {/if}
                  </div>
                  <div class="followup-actions">
                    <button
                      aria-label={`Resolve ${followup.text}`}
                      disabled={followupUpdating[followup.flag_id]}
                      onclick={() => void setFollowupStatus(followup, "resolved")}
                    >Resolve</button>
                    <button
                      aria-label={`Dismiss ${followup.text}`}
                      disabled={followupUpdating[followup.flag_id]}
                      onclick={() => void setFollowupStatus(followup, "dismissed")}
                    >Dismiss</button>
                  </div>
                </article>
              {:else}
                <div class="load-state empty-state"><Bell size={18} /><p>No open follow-ups.</p></div>
              {/each}
            </div>
          {:else}
            <p class="workflow-disclosure">
              Keep pattern saves a detection in local memory. It never runs actions or automation.
            </p>
            <div class="routed-list" aria-label="Detected workflows">
              {#each workPatterns?.recent ?? [] as workflow (workflow.workflow_id ?? workflow.name)}
                <article>
                  <span class="route-icon"><Sparkles size={16} /></span>
                  <div class="workflow-copy">
                    <b>{workflow.name}</b>
                    <small>
                      {workflow.frequency_label?.trim() || `Last noticed ${relativeTime(workflow.last_detected_at)}`}
                      {workflow.apps?.length ? ` · ${workflow.apps.join(", ")}` : ""}
                    </small>
                    {#if workflow.excerpt?.trim()}<p>{workflow.excerpt}</p>{/if}
                    {#if workflow.workflow_id && workflowErrors[workflow.workflow_id]}
                      <em role="alert">{workflowErrors[workflow.workflow_id]}</em>
                    {/if}
                    {#if workflow.status === "accepted"}
                      <strong>Kept locally — no automation runs.</strong>
                    {/if}
                  </div>
                  {#if workflow.status === "proposed" && workflow.workflow_id}
                    <div class="followup-actions">
                      <button
                        aria-label={`Keep pattern ${workflow.name}`}
                        disabled={workflowUpdating[workflow.workflow_id]}
                        onclick={() => void setWorkflowStatus(workflow, "accepted")}
                      >Keep pattern</button>
                      <button
                        aria-label={`Dismiss ${workflow.name}`}
                        disabled={workflowUpdating[workflow.workflow_id]}
                        onclick={() => void setWorkflowStatus(workflow, "dismissed")}
                      >Dismiss</button>
                    </div>
                  {:else}
                    <i>{Math.round(workflow.confidence * 100)}% · {workflow.status}</i>
                  {/if}
                </article>
              {:else}
                <div class="load-state empty-state"><Sparkles size={18} /><p>No recurring workflows detected yet.</p></div>
              {/each}
            </div>
          {/if}
        </div>
      {:else if $appState.activeNav === "home"}
        <div class="scroll home">
          <section class="working-memory">
            <div class="section-heading">
              <div><span class="eyebrow">Working memory</span><h2>What’s on your mind</h2></div>
              <button onclick={() => activate("memory")}>See memory <ChevronRight size={13} /></button>
            </div>
            <div class="memory-hero">
              {#if homeStatus === "loading"}
                <div class="load-state compact-load"><span class="loader"></span><p>Reading local working memory…</p></div>
              {:else if homeStatus === "error"}
                <div class="load-state compact-load error-state">
                  <AlertCircle size={18} /><p>{homeError}</p>
                  <button onclick={refreshHome}><RefreshCw size={12} /> Retry</button>
                </div>
              {:else if primaryMemory}
                <div class="hero-copy">
                  <span class="pulse"><i></i> Updated {relativeTime(primaryMemory.added_at)}</span>
                  <h3>{primaryMemory.window_title || primaryMemory.app}</h3>
                  <p>{primaryMemory.content}</p>
                  {#if workingApps.length}
                    <div class="chips">
                      {#each workingApps as app}<span>{app}</span>{/each}
                    </div>
                  {/if}
                </div>
                <div class="hero-mascot"><Mascot size={132} mood="thinking" /></div>
              {:else}
                <div class="load-state compact-load empty-state">
                  <Mascot size={70} mood="calm" />
                  <div><h3>Your working memory is quiet.</h3><p>It will fill as local capture observes useful context.</p></div>
                </div>
              {/if}
            </div>
          </section>

          <div class="grid">
            <section class="activity-list">
              <div class="section-heading compact">
                <div><span class="eyebrow">Recent activity</span><h2>Last hour</h2></div>
                <Activity size={17} />
              </div>
              {#if homeStatus === "loading"}
                <div class="load-state"><span class="loader"></span><p>Loading recent local activity…</p></div>
              {:else if homeStatus === "error"}
                <div class="load-state error-state"><AlertCircle size={17} /><p>{homeError}</p></div>
              {:else if recentActivity.length}
                {#each recentActivity as item (item.event_id)}
                  <article>
                    <time>{formatClock(item.last_seen_at)}</time>
                    <div class="app-icon">{item.app.slice(0, 1).toUpperCase()}</div>
                    <div class="activity-copy">
                      <div><b>{item.window_title || item.app}</b><span>{formatDuration(item.duration_s)}</span></div>
                      <small>{item.app}{item.domain ? ` · ${item.domain}` : ""}</small>
                      <p>{item.content_excerpt || "No text excerpt was stored for this event."}</p>
                    </div>
                  </article>
                {/each}
              {:else}
                <div class="load-state empty-state">
                  <Activity size={18} /><p>No captured activity in the last hour.</p>
                </div>
              {/if}
            </section>

            <aside class="right-column">
              <section class="nudge">
                <div class="nudge-top"><Activity size={15} /><span>capture status</span></div>
                <h3>
                  {displayedCaptureState === "paused"
                    ? "Capture is paused."
                    : displayedCaptureState === "active"
                      ? "Woof is noticing locally."
                      : displayedCaptureState === "permission-revoked"
                        ? "Accessibility permission is needed."
                        : displayedCaptureState === "error"
                          ? "Capture is unavailable."
                          : "Capture is waiting to start."}
                </h3>
                <p>
                  {#if typeof captureStatus?.runtime.last_capture_at === "number"}
                    Last local capture {relativeTime(captureStatus.runtime.last_capture_at)}.
                  {:else if captureStatus}
                    Visible interface text is processed locally, never screenshots.
                  {:else}
                    Capture status is temporarily unavailable.
                  {/if}
                </p>
                <button onclick={refreshCapture}><RefreshCw size={12} /> Refresh status</button>
              </section>
              <section class="stats">
                <div><span>Working-memory frames</span><b>{workingMemory.length}</b></div>
                <div class="bar"><i style:width={`${Math.min(100, workingMemory.length * 5)}%`}></i></div>
                {#each workingApps.slice(0, 2) as app, index}
                  <p>
                    <span class:research={index === 1} class="legend development"></span>
                    {app}
                    <b>{workingMemory.filter((item) => item.app === app).length}</b>
                  </p>
                {/each}
                {#if !workingApps.length}<p>No active apps in working memory.</p>{/if}
              </section>
            </aside>
          </div>
        </div>
      {:else if $appState.activeNav === "memory"}
        <div class="scroll memory">
          <label class="search">
            <Search size={16} />
            <input bind:value={query} placeholder="Search people, projects, topics, tools…" />
            <kbd>⌘ K</kbd>
          </label>
          <div class="memory-layout">
            <div class="memory-list">
              <div class="list-meta"><span>{wikiPages.length} pages</span><button onclick={loadWiki}>Recently updated</button></div>
              {#if memoryStatus === "loading"}
                <div class="load-state"><span class="loader"></span><p>{query.trim() ? "Searching local wiki…" : "Loading local wiki…"}</p></div>
              {:else if memoryStatus === "error"}
                <div class="load-state error-state">
                  <AlertCircle size={18} /><p>{memoryError}</p>
                  <button onclick={() => searchWiki(query.trim())}><RefreshCw size={12} /> Retry</button>
                </div>
              {:else if wikiPages.length}
                {#each wikiPages as item (item.slug ?? item.title)}
                  {@const Icon = icons[item.page_type]}
                  <button class:selected={selectedMemory?.slug === item.slug} onclick={() => selectMemory(item)}>
                    <span class="page-icon"><Icon size={17} /></span>
                    <span><i>{item.page_type}</i><b>{item.title}</b><p>{item.summary || "No summary generated yet."}</p></span>
                    <time>{relativeTime(item.last_seen)}</time>
                  </button>
                {/each}
              {:else}
                <div class="load-state empty-state">
                  <BookOpen size={19} />
                  <p>{query.trim() ? "No local wiki pages match this search." : "No wiki pages have been generated yet."}</p>
                </div>
              {/if}
            </div>
            <article class="memory-detail">
              {#if selectedMemory}
                {@const SelectedIcon = icons[selectedMemory.page_type]}
                <div class="detail-icon"><SelectedIcon size={24} /></div>
                <span class="eyebrow">{selectedMemory.page_type}</span>
                <h2>{selectedMemory.title}</h2>
                <p>{selectedMemory.summary}</p>
                {#if detailStatus === "loading"}
                  <div class="load-state detail-load"><span class="loader"></span><p>Opening source-backed page…</p></div>
                {:else if detailStatus === "error"}
                  <div class="load-state detail-load error-state">
                    <AlertCircle size={17} /><p>This page could not be opened.</p>
                    <button onclick={() => selectMemory(selectedMemory!)}><RefreshCw size={12} /> Retry</button>
                  </div>
                {:else if selectedPage}
                  <hr />
                  <h3>What woof remembers</h3>
                  <p class="wiki-body">{selectedPage.body || selectedPage.summary || "This page has no generated body yet."}</p>
                  <div class="source-row">
                    <span>{countSnapshotIds(selectedPage.snapshot_ids)} local sources</span>
                    <span>Updated {relativeTime(selectedPage.updated_at)}</span>
                  </div>
                {/if}
              {:else}
                <Mascot size={96} mood="calm" />
                <h2>Pick a memory page</h2>
                <p>People, projects, topics, tools, and organizations live here as woof learns.</p>
              {/if}
            </article>
          </div>
        </div>
      {:else}
        <div class="scroll time-view">
          {#if timeStatus === "loading" || timeStatus === "idle"}
            <div class="load-state time-load"><span class="loader"></span><p>Calculating tracked foreground time…</p></div>
          {:else if timeStatus === "error"}
            <div class="load-state time-load error-state">
              <AlertCircle size={20} /><p>{timeError}</p>
              <button onclick={loadTime}><RefreshCw size={12} /> Retry</button>
            </div>
          {:else if timeReport}
            <div class="time-summary">
              <div>
                <span class="eyebrow">Today</span>
                <strong>{formatDuration(timeReport.total_seconds)}</strong>
                <p>Tracked foreground time</p>
              </div>
              <div class="ring" style={`--focus: ${focusPercent}%`}>
                <span>{focusPercent}%</span><small>of 8h</small>
              </div>
            </div>
            <div class="timeline">
              <div class="section-heading compact">
                <div><span class="eyebrow">Projects</span><h2>Today’s allocation</h2></div>
                <button onclick={loadTime}><RefreshCw size={12} /> Refresh</button>
              </div>
              {#if timeReport.projects.length}
                {#each timeReport.projects as project, index (project.project)}
                  <div class="timeline-row">
                    <time>{index + 1}</time>
                    <span
                      class:alternate={index % 2 === 1}
                      class="timeline-bar"
                      style:width={`${Math.max(5, (project.seconds / Math.max(1, timeReport.total_seconds)) * 100)}%`}
                    ></span>
                    <b>{project.project}</b><small>{formatDuration(project.seconds)}</small>
                  </div>
                {/each}
              {:else}
                <div class="load-state empty-state"><Clock3 size={18} /><p>No foreground time has been recorded today.</p></div>
              {/if}
            </div>
            <div class="time-rules">
              <div class="section-heading compact">
                <div><span class="eyebrow">Classification</span><h2>Time rules</h2></div>
                <span>{timeRules.length}</span>
              </div>
              {#if timeRules.length}
                {#each timeRules as rule (rule.rule_id)}
                  <div class="rule-row">
                    <span class="rule-dot"></span>
                    <div><b>{rule.project}</b><small>{rule.app ?? rule.domain ?? rule.title_contains ?? "All matching activity"}</small></div>
                    <i>{rule.source}</i>
                  </div>
                {/each}
              {:else}
                <div class="load-state empty-state"><p>No time-classification rules yet.</p></div>
              {/if}
            </div>
          {/if}
        </div>
      {/if}
    {/if}
  </section>
</main>

<style>
  .memory-hub {
    width: 100%;
    height: 100%;
    display: grid;
    grid-template-columns: 188px 1fr;
    overflow: hidden;
    border-radius: 18px;
    background: var(--glass-strong);
  }

  .memory-hub > aside {
    display: flex;
    flex-direction: column;
    padding: 17px 12px 12px;
    border-right: 1px solid var(--line);
    background:
      linear-gradient(180deg, color-mix(in srgb, var(--fawn) 9%, transparent), transparent 25%),
      color-mix(in srgb, var(--cream-solid) 50%, transparent);
  }

  .brand {
    height: 43px;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 9px;
    font-size: 13px;
    letter-spacing: -0.02em;
  }

  .brand > span {
    width: 19px;
    height: 19px;
    border-radius: 8px 8px 10px 10px;
    background: var(--fawn);
    box-shadow: inset 0 -4px 0 rgba(74, 50, 40, 0.2);
  }

  nav {
    display: grid;
    gap: 4px;
    margin-top: 12px;
  }

  nav button,
  .settings-link {
    height: 36px;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 10px;
    border: 0;
    border-radius: 10px;
    color: var(--ink-muted);
    background: transparent;
    text-align: left;
    font-size: 10.5px;
    font-weight: 560;
    cursor: pointer;
  }

  nav button.active,
  .settings-link.active {
    color: var(--ink);
    background: color-mix(in srgb, var(--fawn) 13%, transparent);
  }

  .aside-spacer {
    flex: 1;
  }

  .capture-card {
    margin-bottom: 9px;
    padding: 11px;
    border: 1px solid color-mix(in srgb, var(--sage) 20%, transparent);
    border-radius: 13px;
    background: color-mix(in srgb, var(--sage) 7%, transparent);
  }

  .capture-card.paused {
    border-color: color-mix(in srgb, var(--amber) 24%, transparent);
    background: color-mix(in srgb, var(--amber) 7%, transparent);
  }

  .capture-card.attention {
    border-color: color-mix(in srgb, var(--rose) 24%, transparent);
    background: color-mix(in srgb, var(--rose) 7%, transparent);
  }

  .capture-heading {
    display: flex;
    align-items: center;
    gap: 7px;
    color: var(--sage);
    font-size: 9px;
  }

  .paused .capture-heading {
    color: var(--amber);
  }

  .attention .capture-heading {
    color: var(--rose);
  }

  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
    box-shadow: 0 0 0 4px color-mix(in srgb, currentColor 12%, transparent);
  }

  .capture-card p {
    margin: 7px 0 8px;
    color: var(--ink-faint);
    font-size: 8px;
  }

  .capture-card button {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 0;
    border: 0;
    color: var(--ink-muted);
    background: transparent;
    font-size: 8.5px;
    font-weight: 650;
    cursor: pointer;
  }

  .settings-link {
    width: 100%;
  }

  .profile {
    height: 48px;
    display: grid;
    grid-template-columns: 29px 1fr 15px;
    align-items: center;
    gap: 8px;
    margin-top: 7px;
    padding: 0 7px;
    border-top: 1px solid var(--line);
  }

  .avatar {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border-radius: 10px;
    color: #fff9f0;
    background: var(--brown);
    font-size: 10px;
    font-weight: 700;
  }

  .profile b,
  .profile span {
    display: block;
  }

  .profile b {
    font-size: 9px;
  }

  .profile span {
    margin-top: 2px;
    color: var(--ink-faint);
    font-size: 7.5px;
  }

  .surface {
    min-width: 0;
    overflow: hidden;
    background:
      radial-gradient(circle at 95% 0, rgba(231, 173, 117, 0.09), transparent 29%),
      color-mix(in srgb, var(--cream) 55%, transparent);
  }

  .surface > header {
    height: 87px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 26px 10px 30px;
  }

  .surface > header h1 {
    margin: 5px 0 0;
    font-size: 25px;
    line-height: 1;
    letter-spacing: -0.046em;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .header-actions > button {
    height: 34px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    border: 1px solid var(--line);
    border-radius: 11px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 60%, transparent);
    cursor: pointer;
  }

  .header-actions > button:not(.ask) {
    position: relative;
    width: 34px;
  }

  .header-actions button i {
    position: absolute;
    top: 7px;
    right: 7px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--rose);
  }

  .header-actions .ask {
    padding: 0 13px;
    color: #fff8f0;
    border-color: transparent;
    background: var(--brown);
    font-size: 9.5px;
    font-weight: 650;
  }

  .scroll {
    height: calc(100% - 87px);
    overflow: auto;
    padding: 0 30px 30px;
  }

  .section-heading {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    margin-bottom: 13px;
  }

  .section-heading h2 {
    margin: 4px 0 0;
    font-size: 16px;
    letter-spacing: -0.034em;
  }

  .section-heading button {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 6px;
    border: 0;
    color: var(--ink-faint);
    background: transparent;
    font-size: 8.5px;
    cursor: pointer;
  }

  .memory-hero {
    position: relative;
    min-height: 176px;
    display: grid;
    grid-template-columns: 1fr 170px;
    overflow: hidden;
    padding: 24px 25px;
    border: 1px solid var(--line);
    border-radius: 20px;
    background:
      radial-gradient(circle at 88% 47%, rgba(231, 173, 117, 0.26), transparent 29%),
      linear-gradient(120deg, color-mix(in srgb, var(--fawn) 11%, transparent), transparent 65%),
      color-mix(in srgb, var(--cream-solid) 70%, transparent);
    box-shadow: 0 8px 28px rgba(74, 50, 40, 0.08);
  }

  .hero-copy {
    max-width: 510px;
  }

  .pulse {
    display: flex;
    align-items: center;
    gap: 7px;
    color: var(--sage);
    font-size: 8px;
    font-weight: 650;
  }

  .pulse i {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--sage);
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--sage) 12%, transparent);
  }

  .hero-copy h3 {
    max-width: 470px;
    margin: 13px 0 8px;
    font-size: 20px;
    line-height: 1.1;
    letter-spacing: -0.037em;
  }

  .hero-copy p {
    max-width: 490px;
    display: -webkit-box;
    overflow: hidden;
    margin: 0;
    color: var(--ink-muted);
    font-size: 10px;
    line-height: 1.55;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
  }

  .chips {
    display: flex;
    gap: 6px;
    margin-top: 14px;
  }

  .chips span {
    padding: 5px 8px;
    border-radius: 7px;
    color: var(--fawn-deep);
    background: color-mix(in srgb, var(--fawn) 11%, transparent);
    font-size: 7.5px;
    font-weight: 650;
  }

  .hero-mascot {
    display: grid;
    place-items: center;
  }

  .grid {
    display: grid;
    grid-template-columns: 1.38fr 0.62fr;
    gap: 14px;
    margin-top: 17px;
  }

  .activity-list,
  .nudge,
  .stats {
    border: 1px solid var(--line);
    border-radius: 18px;
    background: color-mix(in srgb, var(--cream-solid) 60%, transparent);
  }

  .activity-list {
    padding: 18px;
  }

  .section-heading.compact {
    align-items: center;
  }

  .activity-list article {
    display: grid;
    grid-template-columns: 30px 30px 1fr;
    gap: 10px;
    padding: 10px 0;
    border-top: 1px solid var(--line);
  }

  .activity-list article time {
    padding-top: 7px;
    color: var(--ink-faint);
    font-size: 7.5px;
  }

  .app-icon {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border-radius: 9px;
    color: #fff;
    background: var(--fawn-deep);
    font-size: 9px;
    font-weight: 700;
  }

  .activity-copy > div {
    display: flex;
    justify-content: space-between;
  }

  .activity-copy b {
    font-size: 9px;
  }

  .activity-copy span,
  .activity-copy small {
    color: var(--ink-faint);
    font-size: 7px;
  }

  .activity-copy p {
    margin: 4px 0 0;
    overflow: hidden;
    color: var(--ink-muted);
    font-size: 8px;
    line-height: 1.4;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .right-column {
    display: grid;
    gap: 14px;
  }

  .nudge {
    padding: 17px;
    background:
      linear-gradient(145deg, color-mix(in srgb, var(--fawn) 15%, transparent), transparent 65%),
      color-mix(in srgb, var(--cream-solid) 65%, transparent);
  }

  .nudge-top {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--fawn-deep);
    font-size: 8px;
    font-weight: 680;
  }

  .nudge h3 {
    margin: 12px 0 7px;
    font-size: 13px;
    line-height: 1.18;
    letter-spacing: -0.025em;
  }

  .nudge p {
    margin: 0;
    color: var(--ink-muted);
    font-size: 8px;
    line-height: 1.5;
  }

  .nudge button {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-top: 12px;
    padding: 0;
    border: 0;
    color: var(--fawn-deep);
    background: transparent;
    font-size: 8px;
    font-weight: 650;
  }

  .stats {
    padding: 15px;
  }

  .stats > div:first-child {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    color: var(--ink-faint);
    font-size: 8px;
  }

  .stats b {
    color: var(--ink);
  }

  .stats > div:first-child b {
    font-size: 13px;
  }

  .bar {
    height: 5px;
    margin: 11px 0 10px;
    overflow: hidden;
    border-radius: 99px;
    background: var(--cream-dim);
  }

  .bar i {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: var(--fawn);
  }

  .stats p {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 7px 0;
    color: var(--ink-muted);
    font-size: 7.5px;
  }

  .stats p b {
    margin-left: auto;
  }

  .legend {
    width: 6px;
    height: 6px;
    border-radius: 2px;
  }

  .legend.development {
    background: var(--fawn);
  }

  .legend.research {
    background: var(--blue);
  }

  .search {
    height: 43px;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 11px;
    border: 1px solid var(--line);
    border-radius: 13px;
    color: var(--ink-faint);
    background: color-mix(in srgb, var(--cream-solid) 65%, transparent);
  }

  .search input {
    flex: 1;
    border: 0;
    outline: 0;
    color: var(--ink);
    background: transparent;
    font-size: 10px;
    user-select: text;
  }

  .search kbd {
    padding: 4px 6px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--cream);
    font-size: 7px;
  }

  .memory-layout {
    height: calc(100% - 57px);
    display: grid;
    grid-template-columns: 1fr 0.86fr;
    gap: 14px;
    margin-top: 14px;
  }

  .memory-list,
  .memory-detail {
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 17px;
    background: color-mix(in srgb, var(--cream-solid) 54%, transparent);
  }

  .list-meta {
    height: 38px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 14px;
    border-bottom: 1px solid var(--line);
    color: var(--ink-faint);
    font-size: 8px;
  }

  .list-meta button {
    border: 0;
    color: inherit;
    background: transparent;
    font-size: inherit;
    cursor: pointer;
  }

  .memory-list > button {
    width: 100%;
    min-height: 82px;
    display: grid;
    grid-template-columns: 34px 1fr auto;
    gap: 11px;
    padding: 13px;
    border: 0;
    border-bottom: 1px solid var(--line);
    color: var(--ink);
    background: transparent;
    text-align: left;
    cursor: pointer;
  }

  .memory-list > button.selected {
    background: color-mix(in srgb, var(--fawn) 10%, transparent);
  }

  .page-icon {
    width: 34px;
    height: 34px;
    display: grid;
    place-items: center;
    border-radius: 11px;
    color: var(--fawn-deep);
    background: color-mix(in srgb, var(--fawn) 12%, transparent);
  }

  .memory-list i,
  .memory-list b,
  .memory-list p {
    display: block;
  }

  .memory-list i {
    margin-bottom: 3px;
    color: var(--fawn-deep);
    font-size: 6.5px;
    font-style: normal;
    font-weight: 680;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .memory-list b {
    font-size: 10px;
  }

  .memory-list p {
    margin: 4px 0 0;
    overflow: hidden;
    color: var(--ink-muted);
    font-size: 8px;
    line-height: 1.35;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .memory-list time {
    color: var(--ink-faint);
    font-size: 7px;
  }

  .memory-detail {
    padding: 27px;
  }

  .memory-detail:has(> :global(.mascot)) {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
  }

  .detail-icon {
    width: 48px;
    height: 48px;
    display: grid;
    place-items: center;
    margin-bottom: 14px;
    border-radius: 16px;
    color: var(--fawn-deep);
    background: color-mix(in srgb, var(--fawn) 13%, transparent);
  }

  .memory-detail h2 {
    margin: 7px 0 10px;
    font-size: 21px;
    letter-spacing: -0.04em;
  }

  .memory-detail h3 {
    margin: 0 0 8px;
    font-size: 11px;
  }

  .memory-detail p {
    max-width: 340px;
    margin: 0;
    color: var(--ink-muted);
    font-size: 9px;
    line-height: 1.55;
  }

  .memory-detail .wiki-body {
    max-height: 245px;
    overflow: auto;
    padding-right: 5px;
    white-space: pre-wrap;
    user-select: text;
  }

  .memory-detail hr {
    margin: 23px 0;
    border: 0;
    border-top: 1px solid var(--line);
  }

  .source-row {
    display: flex;
    justify-content: space-between;
    margin-top: 22px;
    padding-top: 12px;
    border-top: 1px solid var(--line);
    color: var(--ink-faint);
    font-size: 7.5px;
  }

  .time-view {
    display: grid;
    grid-template-columns: 0.36fr 0.64fr;
    grid-template-rows: auto 1fr;
    align-items: start;
    gap: 16px;
  }

  .time-summary,
  .timeline,
  .time-rules {
    border: 1px solid var(--line);
    border-radius: 18px;
    background: color-mix(in srgb, var(--cream-solid) 60%, transparent);
  }

  .time-summary {
    height: 255px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: space-around;
    padding: 24px;
    text-align: center;
  }

  .time-summary strong {
    display: block;
    margin-top: 9px;
    font-size: 38px;
    letter-spacing: -0.06em;
  }

  .time-summary p {
    margin: 3px 0;
    color: var(--ink-faint);
    font-size: 8px;
  }

  .ring {
    width: 92px;
    height: 92px;
    display: grid;
    place-content: center;
    border-radius: 50%;
    background:
      radial-gradient(circle, var(--cream-solid) 57%, transparent 59%),
      conic-gradient(var(--fawn) var(--focus, 0%), var(--cream-dim) 0);
  }

  .ring span,
  .ring small {
    display: block;
  }

  .ring span {
    font-size: 17px;
    font-weight: 720;
  }

  .ring small {
    color: var(--ink-faint);
    font-size: 7px;
  }

  .timeline {
    grid-row: 1 / span 2;
    grid-column: 2;
    min-height: 360px;
    padding: 20px;
  }

  .timeline-row {
    height: 62px;
    display: grid;
    grid-template-columns: 35px minmax(20px, 1fr) 85px 45px;
    align-items: center;
    gap: 9px;
    border-top: 1px solid var(--line);
  }

  .timeline-row time,
  .timeline-row small {
    color: var(--ink-faint);
    font-size: 7.5px;
  }

  .timeline-row b {
    font-size: 9px;
  }

  .timeline-bar {
    height: 9px;
    min-width: 18px;
    max-width: 100%;
    border-radius: 99px;
    background: var(--fawn);
  }

  .timeline-bar.alternate {
    background: var(--blue);
  }

  .time-rules {
    grid-row: 2;
    grid-column: 1;
    overflow: hidden;
    padding: 15px;
  }

  .time-rules .section-heading {
    align-items: center;
    margin-bottom: 7px;
  }

  .time-rules .section-heading > span {
    color: var(--ink-faint);
    font-size: 8px;
  }

  .rule-row {
    min-height: 46px;
    display: grid;
    grid-template-columns: 7px 1fr auto;
    align-items: center;
    gap: 8px;
    border-top: 1px solid var(--line);
  }

  .rule-dot {
    width: 6px;
    height: 6px;
    border-radius: 2px;
    background: var(--fawn);
  }

  .rule-row b,
  .rule-row small {
    display: block;
  }

  .rule-row b {
    font-size: 8.5px;
  }

  .rule-row small {
    max-width: 125px;
    margin-top: 2px;
    overflow: hidden;
    color: var(--ink-faint);
    font-size: 7px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rule-row i {
    color: var(--ink-faint);
    font-size: 6.5px;
    font-style: normal;
  }

  .load-state {
    min-height: 96px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 18px;
    color: var(--ink-faint);
    text-align: center;
  }

  .load-state p {
    margin: 0;
    color: inherit;
    font-size: 8.5px;
    line-height: 1.45;
  }

  .load-state button {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 8px;
    color: var(--ink-muted);
    background: var(--cream);
    font-size: 7.5px;
    cursor: pointer;
  }

  .memory-hero > .compact-load {
    grid-column: 1 / -1;
    width: 100%;
    min-height: 126px;
  }

  .compact-load.empty-state {
    text-align: left;
  }

  .compact-load.empty-state h3 {
    margin: 0 0 5px;
    color: var(--ink);
    font-size: 13px;
  }

  .error-state {
    color: var(--rose);
  }

  .empty-state {
    color: var(--ink-faint);
  }

  .detail-load {
    justify-content: flex-start;
    min-height: 70px;
    padding: 18px 0 0;
  }

  .time-load {
    grid-column: 1 / -1;
    min-height: 300px;
    border: 1px solid var(--line);
    border-radius: 18px;
    background: color-mix(in srgb, var(--cream-solid) 60%, transparent);
  }

  .routed-heading {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    padding: 19px 21px;
    border: 1px solid var(--line);
    border-radius: 18px;
    background: color-mix(in srgb, var(--cream-solid) 65%, transparent);
  }

  .routed-heading h2 {
    margin: 5px 0 6px;
    font-size: 18px;
    letter-spacing: -0.035em;
  }

  .routed-heading p {
    margin: 0;
    color: var(--ink-muted);
    font-size: 9px;
  }

  .routed-heading button {
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    color: var(--ink-muted);
    background: var(--cream);
    font-size: 8px;
    cursor: pointer;
  }

  .routed-list,
  .routed-load {
    margin-top: 14px;
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 18px;
    background: color-mix(in srgb, var(--cream-solid) 60%, transparent);
  }

  .workflow-disclosure {
    margin: 14px 2px -5px;
    color: var(--ink-faint);
    font-size: 7.5px;
  }

  .routed-list article {
    display: grid;
    grid-template-columns: 34px 1fr auto;
    align-items: center;
    gap: 11px;
    min-height: 62px;
    padding: 9px 15px;
    border-top: 1px solid var(--line);
  }

  .routed-list article:first-child {
    border-top: 0;
  }

  .route-icon {
    width: 32px;
    height: 32px;
    display: grid;
    place-items: center;
    border-radius: 10px;
    color: var(--fawn-deep);
    background: color-mix(in srgb, var(--fawn) 12%, transparent);
  }

  .routed-list b,
  .routed-list small {
    display: block;
  }

  .routed-list b {
    font-size: 9.5px;
  }

  .routed-list small,
  .routed-list i {
    margin-top: 3px;
    color: var(--ink-faint);
    font-size: 7px;
    font-style: normal;
  }

  .followup-copy em {
    display: block;
    margin-top: 4px;
    color: var(--rose);
    font-size: 7px;
    font-style: normal;
  }

  .workflow-copy p {
    margin: 5px 0 0;
    color: var(--ink-muted);
    font-size: 8px;
    line-height: 1.35;
  }

  .workflow-copy em {
    display: block;
    margin-top: 4px;
    color: var(--rose);
    font-size: 7px;
    font-style: normal;
  }

  .workflow-copy strong {
    display: block;
    margin-top: 5px;
    color: var(--sage);
    font-size: 7px;
  }

  .followup-actions {
    display: flex;
    gap: 5px;
  }

  .followup-actions button {
    padding: 5px 7px;
    border: 1px solid var(--line);
    border-radius: 7px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 75%, transparent);
    font-size: 7px;
    cursor: pointer;
  }

  .followup-actions button:disabled {
    cursor: wait;
    opacity: 0.55;
  }

  .loader {
    width: 15px;
    height: 15px;
    flex: 0 0 auto;
    border: 2px solid color-mix(in srgb, var(--fawn-deep) 18%, transparent);
    border-top-color: var(--fawn-deep);
    border-radius: 50%;
    animation: memory-hub-spin 0.75s linear infinite;
  }

  @keyframes memory-hub-spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
