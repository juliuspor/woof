/** The single woof-native command and event contract used by every UI surface. */
export const COMMANDS = {
  // Application and onboarding.
  skipOnboarding: "skip_onboarding_cmd",
  finishOnboarding: "finish_onboarding",
  openOnboarding: "open_onboarding_window_cmd",
  memoryHubOpenRoute: "memory_hub_open_route",
  saveContactInfo: "save_contact_info",
  loadContactInfo: "load_contact_info",

  // Permissions.
  accessibilityStatus: "accessibility_status",
  accessibilityTrusted: "accessibility_trusted",
  requestAccessibility: "request_accessibility",
  microphoneStatus: "microphone_status",
  inputMonitoringTrusted: "input_monitoring_trusted",
  requestInputMonitoring: "request_input_monitoring",
  openInputMonitoringSettings: "open_input_monitoring_settings",
  openAccessibilitySettings: "open_accessibility_settings",

  // Companion and chat panels.
  companionGetPosition: "companion_chat_get_position",
  companionSetPosition: "companion_chat_set_position",
  companionSetState: "companion_chat_set_state",
  companionGetHoverOpen: "companion_chat_get_hover_open",
  companionSetHoverOpen: "companion_chat_set_hover_open",
  companionGetCollapsedAutoHide: "companion_chat_get_collapsed_auto_hide",
  companionSetCollapsedAutoHide: "companion_chat_set_collapsed_auto_hide",
  companionOpenFocused: "companion_chat_open_focused",
  companionPointerReady: "companion_chat_pointer_ready",
  companionRollup: "companion_chat_rollup",
  companionDragStart: "companion_chat_drag_start",
  companionDragFrame: "companion_chat_drag_frame",
  companionDragEnd: "companion_chat_drag_end",
  companionSetNudgeActive: "companion_chat_set_nudge_card_active",
  companionOpenNudge: "companion_open_nudge",
  companionDismissNudge: "companion_dismiss_nudge",
  companionSetNotificationActive: "companion_chat_set_notification_active",
  chatSend: "chat_send",
  chatCancel: "chat_cancel",
  generateChatSuggestions: "generate_chat_suggestions",

  // Caret, rewrite, and dictation.
  caretReady: "caret_overlay_ready",
  caretCancel: "caret_overlay_cancel",
  editReady: "edit_mode_ready",
  editClose: "edit_mode_close",
  editSubmit: "edit_mode_submit",
  editSetContentHeight: "edit_mode_set_content_height",
  editSetGlass: "edit_mode_set_glass_appearance",
  transcriptionStart: "transcription_start",
  transcriptionFinalize: "transcription_finalize",
  transcriptionCancel: "transcription_cancel",

  // Memory and preferences.
  memoryWikiList: "memory_wiki_list",
  memoryWikiPage: "memory_wiki_page",
  memoryWikiSearch: "memory_wiki_search",
  memoryFollowups: "memory_followups",
  memoryFollowupSetStatus: "memory_followup_set_status",
  memoryWorkPatterns: "memory_work_patterns",
  memoryWorkPatternSetStatus: "memory_work_pattern_set_status",
  memoryRecentActivity: "memory_recent_activity",
  memoryWorkingMemory: "memory_working_memory",
  memoryTimeReport: "memory_time_report",
  memoryTimeRules: "memory_time_rules",
  memoryIdentitySave: "memory_identity_save",
  captureIsPaused: "capture_is_paused",
  captureStatus: "capture_status",
  capturePause: "capture_pause",
  captureResume: "capture_resume",
  getCaptureBlacklist: "get_capture_blacklist",
  setCaptureBlacklist: "set_capture_blacklist",
  memoryDeleteAll: "memory_delete_all",
  getReduceVisualEffects: "get_reduce_visual_effects",
  setReduceVisualEffects: "set_reduce_visual_effects",
  getCaretSoundsEnabled: "get_caret_sounds_enabled",
  setCaretSoundsEnabled: "set_caret_sounds_enabled",
  getVoiceDictationEnabled: "get_voice_dictation_enabled",
  setVoiceDictationEnabled: "set_voice_dictation_enabled",
  getTranscriptionModifierKey: "get_transcription_modifier_key",
  setTranscriptionModifierKey: "set_transcription_modifier_key",
  recordModifierKey: "record_modifier_key",
  getDefaultWoofModifierKey: "get_default_woof_modifier_key",
  getWoofModifierKey: "get_woof_modifier_key",
  setWoofModifierKey: "set_woof_modifier_key",
  setModifierKeys: "set_modifier_keys",
  getWoofModifierEnabled: "get_woof_modifier_enabled",
  setWoofModifierEnabled: "set_woof_modifier_enabled",
  getSecondaryShortcut: "get_secondary_shortcut",
  setSecondaryShortcut: "set_secondary_shortcut",
  getSecondaryShortcutEnabled: "get_secondary_shortcut_enabled",
  getSecondaryShortcutError: "get_secondary_shortcut_error",
  setSecondaryShortcutEnabled: "set_secondary_shortcut_enabled",
  recordSecondaryShortcut: "record_secondary_shortcut",
  getApiKeyStatus: "get_api_key_status",
  setOpenAiApiKey: "set_openai_api_key",
  clearOpenAiApiKey: "clear_openai_api_key",
  setLoginItemEnabled: "set_login_item_enabled",
  getLoginItemEnabled: "get_login_item_enabled",
  daemonHealth: "daemon_health",
  mcpClientConfiguration: "mcp_client_configuration",
  notificationOpenSettings: "notification_open_settings",
  getNudgesEnabled: "get_nudges_enabled",
  setNudgesEnabled: "set_nudges_enabled",
  scheduledReminderList: "scheduled_reminder_list",
  scheduledReminderCreate: "scheduled_reminder_create",
  scheduledReminderDelete: "scheduled_reminder_delete",
  getDataRetention: "get_data_retention",
  setDataRetention: "set_data_retention"
} as const;

