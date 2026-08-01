<script lang="ts">
  import { onMount, tick } from "svelte";
  import {
    AlertTriangle,
    ArrowUp,
    Inbox,
    LoaderCircle,
    Mic,
    Pause,
    Play,
    Plus,
    Settings,
    Share2,
    Square
  } from "lucide-svelte";
  import Mascot from "./Mascot.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import { appState, addMessage, appendMessage, finishMessage, updateState } from "$lib/state/app";
  import {
    COMMANDS,
    EVENTS,
    type ChatHistoryMessage,
    type ChatRequest,
    type CaptureState,
    type CompanionMode,
    type DockPosition,
    type HealthChangedPayload,
    type MemoryHubRoute,
    type NativeChatState,
    type OpenChatPayload,
    type PositionDragPayload,
    type TranscriptionItemPayload,
    type TranscriptionLevelPayload,
    type TranscriptionProcessingPayload,
    type TranscriptionStartPayload,
    companionModeFromState,
    transcriptionItemFromPayload,
    transcriptionLevelFromPayload,
    type WorkingMemoryItem
  } from "$lib/contracts/ipc";
  import { invokeCommand, listenEvent } from "$lib/contracts/bridge";
  import { MOTION, WINDOWS } from "$lib/contracts/geometry";

  let { mode = "collapsed", initialSettings = false } = $props<{
    mode?: CompanionMode;
    initialSettings?: boolean;
  }>();

  let activeMode = $state<CompanionMode>("collapsed");
  let contentVisible = $state(false);
  let focusWithin = $state(false);
  let prompt = $state("");
  let sending = $state(false);
  let transcript = $state("");
  let transcriptItems = $state<Array<{ item_id: string; text: string }>>([]);
  let pendingAttachment = $state<string | null>(null);
  let pendingOpenSource = $state<string | null>(null);
  let activeResponseId = $state<string | null>(null);
  let receivedStreamingDelta = $state(false);
  let shell = $state<HTMLElement>();
  let composer = $state<HTMLTextAreaElement>();
  let messagesEnd = $state<HTMLDivElement>();
  let settingsMounted = $state(false);
  let settingsClosing = $state(false);
  let initialSettingsApplied = false;
  let suggestions = $state<string[]>([]);
  let suggestionsLoaded = $state(false);
  let utilityPanel = $state<"memory" | "history" | null>(null);
  let memoryItems = $state<WorkingMemoryItem[]>([]);
  let memoryPanelState = $state<"idle" | "loading" | "ready" | "error">("idle");
  type NudgePayload = {
    nudge_id: string;
    title: string;
    body: string;
    deep_link?: string | null;
  };
  let nudgeQueue = $state<NudgePayload[]>([]);
  let nudgeActionError = $state("");
  let closeTimer: number | null = null;
  let settingsCloseTimer: number | null = null;
  let dragFrame: number | null = null;
  let dragPointerId: number | null = null;
  let dragOrigin: { x: number; y: number } | null = null;
  let dragScreen = { x: 0, y: 0 };
  let dragging = $state(false);
  let dockPosition = $state<DockPosition>("top");
  let collapsedConcealed = $state(false);
  let suppressOpenUntil = 0;
  let pointerInside = false;
  let pointerLeftAt = 0;
  let notificationStatus = $state<"denied" | "failed" | null>(null);
  let suppressNextUnverifiedActive = false;
  let chatThreadId = crypto.randomUUID();

  type NudgeTarget =
    | { kind: "settings" }
    | { kind: "chat"; prompt: string | null }
    | { kind: "memory-hub"; route: MemoryHubRoute };

  function parseNudgeDeepLink(deepLink: string | null | undefined): NudgeTarget | null {
    if (!deepLink?.startsWith("woof://")) return null;
    try {
      const target = new URL(deepLink);
      if (
        target.protocol !== "woof:" ||
        target.username ||
        target.password ||
        target.port ||
        target.hash
      ) {
        return null;
      }
      const rootPath = target.pathname === "" || target.pathname === "/";
      if (target.hostname === "settings" && rootPath && !target.search) {
        return { kind: "settings" };
      }
      if (target.hostname === "memory-hub" && !target.search) {
        if (target.pathname === "/followups") {
          return { kind: "memory-hub", route: "followups" };
        }
        if (target.pathname === "/workflows") {
          return { kind: "memory-hub", route: "workflows" };
        }
        return null;
      }
      if (target.hostname === "chat" && rootPath) {
        let validQuery = true;
        target.searchParams.forEach((_value, key) => {
          if (key !== "prompt") validQuery = false;
        });
        if (!validQuery) return null;
        return { kind: "chat", prompt: target.searchParams.get("prompt") };
      }
    } catch {
      return null;
    }
    return null;
  }

  const isExpanded = $derived(activeMode === "expanded");
  const isCollapsed = $derived(activeMode === "collapsed");
  const isSideDock = $derived(dockPosition === "left" || dockPosition === "right");
  const isCornerDock = $derived(
    dockPosition === "bottom-left" || dockPosition === "bottom-right"
  );
  const nudge = $derived(nudgeQueue[0] ?? null);
  const nudgeTarget = $derived(parseNudgeDeepLink(nudge?.deep_link));

  $effect(() => {
    activeMode = mode;
  });

  $effect(() => {
    if (!initialSettings || initialSettingsApplied) return;
    initialSettingsApplied = true;
    settingsMounted = true;
    updateState({ settingsOpen: true });
  });

  $effect(() => {
    if (activeMode !== "expanded") {
      contentVisible = false;
      return;
    }

    contentVisible = false;
    const timer = window.setTimeout(() => {
      contentVisible = true;
    }, MOTION.expandedBodyDelay);
    return () => window.clearTimeout(timer);
  });

  $effect(() => {
    if (activeMode === "expanded" && !suggestionsLoaded) void refreshSuggestions();
  });

  function clearCollapseTimer(): void {
    if (closeTimer !== null) window.clearTimeout(closeTimer);
    closeTimer = null;
  }

  function clearSettingsCloseTimer(): void {
    if (settingsCloseTimer !== null) window.clearTimeout(settingsCloseTimer);
    settingsCloseTimer = null;
  }

  function canStartDrag(event: PointerEvent): boolean {
    if (event.button !== 0) return false;
    const target = event.target;
    if (!(target instanceof Element)) return true;
    if (!isExpanded && target.closest(".collapsed-hit")) return true;
    return !target.closest("button, input, textarea, select, a, [contenteditable='true']");
  }

  function handleDragPointerDown(event: PointerEvent): void {
    if (!canStartDrag(event)) return;
    dragPointerId = event.pointerId;
    dragOrigin = { x: event.screenX, y: event.screenY };
    dragScreen = { ...dragOrigin };
    dragging = false;
    try {
      shell?.setPointerCapture(event.pointerId);
    } catch {
      dragPointerId = null;
      dragOrigin = null;
    }
  }

  function scheduleDragFrame(): void {
    if (dragFrame !== null) return;
    dragFrame = window.requestAnimationFrame(() => {
      dragFrame = null;
      void invokeCommand(COMMANDS.companionDragFrame, {
        x: dragScreen.x - 75,
        yFromTop: dragScreen.y - 17,
        w: 150,
        h: 34
      }).catch(() => undefined);
    });
  }

  function handleDragPointerMove(event: PointerEvent): void {
    if (dragPointerId !== event.pointerId || !dragOrigin) return;
    if (!dragging && Math.hypot(event.screenX - dragOrigin.x, event.screenY - dragOrigin.y) < 10) {
      return;
    }
    if (!dragging) {
      dragging = true;
      clearCollapseTimer();
      void invokeCommand(COMMANDS.companionDragStart).catch(() => undefined);
    }
    dragScreen = { x: event.screenX, y: event.screenY };
    scheduleDragFrame();
  }

  async function finishDrag(event: PointerEvent): Promise<void> {
    if (dragPointerId !== event.pointerId) return;
    const didDrag = dragging;
    dragPointerId = null;
    dragOrigin = null;
    dragging = false;
    if (dragFrame !== null) {
      window.cancelAnimationFrame(dragFrame);
      dragFrame = null;
    }
    if (!didDrag) return;
    suppressOpenUntil = performance.now() + 250;
    await invokeCommand(COMMANDS.companionDragEnd, { position: null }).catch(() => null);
  }

  function openCollapsed(): void {
    if (performance.now() < suppressOpenUntil) return;
    void openPassive();
  }

  function openSettings(): void {
    utilityPanel = null;
    clearSettingsCloseTimer();
    settingsMounted = true;
    settingsClosing = false;
    updateState({ settingsOpen: true });
  }

  function closeSettings(): void {
    updateState({ settingsOpen: false });
    if (!settingsMounted || settingsClosing) return;

    settingsClosing = true;
    clearSettingsCloseTimer();
    settingsCloseTimer = window.setTimeout(() => {
      settingsCloseTimer = null;
      settingsMounted = false;
      settingsClosing = false;
    }, MOTION.settingsClose);
  }

  function teardownSettings(): void {
    clearSettingsCloseTimer();
    settingsMounted = false;
    settingsClosing = false;
    updateState({ settingsOpen: false });
  }

  async function refreshSuggestions(): Promise<void> {
    try {
      const result = await invokeCommand<string[] | { suggestions?: string[] }>(
        COMMANDS.generateChatSuggestions
      );
      const values = Array.isArray(result) ? result : result?.suggestions ?? [];
      suggestions = values
        .filter((value): value is string => typeof value === "string" && value.trim().length > 0)
        .map((value) => value.trim())
        .slice(0, 4);
    } catch {
      suggestions = [];
    } finally {
      suggestionsLoaded = true;
      await invokeCommand(COMMANDS.companionSetNudgeActive, {
        active: suggestions.length > 0
      }).catch(() => undefined);
    }
  }

  async function toggleUtilityPanel(panel: "memory" | "history"): Promise<void> {
    utilityPanel = utilityPanel === panel ? null : panel;
    await invokeCommand(COMMANDS.companionSetNotificationActive, {
      active: utilityPanel === "history"
    }).catch(() => undefined);
    if (utilityPanel !== "memory") return;

    memoryPanelState = "loading";
    try {
      const response = await invokeCommand<{ items?: WorkingMemoryItem[] }>(
        COMMANDS.memoryWorkingMemory,
        { limit: 6 }
      );
      memoryItems = response?.items ?? [];
      memoryPanelState = "ready";
    } catch {
      memoryItems = [];
      memoryPanelState = "error";
    }
  }

  async function dismissNudge(): Promise<void> {
    const nudgeId = nudge?.nudge_id;
    if (!nudgeId) return;
    nudgeActionError = "";
    try {
      const response = await invokeCommand<{ dismissed?: boolean }>(
        COMMANDS.companionDismissNudge,
        { nudgeId }
      );
      if (response?.dismissed !== true) throw new Error("nudge was not dismissed");
      nudgeQueue = nudgeQueue.filter((candidate) => candidate.nudge_id !== nudgeId);
      notificationStatus = null;
      await invokeCommand(COMMANDS.companionSetNudgeActive, {
        active: nudgeQueue.length > 0
      }).catch(() => undefined);
    } catch {
      nudgeActionError = "This reminder could not be dismissed.";
    }
  }

  async function openNudge(): Promise<void> {
    const nudgeId = nudge?.nudge_id;
    if (!nudgeId || !nudgeTarget) return;
    nudgeActionError = "";
    try {
      const response = await invokeCommand<{ opened?: boolean }>(COMMANDS.companionOpenNudge, {
        nudgeId
      });
      if (response?.opened !== true) throw new Error("nudge was not opened");
      nudgeQueue = nudgeQueue.filter((candidate) => candidate.nudge_id !== nudgeId);
      notificationStatus = null;
      await invokeCommand(COMMANDS.companionSetNudgeActive, {
        active: nudgeQueue.length > 0
      }).catch(() => undefined);
    } catch {
      nudgeActionError = "This reminder could not be opened.";
    }
  }

  function syncMode(next: CompanionMode): void {
    activeMode = next;
    updateState({ companionMode: next });
  }

  async function openPassive(): Promise<void> {
    if (activeMode === "expanded") return;
    clearCollapseTimer();
    collapsedConcealed = false;
    focusWithin = false;
    try {
      await invokeCommand(COMMANDS.companionSetState, { state: "expanded" });
    } catch {
      return;
    }
    syncMode("expanded");
    // A collapsed-tab click reveals the panel without making the composer key.
    // If the pointer left while the native morph was running, preserve the
    // passive interaction by starting the normal retraction delay now.
    focusWithin = false;
    if (!pointerInside) scheduleAutoCollapse();
  }

  async function openFocused(): Promise<void> {
    clearCollapseTimer();
    try {
      await invokeCommand(COMMANDS.companionOpenFocused);
    } catch {
      return;
    }
    syncMode("expanded");
    focusWithin = true;

    const deadline = performance.now() + 1500;
    while (!composer && performance.now() < deadline) {
      await new Promise((resolve) => window.setTimeout(resolve, 30));
    }
    composer?.focus();
  }

  function transcriptionIsActive(): boolean {
    return (
      $appState.transcription === "listening" ||
      $appState.transcription === "processing" ||
      $appState.transcription === "limit"
    );
  }

  async function cancelTranscription(): Promise<void> {
    if (!transcriptionIsActive()) return;
    updateState({ transcription: "cancelled", transcriptionLevel: 0 });
    await invokeCommand(COMMANDS.transcriptionCancel).catch(() => undefined);
  }

  async function collapse(): Promise<void> {
    await cancelTranscription();
    clearCollapseTimer();
    contentVisible = false;
    focusWithin = false;
    try {
      await invokeCommand(COMMANDS.companionRollup, {
        durationMs: MOTION.panelExit
      });
    } catch {
      contentVisible = true;
      return;
    }
    teardownSettings();
    syncMode("collapsed");
    collapsedConcealed = await invokeCommand<boolean>(
      COMMANDS.companionGetCollapsedAutoHide
    ).catch(() => false);
  }

  function canAutoCollapse(): boolean {
    return (
      !$appState.settingsOpen &&
      !focusWithin &&
      !sending &&
      $appState.transcription !== "listening" &&
      $appState.transcription !== "processing"
    );
  }

  async function handlePointerEnter(): Promise<void> {
    pointerInside = true;
    collapsedConcealed = false;
    if (closeTimer !== null) window.clearTimeout(closeTimer);
    closeTimer = null;
    if (activeMode !== "collapsed" || dragging) return;
    const hoverOpen = await invokeCommand<boolean>(COMMANDS.companionGetHoverOpen).catch(
      () => false
    );
    if (hoverOpen && pointerInside && activeMode === "collapsed") await openPassive();
  }

  function handlePointerLeave(): void {
    pointerInside = false;
    pointerLeftAt = performance.now();
    if (activeMode === "collapsed") {
      void invokeCommand<boolean>(COMMANDS.companionGetCollapsedAutoHide)
        .then((autoHide) => {
          if (autoHide && !pointerInside && activeMode === "collapsed" && !dragging) {
            collapsedConcealed = true;
          }
        })
        .catch(() => undefined);
      return;
    }
    if (activeMode !== "expanded" || !canAutoCollapse()) return;
    scheduleAutoCollapse();
  }

  function scheduleAutoCollapse(): void {
    if (closeTimer !== null) window.clearTimeout(closeTimer);
    const elapsed = Math.max(0, performance.now() - pointerLeftAt);
    const delay = Math.max(0, MOTION.hoverCloseDelay - elapsed);
    closeTimer = window.setTimeout(() => {
      closeTimer = null;
      if (canAutoCollapse()) void collapse();
    }, delay);
  }

  function handleFocusOut(): void {
    window.setTimeout(() => {
      focusWithin = !!shell?.contains(document.activeElement);
    }, 0);
  }

  function toggleSettings(): void {
    if ($appState.settingsOpen || settingsMounted) closeSettings();
    else openSettings();
  }

  function newChat(): void {
    appState.update((state) => ({ ...state, messages: [] }));
    chatThreadId = crypto.randomUUID();
    prompt = "";
    activeResponseId = null;
    sending = false;
    utilityPanel = null;
    pendingAttachment = null;
    pendingOpenSource = null;
    suggestionsLoaded = false;
    void refreshSuggestions();
    void tick().then(() => composer?.focus());
  }

  async function toggleCapture(): Promise<void> {
    const paused = $appState.capture === "paused";
    const previous = $appState.capture;
    suppressNextUnverifiedActive = paused;
    updateState({ capture: paused ? "starting" : "paused" });
    try {
      await invokeCommand(paused ? COMMANDS.captureResume : COMMANDS.capturePause);
    } catch {
      suppressNextUnverifiedActive = false;
      updateState({ capture: previous });
    }
  }

  async function send(text = prompt): Promise<void> {
    const clean = text.trim();
    if (!clean || sending) return;
    const history: ChatHistoryMessage[] = [];
    for (const message of $appState.messages) {
      if (
        message.id === "welcome" ||
        message.pending ||
        !message.content.trim() ||
        message.role !== (history.length % 2 === 0 ? "user" : "assistant")
      ) {
        continue;
      }
      history.push({ role: message.role, content: message.content });
    }
    if (history.length % 2 !== 0) history.pop();
    const boundedHistory = history.slice(-20);
    prompt = "";
    utilityPanel = null;
    sending = true;
    addMessage({ role: "user", content: clean });
    const responseId = addMessage({ role: "assistant", content: "", pending: true });
    activeResponseId = responseId;
    receivedStreamingDelta = false;
    await tick();
    messagesEnd?.scrollIntoView({ behavior: "smooth" });

    try {
      const request: ChatRequest = {
        text: clean,
        threadId: chatThreadId,
        history: boundedHistory,
        mode: "chat"
      };
      const result = await invokeCommand<string | { content: string }>(COMMANDS.chatSend, {
        request
      });
      const content =
        typeof result === "string"
          ? result
          : result?.content ?? "I couldn’t find enough local context to answer that yet.";
      if (!receivedStreamingDelta) {
        for (const token of content.match(/.{1,18}(?:\s|$)/g) ?? [content]) {
          appendMessage(responseId, token);
          await new Promise((resolve) => window.setTimeout(resolve, 18));
        }
      }
      finishMessage(responseId);
    } catch {
      if (activeResponseId === responseId) {
        appendMessage(responseId, "I couldn’t complete that request. Please try again.");
        finishMessage(responseId);
      }
    } finally {
      sending = false;
      activeResponseId = null;
      await tick();
      messagesEnd?.scrollIntoView({ behavior: "smooth" });
    }
  }

  async function handleOpenChat(payload: OpenChatPayload = {}): Promise<void> {
    // `attachment` is local UI metadata. Preserve it in memory, but never
    // render, log, read as a path, or add it to ChatRequest until the native
    // request contract defines attachment transport.
    if (typeof payload.attachment === "string" && payload.attachment.length > 0) {
      pendingAttachment = payload.attachment;
    }
    pendingOpenSource =
      typeof payload.source === "string" && payload.source.length > 0
        ? payload.source
        : null;
    const prefill = typeof payload.prefill === "string" ? payload.prefill : "";
    if (prefill) prompt = prefill;

    await openFocused();
    if (payload.auto_send === true && prefill.trim()) {
      await send(prefill);
    }
  }

  async function toggleTranscription(): Promise<void> {
    if ($appState.transcription === "listening") {
      updateState({ transcription: "processing" });
      await invokeCommand(COMMANDS.transcriptionFinalize).catch(() =>
        updateState({ transcription: "failed", transcriptionLevel: 0 })
      );
      return;
    }
    if (transcriptionIsActive()) {
      await cancelTranscription();
      return;
    }

    transcript = "";
    transcriptItems = [];
    updateState({ transcription: "listening", transcriptionLevel: 0.15 });
    await invokeCommand(COMMANDS.transcriptionStart, { trigger: "fn_voice_chat" }).catch(
      () => updateState({ transcription: "failed", transcriptionLevel: 0 })
    );
  }

  function updateTranscriptItem(payload: TranscriptionItemPayload): void {
    const item = transcriptionItemFromPayload(payload);
    if (!item) return;
    const index = transcriptItems.findIndex((candidate) => candidate.item_id === item.item_id);
    if (index === -1) {
      transcriptItems = [...transcriptItems, item];
    } else {
      transcriptItems = transcriptItems.map((candidate, candidateIndex) =>
        candidateIndex === index ? item : candidate
      );
    }
    transcript = transcriptItems
      .map((candidate) => candidate.text.trim())
      .filter(Boolean)
      .join(" ");
    prompt = transcript;
  }

  async function cancelChat(): Promise<void> {
    await invokeCommand(COMMANDS.chatCancel).catch(() => undefined);
    if (activeResponseId) finishMessage(activeResponseId);
    activeResponseId = null;
    sending = false;
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape") return;
    event.preventDefault();
    if (transcriptionIsActive()) void cancelTranscription();
    if ($appState.settingsOpen || settingsMounted) closeSettings();
    else void collapse();
  }

  onMount(() => {
    if (initialSettings) updateState({ settingsOpen: true });
    if (activeMode === "expanded") void refreshSuggestions();
    const unlisteners: Array<() => void> = [];
    void invokeCommand<DockPosition>(COMMANDS.companionGetPosition)
      .then((position) => (dockPosition = position))
      .catch(() => undefined);
    void invokeCommand<boolean>(COMMANDS.companionGetCollapsedAutoHide)
      .then((autoHide) => (collapsedConcealed = autoHide && activeMode === "collapsed"))
      .catch(() => undefined);
    void listenEvent<DockPosition>(EVENTS.panelPosition, (position) => {
      dockPosition = position;
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<PositionDragPayload>(EVENTS.positionDrag, (payload) => {
      if (!payload.active) dragging = false;
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<TranscriptionLevelPayload>(EVENTS.transcriptionLevel, (payload) => {
      const level = transcriptionLevelFromPayload(payload);
      updateState({ transcriptionLevel: level, transcription: "listening" });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<TranscriptionStartPayload>(EVENTS.transcriptionStart, () => {
      transcript = "";
      transcriptItems = [];
      updateState({ transcription: "listening" });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<TranscriptionItemPayload>(EVENTS.transcriptionPartial, (payload) => {
      updateTranscriptItem(payload);
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<TranscriptionItemPayload>(
      EVENTS.transcriptionItemCompleted,
      (payload) => updateTranscriptItem(payload)
    ).then((fn) => unlisteners.push(fn));
    void listenEvent<string>(EVENTS.transcriptionCompleted, (text) => {
      if (typeof text !== "string") return;
      prompt = text;
      transcript = text;
      transcriptItems = [];
      composer?.focus();
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<TranscriptionProcessingPayload | undefined>(
      EVENTS.transcriptionProcessing,
      () => {
        updateState({ transcription: "processing", transcriptionLevel: 0 });
      }
    ).then((fn) => unlisteners.push(fn));
    void listenEvent(EVENTS.transcriptionDone, () => {
      updateState({ transcription: "done", transcriptionLevel: 0 });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent(EVENTS.transcriptionCancelled, () => {
      updateState({ transcription: "cancelled", transcriptionLevel: 0 });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent(EVENTS.transcriptionFailed, () => {
      updateState({ transcription: "failed", transcriptionLevel: 0 });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent(EVENTS.transcriptionOverflow, () => {
      updateState({ transcription: "overflow", transcriptionLevel: 0 });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent(EVENTS.transcriptionLimit, () => {
      updateState({ transcription: "limit", transcriptionLevel: 0 });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<HealthChangedPayload>(EVENTS.healthChanged, (payload) => {
      updateState({ health: payload.state });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<NativeChatState>(EVENTS.chatState, (payload) => {
      const next = companionModeFromState(payload);
      if (next !== "expanded") teardownSettings();
      syncMode(next);
    }).then((fn) => unlisteners.push(fn));
    void listenEvent(EVENTS.willRetract, () => {
      contentVisible = false;
      teardownSettings();
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<OpenChatPayload | undefined>(EVENTS.openChat, (payload) => {
      void handleOpenChat(payload ?? {});
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<boolean>(EVENTS.capturePaused, (paused) => {
      suppressNextUnverifiedActive = !paused;
      updateState({ capture: paused ? "paused" : "starting" });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<{ state: CaptureState }>(EVENTS.captureChanged, ({ state }) => {
      if (!["active", "paused", "starting", "permission-revoked", "error"].includes(state)) {
        return;
      }
      if (state === "active" && suppressNextUnverifiedActive) {
        suppressNextUnverifiedActive = false;
        updateState({ capture: "starting" });
        return;
      }
      suppressNextUnverifiedActive = false;
      updateState({ capture: state });
    }).then((fn) => unlisteners.push(fn));
    void listenEvent(EVENTS.openSettings, () => {
      void openFocused().then(openSettings);
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<string>(EVENTS.chatDelta, (delta) => {
      if (!activeResponseId) return;
      receivedStreamingDelta = true;
      appendMessage(activeResponseId, delta);
      void tick().then(() => messagesEnd?.scrollIntoView({ behavior: "smooth" }));
    }).then((fn) => unlisteners.push(fn));
    void listenEvent(EVENTS.chatComplete, () => {
      if (activeResponseId) finishMessage(activeResponseId);
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<NudgePayload>(EVENTS.nudgeReady, (payload) => {
      if (
        !payload?.nudge_id ||
        !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
          payload.nudge_id
        ) ||
        !payload.title ||
        !payload.body
      ) return;
      const existing = nudgeQueue.findIndex(
        (candidate) => candidate.nudge_id === payload.nudge_id
      );
      if (existing >= 0) {
        nudgeQueue = nudgeQueue.map((candidate, index) =>
          index === existing ? payload : candidate
        );
      } else if (nudgeQueue.length < 500) {
        nudgeQueue = [...nudgeQueue, payload];
      }
      nudgeActionError = "";
      notificationStatus = null;
      utilityPanel = null;
      void invokeCommand(COMMANDS.companionSetNudgeActive, { active: true }).catch(
        () => undefined
      );
    }).then((fn) => unlisteners.push(fn));
    void listenEvent<{ status?: "denied" | "failed" }>(
      EVENTS.notificationStatus,
      (payload) => {
        notificationStatus = payload?.status ?? null;
      }
    ).then((fn) => unlisteners.push(fn));

    return () => {
      clearCollapseTimer();
      clearSettingsCloseTimer();
      if (dragFrame !== null) window.cancelAnimationFrame(dragFrame);
      unlisteners.forEach((unlisten) => unlisten());
      if (transcriptionIsActive()) {
        void invokeCommand(COMMANDS.transcriptionCancel).catch(() => undefined);
      }
      void invokeCommand(COMMANDS.companionSetNudgeActive, { active: false }).catch(
        () => undefined
      );
      void invokeCommand(COMMANDS.companionSetNotificationActive, { active: false }).catch(
        () => undefined
      );
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<main
  bind:this={shell}
  class:expanded={isExpanded}
  class:collapsed={isCollapsed}
  class:hidden={activeMode === "hidden"}
  class:paused={$appState.capture === "paused"}
  class:dragging
  class:auto-concealed={collapsedConcealed && !dragging}
  class:pos-top={dockPosition === "top"}
  class:pos-left={dockPosition === "left"}
  class:pos-right={dockPosition === "right"}
  class:pos-bottom={dockPosition === "bottom"}
  class:pos-bottom-left={dockPosition === "bottom-left"}
  class:pos-bottom-right={dockPosition === "bottom-right"}
  class:pos-corner={isCornerDock}
  class:pos-edge={isSideDock}
  class="dock-shell"
  data-state={activeMode}
  data-testid="companion-shell"
  onpointerenter={() => void handlePointerEnter()}
  onpointerleave={handlePointerLeave}
  onpointerdown={handleDragPointerDown}
  onpointermove={handleDragPointerMove}
  onpointerup={(event) => void finishDrag(event)}
  onpointercancel={(event) => void finishDrag(event)}
  onfocusin={() => (focusWithin = true)}
  onfocusout={handleFocusOut}
>
  {#if isCollapsed}
    <button
      class="collapsed-hit"
      onclick={openCollapsed}
      aria-label="Open woof"
    ></button>
  {:else if settingsMounted}
    <div
      class:visible={contentVisible && !settingsClosing}
      class:closing={settingsClosing}
      class="settings-wrap"
      data-state={settingsClosing ? "closing" : "open"}
      data-testid="companion-settings"
      style:transition-duration={`${MOTION.settingsClose}ms`}
    >
      <SettingsPanel dock onclose={closeSettings} />
    </div>
  {:else}
    <section class:visible={contentVisible} class="chat fade-target" aria-label="woof chat">
      <header class="chat-header">
        <div class="header-left no-drag">
          <button class="brand" aria-label="woof home">
            <Mascot
              size={28}
              animate={false}
              mood={sending
                ? "thinking"
                : $appState.transcription === "listening"
                  ? "listening"
                  : "calm"}
            />
          </button>
          <button class="round-button" aria-label="Settings" onclick={toggleSettings}>
            <Settings size={14} strokeWidth={1.8} />
          </button>
          <button
            class:active={utilityPanel === "memory"}
            class="round-button"
            aria-label="Memory"
            aria-pressed={utilityPanel === "memory"}
            onclick={() => void toggleUtilityPanel("memory")}
          >
            <Share2 size={14} strokeWidth={1.8} />
          </button>
        </div>
        <div class="header-actions no-drag">
          <button
            class:active={utilityPanel === "history"}
            class="round-button"
            aria-label="Chat history"
            aria-pressed={utilityPanel === "history"}
            onclick={() => void toggleUtilityPanel("history")}
          >
            <Inbox size={14} strokeWidth={1.8} />
          </button>
          <button class="round-button" aria-label="New chat" onclick={newChat}>
            <Plus size={16} strokeWidth={1.8} />
          </button>
          <button class="esc-button" aria-label="Collapse" onclick={collapse}>esc</button>
        </div>
      </header>

      {#if $appState.capture === "paused"}
        <div class="capture-banner">
          <Pause size={11} />
          <span>Capture is paused</span>
          <button onclick={toggleCapture}><Play size={10} /> Resume</button>
        </div>
      {:else if $appState.capture === "permission-revoked"}
        <div class="capture-banner attention" role="status">
          <AlertTriangle size={11} />
          <span>Accessibility permission is needed</span>
          <button onclick={() => void invokeCommand(COMMANDS.openAccessibilitySettings).catch(() => undefined)}>
            <Settings size={10} /> Open settings
          </button>
        </div>
      {:else if $appState.capture === "error"}
        <div class="capture-banner attention" role="status">
          <AlertTriangle size={11} />
          <span>Capture is unavailable</span>
        </div>
      {:else if $appState.capture === "starting"}
        <div class="capture-banner" role="status">
          <LoaderCircle class="spin" size={11} />
          <span>Capture is starting</span>
        </div>
      {/if}

      {#if nudge}
        <aside class="nudge-card" aria-label="woof nudge">
          <span>
            <b>{nudge.title}</b>
            <small>{nudge.body}</small>
            {#if notificationStatus === "denied"}
              <small class="notification-warning">System notifications are disabled.</small>
            {:else if notificationStatus === "failed"}
              <small class="notification-warning">System notification delivery failed.</small>
            {/if}
            {#if nudgeActionError}
              <small class="notification-warning" role="alert">{nudgeActionError}</small>
            {/if}
          </span>
          {#if nudgeTarget}
            <button onclick={() => void openNudge()}>Open</button>
          {/if}
          <button aria-label="Dismiss nudge" onclick={() => void dismissNudge()}>×</button>
        </aside>
      {/if}

      <div class="messages" aria-live="polite">
        <div class="messages-inner">
          <div class="messages-spacer"></div>
          {#if utilityPanel === "memory"}
            <section class="utility-card" aria-label="Working memory">
              <header><span>Working memory</span><button onclick={() => void toggleUtilityPanel("memory")}>Done</button></header>
              {#if memoryPanelState === "loading"}
                <p class="utility-state">Loading local context…</p>
              {:else if memoryPanelState === "error"}
                <p class="utility-state">The local memory service is unavailable.</p>
              {:else if memoryItems.length === 0}
                <p class="utility-state">No working-memory items yet.</p>
              {:else}
                <div class="utility-list">
                  {#each memoryItems as item (item.wm_id)}
                    <button onclick={() => (prompt = `Tell me about ${item.window_title || item.app}`)}>
                      <b>{item.window_title || item.app}</b>
                      <span>{item.content}</span>
                      <small>{item.app}</small>
                    </button>
                  {/each}
                </div>
              {/if}
            </section>
          {:else if utilityPanel === "history"}
            <section class="utility-card" aria-label="Chat history panel">
              <header><span>Current chat</span><button onclick={() => void toggleUtilityPanel("history")}>Done</button></header>
              {#if $appState.messages.length === 0}
                <p class="utility-state">No messages in this local session.</p>
              {:else}
                <div class="history-list">
                  {#each $appState.messages as message (message.id)}
                    <p><b>{message.role === "user" ? "You" : "woof"}</b><span>{message.content || "Thinking…"}</span></p>
                  {/each}
                </div>
              {/if}
            </section>
          {:else if $appState.messages.length === 0}
            <div class="empty">Message woof to search your local memory.</div>
          {:else}
            {#each $appState.messages as message (message.id)}
              <article class:user={message.role === "user"} class:assistant={message.role === "assistant"}>
                <div class:pending={message.pending} class="message-card">
                  {#if message.content}
                    {message.content}
                  {:else}
                    <span class="thinking-dots"><i></i><i></i><i></i></span>
                  {/if}
                </div>
              </article>
            {/each}
          {/if}
          <div bind:this={messagesEnd}></div>
        </div>
      </div>

      {#if utilityPanel === null && $appState.messages.length <= 1 && suggestions.length > 0}
        <div class="suggestion-chips">
          {#each suggestions as suggestion}
            <button onclick={() => send(suggestion)}>{suggestion}</button>
          {/each}
        </div>
      {/if}

      <div class="composer">
        <div class="input-wrap">
          <textarea
            bind:this={composer}
            bind:value={prompt}
            rows="1"
            placeholder="Message woof…"
            aria-label="Message woof"
            disabled={sending}
            onkeydown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void send();
              }
            }}
          ></textarea>
          {#if $appState.transcription === "listening"}
            <button class="voice-button" onclick={toggleTranscription} aria-label="Finish dictation">
              <span class="voice-bars" aria-hidden="true">
                {#each [0.45, 0.8, 1, 0.62, 0.36] as factor}
                  <i style:height={`${Math.max(3, $appState.transcriptionLevel * factor * 22)}px`}></i>
                {/each}
              </span>
            </button>
          {:else}
            <button
              class="send-button"
              disabled={!prompt.trim() && !sending}
              onclick={() => (sending ? cancelChat() : send())}
              aria-label={sending ? "Stop response" : "Send"}
            >
              {#if sending}
                <Square size={10} fill="currentColor" />
              {:else}
                <ArrowUp size={15} strokeWidth={2.2} />
              {/if}
            </button>
          {/if}
        </div>
        <button class="dictation-shortcut" onclick={toggleTranscription} aria-label="Start dictation">
          <Mic size={10} />
          <span>hold to talk</span>
        </button>
      </div>
    </section>
  {/if}

  {#if isExpanded}
    <div
      class="resize-grip"
      data-testid="companion-resize-grip"
      aria-hidden="true"
      style:width={`${WINDOWS.companion.resizeGripSize}px`}
      style:height={`${WINDOWS.companion.resizeGripSize}px`}
    >
      {#each Array(6) as _}<i></i>{/each}
    </div>
  {/if}
</main>

<style>
  .dock-shell {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    color: rgba(255, 255, 255, 0.92);
    border-radius: 0 0 16px 16px;
    isolation: isolate;
    transition:
      background 220ms ease,
      border-color 220ms ease,
      box-shadow 220ms ease;
  }

  .dock-shell::before {
    content: "";
    position: absolute;
    inset: 0;
    z-index: -1;
    pointer-events: none;
  }

  .dock-shell.pos-bottom {
    border-radius: 16px 16px 0 0;
  }

  .dock-shell.pos-left {
    border-radius: 0 16px 16px 0;
  }

  .dock-shell.pos-right {
    border-radius: 16px 0 0 16px;
  }

  .dock-shell.pos-bottom-left {
    border-radius: 0 16px 0 0;
  }

  .dock-shell.pos-bottom-right {
    border-radius: 16px 0 0 0;
  }

  .dock-shell.collapsed {
    border: 1px solid rgba(232, 113, 42, 0.52);
    border-top: 0;
    background: rgba(69, 55, 45, 0.44);
    box-shadow:
      inset 0 -1px rgba(255, 190, 126, 0.2),
      0 4px 12px rgba(42, 24, 12, 0.22),
      0 0 12px rgba(232, 113, 42, 0.16);
    -webkit-backdrop-filter: blur(22px) saturate(1.3);
    backdrop-filter: blur(22px) saturate(1.3);
  }

  .dock-shell.dragging {
    cursor: grabbing;
  }

  .dock-shell.collapsed.pos-bottom {
    border-top: 1px solid rgba(232, 113, 42, 0.52);
    border-bottom: 0;
    border-radius: 16px 16px 0 0;
  }

  .dock-shell.collapsed.pos-left,
  .dock-shell.collapsed.pos-right {
    border-top: 1px solid rgba(232, 113, 42, 0.52);
  }

  .dock-shell.collapsed.pos-left {
    border-left: 0;
    border-radius: 0 16px 16px 0;
  }

  .dock-shell.collapsed.pos-right {
    border-right: 0;
    border-radius: 16px 0 0 16px;
  }

  .dock-shell.collapsed.pos-corner {
    border-top: 1px solid rgba(232, 113, 42, 0.52);
    border-bottom: 0;
  }

  .dock-shell.collapsed.pos-bottom-left {
    border-left: 0;
  }

  .dock-shell.collapsed.pos-bottom-right {
    border-right: 0;
  }

  .dock-shell.collapsed::before {
    background:
      linear-gradient(180deg, rgba(255, 180, 104, 0.18), rgba(105, 72, 47, 0.08) 58%, rgba(25, 25, 25, 0.18)),
      radial-gradient(ellipse at 50% -20%, rgba(255, 181, 104, 0.2), transparent 72%);
  }

  .dock-shell.collapsed::after {
    content: "";
    position: absolute;
    left: 50%;
    bottom: 1px;
    width: min(110px, 46vw);
    height: 2px;
    border-radius: 999px;
    transform: translateX(-50%);
    background: rgba(232, 113, 42, 0.72);
    box-shadow: 0 0 8px rgba(232, 113, 42, 0.42);
    pointer-events: none;
  }

  .dock-shell.collapsed.pos-bottom::after {
    top: 1px;
    bottom: auto;
  }

  .dock-shell.collapsed.pos-left::after,
  .dock-shell.collapsed.pos-right::after {
    top: 50%;
    bottom: auto;
    left: auto;
    right: 1px;
    width: 2px;
    height: min(110px, 46vh);
    transform: translateY(-50%);
  }

  .dock-shell.collapsed.pos-right::after {
    right: auto;
    left: 1px;
  }

  .dock-shell.collapsed.pos-corner::after {
    top: 50%;
    bottom: auto;
    width: 5px;
    height: 5px;
    transform: translate(-50%, -50%);
  }

  .dock-shell.collapsed.paused {
    filter: saturate(0.62);
  }

  .dock-shell.collapsed.auto-concealed {
    border-color: transparent;
    background: transparent;
    box-shadow: none;
  }

  .dock-shell.collapsed.auto-concealed::before,
  .dock-shell.collapsed.auto-concealed::after {
    opacity: 0;
  }

  .collapsed-hit {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    padding: 0;
    border: 0;
    border-radius: inherit;
    background: transparent;
    cursor: pointer;
  }

  .dock-shell.expanded {
    border: 1px solid rgba(255, 255, 255, 0.17);
    border-top: 0;
    background: rgba(31, 30, 30, 0.56);
    box-shadow:
      inset 0 1px rgba(255, 255, 255, 0.08),
      0 12px 38px rgba(0, 0, 0, 0.3),
      0 0 14px rgba(232, 113, 42, 0.12);
    -webkit-backdrop-filter: blur(30px) saturate(1.3);
    backdrop-filter: blur(30px) saturate(1.3);
  }

  .dock-shell.expanded.pos-bottom,
  .dock-shell.expanded.pos-corner {
    border-top: 1px solid rgba(255, 255, 255, 0.17);
    border-bottom: 0;
  }

  .dock-shell.expanded.pos-left,
  .dock-shell.expanded.pos-right {
    border-top: 1px solid rgba(255, 255, 255, 0.17);
  }

  .dock-shell.expanded.pos-left,
  .dock-shell.expanded.pos-bottom-left {
    border-left: 0;
  }

  .dock-shell.expanded.pos-right,
  .dock-shell.expanded.pos-bottom-right {
    border-right: 0;
  }

  .dock-shell.expanded::before {
    background:
      radial-gradient(ellipse 85% 165px at 24% -10px, rgba(244, 136, 45, 0.54), transparent 76%),
      linear-gradient(180deg, rgba(109, 54, 22, 0.46) 0, rgba(55, 37, 30, 0.35) 130px, rgba(26, 27, 28, 0.35) 255px, rgba(26, 27, 28, 0.48));
  }

  .fade-target {
    opacity: 0;
    pointer-events: none;
    transition: opacity 220ms ease;
  }

  .fade-target.visible {
    opacity: 1;
    pointer-events: auto;
  }

  .chat {
    position: absolute;
    inset: 0;
    display: grid;
    grid-template-rows: auto auto auto minmax(0, 1fr) auto auto;
    grid-template-columns: minmax(0, 1fr);
  }

  .chat-header {
    grid-row: 1;
    grid-column: 1;
    z-index: 3;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 12px 4px;
  }

  .header-left,
  .header-actions {
    display: flex;
    align-items: center;
  }

  .header-left {
    gap: 6px;
  }

  .header-actions {
    gap: 4px;
  }

  .brand,
  .round-button {
    box-sizing: border-box;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-radius: 50%;
    color: rgba(255, 255, 255, 0.66);
    background: rgba(60, 60, 60, 0.32);
    -webkit-backdrop-filter: blur(18px) saturate(1.25);
    backdrop-filter: blur(18px) saturate(1.25);
    cursor: pointer;
    transition:
      opacity 120ms,
      color 120ms,
      background 120ms,
      border-color 120ms,
      transform 120ms;
  }

  .brand {
    width: 30px;
    height: 30px;
    overflow: hidden;
    color: inherit;
  }

  .brand:active {
    transform: scale(0.94);
  }

  .round-button {
    width: 26px;
    height: 26px;
    opacity: 0.75;
  }

  .brand:hover,
  .round-button:hover {
    opacity: 1;
    color: rgba(255, 255, 255, 0.95);
    border-color: rgba(255, 255, 255, 0.3);
    background: rgba(60, 60, 60, 0.5);
  }

  .round-button.active {
    opacity: 1;
    color: #ffb27d;
    border-color: rgba(232, 113, 42, 0.42);
    background: rgba(232, 113, 42, 0.18);
  }

  .esc-button {
    width: 36px;
    height: 26px;
    padding: 0;
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-radius: 999px;
    color: rgba(255, 255, 255, 0.78);
    background: rgba(60, 60, 60, 0.32);
    -webkit-backdrop-filter: blur(18px) saturate(1.25);
    backdrop-filter: blur(18px) saturate(1.25);
    font-size: 11px;
    line-height: 1;
    cursor: pointer;
  }

  .esc-button:hover {
    color: #fff;
    background: rgba(60, 60, 60, 0.5);
  }

  .capture-banner {
    grid-row: 2;
    grid-column: 1;
    z-index: 3;
    height: 29px;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 13px;
    color: #ffb37a;
    border-top: 1px solid rgba(232, 113, 42, 0.12);
    border-bottom: 1px solid rgba(232, 113, 42, 0.18);
    background: rgba(232, 113, 42, 0.08);
    font-size: 10px;
    font-weight: 620;
  }

  .capture-banner button {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-left: auto;
    padding: 0;
    border: 0;
    color: inherit;
    background: transparent;
    font-size: inherit;
    cursor: pointer;
  }

  .capture-banner.attention {
    color: #ff9f91;
    border-color: rgba(235, 111, 94, 0.2);
    background: rgba(235, 111, 94, 0.09);
  }

  .capture-banner :global(.spin) {
    animation: capture-spin 0.9s linear infinite;
  }

  @keyframes capture-spin {
    to {
      transform: rotate(360deg);
    }
  }

  .nudge-card {
    grid-row: 3;
    grid-column: 1;
    z-index: 3;
    min-height: 47px;
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 2px 11px 5px;
    padding: 7px 8px 7px 11px;
    border: 1px solid rgba(255, 178, 125, 0.28);
    border-radius: 13px;
    color: rgba(255, 255, 255, 0.9);
    background: rgba(84, 53, 35, 0.54);
    box-shadow: inset 0 1px rgba(255, 255, 255, 0.07);
    animation: nudge-in 220ms cubic-bezier(0.16, 1, 0.3, 1);
  }

  .nudge-card > span {
    min-width: 0;
    flex: 1;
  }

  .nudge-card b,
  .nudge-card small {
    display: block;
  }

  .nudge-card b {
    font-size: 9.5px;
  }

  .nudge-card small {
    display: -webkit-box;
    margin-top: 3px;
    overflow: hidden;
    color: rgba(255, 255, 255, 0.56);
    font-size: 8.5px;
    line-height: 1.3;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  .nudge-card .notification-warning {
    color: #ffb27d;
  }

  .nudge-card > button {
    flex: 0 0 auto;
    min-width: 24px;
    height: 24px;
    padding: 0 7px;
    border: 1px solid rgba(255, 255, 255, 0.13);
    border-radius: 8px;
    color: #ffb27d;
    background: rgba(255, 255, 255, 0.06);
    font-size: 9px;
    cursor: pointer;
  }

  @keyframes nudge-in {
    from {
      opacity: 0;
      transform: translateY(-5px);
    }
  }

  .messages {
    grid-row: 4;
    grid-column: 1;
    z-index: 1;
    min-height: 0;
    overflow-x: hidden;
    overflow-y: auto;
    padding: 4px 12px;
    scrollbar-color: rgba(255, 255, 255, 0.18) transparent;
    -webkit-mask-image: linear-gradient(to bottom, transparent 0, black 14px, black 100%);
    mask-image: linear-gradient(to bottom, transparent 0, black 14px, black 100%);
  }

  .messages::-webkit-scrollbar {
    width: 3px;
  }

  .messages::-webkit-scrollbar-thumb {
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.18);
  }

  .messages-inner {
    min-height: 100%;
    display: flex;
    flex-direction: column;
    gap: 3px;
    overflow-anchor: none;
  }

  .messages-spacer {
    min-height: 0;
    flex: 1 0 0;
  }

  .empty {
    margin: auto 0 8px;
    padding: 0 16px;
    color: rgba(255, 255, 255, 0.5);
    font-size: 12px;
    line-height: 1.6;
    text-align: center;
  }

  .utility-card {
    max-height: 255px;
    overflow: hidden;
    border: 1px solid rgba(255, 255, 255, 0.13);
    border-radius: 15px;
    background: rgba(35, 35, 37, 0.66);
    box-shadow: inset 0 1px rgba(255, 255, 255, 0.06);
  }

  .utility-card > header {
    min-height: 37px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 11px 0 13px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.1);
    color: rgba(255, 255, 255, 0.82);
    font-size: 10px;
    font-weight: 680;
  }

  .utility-card > header button {
    padding: 4px 7px;
    border: 0;
    color: #ffb27d;
    background: transparent;
    font-size: 9px;
    cursor: pointer;
  }

  .utility-state {
    min-height: 74px;
    display: grid;
    place-items: center;
    margin: 0;
    padding: 12px;
    color: rgba(255, 255, 255, 0.48);
    font-size: 10px;
    text-align: center;
  }

  .utility-list,
  .history-list {
    max-height: 214px;
    overflow-y: auto;
  }

  .utility-list > button {
    width: 100%;
    display: block;
    padding: 9px 12px;
    border: 0;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    color: rgba(255, 255, 255, 0.82);
    background: transparent;
    text-align: left;
    cursor: pointer;
  }

  .utility-list > button:hover {
    background: rgba(255, 255, 255, 0.06);
  }

  .utility-list b,
  .utility-list span,
  .utility-list small {
    display: block;
  }

  .utility-list b {
    font-size: 9.5px;
  }

  .utility-list span {
    display: -webkit-box;
    margin-top: 3px;
    overflow: hidden;
    color: rgba(255, 255, 255, 0.56);
    font-size: 8.5px;
    line-height: 1.35;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  .utility-list small {
    margin-top: 3px;
    color: rgba(255, 178, 125, 0.72);
    font-size: 7.5px;
  }

  .history-list p {
    display: grid;
    grid-template-columns: 39px minmax(0, 1fr);
    gap: 7px;
    margin: 0;
    padding: 8px 11px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    font-size: 8.5px;
    line-height: 1.4;
  }

  .history-list b {
    color: #ffb27d;
  }

  .history-list span {
    overflow-wrap: anywhere;
    color: rgba(255, 255, 255, 0.64);
  }

  article {
    display: flex;
    margin-top: 2px;
  }

  article.user {
    justify-content: flex-end;
  }

  article.assistant {
    justify-content: flex-start;
  }

  .message-card {
    position: relative;
    max-width: 80%;
    padding: 7px 10px;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 17px;
    color: rgba(255, 255, 255, 0.95);
    background: rgba(60, 60, 60, 0.6);
    box-shadow: 0 2px 16px rgba(0, 0, 0, 0.18), inset 0 1px rgba(255, 255, 255, 0.08);
    font-size: 12px;
    line-height: 1.45;
    overflow-wrap: break-word;
    user-select: text;
  }

  article.assistant .message-card {
    border-bottom-left-radius: 5px;
  }

  article.user .message-card {
    color: #fff;
    border-color: transparent;
    border-bottom-right-radius: 5px;
    background: rgba(232, 113, 42, 0.85);
  }

  .message-card.pending::after {
    content: "";
    display: inline-block;
    width: 1px;
    height: 12px;
    margin-left: 2px;
    vertical-align: -2px;
    background: #ffb37a;
    animation: blink 760ms infinite;
  }

  .thinking-dots {
    display: flex;
    align-items: center;
    gap: 6px;
    width: fit-content;
    padding: 2px 0;
  }

  .thinking-dots i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: rgba(232, 113, 42, 0.7);
    animation: typing 1.2s ease-in-out infinite;
  }

  .thinking-dots i:nth-child(2) {
    animation-delay: 200ms;
  }

  .thinking-dots i:nth-child(3) {
    animation-delay: 400ms;
  }

  .suggestion-chips {
    grid-row: 5;
    grid-column: 1;
    z-index: 3;
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    padding: 0 12px 6px;
  }

  .suggestion-chips button {
    padding: 4px 10px;
    border: 1px solid rgba(255, 255, 255, 0.13);
    border-radius: 999px;
    color: rgba(255, 255, 255, 0.7);
    background: rgba(255, 255, 255, 0.07);
    font-size: 11px;
    line-height: 1.4;
    cursor: pointer;
  }

  .suggestion-chips button:hover {
    color: rgba(255, 255, 255, 0.95);
    background: rgba(255, 255, 255, 0.14);
  }

  .composer {
    grid-row: 6;
    grid-column: 1;
    z-index: 3;
    position: relative;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0;
    padding: 4px 10px 7px;
  }

  .input-wrap {
    position: relative;
    isolation: isolate;
  }

  textarea {
    width: 100%;
    min-height: 38px;
    max-height: 120px;
    box-sizing: border-box;
    resize: none;
    padding: 9px 46px 9px 14px;
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-radius: 19px;
    outline: none;
    color: inherit;
    background: rgba(52, 52, 56, 0.82);
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.1), inset 0 1px rgba(255, 255, 255, 0.12);
    font-size: 12.5px;
    line-height: 1.4;
    user-select: text;
    transition:
      border-color 150ms,
      background 150ms,
      box-shadow 150ms;
  }

  textarea::placeholder {
    color: rgba(255, 255, 255, 0.35);
  }

  textarea:focus {
    border-color: rgba(255, 255, 255, 0.3);
    background: rgba(58, 58, 62, 0.9);
    box-shadow: 0 2px 16px rgba(0, 0, 0, 0.12), inset 0 1px rgba(255, 255, 255, 0.16), 0 0 0 1px rgba(255, 255, 255, 0.06);
  }

  textarea:disabled {
    opacity: 0.72;
  }

  .send-button,
  .voice-button {
    position: absolute;
    right: 8px;
    bottom: 7px;
    width: 24px;
    height: 24px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: 0;
    border-radius: 50%;
    color: rgba(0, 0, 0, 0.65);
    background: rgba(255, 255, 255, 0.8);
    cursor: pointer;
    transition:
      color 120ms,
      background 120ms,
      opacity 120ms;
  }

  .send-button:hover:not(:disabled) {
    color: rgba(0, 0, 0, 0.85);
    background: #fff;
  }

  .send-button:disabled {
    opacity: 0.25;
    cursor: default;
  }

  .voice-button {
    color: #fff;
    background: rgba(232, 113, 42, 0.8);
  }

  .voice-bars {
    width: 18px;
    height: 18px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 1px;
  }

  .voice-bars i {
    width: 2px;
    min-height: 3px;
    border-radius: 2px;
    background: #fff;
    transition: height 60ms ease-out;
  }

  .dictation-shortcut {
    position: absolute;
    right: 46px;
    bottom: 17px;
    display: none;
    align-items: center;
    gap: 3px;
    padding: 0;
    border: 0;
    color: rgba(255, 255, 255, 0.42);
    background: transparent;
    font-size: 9px;
    cursor: pointer;
  }

  .settings-wrap {
    position: absolute;
    inset: 0;
    overflow: hidden;
    border-radius: 0 0 16px 16px;
    background: rgba(0, 0, 0, 0.24);
    opacity: 0;
    pointer-events: none;
    transition-property: opacity;
    transition-timing-function: ease;
  }

  .settings-wrap.visible {
    opacity: 1;
    pointer-events: auto;
  }

  .settings-wrap.closing {
    opacity: 0;
    pointer-events: none;
  }

  .resize-grip {
    position: absolute;
    right: 2px;
    bottom: 2px;
    z-index: 8;
    pointer-events: none;
    opacity: 0.58;
  }

  .resize-grip i {
    position: absolute;
    width: 2px;
    height: 2px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.72);
    box-shadow: 0 0 2px rgba(0, 0, 0, 0.38);
  }

  .resize-grip i:nth-child(1) {
    right: 2px;
    bottom: 2px;
  }

  .resize-grip i:nth-child(2) {
    right: 7px;
    bottom: 2px;
  }

  .resize-grip i:nth-child(3) {
    right: 12px;
    bottom: 2px;
  }

  .resize-grip i:nth-child(4) {
    right: 2px;
    bottom: 7px;
  }

  .resize-grip i:nth-child(5) {
    right: 7px;
    bottom: 7px;
  }

  .resize-grip i:nth-child(6) {
    right: 2px;
    bottom: 12px;
  }

  @keyframes blink {
    50% {
      opacity: 0;
    }
  }

  @keyframes typing {
    0%,
    60%,
    100% {
      transform: translateY(0);
      opacity: 0.35;
    }
    30% {
      transform: translateY(-3px);
      opacity: 0.9;
    }
  }

  @media (prefers-color-scheme: light) {
    .dock-shell.expanded {
      color: rgba(18, 18, 18, 0.92);
      background: rgba(246, 241, 236, 0.56);
    }

    .round-button,
    .esc-button,
    .brand {
      color: rgba(10, 10, 10, 0.68);
      border-color: rgba(0, 0, 0, 0.18);
      background: rgba(255, 255, 255, 0.5);
    }

    .message-card {
      color: rgba(10, 10, 10, 0.92);
      border-color: rgba(0, 0, 0, 0.12);
      background: rgba(255, 255, 255, 0.62);
    }

    textarea {
      color: rgba(10, 10, 10, 0.9);
      border-color: rgba(0, 0, 0, 0.14);
      background: rgba(255, 255, 255, 0.72);
    }

    textarea::placeholder {
      color: rgba(0, 0, 0, 0.38);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .fade-target {
      transition: none;
    }
  }
</style>
