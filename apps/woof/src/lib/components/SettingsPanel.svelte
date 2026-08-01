<script lang="ts">
  import {
    Accessibility,
    AlertTriangle,
    ArrowLeft,
    Bell,
    BookOpen,
    Bug,
    Check,
    ChevronRight,
    ContactRound,
    Copy,
    Database,
    EyeOff,
    FileText,
    KeyRound,
    Keyboard,
    LoaderCircle,
    LockKeyhole,
    MessageSquare,
    Mic,
    Pause,
    Palette,
    Play,
    Plus,
    ShieldCheck,
    Sparkles,
    Terminal,
    Trash2,
    UserRound,
    UsersRound,
    Video,
    Volume2,
    X
  } from "lucide-svelte";
  import { invokeCommand } from "$lib/contracts/bridge";
  import {
    CAPTURE_BLACKLIST_KINDS,
    COMMANDS,
    type CaptureBlacklistEntry,
    type CaptureBlacklistKind,
    type CaptureBlacklistResponse,
    type CaptureStatus,
    type DataRetentionPolicy,
    type DockPosition,
    type ModifierKey,
    type ScheduledReminder,
    type ScheduledReminderDraft,
    type ScheduledReminderKind,
    type ShortcutChord,
    type WorkingMemoryItem
  } from "$lib/contracts/ipc";

  let { embedded = false, dock = false, onclose = () => {} } = $props<{
    embedded?: boolean;
    dock?: boolean;
    onclose?: () => void;
  }>();

  type Section = "general" | "privacy" | "shortcuts" | "openai";
  type PrivacyView = "overview" | "blacklist" | "retention";
  type BlacklistStatus = "idle" | "loading" | "ready" | "saving" | "saved" | "error";
  type DockSection =
    | "account"
    | "privacy"
    | "appearance"
    | "identity"
    | "memory"
    | "notifications"
    | "community"
    | "mcp"
    | "bug-report"
    | "shortcuts"
    | "tutorials"
    | "release-notes";

  const DEFAULT_SECONDARY_SHORTCUT: ShortcutChord = {
    meta: true,
    shift: true,
    alt: false,
    control: false,
    key: "g"
  };
  const MODIFIER_COLLISION_MESSAGE =
    "Inline help and hold to talk must use different modifier keys.";
  const modifierOptions: { value: ModifierKey; label: string }[] = [
    { value: "fn", label: "Fn / Globe" },
    { value: "left_option", label: "Left Option" },
    { value: "right_option", label: "Right Option" },
    { value: "left_command", label: "Left Command" },
    { value: "right_command", label: "Right Command" },
    { value: "left_shift", label: "Left Shift" },
    { value: "right_shift", label: "Right Shift" },
    { value: "left_control", label: "Left Control" },
    { value: "right_control", label: "Right Control" }
  ];
  const companionPositions: { value: DockPosition; label: string }[] = [
    { value: "top", label: "Top" },
    { value: "left", label: "Left" },
    { value: "right", label: "Right" },
    { value: "bottom", label: "Bottom" },
    { value: "bottom-left", label: "Bottom-left corner" },
    { value: "bottom-right", label: "Bottom-right corner" }
  ];
  const blacklistKindLabels: Record<CaptureBlacklistKind, string> = {
    bundle_id: "Bundle ID",
    bundle_prefix: "Bundle prefix",
    app_name: "App name",
    window_title: "Window title",
    browser_host: "Website",
    regex: "Regular expression"
  };

  let active = $state<Section>("general");
  let dockActive = $state<DockSection>("privacy");
  let privacyView = $state<PrivacyView>("overview");
  let reduceMotion = $state(false);
  let companionPosition = $state<DockPosition>("top");
  let hoverOpen = $state(false);
  let collapsedAutoHide = $state(false);
  let appearanceStatus = $state<"idle" | "loading" | "saving" | "error">("idle");
  let appearanceError = $state("");
  let caretSounds = $state(true);
  let voice = $state(true);
  let loginItem = $state(false);
  let capturePaused = $state(false);
  let captureStatus = $state<CaptureStatus | null>(null);
  let keyConfigured = $state(false);
  let apiKey = $state("");
  let saved = $state(false);
  let keyStatus = $state<"idle" | "saving" | "deleting" | "error">("idle");
  let keyError = $state("");
  let blacklist = $state<CaptureBlacklistEntry[]>([]);
  let blacklistStatus = $state<BlacklistStatus>("idle");
  let blacklistLoaded = $state(false);
  let blacklistError = $state("");
  let blacklistDirty = $state(false);
  let draftKind = $state<CaptureBlacklistKind>("app_name");
  let draftPattern = $state("");
  let accessibilityGranted = $state(false);
  let inputMonitoringGranted = $state(false);
  let microphonePermission = $state("not-determined");
  let contactName = $state("");
  let contactCompany = $state("");
  let persistedContactName = "";
  let identityStatus = $state<"idle" | "loading" | "saving" | "saved" | "error">("idle");
  let identityError = $state("");
  let workingMemory = $state<WorkingMemoryItem[]>([]);
  let memoryStatus = $state<"idle" | "loading" | "ready" | "error">("idle");
  let memoryError = $state("");
  let daemonStatus = $state("Checking local service…");
  let nudgesEnabled = $state(false);
  let notificationStatus = $state<"idle" | "loading" | "saving" | "error">("idle");
  let notificationError = $state("");
  let reminders = $state<ScheduledReminder[]>([]);
  let reminderLabel = $state("");
  let reminderPrompt = $state("");
  let reminderKind = $state<ScheduledReminderKind>("once");
  let reminderOnceAt = $state(defaultOnceDateTime());
  let reminderDailyTime = $state("09:00");
  let reminderStatus = $state<"idle" | "loading" | "saving" | "saved" | "error">("idle");
  let reminderError = $state("");
  let retention = $state<DataRetentionPolicy>({ mode: "keep_forever" });
  let retentionDraft = $state("keep_forever");
  let retentionStatus = $state<"idle" | "loading" | "saving" | "saved" | "error">("idle");
  let retentionError = $state("");
  let mcpCopied = $state(false);
  let mcpConfig = $state("");
  let mcpConfigError = $state("");
  let woofModifier = $state<ModifierKey>("right_option");
  let woofModifierEnabled = $state(true);
  let transcriptionModifier = $state<ModifierKey>("fn");
  let secondaryShortcut = $state<ShortcutChord>({ ...DEFAULT_SECONDARY_SHORTCUT });
  let secondaryShortcutEnabled = $state(true);
  let shortcutRecording = $state<"woof" | "transcription" | "secondary" | null>(null);
  let shortcutStatus = $state<"idle" | "loading" | "saving" | "saved" | "error">("idle");
  let shortcutError = $state("");
  const modifiersCollide = $derived(woofModifier === transcriptionModifier);
  let deleteConfirming = $state(false);
  let deleteStatus = $state<"idle" | "deleting" | "deleted" | "error">("idle");
  let deleteError = $state("");
  let permissionPollEpoch = 0;
  let permissionRefreshRequest = 0;
  let permissionFollowupTimer: number | undefined;

  const sections = [
    { id: "general", label: "General", icon: Sparkles },
    { id: "privacy", label: "Privacy", icon: ShieldCheck },
    { id: "shortcuts", label: "Shortcuts", icon: Accessibility },
    { id: "openai", label: "OpenAI", icon: KeyRound }
  ] as const;

  const dockSections = [
    { id: "account", label: "Account", icon: UserRound },
    { id: "privacy", label: "Privacy", icon: ShieldCheck },
    { id: "appearance", label: "Appearance", icon: Palette },
    { id: "identity", label: "Identity", icon: ContactRound },
    { id: "memory", label: "Memory", icon: BookOpen },
    { id: "notifications", label: "Notifications", icon: Bell },
    { id: "community", label: "Community", icon: UsersRound },
    { id: "mcp", label: "MCP", icon: Terminal },
    { id: "bug-report", label: "Bug report", icon: Bug },
    { id: "shortcuts", label: "Shortcuts", icon: Keyboard },
    { id: "tutorials", label: "Tutorials", icon: Video },
    { id: "release-notes", label: "Release notes", icon: FileText }
  ] as const;

  function readableError(error: unknown, fallback: string): string {
    if (typeof error === "string" && error.trim()) return error;
    if (error instanceof Error && error.message.trim()) return error.message;
    return fallback;
  }

  function defaultOnceDateTime(): string {
    const date = new Date(Date.now() + 60 * 60 * 1_000);
    date.setSeconds(0, 0);
    const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
    return local.toISOString().slice(0, 16);
  }

  function retentionValue(policy: DataRetentionPolicy): string {
    return policy.mode === "keep_forever" ? "keep_forever" : String(policy.days);
  }

  function activateSection(section: Section): void {
    active = section;
    privacyView = "overview";
    if (section === "shortcuts") void loadShortcuts();
  }

  function activateDockSection(section: DockSection): void {
    dockActive = section;
    privacyView = "overview";

    if (section === "privacy") active = "privacy";
    else if (section === "shortcuts") active = "shortcuts";
    else if (section === "account") active = "openai";
    else if (section === "appearance") active = "general";

    if (section === "identity") void loadIdentity();
    if (section === "memory") void loadMemory();
    if (section === "mcp") void loadMcpStatus();
    if (section === "shortcuts") void loadShortcuts();
    if (section === "notifications") void loadNotificationSettings();
    if (section === "appearance") void loadAppearance();
  }

  async function loadAppearance(): Promise<void> {
    appearanceStatus = "loading";
    appearanceError = "";
    try {
      const [position, hover, autoHide] = await Promise.all([
        invokeCommand<DockPosition>(COMMANDS.companionGetPosition),
        invokeCommand<boolean>(COMMANDS.companionGetHoverOpen),
        invokeCommand<boolean>(COMMANDS.companionGetCollapsedAutoHide)
      ]);
      companionPosition = position;
      hoverOpen = hover;
      collapsedAutoHide = autoHide;
      appearanceStatus = "idle";
    } catch (error) {
      appearanceStatus = "error";
      appearanceError = readableError(error, "Woof couldn’t load companion settings.");
    }
  }

  async function setCompanionPosition(position: DockPosition): Promise<void> {
    if (appearanceStatus === "saving" || position === companionPosition) return;
    const previous = companionPosition;
    companionPosition = position;
    appearanceStatus = "saving";
    appearanceError = "";
    try {
      await invokeCommand(COMMANDS.companionSetPosition, { position });
      appearanceStatus = "idle";
    } catch (error) {
      companionPosition = previous;
      appearanceStatus = "error";
      appearanceError = readableError(error, "Woof couldn’t move the companion.");
    }
  }

  async function setHoverOpen(enabled: boolean): Promise<void> {
    const previous = hoverOpen;
    hoverOpen = enabled;
    try {
      hoverOpen = await invokeCommand<boolean>(COMMANDS.companionSetHoverOpen, { enabled });
    } catch (error) {
      hoverOpen = previous;
      appearanceError = readableError(error, "Woof couldn’t save hover behavior.");
    }
  }

  async function setCollapsedAutoHide(enabled: boolean): Promise<void> {
    const previous = collapsedAutoHide;
    collapsedAutoHide = enabled;
    try {
      await invokeCommand(COMMANDS.companionSetCollapsedAutoHide, { enabled });
    } catch (error) {
      collapsedAutoHide = previous;
      appearanceError = readableError(error, "Woof couldn’t save auto-hide behavior.");
    }
  }

  async function clearKey(): Promise<void> {
    keyStatus = "deleting";
    keyError = "";
    try {
      await invokeCommand(COMMANDS.clearOpenAiApiKey);
      keyConfigured = false;
      apiKey = "";
      saved = false;
      keyStatus = "idle";
    } catch (error) {
      keyStatus = "error";
      keyError = readableError(error, "Woof couldn’t remove the API key from Keychain.");
    }
  }

  async function deleteAllData(): Promise<void> {
    deleteStatus = "deleting";
    deleteError = "";
    try {
      await invokeCommand(COMMANDS.memoryDeleteAll);
      workingMemory = [];
      contactName = "";
      contactCompany = "";
      deleteConfirming = false;
      deleteStatus = "deleted";
    } catch (error) {
      deleteError = readableError(error, "Woof could not delete local memory.");
      deleteStatus = "error";
    }
  }

  async function loadNotificationSettings(): Promise<void> {
    notificationStatus = "loading";
    reminderStatus = "loading";
    notificationError = "";
    reminderError = "";
    try {
      const [nudges, response] = await Promise.all([
        invokeCommand<boolean>(COMMANDS.getNudgesEnabled),
        invokeCommand<{ rules: ScheduledReminder[] }>(COMMANDS.scheduledReminderList)
      ]);
      nudgesEnabled = nudges;
      reminders = response.rules ?? [];
      notificationStatus = "idle";
      reminderStatus = "idle";
    } catch (error) {
      notificationStatus = "error";
      reminderStatus = "error";
      notificationError = readableError(error, "Woof couldn’t read notification settings.");
      reminderError = notificationError;
    }
  }

  async function setNotificationNudges(enabled: boolean): Promise<void> {
    const previous = nudgesEnabled;
    nudgesEnabled = enabled;
    notificationStatus = "saving";
    notificationError = "";
    try {
      nudgesEnabled = await invokeCommand<boolean>(COMMANDS.setNudgesEnabled, { enabled });
      notificationStatus = "idle";
    } catch (error) {
      nudgesEnabled = previous;
      notificationStatus = "error";
      notificationError = readableError(error, "Woof couldn’t save notification settings.");
    }
  }

  async function createReminder(): Promise<void> {
    reminderError = "";
    const label = reminderLabel.trim();
    const prompt = reminderPrompt.trim();
    if (!label || !prompt) {
      reminderError = "Enter both a label and what woof should remind you about.";
      reminderStatus = "error";
      return;
    }

    let hour: number;
    let minute: number;
    let fireAt: number | null;
    if (reminderKind === "once") {
      const date = new Date(reminderOnceAt);
      fireAt = Math.floor(date.getTime() / 1_000);
      if (!Number.isFinite(fireAt) || fireAt <= Math.floor(Date.now() / 1_000)) {
        reminderError = "Choose a future date and time.";
        reminderStatus = "error";
        return;
      }
      hour = date.getHours();
      minute = date.getMinutes();
    } else {
      const match = /^(\d{2}):(\d{2})$/.exec(reminderDailyTime);
      if (!match) {
        reminderError = "Choose a valid daily time.";
        reminderStatus = "error";
        return;
      }
      hour = Number(match[1]);
      minute = Number(match[2]);
      fireAt = null;
    }

    const reminder: ScheduledReminderDraft = reminderKind === "once"
      ? { label, prompt, scheduleKind: "once", fireAt: fireAt! }
      : { label, prompt, scheduleKind: "daily", hour, minute };
    reminderStatus = "saving";
    try {
      const response = await invokeCommand<{ rule: ScheduledReminder }>(
        COMMANDS.scheduledReminderCreate,
        { reminder }
      );
      reminders = [response.rule, ...reminders];
      reminderLabel = "";
      reminderPrompt = "";
      reminderOnceAt = defaultOnceDateTime();
      reminderStatus = "saved";
    } catch (error) {
      reminderStatus = "error";
      reminderError = readableError(error, "Woof couldn’t create the reminder.");
    }
  }

  async function deleteReminder(ruleId: string): Promise<void> {
    reminderStatus = "saving";
    reminderError = "";
    try {
      const response = await invokeCommand<{ deleted: boolean }>(
        COMMANDS.scheduledReminderDelete,
        { ruleId }
      );
      if (!response.deleted) throw new Error("Reminder was not found.");
      reminders = reminders.filter((reminder) => reminder.rule_id !== ruleId);
      reminderStatus = "saved";
    } catch (error) {
      reminderStatus = "error";
      reminderError = readableError(error, "Woof couldn’t delete the reminder.");
    }
  }

  function reminderSchedule(reminder: ScheduledReminder): string {
    if (reminder.schedule_kind === "once" && reminder.fire_at) {
      return new Intl.DateTimeFormat(undefined, {
        dateStyle: "medium",
        timeStyle: "short"
      }).format(new Date(reminder.fire_at * 1_000));
    }
    const date = new Date();
    date.setHours(reminder.hour, reminder.minute, 0, 0);
    return `Daily at ${new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(date)}`;
  }

  async function openRetention(): Promise<void> {
    privacyView = "retention";
    retentionStatus = "loading";
    retentionError = "";
    try {
      const response = await invokeCommand<{ retention: DataRetentionPolicy }>(
        COMMANDS.getDataRetention
      );
      retention = response.retention;
      retentionDraft = retentionValue(retention);
      retentionStatus = "idle";
    } catch (error) {
      retentionStatus = "error";
      retentionError = readableError(error, "Woof couldn’t load data retention.");
    }
  }

  async function saveRetention(): Promise<void> {
    const policy: DataRetentionPolicy = retentionDraft === "keep_forever"
      ? { mode: "keep_forever" }
      : { mode: "days", days: Number(retentionDraft) };
    retentionStatus = "saving";
    retentionError = "";
    try {
      const response = await invokeCommand<{ retention: DataRetentionPolicy }>(
        COMMANDS.setDataRetention,
        { retention: policy }
      );
      retention = response.retention;
      retentionDraft = retentionValue(retention);
      retentionStatus = "saved";
    } catch (error) {
      retentionStatus = "error";
      retentionError = readableError(error, "Woof couldn’t save data retention.");
    }
  }

  async function loadIdentity(): Promise<void> {
    identityStatus = "loading";
    identityError = "";
    try {
      const contact = await invokeCommand<{ name?: string; company?: string }>(
        COMMANDS.loadContactInfo
      );
      contactName = contact?.name ?? "";
      contactCompany = contact?.company ?? "";
      persistedContactName = contactName;
      identityStatus = "idle";
    } catch (error) {
      identityStatus = "error";
      identityError = readableError(error, "Woof couldn’t read your local identity settings.");
    }
  }

  async function saveIdentity(): Promise<void> {
    identityStatus = "saving";
    identityError = "";
    try {
      const name = contactName.trim();
      const company = contactCompany.trim();
      await invokeCommand(COMMANDS.memoryIdentitySave, { name });
      try {
        await invokeCommand(COMMANDS.saveContactInfo, { contact: { name, company } });
      } catch (error) {
        await invokeCommand(COMMANDS.memoryIdentitySave, {
          name: persistedContactName
        }).catch(() => undefined);
        throw error;
      }
      contactName = name;
      contactCompany = company;
      persistedContactName = name;
      identityStatus = "saved";
      window.setTimeout(() => {
        if (identityStatus === "saved") identityStatus = "idle";
      }, 1400);
    } catch (error) {
      identityStatus = "error";
      identityError = readableError(error, "Woof couldn’t save your identity settings.");
    }
  }

  async function loadMemory(): Promise<void> {
    memoryStatus = "loading";
    memoryError = "";
    try {
      const response = await invokeCommand<{ items?: WorkingMemoryItem[] }>(
        COMMANDS.memoryWorkingMemory,
        { limit: 5 }
      );
      workingMemory = response?.items ?? [];
      memoryStatus = "ready";
    } catch (error) {
      workingMemory = [];
      memoryStatus = "error";
      memoryError = readableError(error, "The local memory service is unavailable.");
    }
  }

  async function loadMcpStatus(): Promise<void> {
    daemonStatus = "Checking local service…";
    mcpConfigError = "";
    const [healthResult, configResult] = await Promise.allSettled([
      invokeCommand<{ status?: string; address?: string }>(COMMANDS.daemonHealth),
      invokeCommand<string>(COMMANDS.mcpClientConfiguration)
    ]);
    if (healthResult.status === "fulfilled") {
      const health = healthResult.value;
      const status = health?.status === "healthy" ? "Ready" : health?.status ?? "Unavailable";
      daemonStatus = `${status} on ${health?.address ?? "127.0.0.1:3334"}`;
    } else {
      daemonStatus = "Local service unavailable";
    }
    if (configResult.status === "fulfilled" && configResult.value.trim()) {
      mcpConfig = configResult.value;
    } else {
      mcpConfig = "";
      mcpConfigError =
        configResult.status === "rejected"
          ? readableError(configResult.reason, "The bundled woof MCP bridge is unavailable.")
          : "The bundled woof MCP bridge is unavailable.";
    }
  }

  async function copyMcpConfig(): Promise<void> {
    if (!mcpConfig) return;
    try {
      await navigator.clipboard.writeText(mcpConfig);
      mcpCopied = true;
      window.setTimeout(() => (mcpCopied = false), 1400);
    } catch {
      mcpCopied = false;
    }
  }

  async function loadShortcuts(): Promise<void> {
    shortcutStatus = "loading";
    shortcutError = "";
    try {
      const [
        defaultWoof,
        woof,
        woofEnabled,
        transcription,
        secondary,
        secondaryEnabled,
        secondaryError
      ] = await Promise.all([
        invokeCommand<ModifierKey>(COMMANDS.getDefaultWoofModifierKey).catch(
          () => "right_option" as ModifierKey
        ),
        invokeCommand<ModifierKey>(COMMANDS.getWoofModifierKey),
        invokeCommand<boolean>(COMMANDS.getWoofModifierEnabled),
        invokeCommand<ModifierKey>(COMMANDS.getTranscriptionModifierKey),
        invokeCommand<ShortcutChord>(COMMANDS.getSecondaryShortcut),
        invokeCommand<boolean>(COMMANDS.getSecondaryShortcutEnabled),
        invokeCommand<string | null>(COMMANDS.getSecondaryShortcutError)
      ]);
      woofModifier = woof || defaultWoof || "right_option";
      woofModifierEnabled = woofEnabled;
      transcriptionModifier = transcription || "fn";
      secondaryShortcut = secondary || { ...DEFAULT_SECONDARY_SHORTCUT };
      secondaryShortcutEnabled = secondaryEnabled;
      shortcutError = secondaryError ?? "";
      shortcutStatus = "idle";
    } catch (error) {
      shortcutStatus = "error";
      shortcutError = readableError(error, "Woof couldn’t load the shortcut bindings.");
    }
  }

  async function saveShortcuts(): Promise<void> {
    if (modifiersCollide) {
      shortcutStatus = "error";
      shortcutError = MODIFIER_COLLISION_MESSAGE;
      return;
    }
    shortcutStatus = "saving";
    shortcutError = "";
    try {
      // These commands replace native registrations, so run them in order.
      await invokeCommand(COMMANDS.setModifierKeys, {
        woofKey: woofModifier,
        transcriptionKey: transcriptionModifier
      });
      await invokeCommand(COMMANDS.setWoofModifierEnabled, { enabled: woofModifierEnabled });
      await invokeCommand(COMMANDS.setSecondaryShortcut, { chord: secondaryShortcut });
      await invokeCommand(COMMANDS.setSecondaryShortcutEnabled, { enabled: secondaryShortcutEnabled });
      shortcutStatus = "saved";
      window.setTimeout(() => {
        if (shortcutStatus === "saved") shortcutStatus = "idle";
      }, 1400);
    } catch (error) {
      shortcutStatus = "error";
      shortcutError = readableError(error, "That shortcut could not be registered.");
    }
  }

  async function recordModifier(target: "woof" | "transcription"): Promise<void> {
    shortcutRecording = target;
    shortcutError = "";
    try {
      const key = await invokeCommand<ModifierKey>(COMMANDS.recordModifierKey);
      if (target === "woof") woofModifier = key;
      else transcriptionModifier = key;
    } catch (error) {
      shortcutError = readableError(error, "Woof couldn’t record that modifier key.");
    } finally {
      shortcutRecording = null;
    }
  }

  async function recordSecondaryShortcut(): Promise<void> {
    shortcutRecording = "secondary";
    shortcutError = "";
    try {
      secondaryShortcut = await invokeCommand<ShortcutChord>(COMMANDS.recordSecondaryShortcut);
    } catch (error) {
      shortcutError = readableError(error, "Woof couldn’t record that shortcut chord.");
    } finally {
      shortcutRecording = null;
    }
  }

  function shortcutChordLabel(chord: ShortcutChord): string {
    const parts = [
      chord.meta ? "⌘" : "",
      chord.shift ? "⇧" : "",
      chord.alt ? "⌥" : "",
      chord.control ? "⌃" : "",
      chord.key.length === 1 ? chord.key.toUpperCase() : chord.key
    ];
    return parts.filter(Boolean).join(" ");
  }

  function modifierLabel(key: ModifierKey): string {
    return modifierOptions.find((option) => option.value === key)?.label ?? key;
  }

  function microphoneLabel(status: string): string {
    if (status === "authorized" || status === "granted") return "Granted";
    if (status === "denied" || status === "restricted") return "Denied";
    return "Not requested";
  }

  async function refreshPermissions(epoch: number): Promise<void> {
    const request = ++permissionRefreshRequest;
    const [accessibility, inputMonitoring, microphone] = await Promise.all([
      invokeCommand<boolean>(COMMANDS.accessibilityTrusted).catch(() => false),
      invokeCommand<boolean>(COMMANDS.inputMonitoringTrusted).catch(() => false),
      invokeCommand<string>(COMMANDS.microphoneStatus).catch(() => "not-determined")
    ]);
    if (epoch !== permissionPollEpoch || request !== permissionRefreshRequest) return;
    accessibilityGranted = accessibility;
    inputMonitoringGranted = inputMonitoring;
    microphonePermission = microphone ?? "not-determined";
  }

  function schedulePermissionRefresh(epoch: number): void {
    if (epoch !== permissionPollEpoch) return;
    if (permissionFollowupTimer !== undefined) {
      window.clearTimeout(permissionFollowupTimer);
    }
    permissionFollowupTimer = window.setTimeout(() => {
      permissionFollowupTimer = undefined;
      void refreshPermissions(epoch);
    }, 800);
  }

  async function openAccessibilityPermission(): Promise<void> {
    const epoch = permissionPollEpoch;
    await invokeCommand(COMMANDS.openAccessibilitySettings).catch(() => undefined);
    schedulePermissionRefresh(epoch);
  }

  async function openMicrophonePermission(): Promise<void> {
    const epoch = permissionPollEpoch;
    const shouldRequest = microphoneLabel(microphonePermission) === "Not requested";
    await invokeCommand(COMMANDS.microphoneStatus, {
      ...(shouldRequest ? { request: true } : { openSettings: true })
    }).catch(() => undefined);
    schedulePermissionRefresh(epoch);
  }

  async function openInputMonitoringPermission(): Promise<void> {
    const epoch = permissionPollEpoch;
    const granted = await invokeCommand<boolean>(COMMANDS.requestInputMonitoring).catch(
      () => false
    );
    inputMonitoringGranted = granted;
    if (!granted) {
      await invokeCommand(COMMANDS.openInputMonitoringSettings).catch(() => undefined);
    }
    schedulePermissionRefresh(epoch);
  }

  async function load(): Promise<void> {
    const values = await Promise.all([
      invokeCommand<boolean>(COMMANDS.getReduceVisualEffects),
      invokeCommand<boolean>(COMMANDS.getCaretSoundsEnabled),
      invokeCommand<boolean>(COMMANDS.getVoiceDictationEnabled),
      invokeCommand<boolean>(COMMANDS.getLoginItemEnabled),
      invokeCommand<boolean>(COMMANDS.captureIsPaused),
      invokeCommand<{ configured: boolean }>(COMMANDS.getApiKeyStatus)
    ]).catch(() => [false, true, true, false, false, { configured: false }] as const);
    reduceMotion = values[0] as boolean;
    caretSounds = values[1] as boolean;
    voice = values[2] as boolean;
    loginItem = values[3] as boolean;
    capturePaused = values[4] as boolean;
    keyConfigured = (values[5] as { configured: boolean }).configured;
  }

  async function refreshCaptureStatus(): Promise<void> {
    try {
      const status = await invokeCommand<CaptureStatus>(COMMANDS.captureStatus);
      captureStatus = status;
      capturePaused = status.paused;
    } catch {
      captureStatus = null;
    }
  }

  function captureSummary(): string {
    if (capturePaused || captureStatus?.paused) {
      return "Paused — no new activity is being recorded.";
    }
    if (captureStatus?.runtime.permission === "denied") {
      return "Unavailable — Accessibility permission is required.";
    }
    if (
      captureStatus?.runtime.last_error === "permission_denied" ||
      captureStatus?.runtime.last_error === "accessibility"
    ) {
      return "Unavailable — Accessibility permission is required.";
    }
    if (captureStatus?.runtime.last_error === "storage") {
      return "Unavailable — local storage needs attention.";
    }
    if (captureStatus?.capturing === true && captureStatus.runtime.running === true) {
      return "Active — visible accessibility text is stored locally.";
    }
    return "Unavailable — local capture is not ready.";
  }

  function captureActionLabel(): string {
    if (capturePaused || captureStatus?.paused) return "Resume";
    return captureStatus?.capturing === true ? "Pause" : "Retry";
  }

  async function toggle(
    key: "reduce" | "sounds" | "voice" | "login",
    value: boolean
  ): Promise<void> {
    const previous = {
      reduce: reduceMotion,
      sounds: caretSounds,
      voice,
      login: loginItem
    }[key];
    appearanceError = "";
    try {
      if (key === "reduce") {
        reduceMotion = value;
        await invokeCommand(COMMANDS.setReduceVisualEffects, { enabled: value });
      } else if (key === "sounds") {
        caretSounds = value;
        await invokeCommand(COMMANDS.setCaretSoundsEnabled, { enabled: value });
      } else if (key === "voice") {
        voice = value;
        await invokeCommand(COMMANDS.setVoiceDictationEnabled, { enabled: value });
      } else {
        loginItem = value;
        await invokeCommand(COMMANDS.setLoginItemEnabled, { enabled: value });
      }
    } catch (error) {
      if (key === "reduce") reduceMotion = previous;
      else if (key === "sounds") caretSounds = previous;
      else if (key === "voice") voice = previous;
      else loginItem = previous;
      appearanceError = readableError(error, "Woof couldn’t save that appearance setting.");
    }
  }

  async function toggleCapture(): Promise<void> {
    const shouldResume = capturePaused || captureStatus?.capturing !== true;
    try {
      await invokeCommand(shouldResume ? COMMANDS.captureResume : COMMANDS.capturePause);
    } finally {
      await refreshCaptureStatus();
    }
  }

  async function saveKey(): Promise<void> {
    if (apiKey.length < 12) return;
    keyStatus = "saving";
    keyError = "";
    try {
      await invokeCommand(COMMANDS.setOpenAiApiKey, { apiKey });
      apiKey = "";
      keyConfigured = true;
      saved = true;
      keyStatus = "idle";
      window.setTimeout(() => (saved = false), 1400);
    } catch (error) {
      keyStatus = "error";
      keyError = readableError(error, "Woof couldn’t save the API key to Keychain.");
    }
  }

  async function openBlacklist(): Promise<void> {
    privacyView = "blacklist";
    blacklistStatus = "loading";
    blacklistLoaded = false;
    blacklistError = "";
    try {
      const response = await invokeCommand<CaptureBlacklistResponse>(
        COMMANDS.getCaptureBlacklist
      );
      blacklist = (response.blacklist ?? []).filter(
        (entry): entry is CaptureBlacklistEntry =>
          CAPTURE_BLACKLIST_KINDS.includes(entry.kind) &&
          typeof entry.pattern === "string"
      );
      blacklistDirty = false;
      blacklistLoaded = true;
      blacklistStatus = "ready";
    } catch (error) {
      blacklistError = readableError(
        error,
        "Woof couldn’t load the local capture blacklist."
      );
      blacklistStatus = "error";
    }
  }

  function setBlacklistEntry(
    index: number,
    field: "kind" | "pattern",
    value: string
  ): void {
    const current = blacklist[index];
    if (!current) return;
    blacklist[index] = {
      ...current,
      [field]: value
    } as CaptureBlacklistEntry;
    blacklistDirty = true;
    blacklistError = "";
    blacklistStatus = "ready";
  }

  function removeBlacklistEntry(index: number): void {
    blacklist = blacklist.filter((_, entryIndex) => entryIndex !== index);
    blacklistDirty = true;
    blacklistError = "";
    blacklistStatus = "ready";
  }

  function addBlacklistEntry(): void {
    const pattern = draftPattern.trim();
    if (!pattern) {
      blacklistError = "Enter an app, website, title, bundle identifier, or pattern.";
      return;
    }
    if (blacklist.length >= 100) {
      blacklistError = "The capture blacklist supports up to 100 rules.";
      return;
    }
    if (draftKind === "regex") {
      try {
        new RegExp(pattern);
      } catch {
        blacklistError = "That regular expression is not valid.";
        return;
      }
    }
    const duplicate = blacklist.some(
      (entry) =>
        entry.kind === draftKind &&
        entry.pattern.trim().toLocaleLowerCase() === pattern.toLocaleLowerCase()
    );
    if (duplicate) {
      blacklistError = "That capture rule is already in the list.";
      return;
    }
    blacklist = [...blacklist, { kind: draftKind, pattern }];
    draftPattern = "";
    blacklistDirty = true;
    blacklistError = "";
    blacklistStatus = "ready";
  }

  function validateBlacklist(): string | null {
    for (const entry of blacklist) {
      if (!entry.pattern.trim()) return "Every capture rule needs a pattern.";
      if (entry.kind === "regex") {
        try {
          new RegExp(entry.pattern);
        } catch {
          return `“${entry.pattern}” is not a valid regular expression.`;
        }
      }
    }
    return null;
  }

  async function saveBlacklist(): Promise<void> {
    const validationError = validateBlacklist();
    if (validationError) {
      blacklistError = validationError;
      return;
    }
    blacklistStatus = "saving";
    blacklistError = "";
    try {
      const response = await invokeCommand<CaptureBlacklistResponse>(
        COMMANDS.setCaptureBlacklist,
        { blacklist }
      );
      blacklist = response?.blacklist ?? blacklist;
      blacklistDirty = false;
      blacklistStatus = "saved";
      window.setTimeout(() => {
        if (blacklistStatus === "saved") blacklistStatus = "ready";
      }, 1400);
    } catch (error) {
      blacklistError = readableError(
        error,
        "Woof couldn’t save the local capture blacklist."
      );
      blacklistStatus = "error";
    }
  }

  $effect(() => {
    if (dock) active = "privacy";
    void load();
    void refreshCaptureStatus();
    void loadAppearance();
  });

  $effect(() => {
    if (!(dock && dockActive === "privacy" && privacyView === "overview")) return;

    const epoch = ++permissionPollEpoch;
    void refreshPermissions(epoch);
    const interval = window.setInterval(() => void refreshPermissions(epoch), 4_000);

    return () => {
      permissionPollEpoch += 1;
      permissionRefreshRequest += 1;
      window.clearInterval(interval);
      if (permissionFollowupTimer !== undefined) {
        window.clearTimeout(permissionFollowupTimer);
        permissionFollowupTimer = undefined;
      }
    };
  });