export type CommandName = (typeof COMMANDS)[keyof typeof COMMANDS];

export type ModifierKey =
  | "fn"
  | "left_option"
  | "right_option"
  | "left_command"
  | "right_command"
  | "left_shift"
  | "right_shift"
  | "left_control"
  | "right_control";

export interface ShortcutChord {
  meta: boolean;
  shift: boolean;
  alt: boolean;
  control: boolean;
  key: string;
}

export const EVENTS = {
  onboardingComplete: "woof:onboarding-complete",
  companionCollapsedUnlock: "woof:companion-collapsed-unlock",
  companionFlyOut: "woof:companion-fly-out",
  caretInit: "woof:caret-init",
  caretStatus: "woof:caret-status",
  caretFadeout: "woof:caret-fadeout",
  editInit: "woof:edit-init",
  editState: "woof:edit-state",
  editFadeout: "woof:edit-fadeout",
  editContext: "woof:edit-context",
  inlineRefused: "woof:inline-refused",
  willRetract: "woof:will-retract",
  chatState: "woof:chat-state",
  chatDelta: "woof:chat-delta",
  chatComplete: "woof:chat-complete",
  companionPointer: "woof:companion-pointer",
  panelPosition: "woof:panel-position",
  positionDrag: "woof:position-drag",
  openChat: "woof:open-chat",
  openSettings: "woof:open-settings",
  nudgeReady: "woof:nudge-ready",
  notificationStatus: "woof:notification-status",
  transcriptionStart: "woof:transcription-start",
  transcriptionLevel: "woof:transcription-level",
  transcriptionPartial: "woof:transcription-partial",
  transcriptionItemCompleted: "woof:transcription-item-completed",
  transcriptionProcessing: "woof:transcription-processing",
  transcriptionCompleted: "woof:transcription-completed",
  transcriptionDone: "woof:transcription-done",
  transcriptionCancelled: "woof:transcription-cancelled",
  transcriptionFailed: "woof:transcription-failed",
  transcriptionOverflow: "woof:transcription-overflow",
  transcriptionLimit: "woof:transcription-limit",
  healthChanged: "woof:health-changed",
  databaseReset: "woof:database-reset",
  capturePaused: "woof:capture-paused",
  captureChanged: "woof:capture-changed",
  memoryHubRefreshRequested: "woof:memory-hub-refresh-requested",
  memoryHubNavigate: "woof:memory-hub-navigate",
  preferencesChanged: "woof:preferences-changed",
  permissionsChanged: "woof:permissions-changed"
} as const;

