import { COMMANDS, EVENTS, type CommandName, type DockPosition } from "../src/lib/contracts/ipc";

const commandKey = (command: CommandName): string => `woof:command:${command}`;
const mutationKey = (command: CommandName): string => `woof:mutation:${command}`;

function stored(command: CommandName): unknown | undefined {
  const value = window.localStorage.getItem(commandKey(command));
  return value === null ? undefined : JSON.parse(value);
}

function store(command: CommandName, value: unknown): void {
  window.localStorage.setItem(commandKey(command), JSON.stringify(value));
}

function mutation(command: CommandName, value: unknown): void {
  window.localStorage.setItem(mutationKey(command), JSON.stringify(value));
}

const defaults: Partial<Record<CommandName, unknown>> = {
  [COMMANDS.accessibilityTrusted]: false,
  [COMMANDS.inputMonitoringTrusted]: false,
  [COMMANDS.microphoneStatus]: "not-determined",
  [COMMANDS.companionGetPosition]: "top",
  [COMMANDS.companionGetHoverOpen]: true,
  [COMMANDS.companionGetCollapsedAutoHide]: false,
  [COMMANDS.captureIsPaused]: true,
  [COMMANDS.getReduceVisualEffects]: false,
  [COMMANDS.getCaretSoundsEnabled]: true,
  [COMMANDS.getVoiceDictationEnabled]: true,
  [COMMANDS.getTranscriptionModifierKey]: "fn",
  [COMMANDS.recordModifierKey]: "right_option",
  [COMMANDS.getDefaultWoofModifierKey]: "right_option",
  [COMMANDS.getWoofModifierKey]: "right_option",
  [COMMANDS.getWoofModifierEnabled]: true,
  [COMMANDS.getSecondaryShortcut]: {
    meta: true,
    shift: true,
    alt: false,
    control: false,
    key: "g"
  },
  [COMMANDS.getSecondaryShortcutEnabled]: true,
  [COMMANDS.getSecondaryShortcutError]: null,
  [COMMANDS.recordSecondaryShortcut]: {
    meta: true,
    shift: true,
    alt: false,
    control: false,
    key: "g"
  },
  [COMMANDS.getApiKeyStatus]: { configured: false, hint: null },
  [COMMANDS.getLoginItemEnabled]: false,
  [COMMANDS.getNudgesEnabled]: false,
  [COMMANDS.scheduledReminderList]: { rules: [] },
  [COMMANDS.getDataRetention]: { retention: { mode: "keep_forever" } },
  [COMMANDS.daemonHealth]: {
    status: "healthy",
    healthy: true,
    capture: "active",
    address: "127.0.0.1:3334"
  },
  [COMMANDS.mcpClientConfiguration]: JSON.stringify(
    {
      mcpServers: {
        woof: { command: "/Applications/woof.app/Contents/MacOS/woof-mcp" }
      }
    },
    null,
    2
  ),
  [COMMANDS.captureStatus]: {
    paused: false,
    capturing: true,
    runtime: { running: true, permission: "granted", last_capture_at: 1_753_948_920 }
  },
  [COMMANDS.getCaptureBlacklist]: { blacklist: [] },
  [COMMANDS.memoryRecentActivity]: {
    activity: [
      {
        event_id: 3,
        snapshot_id: "browser-activity-3",
        app: "Notes",
        window_title: "woof launch checklist",
        url: null,
        domain: null,
        started_at: 1_753_948_740,
        last_seen_at: 1_753_948_920,
        duration_s: 1080,
        content_excerpt: "Confirm local capture, signed app smoke test, and MCP conformance.",
        focused_name: "checklist",
        focused_role: "AXTextArea",
        focused_path: "Notes / woof launch checklist"
      }
    ]
  },
  [COMMANDS.memoryWorkingMemory]: {
    items: [
      {
        wm_id: 1,
        added_at: 1_753_948_920,
        relevance: 0.94,
        snapshot_id: "browser-memory-1",
        content: "Desktop work now centers on native panel behavior and local daemon integration.",
        app: "Xcode",
        window_title: "woof — desktop",
        url: null,
        domain: null,
        captured_at: 1_753_948_700,
        last_seen_at: 1_753_948_920,
        duration_s: 1560,
        sighting_count: 7,
        focused_name: "MemoryHub.svelte",
        focused_role: "AXTextArea",
        focused_path: "Xcode / woof / MemoryHub.svelte"
      }
    ]
  },
  [COMMANDS.memoryWikiList]: {
    pages: [
      {
        slug: "woof",
        page_type: "project",
        title: "woof",
        summary: "A private local memory companion for macOS.",
        mention_count: 18,
        last_seen: 1_753_948_920
      }
    ]
  },
  [COMMANDS.memoryWikiSearch]: {
    pages: [
      {
        slug: "woof",
        page_type: "project",
        title: "woof",
        summary: "A private local memory companion for macOS.",
        mention_count: 18,
        last_seen: 1_753_948_920
      }
    ]
  },
  [COMMANDS.memoryWikiPage]: {
    page: {
      slug: "woof",
      page_type: "project",
      title: "woof",
      aliases: "[]",
      summary: "A private local memory companion for macOS.",
      body: "Woof captures visible interface text locally and retrieves it through authenticated loopback APIs.",
      links: "[]",
      snapshot_ids: '["browser-memory-1"]',
      mention_count: 18,
      first_seen: 1_753_940_000,
      last_seen: 1_753_948_920,
      is_dirty: 0,
      updated_at: 1_753_948_920,
      model_used: "gpt-5.6-terra"
    }
  },
  [COMMANDS.memoryTimeReport]: {
    from: 1_753_920_000,
    to: 1_754_006_400,
    total_seconds: 8040,
    projects: [
      {
        project: "woof",
        seconds: 5520,
        by_day: { "2026-07-31": 5520 },
        top_segments: [{ app: "Xcode", domain: "", title: "woof — desktop", seconds: 5520 }]
      },
      {
        project: "Research",
        seconds: 2520,
        by_day: { "2026-07-31": 2520 },
        top_segments: [
          { app: "Safari", domain: "platform.openai.com", title: "OpenAI docs", seconds: 2520 }
        ]
      }
    ]
  },
  [COMMANDS.memoryTimeRules]: { rules: [] },
  [COMMANDS.memoryFollowups]: {
    followups: [
      {
        flag_id: 7,
        kind: "followup",
        text: "Review the launch decision",
        snapshot_id: "browser-memory-1",
        period_key: "2026-07-31",
        status: "open",
        created_at: 1_753_948_920
      }
    ]
  },
  [COMMANDS.memoryWorkPatterns]: {
    status: {
      total: 1,
      by_status: { proposed: 1 },
      recent: [
        {
          workflow_id: "0192f3cb-16d8-7f10-a922-4379a7c54d31",
          name: "Review launch decisions",
          excerpt: "Launch review",
          apps: ["Browser"],
          frequency_label: "3 recurrences across 2 days",
          observations: [],
          status: "proposed",
          confidence: 0.86,
          first_detected_at: 1_753_862_520,
          last_detected_at: 1_753_948_920
        }
      ]
    }
  }
};