</script>

<section class:embedded class:dock class="settings glass">
  <header class:drag-region={!dock}>
    {#if dock}
      <h2>Settings</h2>
      <button class="chat-return no-drag" onclick={onclose} aria-label="Return to chat">
        <MessageSquare size={14} />
      </button>
    {:else}
      <div><span class="eyebrow">woof</span><h2>Settings</h2></div>
    {/if}
    {#if !embedded && !dock}
      <button class="close no-drag" onclick={onclose} aria-label="Close settings"><X size={16} /></button>
    {/if}
  </header>

  <div class="settings-body">
    {#if dock}
      <nav class="dock-nav" aria-label="Settings sections">
        <div class="dock-nav-items">
          {#each dockSections as section}
            <button
              class:active={dockActive === section.id}
              onclick={() => activateDockSection(section.id)}
            >
              <section.icon size={14} strokeWidth={1.8} />
              {section.label}
            </button>
          {/each}
        </div>
        <span class="dock-version">Version 0.1.0</span>
      </nav>
    {:else}
      <nav aria-label="Settings sections">
        {#each sections as section}
          <button class:active={active === section.id} onclick={() => activateSection(section.id)}>
            <section.icon size={15} />
            {section.label}
          </button>
        {/each}
        <div class="nav-spacer"></div>
        <button class:paused={capturePaused} class="capture" onclick={toggleCapture}>
          {#if capturePaused}<Play size={15} /> Resume capture{:else if captureStatus?.capturing === true}<Pause size={15} /> Pause capture{:else}<Play size={15} /> Retry capture{/if}
        </button>
      </nav>
    {/if}

    <div class="pane">
      {#if dock && dockActive === "identity"}
        <div class="pane-title"><h3>Identity</h3><p>How woof addresses you and labels your local memory.</p></div>
        <div class="dock-form-card">
          <label for="identity-name">Your name</label>
          <input id="identity-name" bind:value={contactName} autocomplete="name" placeholder="Name" />
          <label for="identity-company">Company or project <small>optional</small></label>
          <input id="identity-company" bind:value={contactCompany} autocomplete="organization" placeholder="Company" />
          <p>This profile stays in woof’s local preferences. Your name is also sent to the local memory service so recalled notes use it consistently.</p>
          {#if identityError}<p class="inline-error" role="alert">{identityError}</p>{/if}
          <div class="dock-form-actions">
            <span>{identityStatus === "loading" ? "Loading…" : ""}</span>
            <button disabled={identityStatus === "saving" || identityStatus === "loading"} onclick={() => void saveIdentity()}>
              {identityStatus === "saved" ? "Saved" : identityStatus === "saving" ? "Saving…" : "Save identity"}
            </button>
          </div>
        </div>
      {:else if dock && dockActive === "memory"}
        <div class="pane-title"><h3>Memory</h3><p>Inspect the context woof is keeping close at hand.</p></div>
        <section class="dock-card memory-summary">
          <div>
            <span class="dock-control-icon"><Database size={18} /></span>
            <span><b>Local capture</b><small>{captureSummary()}</small></span>
          </div>
          <button class:destructive={captureStatus?.capturing === true} onclick={() => void toggleCapture()}>{captureActionLabel()}</button>
        </section>
        <section class="dock-card memory-list">
          <div class="dock-card-heading compact-heading">
            <span><b>WORKING MEMORY</b><p>Most relevant recent context from the local daemon.</p></span>
            <button aria-label="Refresh working memory" onclick={() => void loadMemory()}>Refresh</button>
          </div>
          {#if memoryStatus === "loading"}
            <div class="compact-state"><LoaderCircle size={15} class="spin" /> Loading local context…</div>
          {:else if memoryStatus === "error"}
            <div class="compact-state error-text"><AlertTriangle size={15} /> {memoryError}</div>
          {:else if workingMemory.length === 0}
            <div class="compact-state">No working-memory items yet.</div>
          {:else}
            {#each workingMemory as item (item.wm_id)}
              <div class="memory-item">
                <b>{item.window_title || item.app}</b>
                <p>{item.content}</p>
                <small>{item.app} · relevance {Math.round(item.relevance * 100)}%</small>
              </div>
            {/each}
          {/if}
        </section>
      {:else if dock && dockActive === "notifications"}
        <div class="pane-title"><h3>Notifications</h3><p>Choose when local reminders can reach you.</p></div>
        <section class="dock-card notification-controls">
          <label>
            <span class="dock-control-icon"><Bell size={19} /></span>
            <span><b>Local nudges</b><small>While woof is running, show due reminders and detected follow-ups in the companion and macOS Notification Center.</small></span>
            <input
              type="checkbox"
              checked={nudgesEnabled}
              disabled={notificationStatus === "loading" || notificationStatus === "saving"}
              onchange={(event) => void setNotificationNudges(event.currentTarget.checked)}
            />
          </label>
          <button onclick={() => void invokeCommand(COMMANDS.notificationOpenSettings)}>Open macOS notification settings</button>
          <p>Notifications are generated locally. The app does not register OS-scheduled alarms, use remote push, or upload notification history.</p>
          {#if notificationError}<p class="inline-error" role="alert">{notificationError}</p>{/if}
        </section>
        <section class="dock-card reminder-editor">
          <div class="dock-card-heading">
            <b>SCHEDULED REMINDERS</b>
            <p>Create a local one-time or daily reminder. The menu-bar app checks it while running; reminders that became due while it was closed are checked after the next launch. Turn on Open at login for continuity after sign-in.</p>
          </div>
          <div class="reminder-form">
            <label for="reminder-label">Label</label>
            <input id="reminder-label" maxlength="120" bind:value={reminderLabel} placeholder="Review launch notes" />
            <label for="reminder-prompt">Reminder</label>
            <textarea id="reminder-prompt" maxlength="1000" bind:value={reminderPrompt} placeholder="Review the open launch decisions."></textarea>
            <div class="reminder-schedule">
              <label>
                <span>Schedule</span>
                <select aria-label="Reminder schedule" bind:value={reminderKind}>
                  <option value="once">Once</option>
                  <option value="daily">Daily</option>
                </select>
              </label>
              {#if reminderKind === "once"}
                <label>
                  <span>Date and time</span>
                  <input aria-label="Reminder date and time" type="datetime-local" bind:value={reminderOnceAt} />
                </label>
              {:else}
                <label>
                  <span>Time</span>
                  <input aria-label="Daily reminder time" type="time" bind:value={reminderDailyTime} />
                </label>
              {/if}
            </div>
            {#if reminderError}<p class="inline-error" role="alert">{reminderError}</p>{/if}
            <button
              class="primary-button"
              disabled={reminderStatus === "saving" || !reminderLabel.trim() || !reminderPrompt.trim()}
              onclick={() => void createReminder()}
            >{reminderStatus === "saving" ? "Creating…" : "Add reminder"}</button>
          </div>
          <div class="reminder-list" aria-label="Scheduled reminders">
            {#each reminders as reminder (reminder.rule_id)}
              <article>
                <div><b>{reminder.label}</b><p>{reminder.prompt}</p><small>{reminderSchedule(reminder)}</small></div>
                <button
                  aria-label={`Delete reminder ${reminder.label}`}
                  disabled={reminderStatus === "saving"}
                  onclick={() => void deleteReminder(reminder.rule_id)}
                ><Trash2 size={13} /> Delete</button>
              </article>
            {:else}
              {#if reminderStatus !== "loading"}<p class="compact-state">No scheduled reminders.</p>{/if}
            {/each}
          </div>
        </section>
      {:else if dock && dockActive === "community"}
        <div class="pane-title"><h3>Community</h3><p>woof is private and personal.</p></div>
        <section class="dock-card honest-state">
          <span class="dock-control-icon"><UsersRound size={19} /></span>
          <div><b>No community account</b><p>woof has no cloud community, analytics identity, billing profile, or referral tracking.</p></div>
        </section>
      {:else if dock && dockActive === "mcp"}
        <div class="pane-title"><h3>MCP</h3><p>Connect local assistants to woof’s read-only memory tools.</p></div>
        <section class="dock-card mcp-card">
          <div class="service-status"><i class:ready={daemonStatus.startsWith("Ready")}></i><span><b>woof</b><small>{daemonStatus}</small></span></div>
          <pre>{mcpConfig || "Configuration unavailable"}</pre>
          <button disabled={!mcpConfig} onclick={() => void copyMcpConfig()}><Copy size={13} /> {mcpCopied ? "Copied" : "Copy configuration"}</button>
          {#if mcpConfigError}<p class="inline-error" role="alert">{mcpConfigError}</p>{/if}
          <p>The bridge uses stdio and authenticates to <code>127.0.0.1:3334</code> with woof’s private local token.</p>
        </section>
      {:else if dock && dockActive === "bug-report"}
        <div class="pane-title"><h3>Bug report</h3><p>Diagnose locally without silently sending your memory anywhere.</p></div>
        <section class="dock-card honest-state">
          <span class="dock-control-icon"><Bug size={19} /></span>
          <div><b>Automatic reports are disabled</b><p>Woof has no analytics or crash-report upload endpoint. Reproduce the issue and share only logs or screenshots you deliberately choose.</p></div>
        </section>
      {:else if dock && dockActive === "tutorials"}
        <div class="pane-title"><h3>Tutorials</h3><p>Revisit the permission and local-memory walkthrough.</p></div>
        <section class="dock-card tutorial-card">
          <span class="dock-control-icon"><Video size={19} /></span>
          <div><b>Getting started</b><p>Replay onboarding in its own focused window. Your existing memory and settings are not reset.</p><button onclick={() => invokeCommand(COMMANDS.openOnboarding)}>Replay onboarding</button></div>
        </section>
      {:else if dock && dockActive === "release-notes"}
        <div class="pane-title"><h3>Release notes</h3><p>What is included in this local build.</p></div>
        <section class="dock-card release-card">
          <b>woof 0.1.0</b><small>Local macOS app</small>
          <ul>
            <li>Top-docked companion and in-shell settings</li>
            <li>Accessibility-text capture with local redaction</li>
            <li>Local memory, OpenAI chat, dictation, and MCP bridge</li>
          </ul>
          <p>woof does not include account authentication, billing, analytics, or feature flags.</p>
        </section>
      {:else if active === "general"}
        <div class="pane-title"><h3>{dock ? "Appearance" : "General"}</h3><p>How woof looks and behaves around your Mac.</p></div>
        <div class="group">
          <div class="companion-position-row">
            <span class="setting-icon"><Palette size={16} /></span>
            <span><b>Companion position</b><small>Edges rest as a thin tab; corners as a small box.</small></span>
            <div class="position-screen" role="group" aria-label="Companion position">
              <i aria-hidden="true"></i>
              {#each companionPositions as position}
                <button
                  type="button"
                  class:active={companionPosition === position.value}
                  class={`position-${position.value}`}
                  disabled={appearanceStatus === "saving"}
                  aria-label={position.label}
                  aria-pressed={companionPosition === position.value}
                  title={position.label}
                  onclick={() => void setCompanionPosition(position.value)}
                ></button>
              {/each}
            </div>
          </div>
          <label>
            <span class="setting-icon"><ChevronRight size={16} /></span>
            <span><b>Open on hover</b><small>Expand when the pointer reaches the collapsed companion.</small></span>
            <input type="checkbox" checked={hoverOpen} onchange={(event) => void setHoverOpen(event.currentTarget.checked)} />
          </label>
          <label>
            <span class="setting-icon"><EyeOff size={16} /></span>
            <span><b>Auto-hide collapsed tab</b><small>Reveal the tab when the pointer reaches its screen edge.</small></span>
            <input type="checkbox" checked={collapsedAutoHide} onchange={(event) => void setCollapsedAutoHide(event.currentTarget.checked)} />
          </label>
          <label>
            <span class="setting-icon"><Sparkles size={16} /></span>
            <span><b>Open at login</b><small>Start the local companion after you sign in.</small></span>
            <input type="checkbox" checked={loginItem} onchange={(event) => toggle("login", event.currentTarget.checked)} />
          </label>
          <label>
            <span class="setting-icon"><EyeOff size={16} /></span>
            <span><b>Reduce visual effects</b><small>Use shorter fades and no breathing glow.</small></span>
            <input type="checkbox" checked={reduceMotion} onchange={(event) => toggle("reduce", event.currentTarget.checked)} />
          </label>
          <label>
            <span class="setting-icon"><Volume2 size={16} /></span>
            <span><b>Caret sounds</b><small>Play a quiet cue when inline help opens.</small></span>
            <input type="checkbox" checked={caretSounds} onchange={(event) => toggle("sounds", event.currentTarget.checked)} />
          </label>
          <label>
            <span class="setting-icon"><Mic size={16} /></span>
            <span><b>Voice dictation</b><small>Hold Right Option to talk into the focused field.</small></span>
            <input type="checkbox" checked={voice} onchange={(event) => toggle("voice", event.currentTarget.checked)} />
          </label>
        </div>
        {#if appearanceError}<p class="inline-error" role="alert">{appearanceError}</p>{/if}
      {:else if active === "privacy"}
        {#if privacyView === "overview"}
          {#if dock}
            <div class="dock-privacy-stack">
              <section class="dock-card dock-permissions">
                <div class="dock-card-heading">
                  <b>PERMISSIONS</b>
                  <p>What woof needs from macOS to build your memory and hear dictation.</p>
                </div>
                <button class="dock-permission-row" onclick={openAccessibilityPermission}>
                  <span>
                    <b>Accessibility</b>
                    <small>Required. Lets woof read on-screen text to build your memory.</small>
                  </span>
                  <span class:granted={accessibilityGranted} class="permission-state">
                    <i></i>{accessibilityGranted ? "Granted" : "Not granted"}
                  </span>
                </button>
                <button class="dock-permission-row" onclick={openMicrophonePermission}>
                  <span>
                    <b>Microphone</b>
                    <small>Only needed if you turn on voice dictation.</small>
                  </span>
                  <span
                    class:granted={microphoneLabel(microphonePermission) === "Granted"}
                    class:denied={microphoneLabel(microphonePermission) === "Denied"}
                    class="permission-state"
                  >
                    <i></i>{microphoneLabel(microphonePermission)}
                  </span>
                </button>
                <button class="dock-permission-row" onclick={openInputMonitoringPermission}>
                  <span>
                    <b>Input Monitoring</b>
                    <small>Used by global modifier shortcuts while woof is in the background.</small>
                  </span>
                  <span class:granted={inputMonitoringGranted} class="permission-state"><i></i>{inputMonitoringGranted ? "Granted" : "Not granted"}</span>
                </button>
              </section>

              <section class="dock-card dock-danger">
                <div class="dock-card-heading">
                  <b>Danger zone</b>
                  <p>This action is irreversible. Make sure you have backups.</p>
                </div>
                <button
                  type="button"
                  aria-haspopup="dialog"
                  onclick={() => {
                    deleteError = "";
                    deleteConfirming = true;
                  }}
                >
                  <Trash2 size={13} /> Delete all data
                </button>
                {#if deleteStatus === "deleted"}
                  <p class="delete-result" role="status">Local memory and identity were permanently deleted.</p>
                {:else if deleteStatus === "error" && !deleteConfirming}
                  <p class="delete-result error-text" role="alert">{deleteError}</p>
                {/if}
              </section>

              <section class="dock-card dock-local-controls">
                <span class="dock-control-icon"><ShieldCheck size={19} /></span>
                <div>
                  <b>Local privacy controls</b>
                  <p>Choose which apps, websites, and windows woof ignores.</p>
                  <span class="dock-control-actions">
                    <button onclick={() => void openBlacklist()}>Capture blacklist</button>
                    <button onclick={() => void openRetention()}>Data retention</button>
                  </span>
                </div>
              </section>
            </div>

            {#if deleteConfirming}
              <div class="confirm-scrim">
                <div
                  class="confirm-dialog"
                  role="alertdialog"
                  aria-modal="true"
                  aria-labelledby="delete-all-title"
                >
                  <span class="confirm-icon"><AlertTriangle size={20} /></span>
                  <h4 id="delete-all-title">Permanently delete woof’s local memory?</h4>
                  <p>This removes captures, activity, chat history, generated memory, reminders, time records, and your local identity. Your API key and app preferences stay configured.</p>
                  {#if deleteError}<p class="inline-error" role="alert">{deleteError}</p>{/if}
                  <div>
                    <button class="secondary-button" disabled={deleteStatus === "deleting"} onclick={() => (deleteConfirming = false)}>Keep my data</button>
                    <button class="primary-button destructive" disabled={deleteStatus === "deleting"} onclick={() => void deleteAllData()}>
                      {deleteStatus === "deleting" ? "Deleting…" : "Delete permanently"}
                    </button>
                  </div>
                </div>
              </div>
            {/if}
          {:else}
            <div class="pane-title"><h3>Privacy</h3><p>Capture stays local and under your control.</p></div>
            <div class="privacy-callout">
              <ShieldCheck size={21} />
              <div><b>Local-first memory</b><p>Visible text is stored in your woof database with file permissions 0600. No screenshots are taken.</p></div>
            </div>
            <div class="group links">
              <button onclick={() => invokeCommand(COMMANDS.openAccessibilitySettings)}>
                <span class="setting-icon"><Accessibility size={16} /></span>
                <span><b>Accessibility permission</b><small>Review access in System Settings.</small></span>
                <ChevronRight size={15} />
              </button>
              <button onclick={() => invokeCommand(COMMANDS.microphoneStatus, { openSettings: true })}>
                <span class="setting-icon"><Mic size={16} /></span>
                <span><b>Microphone permission</b><small>Review voice access in System Settings.</small></span>
                <ChevronRight size={15} />
              </button>
              <button onclick={() => void openBlacklist()}>
                <span class="setting-icon"><EyeOff size={16} /></span>
                <span><b>Capture blacklist</b><small>Apps, domains, and window titles woof ignores.</small></span>
                <ChevronRight size={15} />
              </button>
              <button onclick={() => void openRetention()}>
                <span class="setting-icon"><Database size={16} /></span>
                <span><b>Data retention</b><small>Choose how long local memory is kept.</small></span>
                <ChevronRight size={15} />
              </button>
            </div>
          {/if}
        {:else if privacyView === "blacklist"}
          <div class="nested-title">
            <button
              class="back-button"
              aria-label="Back to Privacy"
              onclick={() => (privacyView = "overview")}
            ><ArrowLeft size={15} /></button>
            <div class="pane-title"><h3>Capture blacklist</h3><p>Woof skips matching apps, sites, and windows before reading visible text.</p></div>
          </div>

          {#if blacklistStatus === "loading"}
            <div class="status-card" aria-live="polite">
              <span class="spinner"><LoaderCircle size={19} /></span>
              <div><b>Loading exclusions</b><p>Reading the daemon-backed privacy rules.</p></div>
            </div>
          {:else if !blacklistLoaded}
            <div class="status-card error-card" role="alert">
              <AlertTriangle size={19} />
              <div><b>Blacklist unavailable</b><p>{blacklistError}</p></div>
              <button class="secondary-button" onclick={() => void openBlacklist()}>Try again</button>
            </div>
          {:else}
            <div class="blacklist-editor">
              <div class="rule-list" aria-label="Capture blacklist rules">
                {#if blacklist.length === 0}
                  <div class="empty-rules">
                    <EyeOff size={18} />
                    <div><b>No custom exclusions</b><p>Woof still refuses password fields and secure keyboard input automatically.</p></div>
                  </div>
                {:else}
                  {#each blacklist as entry, index (index)}
                    <div class="rule-row">
                      <select
                        aria-label={`Rule type ${index + 1}`}
                        value={entry.kind}
                        onchange={(event) =>
                          setBlacklistEntry(index, "kind", event.currentTarget.value)}
                      >
                        {#each CAPTURE_BLACKLIST_KINDS as kind}
                          <option value={kind}>{blacklistKindLabels[kind]}</option>
                        {/each}
                      </select>
                      <input
                        aria-label={`Rule pattern ${index + 1}`}
                        value={entry.pattern}
                        oninput={(event) =>
                          setBlacklistEntry(index, "pattern", event.currentTarget.value)}
                      />
                      <button
                        class="icon-button danger-button"
                        aria-label={`Remove blacklist rule ${index + 1}`}
                        onclick={() => removeBlacklistEntry(index)}
                      ><Trash2 size={14} /></button>
                    </div>
                  {/each}
                {/if}
              </div>

              <div class="rule-add">
                <select aria-label="New rule type" bind:value={draftKind}>
                  {#each CAPTURE_BLACKLIST_KINDS as kind}
                    <option value={kind}>{blacklistKindLabels[kind]}</option>
                  {/each}
                </select>
                <input
                  aria-label="New rule pattern"
                  bind:value={draftPattern}
                  placeholder="e.g. Slack, payroll, or private.example.com"
                  onkeydown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      addBlacklistEntry();
                    }
                  }}
                />
                <button
                  class="icon-button add-button"
                  aria-label="Add capture blacklist rule"
                  disabled={!draftPattern.trim() || blacklist.length >= 100}
                  onclick={addBlacklistEntry}
                ><Plus size={15} /></button>
              </div>

              {#if blacklistError}
                <p class="inline-error" role="alert">{blacklistError}</p>
              {/if}

              <div class="editor-footer">
                <span>{blacklist.length} of 100 rules</span>
                <button
                  class="primary-button"
                  disabled={!blacklistDirty || blacklistStatus === "saving"}
                  onclick={() => void saveBlacklist()}
                >
                  {#if blacklistStatus === "saving"}
                    <span class="spinner"><LoaderCircle size={13} /></span> Saving
                  {:else if blacklistStatus === "saved"}
                    <Check size={13} /> Saved
                  {:else}
                    Save blacklist
                  {/if}
                </button>
              </div>
            </div>
          {/if}
        {:else}
          <div class="nested-title">
            <button
              class="back-button"
              aria-label="Back to Privacy"
              onclick={() => (privacyView = "overview")}
            ><ArrowLeft size={15} /></button>
            <div class="pane-title"><h3>Data retention</h3><p>Control how long woof keeps local memory.</p></div>
          </div>
          {#if retentionStatus === "loading"}
            <div class="status-card" aria-live="polite">
              <span class="spinner"><LoaderCircle size={19} /></span>
              <div><b>Loading retention</b><p>Reading the local memory policy.</p></div>
            </div>
          {:else}
            <div class="dock-form-card retention-card">
              <label for="data-retention">Keep local memory</label>
              <select id="data-retention" aria-label="Data retention" bind:value={retentionDraft}>
                <option value="keep_forever">Forever</option>
                <option value="7">7 days</option>
                <option value="30">30 days</option>
                <option value="90">90 days</option>
                <option value="365">1 year</option>
              </select>
              <p>Choosing a shorter window immediately removes older captures and derived local memory. This does not remove your API key or preferences.</p>
              {#if retentionError}<p class="inline-error" role="alert">{retentionError}</p>{/if}
              <div class="dock-form-actions">
                <span role="status">{retentionStatus === "saved" ? "Saved" : ""}</span>
                <button
                  disabled={retentionStatus === "saving" || retentionDraft === retentionValue(retention)}
                  onclick={() => void saveRetention()}
                >{retentionStatus === "saving" ? "Saving…" : "Save retention"}</button>
              </div>
            </div>
          {/if}
        {/if}
      {:else if active === "shortcuts"}
        <div class="pane-title"><h3>Shortcuts</h3><p>Fast ways to call woof without changing context.</p></div>
        {#if dock}
          <div class="dock-form-card shortcut-form">
            <div class="shortcut-heading">
              <label for="woof-modifier">Inline help and companion</label>
              <label class="shortcut-enabled">
                <input
                  type="checkbox"
                  aria-label="Inline help and companion enabled"
                  bind:checked={woofModifierEnabled}
                />
                Enabled
              </label>
            </div>
            <div class="shortcut-record-row">
              <select
                id="woof-modifier"
                bind:value={woofModifier}
                disabled={!woofModifierEnabled}
                aria-invalid={modifiersCollide}
                aria-describedby={modifiersCollide ? "modifier-collision" : undefined}
              >
                {#each modifierOptions as option}
                  <option value={option.value}>{option.label}</option>
                {/each}
              </select>
              <button
                class="secondary-button"
                aria-label="Record inline modifier"
                disabled={!woofModifierEnabled || shortcutRecording !== null}
                onclick={() => void recordModifier("woof")}
              >{shortcutRecording === "woof" ? "Press a modifier…" : "Record"}</button>
            </div>
            <small>Double-tap the selected modifier to rewrite a field or open woof.</small>
            <label for="transcription-modifier">Hold to talk</label>
            <div class="shortcut-record-row">
              <select
                id="transcription-modifier"
                bind:value={transcriptionModifier}
                aria-invalid={modifiersCollide}
                aria-describedby={modifiersCollide ? "modifier-collision" : undefined}
              >
                {#each modifierOptions as option}
                  <option value={option.value}>{option.label}</option>
                {/each}
              </select>
              <button
                class="secondary-button"
                aria-label="Record dictation modifier"
                disabled={shortcutRecording !== null}
                onclick={() => void recordModifier("transcription")}
              >{shortcutRecording === "transcription" ? "Press a modifier…" : "Record"}</button>
            </div>
            <small>Hold the selected modifier while speaking.</small>
            <div class="shortcut-heading">
              <label for="secondary-shortcut">Secondary shortcut</label>
              <label class="shortcut-enabled">
                <input
                  type="checkbox"
                  aria-label="Secondary shortcut enabled"
                  bind:checked={secondaryShortcutEnabled}
                />
                Enabled
              </label>
            </div>
            <div class="shortcut-record-row">
              <input
                id="secondary-shortcut"
                value={shortcutChordLabel(secondaryShortcut)}
                readonly
                disabled={!secondaryShortcutEnabled}
                spellcheck="false"
              />
              <button
                class="secondary-button"
                aria-label="Record secondary shortcut"
                disabled={!secondaryShortcutEnabled || shortcutRecording !== null}
                onclick={() => void recordSecondaryShortcut()}
              >{shortcutRecording === "secondary" ? "Press a shortcut…" : "Record"}</button>
            </div>
            {#if modifiersCollide}
              <p id="modifier-collision" class="inline-error" role="alert">{MODIFIER_COLLISION_MESSAGE}</p>
            {:else if shortcutError}
              <p class="inline-error" role="alert">{shortcutError}</p>
            {/if}
            <div class="dock-form-actions">
              <span>{shortcutStatus === "loading" ? "Loading…" : ""}</span>
              <button disabled={shortcutStatus === "loading" || shortcutStatus === "saving" || modifiersCollide} onclick={() => void saveShortcuts()}>
                {shortcutStatus === "saved" ? "Saved" : shortcutStatus === "saving" ? "Saving…" : "Save shortcuts"}
              </button>
            </div>
          </div>
        {:else}
          <div class="group shortcut-list">
            <div><span><b>Inline help</b><small>Rewrite selection or whole draft</small></span><kbd>{modifierLabel(woofModifier)}</kbd><i>{woofModifierEnabled ? "× 2" : "off"}</i></div>
            <div><span><b>Hold to talk</b><small>Dictate into the current field</small></span><kbd>{modifierLabel(transcriptionModifier)}</kbd></div>
            <div><span><b>Secondary shortcut</b><small>Works when modifier taps are unavailable</small></span><kbd>{shortcutChordLabel(secondaryShortcut)}</kbd><i>{secondaryShortcutEnabled ? "enabled" : "off"}</i></div>
            <div><span><b>Open companion</b><small>When focus is outside a text field</small></span><kbd>{modifierLabel(woofModifier)}</kbd><i>{woofModifierEnabled ? "× 2" : "off"}</i></div>
          </div>
        {/if}
      {:else}
        <div class="pane-title"><h3>{dock ? "Account" : "OpenAI"}</h3><p>One private Keychain key for chat and realtime transcription.</p></div>
        <div class:keyed={keyConfigured} class="key-status">
          {#if keyConfigured}<Check size={16} /> API key stored in Keychain{:else}<LockKeyhole size={16} /> No API key configured{/if}
        </div>
        <div class="api-form">
          <label for="api-key">Replace API key</label>
          <div>
            <KeyRound size={17} />
            <input id="api-key" type="password" bind:value={apiKey} autocomplete="off" placeholder="sk-proj-…" />
            <button disabled={apiKey.length < 12 || keyStatus === "saving" || keyStatus === "deleting"} onclick={saveKey}>{saved ? "Saved" : keyStatus === "saving" ? "Saving…" : "Save"}</button>
          </div>
          <p>Saved only to macOS Keychain as <code>com.julius.woof.openai</code>.</p>
          {#if keyError}<p class="inline-error" role="alert">{keyError}</p>{/if}
          {#if keyConfigured}<button class="clear-key" disabled={keyStatus === "saving" || keyStatus === "deleting"} onclick={() => void clearKey()}>{keyStatus === "deleting" ? "Removing…" : "Remove key from Keychain"}</button>{/if}
        </div>
        <div class="model-row">
          <span><b>Chat model</b><small>Streaming Chat Completions</small></span><code>gpt-5.6-terra</code>
        </div>
        <div class="model-row">
          <span><b>Transcription model</b><small>OpenAI Realtime</small></span><code>gpt-4o-transcribe</code>
        </div>
      {/if}
    </div>
  </div>
</section>

<style>
  .settings {
    width: 700px;
    height: 520px;
    overflow: hidden;
    border-radius: 23px;
    background: var(--glass-strong);
  }

  .settings.embedded {
    width: 100%;
    height: 100%;
    border: 0;
    border-radius: 0;
    box-shadow: none;
    background: transparent;
    backdrop-filter: none;
  }

  header {
    height: 68px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 19px 0 24px;
    border-bottom: 1px solid var(--line);
  }

  header h2 {
    margin: 3px 0 0;
    font-size: 18px;
    line-height: 1;
    letter-spacing: -0.035em;
  }

  .close {
    width: 31px;
    height: 31px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 10px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--ink) 6%, transparent);
    cursor: pointer;
  }

  .settings-body {
    height: calc(100% - 68px);
    display: grid;
    grid-template-columns: 174px 1fr;
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 14px 10px 12px;
    border-right: 1px solid var(--line);
    background: color-mix(in srgb, var(--cream-solid) 38%, transparent);
  }

  nav button {
    height: 36px;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 11px;
    border: 0;
    border-radius: 10px;
    color: var(--ink-muted);
    background: transparent;
    text-align: left;
    font-size: 11px;
    font-weight: 590;
    cursor: pointer;
  }

  nav button.active {
    color: var(--ink);
    background: color-mix(in srgb, var(--fawn) 13%, transparent);
  }

  .nav-spacer {
    flex: 1;
  }

  nav .capture {
    color: var(--rose);
  }

  nav .capture.paused {
    color: var(--sage);
  }

  .pane {
    position: relative;
    overflow: auto;
    padding: 25px 30px;
  }

  .pane-title h3 {
    margin: 0;
    font-size: 17px;
    letter-spacing: -0.03em;
  }

  .pane-title p {
    margin: 5px 0 20px;
    color: var(--ink-faint);
    font-size: 10px;
  }

  .group {
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 16px;
    background: color-mix(in srgb, var(--cream-solid) 48%, transparent);
  }

  .group label,
  .group.links button,
  .companion-position-row {
    min-height: 66px;
    display: grid;
    grid-template-columns: 34px 1fr auto;
    align-items: center;
    gap: 11px;
    width: 100%;
    padding: 9px 14px;
    border: 0;
    border-bottom: 1px solid var(--line);
    color: var(--ink);
    background: transparent;
    text-align: left;
  }

  .group label:last-child,
  .group.links button:last-child {
    border-bottom: 0;
  }

  .companion-position-row {
    border-bottom: 1px solid var(--line);
  }

  .position-screen {
    position: relative;
    width: 58px;
    height: 38px;
    border: 1px solid var(--line-strong);
    border-radius: 7px;
    background: color-mix(in srgb, var(--cream-solid) 48%, transparent);
  }

  .position-screen > i {
    position: absolute;
    top: 4px;
    left: 50%;
    width: 8px;
    height: 2px;
    border-radius: 2px;
    background: var(--ink-faint);
    transform: translateX(-50%);
  }

  .position-screen button {
    position: absolute;
    width: 8px;
    height: 8px;
    padding: 0;
    border: 1px solid color-mix(in srgb, var(--fawn-deep) 34%, transparent);
    border-radius: 3px;
    background: color-mix(in srgb, var(--fawn) 18%, transparent);
    cursor: pointer;
  }

  .position-screen button.active {
    border-color: var(--fawn-deep);
    background: var(--fawn-deep);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--fawn) 18%, transparent);
  }

  .position-screen button:disabled {
    cursor: default;
    opacity: 0.55;
  }

  .position-top {
    top: -4px;
    left: 24px;
  }

  .position-left {
    top: 15px;
    left: -4px;
  }

  .position-right {
    top: 15px;
    right: -4px;
  }

  .position-bottom {
    bottom: -4px;
    left: 24px;
  }

  .position-bottom-left {
    bottom: -4px;
    left: -4px;
  }

  .position-bottom-right {
    right: -4px;
    bottom: -4px;
  }

  .group.links button {
    cursor: pointer;
  }

  .setting-icon {
    width: 33px;
    height: 33px;
    display: grid;
    place-items: center;
    border-radius: 11px;
    color: var(--fawn-deep);
    background: color-mix(in srgb, var(--fawn) 12%, transparent);
  }

  .group b,
  .group small,
  .model-row b,
  .model-row small {
    display: block;
  }

  .group b,
  .model-row b {
    font-size: 11px;
  }

  .group small,
  .model-row small {
    margin-top: 3px;
    color: var(--ink-faint);
    font-size: 9px;
  }

  input[type="checkbox"] {
    position: relative;
    width: 32px;
    height: 19px;
    appearance: none;
    border-radius: 99px;
    background: var(--cream-dim);
    cursor: pointer;
    transition: background 160ms ease;
  }

  input[type="checkbox"]::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    background: white;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.2);
    transition: transform 160ms var(--spring);
  }

  input[type="checkbox"]:checked {
    background: var(--sage);
  }

  input[type="checkbox"]:checked::after {
    transform: translateX(13px);
  }

  .privacy-callout {
    display: flex;
    gap: 12px;
    margin-bottom: 13px;
    padding: 15px;
    border: 1px solid color-mix(in srgb, var(--sage) 23%, transparent);
    border-radius: 16px;
    color: var(--sage);
    background: color-mix(in srgb, var(--sage) 7%, transparent);
  }

  .privacy-callout b {
    font-size: 11px;
  }

  .privacy-callout p {
    margin: 4px 0 0;
    color: var(--ink-muted);
    font-size: 9px;
    line-height: 1.5;
  }

  .nested-title {
    display: flex;
    align-items: flex-start;
    gap: 10px;
  }

  .nested-title .pane-title {
    min-width: 0;
    flex: 1;
  }

  .back-button {
    width: 29px;
    height: 29px;
    display: grid;
    flex: 0 0 auto;
    place-items: center;
    margin-top: -4px;
    border: 1px solid var(--line);
    border-radius: 9px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 54%, transparent);
    cursor: pointer;
  }

  .back-button:disabled {
    opacity: 0.38;
    cursor: default;
  }

  .status-card {
    min-height: 98px;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 17px;
    border: 1px solid var(--line);
    border-radius: 16px;
    background: color-mix(in srgb, var(--cream-solid) 48%, transparent);
  }

  .status-card > div {
    min-width: 0;
    flex: 1;
  }

  .status-card b {
    font-size: 11px;
  }

  .status-card p {
    margin: 4px 0 0;
    color: var(--ink-faint);
    font-size: 9px;
    line-height: 1.45;
  }

  .error-card {
    border-color: color-mix(in srgb, var(--rose) 26%, transparent);
    color: var(--rose);
    background: color-mix(in srgb, var(--rose) 7%, transparent);
  }

  .blacklist-editor {
    min-width: 0;
  }

  .rule-list {
    max-height: 214px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 14px;
    background: color-mix(in srgb, var(--cream-solid) 42%, transparent);
  }

  .rule-row {
    min-height: 48px;
    display: grid;
    grid-template-columns: 116px minmax(0, 1fr) 30px;
    align-items: center;
    gap: 7px;
    padding: 7px 8px;
    border-bottom: 1px solid var(--line);
  }

  .rule-row:last-child {
    border-bottom: 0;
  }

  .rule-row select,
  .rule-row input,
  .rule-add select,
  .rule-add input {
    min-width: 0;
    height: 32px;
    border: 1px solid var(--line-strong);
    border-radius: 9px;
    outline: 0;
    color: var(--ink);
    background: var(--cream);
    font-size: 9px;
  }

  .rule-row select,
  .rule-add select {
    padding: 0 22px 0 8px;
  }

  .rule-row input,
  .rule-add input {
    padding: 0 9px;
    user-select: text;
  }

  .rule-row input:focus,
  .rule-row select:focus,
  .rule-add input:focus,
  .rule-add select:focus {
    border-color: color-mix(in srgb, var(--fawn-deep) 48%, transparent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--fawn) 10%, transparent);
  }

  .empty-rules {
    min-height: 88px;
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 14px;
    color: var(--ink-faint);
  }

  .empty-rules b {
    color: var(--ink);
    font-size: 10px;
  }

  .empty-rules p {
    margin: 4px 0 0;
    font-size: 8.5px;
    line-height: 1.45;
  }

  .rule-add {
    display: grid;
    grid-template-columns: 116px minmax(0, 1fr) 30px;
    gap: 7px;
    margin-top: 9px;
    padding: 8px;
    border: 1px dashed var(--line-strong);
    border-radius: 13px;
  }

  .icon-button {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 9px;
    cursor: pointer;
  }

  .danger-button {
    color: var(--rose);
    background: color-mix(in srgb, var(--rose) 9%, transparent);
  }

  .add-button {
    color: var(--cream);
    background: var(--brown);
  }

  .icon-button:disabled {
    opacity: 0.3;
    cursor: default;
  }

  .inline-error {
    margin: 8px 2px 0;
    color: var(--rose);
    font-size: 8.5px;
    line-height: 1.4;
  }

  .editor-footer {
    min-height: 42px;
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 12px;
  }

  .editor-footer > span {
    color: var(--ink-faint);
    font-size: 8.5px;
  }

  .primary-button,
  .secondary-button {
    min-height: 31px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 0 12px;
    border-radius: 9px;
    font-size: 9px;
    font-weight: 650;
    cursor: pointer;
  }

  .primary-button {
    border: 0;
    color: var(--cream);
    background: var(--brown);
  }

  .primary-button.destructive {
    background: var(--rose);
  }

  .secondary-button {
    border: 1px solid var(--line-strong);
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 55%, transparent);
  }

  .primary-button:disabled,
  .secondary-button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .confirm-scrim {
    position: absolute;
    z-index: 4;
    inset: 0;
    display: grid;
    place-items: center;
    padding: 26px;
    background: color-mix(in srgb, var(--brown-deep) 24%, transparent);
    -webkit-backdrop-filter: blur(6px);
    backdrop-filter: blur(6px);
  }

  .confirm-dialog {
    width: min(100%, 380px);
    padding: 19px;
    border: 1px solid color-mix(in srgb, var(--rose) 26%, var(--line));
    border-radius: 18px;
    background: var(--cream-solid);
    box-shadow: var(--shadow-tight);
    text-align: center;
  }

  .confirm-icon {
    width: 39px;
    height: 39px;
    display: grid;
    place-items: center;
    margin: 0 auto 10px;
    border-radius: 13px;
    color: var(--rose);
    background: color-mix(in srgb, var(--rose) 10%, transparent);
  }

  .confirm-dialog h4 {
    margin: 0;
    font-size: 13px;
  }

  .confirm-dialog > p {
    margin: 7px 0 10px;
    color: var(--ink-muted);
    font-size: 9px;
    line-height: 1.5;
  }

  .confirm-dialog > div {
    display: flex;
    justify-content: center;
    gap: 8px;
    margin-top: 14px;
  }

  .spinner {
    display: inline-flex;
    animation: settings-spin 0.75s linear infinite;
  }

  @keyframes settings-spin {
    to {
      transform: rotate(360deg);
    }
  }

  .shortcut-list {
    padding: 0 14px;
  }

  .shortcut-list > div {
    min-height: 64px;
    display: flex;
    align-items: center;
    gap: 7px;
    border-bottom: 1px solid var(--line);
  }

  .shortcut-list > div:last-child {
    border-bottom: 0;
  }

  .shortcut-list > div > span {
    flex: 1;
  }

  kbd {
    padding: 6px 8px;
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    background: var(--cream);
    box-shadow: 0 2px 0 var(--line);
    font: 600 9px/1 "Inter Variable", sans-serif;
  }

  .shortcut-list i {
    color: var(--ink-faint);
    font-size: 9px;
    font-style: normal;
  }

  .key-status {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 12px;
    padding: 12px 14px;
    border: 1px solid color-mix(in srgb, var(--amber) 28%, transparent);
    border-radius: 14px;
    color: var(--amber);
    background: color-mix(in srgb, var(--amber) 8%, transparent);
    font-size: 10px;
    font-weight: 620;
  }

  .key-status.keyed {
    border-color: color-mix(in srgb, var(--sage) 28%, transparent);
    color: var(--sage);
    background: color-mix(in srgb, var(--sage) 8%, transparent);
  }

  .api-form {
    margin-bottom: 13px;
    padding: 14px;
    border: 1px solid var(--line);
    border-radius: 15px;
    background: color-mix(in srgb, var(--cream-solid) 46%, transparent);
  }

  .api-form > label {
    display: block;
    margin-bottom: 8px;
    font-size: 10px;
    font-weight: 650;
  }

  .api-form > div {
    height: 40px;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 6px 0 11px;
    border: 1px solid var(--line-strong);
    border-radius: 11px;
    background: var(--cream);
  }

  .api-form input {
    min-width: 0;
    flex: 1;
    border: 0;
    outline: 0;
    color: var(--ink);
    background: transparent;
    font-size: 10px;
    user-select: text;
  }

  .api-form button {
    height: 28px;
    padding: 0 11px;
    border: 0;
    border-radius: 8px;
    color: var(--cream);
    background: var(--brown);
    font-size: 9px;
    font-weight: 650;
    cursor: pointer;
  }

  .api-form button:disabled {
    opacity: 0.35;
  }

  .api-form p {
    margin: 8px 0 0;
    color: var(--ink-faint);
    font-size: 8.5px;
  }

  code {
    font-family: ui-monospace, "SFMono-Regular", monospace;
  }

  .model-row {
    min-height: 54px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 12px;
    border-bottom: 1px solid var(--line);
  }

  .model-row code {
    padding: 5px 7px;
    border-radius: 7px;
    color: var(--fawn-deep);
    background: color-mix(in srgb, var(--fawn) 10%, transparent);
    font-size: 8px;
  }

  /* The dock variant lives inside the native 588 × 440 top companion shell. */
  .settings.dock {
    position: relative;
    width: 100%;
    height: 100%;
    border: 0;
    border-radius: 0;
    background: transparent;
    box-shadow: none;
    -webkit-backdrop-filter: none;
    backdrop-filter: none;
  }

  .dock > header {
    position: absolute;
    inset: 0;
    z-index: 4;
    height: 0;
    padding: 0;
    border: 0;
    pointer-events: none;
  }

  .dock > header h2 {
    position: absolute;
    top: 21px;
    left: 19px;
    margin: 0;
    font-size: 15px;
    font-weight: 630;
    letter-spacing: -0.025em;
  }

  .chat-return {
    position: absolute;
    top: 18px;
    right: 18px;
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border: 1px solid color-mix(in srgb, var(--line-strong) 70%, transparent);
    border-radius: 999px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 28%, transparent);
    cursor: pointer;
    pointer-events: auto;
    transition:
      color 140ms ease,
      background 140ms ease;
  }

  .chat-return:hover {
    color: var(--ink);
    background: color-mix(in srgb, var(--cream-solid) 52%, transparent);
  }

  .dock .settings-body {
    height: 100%;
    grid-template-columns: 156px minmax(0, 1fr);
  }

  .dock nav.dock-nav {
    min-height: 0;
    padding: 60px 10px 10px;
    gap: 0;
    background: color-mix(in srgb, var(--brown-deep) 17%, transparent);
  }

  .dock-nav-items {
    min-height: 0;
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 1px;
  }

  .dock nav.dock-nav button {
    height: 27px;
    flex: 0 0 27px;
    gap: 8px;
    padding: 0 10px;
    border-radius: 8px;
    font-size: 12px;
    font-weight: 570;
    letter-spacing: -0.012em;
  }

  .dock nav.dock-nav button :global(svg) {
    flex: 0 0 auto;
    opacity: 0.83;
  }

  .dock nav.dock-nav button.active {
    color: var(--fawn-bright);
    background: color-mix(in srgb, var(--fawn) 19%, transparent);
  }

  .dock-version {
    display: flex;
    min-height: 24px;
    align-items: flex-end;
    padding: 0 10px 1px;
    color: var(--ink-faint);
    font-size: 9px;
  }

  .dock .pane {
    min-width: 0;
    padding: 70px 20px 24px;
    overscroll-behavior: contain;
  }

  .dock-privacy-stack {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .dock-card {
    flex: 0 0 auto;
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 14px;
    background: color-mix(in srgb, var(--cream-solid) 25%, transparent);
    box-shadow: inset 0 1px 0 color-mix(in srgb, white 5%, transparent);
  }

  .dock-card-heading {
    padding: 15px 16px 13px;
  }

  .dock-card-heading > b {
    display: block;
    color: var(--fawn-bright);
    font-size: 9.5px;
    font-weight: 720;
    letter-spacing: 0.055em;
    text-transform: uppercase;
  }

  .dock-card-heading > p {
    max-width: 330px;
    margin: 9px 0 0;
    color: var(--ink-muted);
    font-size: 10px;
    font-weight: 540;
    line-height: 1.42;
  }

  .dock-permission-row {
    min-height: 61px;
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    width: calc(100% - 32px);
    margin: 0 16px;
    padding: 0;
    border: 0;
    border-top: 1px solid var(--line);
    color: var(--ink);
    background: transparent;
    text-align: left;
    cursor: pointer;
  }

  .dock-permission-row > span:first-child {
    min-width: 0;
  }

  .dock-permission-row b,
  .dock-permission-row small {
    display: block;
  }

  .dock-permission-row b {
    font-size: 10.5px;
    font-weight: 630;
  }

  .dock-permission-row small {
    margin-top: 4px;
    color: var(--ink-faint);
    font-size: 8.5px;
    line-height: 1.25;
  }

  .permission-state {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: 6px;
    color: var(--ink-faint);
    font-size: 9.5px;
    white-space: nowrap;
  }

  .permission-state i {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--ink-faint) 72%, transparent);
  }

  .permission-state.granted {
    color: var(--ink-muted);
  }

  .permission-state.granted i {
    background: #4fd77a;
    box-shadow: 0 0 8px rgba(79, 215, 122, 0.24);
  }

  .permission-state.denied {
    color: var(--rose);
  }

  .permission-state.denied i {
    background: var(--rose);
  }

  .dock-danger {
    padding-bottom: 15px;
  }

  .dock-danger .dock-card-heading {
    padding-bottom: 10px;
  }

  .dock-danger .dock-card-heading > b {
    color: #ff7777;
    letter-spacing: 0;
    text-transform: none;
  }

  .dock-danger .dock-card-heading > p {
    margin-top: 7px;
  }

  .dock-danger > button {
    height: 31px;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    margin-left: 16px;
    padding: 0 13px;
    border: 1px solid color-mix(in srgb, var(--rose) 63%, transparent);
    border-radius: 9px;
    color: #ff7777;
    background: color-mix(in srgb, var(--rose) 5%, transparent);
    font-size: 9.5px;
    font-weight: 650;
    cursor: pointer;
  }

  .delete-result {
    margin: 10px 16px 0;
    color: var(--ink-faint);
    font-size: 8.5px;
    line-height: 1.4;
  }

  .dock-local-controls {
    display: grid;
    grid-template-columns: 45px minmax(0, 1fr);
    gap: 12px;
    padding: 14px 16px 16px;
  }

  .dock-control-icon {
    width: 45px;
    height: 45px;
    display: grid;
    place-items: center;
    border-radius: 13px;
    color: var(--fawn-bright);
    background: color-mix(in srgb, var(--fawn) 12%, transparent);
  }

  .dock-local-controls b {
    font-size: 10.5px;
  }

  .dock-local-controls p {
    margin: 4px 0 10px;
    color: var(--ink-faint);
    font-size: 8.5px;
    line-height: 1.4;
  }

  .dock-control-actions {
    display: flex;
    gap: 7px;
  }

  .dock-control-actions button {
    height: 28px;
    padding: 0 10px;
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 34%, transparent);
    font-size: 8.5px;
    cursor: pointer;
  }

  .dock-form-card {
    display: grid;
    gap: 7px;
    padding: 16px;
    border: 1px solid var(--line);
    border-radius: 14px;
    background: color-mix(in srgb, var(--cream-solid) 25%, transparent);
  }

  .dock-form-card > label {
    margin-top: 5px;
    color: var(--ink);
    font-size: 10px;
    font-weight: 650;
  }

  .dock-form-card > label:first-child {
    margin-top: 0;
  }

  .dock-form-card > label small,
  .dock-form-card > small {
    color: var(--ink-faint);
    font-size: 8px;
    font-weight: 500;
  }

  .dock-form-card > input {
    box-sizing: border-box;
    width: 100%;
    min-height: 34px;
    padding: 0 10px;
    border: 1px solid var(--line-strong);
    border-radius: 9px;
    outline: 0;
    color: var(--ink);
    background: color-mix(in srgb, var(--cream-solid) 34%, transparent);
    font-size: 10px;
    user-select: text;
  }

  .dock-form-card > input:focus {
    border-color: color-mix(in srgb, var(--fawn) 58%, transparent);
  }

  .dock-form-card > p {
    margin: 5px 0 0;
    color: var(--ink-faint);
    font-size: 8.5px;
    line-height: 1.45;
  }

  .dock-form-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    min-height: 31px;
    margin-top: 4px;
  }

  .dock-form-actions span {
    color: var(--ink-faint);
    font-size: 8.5px;
  }

  .dock-form-actions button,
  .tutorial-card button,
  .mcp-card > button {
    min-height: 30px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 0 11px;
    border: 1px solid color-mix(in srgb, var(--fawn) 35%, transparent);
    border-radius: 8px;
    color: var(--fawn-bright);
    background: color-mix(in srgb, var(--fawn) 11%, transparent);
    font-size: 9px;
    font-weight: 650;
    cursor: pointer;
  }

  .dock-form-actions button:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .shortcut-form {
    gap: 6px;
  }

  .shortcut-heading,
  .shortcut-record-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .shortcut-heading {
    justify-content: space-between;
    margin-top: 5px;
  }

  .shortcut-heading:first-child {
    margin-top: 0;
  }

  .shortcut-heading > label:first-child {
    color: var(--ink);
    font-size: 10px;
    font-weight: 650;
  }

  .shortcut-enabled {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--ink-faint);
    font-size: 8px;
  }

  .shortcut-enabled input[type="checkbox"] {
    transform: scale(0.78);
    transform-origin: right center;
  }

  .shortcut-record-row > input,
  .shortcut-record-row > select {
    box-sizing: border-box;
    min-width: 0;
    min-height: 34px;
    flex: 1;
    padding: 0 10px;
    border: 1px solid var(--line-strong);
    border-radius: 9px;
    outline: 0;
    color: var(--ink);
    background: color-mix(in srgb, var(--cream-solid) 34%, transparent);
    font-size: 10px;
  }

  .shortcut-record-row > input:focus,
  .shortcut-record-row > select:focus {
    border-color: color-mix(in srgb, var(--fawn) 58%, transparent);
  }

  .shortcut-record-row > input:disabled,
  .shortcut-record-row > select:disabled {
    opacity: 0.5;
  }

  .shortcut-record-row > button {
    min-width: 92px;
    min-height: 34px;
    white-space: nowrap;
  }

  .memory-summary {
    min-height: 70px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 14px;
    margin-bottom: 10px;
  }

  .memory-summary > div {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }

  .memory-summary b,
  .memory-summary small,
  .service-status b,
  .service-status small {
    display: block;
  }

  .memory-summary b,
  .service-status b,
  .honest-state b,
  .tutorial-card b {
    font-size: 10.5px;
  }

  .memory-summary small,
  .service-status small {
    margin-top: 4px;
    color: var(--ink-faint);
    font-size: 8.5px;
    line-height: 1.3;
  }

  .memory-summary > button,
  .compact-heading > button {
    min-height: 27px;
    padding: 0 10px;
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 30%, transparent);
    font-size: 8.5px;
    cursor: pointer;
  }

  .memory-summary > button.destructive {
    color: #ff8585;
    border-color: color-mix(in srgb, var(--rose) 50%, transparent);
  }

  .memory-list {
    max-height: 220px;
    overflow: auto;
  }

  .compact-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding-top: 12px;
    padding-bottom: 10px;
  }

  .compact-heading span > p {
    margin-top: 5px;
  }

  .compact-state {
    min-height: 62px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    padding: 12px;
    border-top: 1px solid var(--line);
    color: var(--ink-faint);
    font-size: 9px;
    text-align: center;
  }

  .error-text {
    color: var(--rose);
  }

  .memory-item {
    padding: 10px 15px;
    border-top: 1px solid var(--line);
  }

  .memory-item b {
    display: block;
    font-size: 9.5px;
  }

  .memory-item p {
    display: -webkit-box;
    margin: 4px 0;
    overflow: hidden;
    color: var(--ink-muted);
    font-size: 8.5px;
    line-height: 1.35;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  .memory-item small {
    color: var(--ink-faint);
    font-size: 7.5px;
  }

  .honest-state,
  .tutorial-card {
    display: grid;
    grid-template-columns: 45px minmax(0, 1fr);
    gap: 13px;
    align-items: start;
    padding: 16px;
  }

  .notification-controls {
    padding: 15px;
  }

  .notification-controls > label {
    display: grid;
    grid-template-columns: 35px minmax(0, 1fr) auto;
    align-items: center;
    gap: 11px;
    color: var(--ink);
  }

  .notification-controls > label b,
  .notification-controls > label small {
    display: block;
  }

  .notification-controls > label b {
    font-size: 10.5px;
  }

  .notification-controls > label small,
  .notification-controls > p {
    margin-top: 4px;
    color: var(--ink-faint);
    font-size: 8.5px;
    line-height: 1.4;
  }

  .notification-controls > label input {
    accent-color: var(--fawn);
  }

  .notification-controls > button {
    min-height: 30px;
    margin-top: 15px;
    padding: 0 11px;
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--cream-solid) 30%, transparent);
    font-size: 8.5px;
    cursor: pointer;
  }

  .reminder-editor {
    margin-top: 12px;
    overflow: hidden;
  }

  .reminder-editor > .dock-card-heading {
    padding: 13px 15px 10px;
  }

  .reminder-form {
    display: grid;
    gap: 7px;
    padding: 0 15px 14px;
  }

  .reminder-form > label,
  .reminder-schedule span {
    color: var(--ink-faint);
    font-size: 8px;
    font-weight: 650;
  }

  .reminder-form input,
  .reminder-form textarea,
  .reminder-form select,
  .retention-card select {
    width: 100%;
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    color: var(--ink);
    background: color-mix(in srgb, var(--cream-solid) 28%, transparent);
    font: inherit;
    font-size: 9px;
    outline: none;
  }

  .reminder-form input,
  .reminder-form select,
  .retention-card select {
    height: 31px;
    padding: 0 9px;
  }

  .reminder-form textarea {
    min-height: 55px;
    padding: 8px 9px;
    resize: vertical;
  }

  .reminder-schedule {
    display: grid;
    grid-template-columns: 1fr 1.4fr;
    gap: 8px;
  }

  .reminder-schedule label {
    display: grid;
    gap: 5px;
  }

  .reminder-form > button {
    justify-self: end;
  }

  .reminder-list article {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    padding: 10px 15px;
    border-top: 1px solid var(--line);
  }

  .reminder-list article b,
  .reminder-list article small {
    display: block;
  }

  .reminder-list article b {
    font-size: 9.5px;
  }

  .reminder-list article p {
    margin: 3px 0;
    overflow: hidden;
    color: var(--ink-muted);
    font-size: 8px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .reminder-list article small {
    color: var(--ink-faint);
    font-size: 7.5px;
  }

  .reminder-list article button {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 6px 8px;
    border: 1px solid color-mix(in srgb, var(--rose) 38%, var(--line));
    border-radius: 8px;
    color: var(--rose);
    background: transparent;
    font-size: 8px;
    cursor: pointer;
  }

  .retention-card select {
    margin-top: 7px;
  }

  .honest-state p,
  .tutorial-card p,
  .mcp-card > p,
  .release-card p {
    margin: 5px 0 0;
    color: var(--ink-faint);
    font-size: 9px;
    line-height: 1.5;
  }

  .tutorial-card button {
    margin-top: 12px;
  }

  .mcp-card {
    padding: 15px;
  }

  .service-status {
    display: flex;
    align-items: center;
    gap: 9px;
  }

  .service-status > i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ink-faint);
  }

  .service-status > i.ready {
    background: #4fd77a;
    box-shadow: 0 0 8px rgba(79, 215, 122, 0.24);
  }

  .mcp-card pre {
    margin: 13px 0 10px;
    padding: 10px 12px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 9px;
    color: var(--ink-muted);
    background: rgba(0, 0, 0, 0.14);
    font: 8.5px/1.45 ui-monospace, "SFMono-Regular", monospace;
    user-select: text;
  }

  .release-card {
    padding: 16px;
  }

  .release-card > b,
  .release-card > small {
    display: block;
  }

  .release-card > b {
    color: var(--fawn-bright);
    font-size: 13px;
  }

  .release-card > small {
    margin-top: 3px;
    color: var(--ink-faint);
    font-size: 8.5px;
  }

  .release-card ul {
    margin: 14px 0 10px;
    padding-left: 18px;
    color: var(--ink-muted);
    font-size: 9px;
    line-height: 1.7;
  }

  .clear-key {
    width: fit-content;
    margin-top: 9px;
    color: var(--rose) !important;
    background: transparent !important;
  }

</style>