export type EventName = (typeof EVENTS)[keyof typeof EVENTS];

export type CompanionMode = "hidden" | "collapsed" | "expanded";
export type NativeChatState =
  | CompanionMode
  | { state: CompanionMode; requestId?: number };
export type MemoryHubRoute = "followups" | "workflows";

export interface MemoryHubNavigatePayload {
  route: MemoryHubRoute;
}
export type DockPosition =
  | "top"
  | "left"
  | "right"
  | "bottom"
  | "bottom-left"
  | "bottom-right";
export type CaptureState = "active" | "paused" | "starting" | "permission-revoked" | "error";
export type HealthState = "healthy" | "starting" | "degraded" | "offline";

export interface HealthChangedPayload {
  state: HealthState;
}

export interface PreferencesChangedPayload {
  reduceVisualEffects?: boolean;
  caretSoundsEnabled?: boolean;
  companionHoverOpen?: boolean;
  collapsedAutoHide?: boolean;
}

export interface AccessibilityStatus {
  app_trusted: boolean;
  capture_service_trusted: boolean;
  capture_service_operational: boolean;
  ready: boolean;
  next_request: "app" | "capture-service" | null;
}

export interface OpenChatPayload {
  attachment?: string | null;
  prefill?: string | null;
  auto_send?: boolean;
  source?: string;
}

export interface CaretInitPayload {
  session_id: number;
  status: string;
}

export interface CaretStatusPayload {
  session_id: number;
  text: string;
}

export interface EditInitPayload {
  glass?: boolean;
}

export interface EditStatePayload {
  state: string;
  error?: string | null;
}

export interface TranscriptionStartPayload {
  hands_free: boolean;
}

export interface TranscriptionLevelPayload {
  level: number;
}

export interface TranscriptionItemPayload {
  item_id: string;
  text: string;
}

export interface TranscriptionProcessingPayload {
  timeout_ms?: number;
}

export function companionModeFromState(state: NativeChatState): CompanionMode {
  return typeof state === "string" ? state : state.state;
}

export function transcriptionLevelFromPayload(
  payload: TranscriptionLevelPayload
): number {
  const level = payload.level;
  return Number.isFinite(level) ? Math.min(1, Math.max(0, level)) : 0;
}

export function transcriptionItemFromPayload(
  payload: TranscriptionItemPayload
): TranscriptionItemPayload | null {
  if (
    !payload ||
    typeof payload.item_id !== "string" ||
    payload.item_id.length === 0 ||
    typeof payload.text !== "string"
  ) {
    return null;
  }
  return payload;
}

export interface PositionDragPayload {
  active: boolean;
  nearest?: DockPosition;
}

export interface ChatRequest {
  text: string;
  threadId: string;
  history: ChatHistoryMessage[];
  focusedSnapshotIds?: string[];
  mode?: "chat" | "rewrite";
}

export interface ChatHistoryMessage {
  role: "user" | "assistant";
  content: string;
}

export interface ChatChunk {
  delta?: string;
  finishReason?: string;
  toolName?: string;
}

export interface WikiSummary {
  slug: string | null;
  page_type: "person" | "project" | "topic" | "tool" | "org";
  title: string;
  summary: string;
  mention_count: number;
  last_seen: number;
}

export interface WikiPage extends WikiSummary {
  aliases: string;
  body: string;
  links: string;
  snapshot_ids: string;
  first_seen: number;
  is_dirty: number;
  updated_at: number;
  model_used: string | null;
}