function dockPosition(value: unknown): value is DockPosition {
  return ["top", "left", "right", "bottom", "bottom-left", "bottom-right"].includes(
    value as string
  );
}

export async function invokeMock(command: string, args: Record<string, unknown> = {}): Promise<unknown> {
  if (command.startsWith("plugin:")) return 1;
  const name = command as CommandName;
  if (name === COMMANDS.accessibilityStatus) {
    const explicit = stored(name);
    if (explicit !== undefined) return explicit;
    const ready = stored(COMMANDS.accessibilityTrusted) === true;
    return {
      app_trusted: ready,
      capture_service_trusted: ready,
      capture_service_operational: ready,
      ready,
      next_request: ready ? null : "app"
    };
  }
  if (name === COMMANDS.requestAccessibility) {
    const explicit = stored(name);
    return explicit ?? invokeMock(COMMANDS.accessibilityStatus);
  }
  const saved = stored(name);
  if (saved !== undefined) return saved;

  if (name === COMMANDS.companionPointerReady) {
    const inside = window.localStorage.getItem("woof:test:pointer-inside") === "true";
    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: inside }));
    return undefined;
  }
  if (name === COMMANDS.companionSetState) {
    if (args.state !== "collapsed" && args.state !== "expanded" && args.state !== "hidden") {
      throw new Error("invalid companion state");
    }
    const requestId =
      typeof args.requestId === "number" && Number.isSafeInteger(args.requestId)
        ? args.requestId
        : null;
    const detail = requestId !== null
      ? { state: args.state, requestId }
      : args.state;
    window.dispatchEvent(new CustomEvent(EVENTS.chatState, { detail }));
    return undefined;
  }
  if (name === COMMANDS.companionOpenFocused) {
    const requestId =
      typeof args.requestId === "number" && Number.isSafeInteger(args.requestId)
        ? args.requestId
        : null;
    const detail = requestId !== null
      ? { state: "expanded", requestId }
      : "expanded";
    window.dispatchEvent(new CustomEvent(EVENTS.chatState, { detail }));
    return undefined;
  }
  if (name === COMMANDS.companionRollup) {
    const requestId =
      typeof args.requestId === "number" && Number.isSafeInteger(args.requestId)
        ? args.requestId
        : null;
    const detail = requestId !== null
      ? { state: "collapsed", requestId }
      : "collapsed";
    window.dispatchEvent(new CustomEvent(EVENTS.chatState, { detail }));
    return undefined;
  }

  if (name === COMMANDS.companionSetHoverOpen) {
    if (typeof args.enabled !== "boolean") throw new Error("enabled is required");
    store(COMMANDS.companionGetHoverOpen, args.enabled);
    window.dispatchEvent(
      new CustomEvent(EVENTS.preferencesChanged, {
        detail: { companionHoverOpen: args.enabled }
      })
    );
    return args.enabled;
  }
  if (name === COMMANDS.companionSetCollapsedAutoHide) {
    if (typeof args.enabled !== "boolean") throw new Error("enabled is required");
    store(COMMANDS.companionGetCollapsedAutoHide, args.enabled);
    window.dispatchEvent(
      new CustomEvent(EVENTS.preferencesChanged, {
        detail: { collapsedAutoHide: args.enabled }
      })
    );
    return undefined;
  }
  if (name === COMMANDS.companionSetPosition) {
    if (!dockPosition(args.position)) throw new Error("invalid companion position");
    store(COMMANDS.companionGetPosition, args.position);
    window.dispatchEvent(new CustomEvent(EVENTS.panelPosition, { detail: args.position }));
    return undefined;
  }
  if (name === COMMANDS.companionDragStart) {
    window.dispatchEvent(new CustomEvent(EVENTS.positionDrag, { detail: { active: true } }));
    return true;
  }
  if (name === COMMANDS.companionDragFrame) return undefined;
  if (name === COMMANDS.companionDragEnd) {
    const current = (stored(COMMANDS.companionGetPosition) ?? "top") as DockPosition;
    const position = dockPosition(args.position) ? args.position : current;
    window.dispatchEvent(
      new CustomEvent(EVENTS.positionDrag, { detail: { active: false, nearest: position } })
    );
    return position;
  }
  if (name === COMMANDS.memoryIdentitySave) {
    if (typeof args.name !== "string") throw new Error("name is required");
    mutation(name, args);
    return { identity: { name: args.name.trim() || null } };
  }
  if (name === COMMANDS.memoryFollowupSetStatus) {
    if (
      !Number.isInteger(args.flagId) ||
      (args.status !== "resolved" && args.status !== "dismissed")
    ) {
      throw new Error("invalid follow-up status update");
    }
    const current = (stored(COMMANDS.memoryFollowups) ?? defaults[COMMANDS.memoryFollowups]) as {
      followups: Array<{ flag_id: number }>;
    };
    store(COMMANDS.memoryFollowups, {
      followups: current.followups.filter((followup) => followup.flag_id !== args.flagId)
    });
    mutation(name, args);
    return { updated: current.followups.some((followup) => followup.flag_id === args.flagId) };
  }
  if (name === COMMANDS.memoryWorkPatternSetStatus) {
    if (
      typeof args.workflowId !== "string" ||
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(args.workflowId) ||
      (args.status !== "accepted" && args.status !== "dismissed")
    ) {
      throw new Error("invalid work pattern update");
    }
    const current = (stored(COMMANDS.memoryWorkPatterns) ?? defaults[COMMANDS.memoryWorkPatterns]) as {
      status: {
        total: number;
        by_status: Record<string, number>;
        recent: Array<{ workflow_id?: string | null; status: string }>;
      };
    };
    let updated = false;
    const recent = current.status.recent.map((workflow) => {
      if (workflow.workflow_id !== args.workflowId || workflow.status !== "proposed") return workflow;
      updated = true;
      return { ...workflow, status: args.status as string };
    });
    store(COMMANDS.memoryWorkPatterns, {
      status: { ...current.status, recent }
    });
    mutation(name, args);
    return { updated };
  }
  if (name === COMMANDS.setCaptureBlacklist) {
    const response = { blacklist: Array.isArray(args.blacklist) ? args.blacklist : [] };
    store(COMMANDS.getCaptureBlacklist, response);
    mutation(name, args);
    return response;
  }
  if (name === COMMANDS.setNudgesEnabled) {
    const enabled = args.enabled === true;
    store(COMMANDS.getNudgesEnabled, enabled);
    mutation(name, args);
    return enabled;
  }
  if (name === COMMANDS.companionOpenNudge || name === COMMANDS.companionDismissNudge) {
    if (
      typeof args.nudgeId !== "string" ||
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(args.nudgeId)
    ) {
      throw new Error("nudge ID must be a canonical UUID");
    }
    mutation(name, { nudgeId: args.nudgeId });
    return name === COMMANDS.companionOpenNudge
      ? { opened: true }
      : { dismissed: true };
  }
  if (name === COMMANDS.setDataRetention) {
    const retention = args.retention as { mode?: string; days?: number } | undefined;
    if (
      !retention ||
      (retention.mode !== "keep_forever" &&
        (retention.mode !== "days" ||
          !Number.isInteger(retention.days) ||
          (retention.days ?? 0) < 1 ||
          (retention.days ?? 0) > 3650))
    ) {
      throw new Error("data retention must be between 1 and 3650 days");
    }
    const response = { retention, pruned: {}, vector_index: { indexed: 0 } };
    store(COMMANDS.getDataRetention, { retention });
    mutation(name, { retention });
    return response;
  }
  if (name === COMMANDS.scheduledReminderCreate) {
    const reminder = args.reminder as Record<string, unknown> | undefined;
    if (!reminder || (reminder.scheduleKind !== "once" && reminder.scheduleKind !== "daily")) {
      throw new Error("invalid reminder schedule");
    }
    const current = (stored(COMMANDS.scheduledReminderList) ?? { rules: [] }) as {
      rules: Array<Record<string, unknown>>;
    };
    const now = Math.floor(Date.now() / 1000);
    const rule = {
      rule_id: `00000000-0000-4000-8000-${(current.rules.length + 1).toString(16).padStart(12, "0")}`,
      label: reminder.label,
      prompt: reminder.prompt,
      schedule_kind: reminder.scheduleKind,
      days_of_week: [],
      hour: reminder.scheduleKind === "daily" ? reminder.hour : 0,
      minute: reminder.scheduleKind === "daily" ? reminder.minute : 0,
      interval_minutes: 0,
      timezone: "local",
      enabled: true,
      created_at: now,
      updated_at: now,
      last_fired_at: null,
      fire_at: reminder.scheduleKind === "once" ? reminder.fireAt : null
    };
    store(COMMANDS.scheduledReminderList, { rules: [...current.rules, rule] });
    mutation(name, { reminder });
    return { rule };
  }
  if (name === COMMANDS.scheduledReminderDelete) {
    if (typeof args.ruleId !== "string") throw new Error("invalid reminder ID");
    const current = (stored(COMMANDS.scheduledReminderList) ?? { rules: [] }) as {
      rules: Array<{ rule_id: string }>;
    };
    const rules = current.rules.filter((rule) => rule.rule_id !== args.ruleId);
    store(COMMANDS.scheduledReminderList, { rules });
    mutation(name, { ruleId: args.ruleId });
    return { deleted: rules.length !== current.rules.length };
  }
  if (name === COMMANDS.memoryHubOpenRoute) {
    if (args.route !== "followups" && args.route !== "workflows") {
      throw new Error("invalid memory hub route");
    }
    window.dispatchEvent(new CustomEvent(EVENTS.memoryHubNavigate, { detail: { route: args.route } }));
    return undefined;
  }
  if (name === COMMANDS.memoryDeleteAll) {
    return { status: "deleted", deleted_rows: 1, vector_index: { indexed: 0 } };
  }
  if (name === COMMANDS.setOpenAiApiKey) {
    // The test bridge intentionally never persists the supplied key either.
    return undefined;
  }
  if (name.startsWith("set_")) {
    mutation(name, args);
    return undefined;
  }
  return defaults[name];
}