export interface RecentActivityItem {
  event_id: number;
  snapshot_id: string;
  app: string;
  window_title: string;
  url: string | null;
  domain: string | null;
  started_at: number;
  last_seen_at: number;
  duration_s: number;
  content_excerpt: string;
  focused_name: string | null;
  focused_role: string | null;
  focused_path: string | null;
}

export interface MemorySnapshot {
  snapshot_id: string;
  content: string;
  app: string;
  window_title: string;
  url: string | null;
  domain: string | null;
  captured_at: number;
  last_seen_at: number;
  duration_s: number;
  sighting_count: number;
  focused_name: string | null;
  focused_role: string | null;
  focused_path: string | null;
}

export interface WorkingMemoryItem extends MemorySnapshot {
  wm_id: number;
  added_at: number;
  relevance: number;
}

export interface FollowupItem {
  flag_id: number;
  kind: "followup" | "commitment" | "question";
  text: string;
  snapshot_id: string | null;
  period_key: string;
  status: string;
  created_at: number;
}

export interface WorkflowObservation {
  snapshot_id: string;
  app: string;
  domain: string | null;
  window_title: string;
  started_at: number;
  last_seen_at: number;
  duration_s: number;
}

export interface WorkflowSummary {
  workflow_id: string | null;
  name: string;
  excerpt: string;
  apps: string[];
  frequency_label: string;
  observations: WorkflowObservation[];
  status: string;
  confidence: number;
  first_detected_at: number;
  last_detected_at: number;
}

export interface WorkPatternStatus {
  total: number;
  by_status: Record<string, number>;
  recent: WorkflowSummary[];
}

export type ScheduledReminderKind = "once" | "daily";

export type ScheduledReminderDraft =
  | {
      label: string;
      prompt: string;
      scheduleKind: "once";
      fireAt: number;
    }
  | {
      label: string;
      prompt: string;
      scheduleKind: "daily";
      hour: number;
      minute: number;
    };

export interface ScheduledReminder {
  rule_id: string;
  label: string;
  prompt: string;
  schedule_kind: ScheduledReminderKind;
  days_of_week: number[];
  hour: number;
  minute: number;
  interval_minutes: 0;
  timezone: "local";
  enabled: boolean;
  created_at: number;
  updated_at: number;
  last_fired_at: number | null;
  fire_at: number | null;
}

export type DataRetentionPolicy =
  | { mode: "keep_forever" }
  | { mode: "days"; days: number };

export interface CaptureStatus {
  paused: boolean;
  capturing: boolean;
  database_recovery?: {
    occurred: true;
    reason: DatabaseRecoveryReason;
  } | null;
  runtime: {
    running?: boolean;
    last_capture_at?: number | null;
    last_error?: string | null;
    [key: string]: unknown;
  };
}

export type DatabaseRecoveryReason =
  | "corrupt"
  | "incompatible-schema"
  | "unsupported-version";

export interface DatabaseRecoveryPayload {
  reason: DatabaseRecoveryReason;
}

export const CAPTURE_BLACKLIST_KINDS = [
  "bundle_id",
  "bundle_prefix",
  "app_name",
  "window_title",
  "browser_host",
  "regex"
] as const;

export type CaptureBlacklistKind = (typeof CAPTURE_BLACKLIST_KINDS)[number];

export interface CaptureBlacklistEntry {
  kind: CaptureBlacklistKind;
  pattern: string;
}

export interface CaptureBlacklistResponse {
  blacklist: CaptureBlacklistEntry[];
}

export interface TimeSegment {
  app: string;
  domain: string;
  title: string;
  seconds: number;
}

export interface TimeProject {
  project: string;
  seconds: number;
  by_day: Record<string, number>;
  top_segments: TimeSegment[];
}

export interface TimeReport {
  from: number;
  to: number;
  total_seconds: number;
  projects: TimeProject[];
}

export interface TimeRule {
  rule_id: number;
  project: string;
  app: string | null;
  domain: string | null;
  title_contains: string | null;
  source: string;
  created_at: number;
}
