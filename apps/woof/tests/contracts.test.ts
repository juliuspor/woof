import { beforeEach, describe, expect, it } from "vitest";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { CURVES, GLASS, MOTION, SCREENSHOT_VIEWPORTS, WINDOWS } from "../src/lib/contracts/geometry";
import {
  CAPTURE_BLACKLIST_KINDS,
  COMMANDS,
  EVENTS,
  companionModeFromState,
  transcriptionItemFromPayload,
  transcriptionLevelFromPayload,
  type CaptureBlacklistResponse,
  type DockPosition,
  type ModifierKey,
  type ShortcutChord
} from "../src/lib/contracts/ipc";
import { invokeCommand, listenEvent } from "../src/lib/contracts/bridge";

describe("woof desktop contract", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it("keeps every command and event unique", () => {
    const commands = Object.values(COMMANDS);
    const events = Object.values(EVENTS);
    expect(new Set(commands).size).toBe(commands.length);
    expect(new Set(events).size).toBe(events.length);
  });

  it("uses one native event payload shape", () => {
    expect(companionModeFromState("hidden")).toBe("hidden");
    expect(companionModeFromState("collapsed")).toBe("collapsed");
    expect(companionModeFromState("expanded")).toBe("expanded");
    expect(transcriptionLevelFromPayload({ level: 1.7 })).toBe(1);
    expect(transcriptionLevelFromPayload({ level: -0.4 })).toBe(0);
    expect(transcriptionItemFromPayload({ item_id: "item-1", text: "hello" })).toEqual({
      item_id: "item-1",
      text: "hello"
    });
    expect(transcriptionItemFromPayload({ item_id: "", text: "hello" })).toBeNull();

    expect({
      capturePaused: EVENTS.capturePaused,
      caret: [EVENTS.caretInit, EVENTS.caretStatus, EVENTS.caretFadeout],
      edit: [EVENTS.editInit, EVENTS.editState, EVENTS.editFadeout],
      transcriptionCancel: COMMANDS.transcriptionCancel,
      editCommands: [
        COMMANDS.editReady,
        COMMANDS.editClose,
        COMMANDS.editSetContentHeight,
        COMMANDS.editSetGlass
      ]
    }).toEqual({
      capturePaused: "woof:capture-paused",
      caret: ["woof:caret-init", "woof:caret-status", "woof:caret-fadeout"],
      edit: ["woof:edit-init", "woof:edit-state", "woof:edit-fadeout"],
      transcriptionCancel: "transcription_cancel",
      editCommands: [
        "edit_mode_ready",
        "edit_mode_close",
        "edit_mode_set_content_height",
        "edit_mode_set_glass_appearance"
      ]
    });
  });

  it("pins the native non-docking commands", async () => {
    const commands = {
      memoryIdentitySave: COMMANDS.memoryIdentitySave,
      getHoverOpen: COMMANDS.companionGetHoverOpen,
      setHoverOpen: COMMANDS.companionSetHoverOpen,
      setCollapsedAutoHide: COMMANDS.companionSetCollapsedAutoHide
    };
    expect(commands).toEqual({
      memoryIdentitySave: "memory_identity_save",
      getHoverOpen: "companion_chat_get_hover_open",
      setHoverOpen: "companion_chat_set_hover_open",
      setCollapsedAutoHide: "companion_chat_set_collapsed_auto_hide"
    });

    const nativeSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/lib.rs"),
      "utf8"
    );
    for (const command of Object.values(commands)) {
      expect(nativeSource).toContain(`commands::${command}`);
    }

    await invokeCommand(COMMANDS.companionSetHoverOpen, { enabled: true });
    expect(await invokeCommand(COMMANDS.companionGetHoverOpen)).toBe(true);
    await expect(
      invokeCommand(COMMANDS.companionSetHoverOpen, { open: false })
    ).rejects.toThrow("enabled is required");

    await invokeCommand(COMMANDS.companionSetCollapsedAutoHide, { enabled: true });
    expect(await invokeCommand(COMMANDS.companionGetCollapsedAutoHide)).toBe(true);

    expect(
      await invokeCommand(COMMANDS.memoryIdentitySave, { name: "Grace Hopper" })
    ).toEqual({ identity: { name: "Grace Hopper" } });
    await expect(
      invokeCommand(COMMANDS.memoryIdentitySave, { text: "Name: Ada Lovelace" })
    ).rejects.toThrow("name is required");
  });

  it("pins the six-position dock and drag event contract", async () => {
    const positions: DockPosition[] = [
      "top",
      "left",
      "right",
      "bottom",
      "bottom-left",
      "bottom-right"
    ];
    expect(EVENTS.panelPosition).toBe("woof:panel-position");
    expect(EVENTS.positionDrag).toBe("woof:position-drag");
    expect(await invokeCommand<DockPosition>(COMMANDS.companionGetPosition)).toBe("top");

    const published: DockPosition[] = [];
    const drags: Array<{ active: boolean; nearest?: DockPosition }> = [];
    const onPosition = (event: Event) =>
      published.push((event as CustomEvent<DockPosition>).detail);
    const onDrag = (event: Event) =>
      drags.push((event as CustomEvent<{ active: boolean; nearest?: DockPosition }>).detail);
    window.addEventListener(EVENTS.panelPosition, onPosition);
    window.addEventListener(EVENTS.positionDrag, onDrag);

    for (const position of positions) {
      await invokeCommand(COMMANDS.companionSetPosition, { position });
      expect(await invokeCommand<DockPosition>(COMMANDS.companionGetPosition)).toBe(position);
    }
    expect(published).toEqual(positions);

    expect(await invokeCommand<boolean>(COMMANDS.companionDragStart)).toBe(true);
    await invokeCommand(COMMANDS.companionDragFrame, {
      x: 40,
      yFromTop: 60,
      w: 150,
      h: 34
    });
    expect(
      await invokeCommand<DockPosition>(COMMANDS.companionDragEnd, { position: null })
    ).toBe("bottom-right");
    expect(drags).toEqual([
      { active: true },
      { active: false, nearest: "bottom-right" }
    ]);

    window.removeEventListener(EVENTS.panelPosition, onPosition);
    window.removeEventListener(EVENTS.positionDrag, onDrag);

    const nativeSource = readFileSync(resolve(process.cwd(), "src-tauri/src/commands.rs"), "utf8");
    expect(nativeSource).toContain("y_from_top: f64");
    expect(nativeSource).toContain('"woof:position-drag"');
    expect(nativeSource).toContain('"woof:panel-position"');
  });

  it("isolates the woof identity and daemon behind native IPC", () => {
    expect(Object.values(EVENTS).every((event) => event.startsWith("woof:"))).toBe(true);
    const configuration = JSON.parse(
      readFileSync(resolve(process.cwd(), "src-tauri/tauri.conf.json"), "utf8")
    ) as { app: { security: { csp: string } } };
    expect(configuration.app.security.csp).not.toContain("127.0.0.1:3334");
    const bridge = readFileSync(
      resolve(process.cwd(), "src/lib/contracts/bridge.ts"),
      "utf8"
    );
    expect(bridge).not.toContain("localStorage");
    expect(bridge).not.toContain("browserFallback");
  });

  it("refuses commands and events when the native runtime is unavailable", async () => {
    const nativeFlag = Object.getOwnPropertyDescriptor(globalThis, "isTauri");
    const nativeInternals = Object.getOwnPropertyDescriptor(
      globalThis,
      "__TAURI_INTERNALS__"
    );

    try {
      Object.defineProperty(globalThis, "isTauri", {
        configurable: true,
        value: false
      });
      await expect(invokeCommand(COMMANDS.daemonHealth)).rejects.toThrow(
        "the native woof bridge is unavailable"
      );
      await expect(listenEvent(EVENTS.healthChanged, () => undefined)).rejects.toThrow(
        "the native woof bridge is unavailable"
      );

      Object.defineProperty(globalThis, "isTauri", {
        configurable: true,
        value: true
      });
      Reflect.deleteProperty(globalThis, "__TAURI_INTERNALS__");
      await expect(invokeCommand(COMMANDS.daemonHealth)).rejects.toThrow(
        "the native woof bridge is unavailable"
      );
    } finally {
      if (nativeFlag) Object.defineProperty(globalThis, "isTauri", nativeFlag);
      else Reflect.deleteProperty(globalThis, "isTauri");
      if (nativeInternals) {
        Object.defineProperty(globalThis, "__TAURI_INTERNALS__", nativeInternals);
      } else {
        Reflect.deleteProperty(globalThis, "__TAURI_INTERNALS__");
      }
    }
  });

  it("never persists an OpenAI API key in frontend storage", async () => {
    const apiKey = "sk-proj-frontend-storage-regression-secret";
    await invokeCommand(COMMANDS.setOpenAiApiKey, { apiKey });

    const persisted = Array.from({ length: window.localStorage.length }, (_, index) => {
      const key = window.localStorage.key(index) ?? "";
      return `${key}\n${window.localStorage.getItem(key) ?? ""}`;
    }).join("\n");
    expect(persisted).not.toContain(apiKey);

    const settingsSource = readFileSync(
      resolve(process.cwd(), "src/lib/components/SettingsPanel.svelte"),
      "utf8"
    );
    expect(settingsSource).not.toMatch(/localStorage|sessionStorage|indexedDB/);
  });

  it("scopes native commands per webview and denies sensitive cross-window actions", () => {
    const capabilityFiles = {
      companion: "companion.json",
      memoryHub: "memory-hub.json",
      onboarding: "onboarding.json",
      permission: "permission.json",
      caretOverlay: "caret-overlay.json",
      editOverlay: "edit-overlay.json",
      health: "health.json"
    } as const;
    const capabilities = Object.fromEntries(
      Object.entries(capabilityFiles).map(([name, file]) => [
        name,
        JSON.parse(
          readFileSync(resolve(process.cwd(), "src-tauri/capabilities", file), "utf8")
        ) as { windows: string[]; permissions: string[] }
      ])
    ) as Record<keyof typeof capabilityFiles, { windows: string[]; permissions: string[] }>;
    const permissionSource = readFileSync(
      resolve(process.cwd(), "src-tauri/permissions/window-commands.toml"),
      "utf8"
    );
    const commandsForPermission = (permission: string): string[] => {
      const marker = `identifier = "${permission}"`;
      const start = permissionSource.indexOf(marker);
      expect(start).toBeGreaterThanOrEqual(0);
      const next = permissionSource.indexOf("[[permission]]", start);
      const block = permissionSource.slice(start, next < 0 ? undefined : next);
      const commandList = block.match(/commands\.allow = \[([\s\S]*?)\]/)?.[1] ?? "";
      return Array.from(commandList.matchAll(/"([^"]+)"/g), (match) => match[1]);
    };
    const commandsForCapability = (name: keyof typeof capabilityFiles): string[] =>
      capabilities[name].permissions
        .filter((permission) => !permission.startsWith("core:"))
        .flatMap(commandsForPermission);

    expect(Object.values(capabilities).map(({ windows }) => windows)).toEqual([
      ["companion-chat"],
      ["memory-hub"],
      ["onboarding"],
      ["permission"],
      ["caret-overlay"],
      ["edit-mode"],
      ["health"]
    ]);
    for (const capability of Object.values(capabilities)) {
      expect(capability.permissions).toEqual(
        expect.arrayContaining([
          "core:event:allow-listen",
          "core:event:allow-unlisten"
        ])
      );
      expect(capability.permissions).not.toContain("core:default");
    }

    const alwaysDeniedOutsideProductWindows = [
      "memory_delete_all",
      "capture_pause",
      "capture_resume",
      "set_openai_api_key",
      "clear_openai_api_key",
      "transcription_start"
    ];
    for (const name of ["permission", "caretOverlay", "editOverlay", "health"] as const) {
      const commands = commandsForCapability(name);
      for (const command of alwaysDeniedOutsideProductWindows) {
        expect(commands).not.toContain(command);
      }
    }
    const onboardingCommands = commandsForCapability("onboarding");
    for (const command of [
      "memory_delete_all",
      "capture_pause",
      "capture_resume",
      "clear_openai_api_key",
      "transcription_start"
    ]) {
      expect(onboardingCommands).not.toContain(command);
    }
    expect(onboardingCommands).toContain("set_openai_api_key");
    expect(commandsForCapability("companion")).toContain("transcription_start");
    expect(commandsForCapability("memoryHub")).toContain("memory_delete_all");
  });

  it("validates the edit controller before rewrite work and targets chat events", () => {
    const nativeSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/commands.rs"),
      "utf8"
    );
    const editStart = nativeSource.indexOf("pub async fn edit_mode_submit(");
    const editInner = nativeSource.indexOf("async fn edit_mode_submit_inner(", editStart);
    const editOuter = nativeSource.slice(editStart, editInner);
    const expectBefore = (source: string, first: string, second: string): void => {
      const firstIndex = source.indexOf(first);
      const secondIndex = source.indexOf(second);
      expect(firstIndex).toBeGreaterThanOrEqual(0);
      expect(secondIndex).toBeGreaterThanOrEqual(0);
      expect(firstIndex).toBeLessThan(secondIndex);
    };
    expectBefore(
      editOuter,
      "verify_edit_delivery_controller(&window)?;",
      "edit_mode_submit_inner("
    );
    expect(editOuter).toContain('.emit("woof:edit-state"');

    const chatStart = nativeSource.indexOf("pub async fn chat_send(");
    const chatEnd = nativeSource.indexOf("pub fn chat_cancel(", chatStart);
    const chat = nativeSource.slice(chatStart, chatEnd);
    expect(chat).toContain("webview(&event_app, COMPANION_WINDOW_LABEL)");
    expect(chat).toContain("webview(&app, COMPANION_WINDOW_LABEL)?");
    expect(chat).not.toContain('app.emit("woof:chat-');

    const supervisorSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/supervisor.rs"),
      "utf8"
    );
    expect(supervisorSource).toContain("state: health_state");
    expect(supervisorSource).toContain(
      '[companion_panel::WINDOW_LABEL, "memory-hub", "health"]'
    );
    expect(supervisorSource).toContain(
      '[companion_panel::WINDOW_LABEL, "memory-hub"]'
    );
    expect(supervisorSource).toContain('window.emit("woof:health-changed", payload)');
    expect(supervisorSource).toContain('window.emit("woof:capture-changed", payload)');
    expect(supervisorSource).toContain('show_focused_window(app, "permission")');
    expect(supervisorSource).toContain('hide_window(app, "permission")');
    expect(supervisorSource).toContain('show_focused_window(app, "health")');
    expect(supervisorSource).toContain('hide_window(app, "health")');
    expect(supervisorSource).not.toContain('app.emit("woof:health-changed"');

    const pauseStart = nativeSource.indexOf("pub async fn capture_pause(");
    const pauseEnd = nativeSource.indexOf("pub async fn capture_resume(", pauseStart);
    const pause = nativeSource.slice(pauseStart, pauseEnd);
    expectBefore(
      pause,
      "preferences.capture_paused = true",
      'request_capture_transition(&app, "/capture/pause", true)'
    );
    const skipStart = nativeSource.indexOf("pub async fn skip_onboarding_cmd(");
    const skipEnd = nativeSource.indexOf("pub async fn finish_onboarding(", skipStart);
    const skip = nativeSource.slice(skipStart, skipEnd);
    expectBefore(
      skip,
      "preferences.capture_paused = true",
      'request_capture_transition(&app, "/capture/pause", true)'
    );
    expect(skip).toContain("sync_capture_tray_label(&app, paused)");
    expect(pause).toContain('request_capture_transition(&app, "/capture/pause", true)');
    const resumeEnd = nativeSource.indexOf("pub fn get_reduce_visual_effects(", pauseEnd);
    const resume = nativeSource.slice(pauseEnd, resumeEnd);
    expect(resume).toContain('request_capture_transition(&app, "/capture/resume", false)');
    const statusStart = nativeSource.indexOf("pub async fn capture_is_paused(");
    const statusEnd = nativeSource.indexOf("fn live_capture_paused(", statusStart);
    expect(nativeSource.slice(statusStart, statusEnd)).toContain(
      "sync_capture_tray_label(&app, paused)"
    );
    const synchronizeStart = nativeSource.indexOf(
      "pub(crate) async fn synchronize_persisted_capture_pause("
    );
    const synchronizeEnd = nativeSource.indexOf(
      "fn persisted_capture_path(",
      synchronizeStart
    );
    expect(nativeSource.slice(synchronizeStart, synchronizeEnd)).toContain(
      "request_capture_transition(app, path, paused)"
    );
    const transitionStart = nativeSource.indexOf("async fn request_capture_transition(");
    const transitionEnd = nativeSource.indexOf(
      "pub(crate) async fn synchronize_persisted_capture_pause(",
      transitionStart
    );
    const transition = nativeSource.slice(transitionStart, transitionEnd);
    expect(transition).toContain("sync_capture_tray_label(app, paused)");
    expect(transition).toContain('daemon_request(Method::GET, "/capture/status", None)');
    expect(transition).toContain("sync_capture_tray_label(app, observed)");
    const traySource = readFileSync(resolve(process.cwd(), "src-tauri/src/lib.rs"), "utf8");
    expect(traySource).toContain("struct CaptureTrayMenuItem(MenuItem<tauri::Wry>);");
    expect(traySource).toContain("app.manage(CaptureTrayMenuItem(pause.clone()))");
  });

  it("exposes typed privacy contracts through the native bridge", async () => {
    expect(COMMANDS.getCaptureBlacklist).toBe("get_capture_blacklist");
    expect(COMMANDS.setCaptureBlacklist).toBe("set_capture_blacklist");
    expect(CAPTURE_BLACKLIST_KINDS).toEqual([
      "bundle_id",
      "bundle_prefix",
      "app_name",
      "window_title",
      "browser_host",
      "regex"
    ]);

    const saved = await invokeCommand<CaptureBlacklistResponse>(
      COMMANDS.setCaptureBlacklist,
      { blacklist: [{ kind: "browser_host", pattern: "private.example.com" }] }
    );
    const listed = await invokeCommand<CaptureBlacklistResponse>(
      COMMANDS.getCaptureBlacklist
    );
    expect(saved).toEqual(listed);

    expect(await invokeCommand(COMMANDS.getDataRetention)).toEqual({
      retention: { mode: "keep_forever" }
    });
    expect(
      await invokeCommand(COMMANDS.setDataRetention, {
        retention: { mode: "days", days: 90 }
      })
    ).toMatchObject({ retention: { mode: "days", days: 90 } });
    await expect(
      invokeCommand(COMMANDS.setDataRetention, {
        retention: { mode: "days", days: 0 }
      })
    ).rejects.toThrow(/between 1 and 3650 days/i);

    const nativeSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/lib.rs"),
      "utf8"
    );
    for (const command of [COMMANDS.getDataRetention, COMMANDS.setDataRetention]) {
      expect(nativeSource).toContain(`commands::${command}`);
    }
  });

  it("keeps Accessibility consent bound to both native process identities", () => {
    const nativeSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/commands.rs"),
      "utf8"
    );
    const statusStart = nativeSource.indexOf("pub async fn accessibility_trusted(");
    const requestStart = nativeSource.indexOf("pub async fn request_accessibility(", statusStart);
    const settingsStart = nativeSource.indexOf("pub fn open_accessibility_settings(", requestStart);
    const status = nativeSource.slice(statusStart, requestStart);
    const request = nativeSource.slice(requestStart, settingsStart);
    expect(status).toContain("is_accessibility_trusted()");
    expect(status).toContain('daemon_request(Method::GET, "/capture/accessibility", None)');
    expect(status).toContain("accessibility_clients_ready(local_trusted, &status)");
    expect(request).toContain("request_local_accessibility()");
    expect(request).toContain(
      'daemon_request(Method::POST, "/capture/accessibility/request", None)'
    );

    const finishStart = nativeSource.indexOf("pub async fn finish_onboarding(");
    const finishEnd = nativeSource.indexOf("fn mark_onboarding_finished(", finishStart);
    const finish = nativeSource.slice(finishStart, finishEnd);
    expect(finish).toContain(
      "accessibility_clients_ready(is_accessibility_trusted(), &accessibility)"
    );
    expect(finish).toContain("onboarding_resume_ready(is_accessibility_trusted(), &status)");
  });

  it("validates one canonical user-driven reminder contract", async () => {
    const future = Math.floor(Date.now() / 1_000) + 3_600;
    const response = await invokeCommand<{ rule: { rule_id: string } }>(
      COMMANDS.scheduledReminderCreate,
      {
        reminder: {
          label: "Review",
          prompt: "Review the decision.",
          scheduleKind: "once",
          fireAt: future
        }
      }
    );
    expect(response.rule.rule_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(await invokeCommand(COMMANDS.scheduledReminderList)).toMatchObject({
      rules: [expect.objectContaining({ schedule_kind: "once", days_of_week: [] })]
    });
    expect(
      await invokeCommand(COMMANDS.scheduledReminderDelete, {
        ruleId: response.rule.rule_id
      })
    ).toEqual({ deleted: true });
    await expect(
      invokeCommand(COMMANDS.scheduledReminderCreate, {
        reminder: {
          label: "Review",
          prompt: "Review the decision.",
          scheduleKind: "weekly",
          hour: 10,
          minute: 30,
          fireAt: null
        }
      })
    ).rejects.toThrow(/invalid reminder schedule/i);

    const nativeSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/lib.rs"),
      "utf8"
    );
    for (const command of [
      COMMANDS.scheduledReminderList,
      COMMANDS.scheduledReminderCreate,
      COMMANDS.scheduledReminderDelete
    ]) {
      expect(nativeSource).toContain(`commands::${command}`);
    }
  });

  it("restricts process-created files before native startup", () => {
    const mainSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/main.rs"),
      "utf8"
    );
    expect(mainSource).toContain("umask(0o077)");
    expect(mainSource.indexOf("restrict_process_file_creation();")).toBeLessThan(
      mainSource.indexOf("woof_lib::run();")
    );

    const cargoManifest = readFileSync(
      resolve(process.cwd(), "src-tauri/Cargo.toml"),
      "utf8"
    );
    expect(cargoManifest).not.toContain("tauri-plugin-opener");

    const packageManifest = JSON.parse(
      readFileSync(resolve(process.cwd(), "package.json"), "utf8")
    ) as { dependencies: Record<string, string> };
    expect(Object.keys(packageManifest.dependencies)).not.toEqual(
      expect.arrayContaining([
        "@tauri-apps/plugin-autostart",
        "@tauri-apps/plugin-deep-link",
        "@tauri-apps/plugin-global-shortcut",
        "@tauri-apps/plugin-opener"
      ])
    );
  });

  it("pins structured shortcuts and the complete modifier-key contract", async () => {
    const shortcutCommands = {
      getDefault: COMMANDS.getDefaultWoofModifierKey,
      getInlineModifier: COMMANDS.getWoofModifierKey,
      setInlineModifier: COMMANDS.setWoofModifierKey,
      setModifierPair: COMMANDS.setModifierKeys,
      getEnabled: COMMANDS.getWoofModifierEnabled,
      setEnabled: COMMANDS.setWoofModifierEnabled,
      recordModifier: COMMANDS.recordModifierKey,
      getDictationModifier: COMMANDS.getTranscriptionModifierKey,
      setDictationModifier: COMMANDS.setTranscriptionModifierKey,
      getSecondary: COMMANDS.getSecondaryShortcut,
      setSecondary: COMMANDS.setSecondaryShortcut,
      getSecondaryEnabled: COMMANDS.getSecondaryShortcutEnabled,
      getSecondaryError: COMMANDS.getSecondaryShortcutError,
      setSecondaryEnabled: COMMANDS.setSecondaryShortcutEnabled,
      recordSecondary: COMMANDS.recordSecondaryShortcut
    };
    expect(shortcutCommands).toEqual({
      getDefault: "get_default_woof_modifier_key",
      getInlineModifier: "get_woof_modifier_key",
      setInlineModifier: "set_woof_modifier_key",
      setModifierPair: "set_modifier_keys",
      getEnabled: "get_woof_modifier_enabled",
      setEnabled: "set_woof_modifier_enabled",
      recordModifier: "record_modifier_key",
      getDictationModifier: "get_transcription_modifier_key",
      setDictationModifier: "set_transcription_modifier_key",
      getSecondary: "get_secondary_shortcut",
      setSecondary: "set_secondary_shortcut",
      getSecondaryEnabled: "get_secondary_shortcut_enabled",
      getSecondaryError: "get_secondary_shortcut_error",
      setSecondaryEnabled: "set_secondary_shortcut_enabled",
      recordSecondary: "record_secondary_shortcut"
    });
    const nativeSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/lib.rs"),
      "utf8"
    );
    for (const command of Object.values(shortcutCommands)) {
      expect(nativeSource).toContain(`commands::${command}`);
    }
    const commandSource = readFileSync(
      resolve(process.cwd(), "src-tauri/src/commands.rs"),
      "utf8"
    );
    expect(commandSource).toContain("fn validate_modifier_key_pair(");
    expect(commandSource).toContain("replace_modifier_keys(&app, &state, woof_key, transcription_key)");

    const inlineDefault = await invokeCommand<ModifierKey>(COMMANDS.getDefaultWoofModifierKey);
    const dictationDefault = await invokeCommand<ModifierKey>(
      COMMANDS.getTranscriptionModifierKey
    );
    const chord = await invokeCommand<ShortcutChord>(COMMANDS.getSecondaryShortcut);
    expect(inlineDefault).toBe("right_option");
    expect(dictationDefault).toBe("fn");
    expect(chord).toEqual({
      meta: true,
      shift: true,
      alt: false,
      control: false,
      key: "g"
    });
  });

  it("pins window geometry and motion", () => {
    expect(WINDOWS.companion).toMatchObject({
      collapsed: { width: 260, height: 32 },
      expanded: { width: 588, height: 440 }
    });
    expect(WINDOWS.memoryHub).toMatchObject({
      label: "memory-hub", width: 1060, height: 720
    });
    expect(SCREENSHOT_VIEWPORTS.collapsed).toEqual({ width: 260, height: 32 });
    expect(MOTION).toMatchObject({ companionMorph: 180, collapsedContentFade: 180 });
    expect(CURVES.standard).toContain("cubic-bezier");
    expect(GLASS.blur).toBe(28);
  });

  it("pins the native identity, sidecars, and privacy-sensitive windows", () => {
    const config = JSON.parse(
      readFileSync(resolve(process.cwd(), "src-tauri/tauri.conf.json"), "utf8")
    );
    const infoPlist = readFileSync(
      resolve(process.cwd(), "src-tauri/Info.plist"),
      "utf8"
    );
    const notices = readFileSync(
      resolve(process.cwd(), "../../THIRD_PARTY_NOTICES"),
      "utf8"
    );
    expect(config.identifier).toBe("com.julius.woof");
    expect(config.productName).toBe("woof");
    expect(config.plugins["deep-link"].desktop.schemes).toEqual(["woof"]);
    expect(config.bundle.externalBin).toEqual(["binaries/woof_d", "binaries/woof-mcp"]);
    expect(config.bundle.resources).toEqual({
      "../../../LICENSE": "LICENSE",
      "../../../THIRD_PARTY_NOTICES": "THIRD_PARTY_NOTICES"
    });
    expect(
      readFileSync(resolve(process.cwd(), "../../LICENSE"), "utf8")
    ).toContain("MIT License");
    expect(notices).toContain("SIL OPEN FONT LICENSE Version 1.1");
    expect(notices).toContain("Inter Project Authors");
    expect(notices).toContain("Wry 0.54.4");
    expect(config.bundle.macOS.hardenedRuntime).toBe(true);
    expect(config.bundle.macOS).not.toHaveProperty("signingIdentity");
    expect(infoPlist).toMatch(
      /<key>LSMultipleInstancesProhibited<\/key>\s*<true\/>/
    );
    expect(
      config.app.windows.find((window: { label: string }) => window.label === "companion-chat")
        ?.url
    ).toBe("/?view=companion");
    expect(
      Object.fromEntries(
        config.app.windows.map((window: { label: string; width: number; height: number }) => [
          window.label,
          [window.width, window.height]
        ])
      )
    ).toMatchObject({
      "companion-chat": [260, 32],
      "memory-hub": [1060, 720],
      onboarding: [920, 680],
      "caret-overlay": [320, 68],
      "edit-mode": [440, 248]
    });
    expect(JSON.stringify(config)).not.toMatch(/updater|analytics|billing|auth/i);
  });

  it("keeps production signing separate from local development signing", () => {
    const releaseScript = resolve(process.cwd(), "../../scripts/build-release.sh");
    const releaseSource = readFileSync(releaseScript, "utf8");
    expect(releaseSource).toMatch(
      /^#!\/usr\/bin\/env -S -i WOOF_RELEASE_CLEAN_ENV=1 \/bin\/sh\n/
    );
    expect(releaseSource).toContain("Developer ID Application");
    expect(releaseSource).toContain("--options runtime --timestamp");
    expect(releaseSource).toContain("notarytool submit");
    expect(releaseSource).toContain("stapler staple");
    expect(releaseSource).toContain("stapler validate");
    expect(releaseSource).toContain("spctl --assess --type execute");
    expect(releaseSource).toContain("--keychain-profile");
    expect(releaseSource).toContain("CARGO_ENCODED_RUSTFLAGS");
    expect(releaseSource).toContain('export CARGO_HOME="$account_home/.cargo"');
    expect(releaseSource).toContain('export CARGO_TARGET_DIR="$repo_dir/target"');
    expect(releaseSource).toContain("--remap-path-prefix=$repo_dir=woof-source");
    expect(releaseSource).toContain("--remap-path-prefix=$account_home=rust-build-home");
    expect(releaseSource).toContain("Release binary contains a build-host path");
    expect(releaseSource).toContain("scripts/verify.sh --sidecars-pre-staged");
    expect(releaseSource).toContain(
      "npm run tauri:build:pre-staged --workspace apps/woof"
    );
    expect(releaseSource).toContain(
      '"$bundle/Contents/Resources/THIRD_PARTY_NOTICES"'
    );
    expect(releaseSource).toContain('"$bundle/Contents/Resources/LICENSE"');
    expect(releaseSource).not.toContain("woof local development signing");
    expect(releaseSource).not.toContain("timestamp=none");

    const sidecarSource = readFileSync(
      resolve(process.cwd(), "../../scripts/stage-sidecars.sh"),
      "utf8"
    );
    expect(sidecarSource).toContain("CARGO_ENCODED_RUSTFLAGS");
    expect(sidecarSource).toContain("${CARGO_HOME+x}");
    expect(sidecarSource).toContain(
      '$CARGO_HOME" != "$build_account_home/.cargo"'
    );
    expect(sidecarSource).toContain('export CARGO_HOME="$build_account_home/.cargo"');
    expect(sidecarSource).toContain("${CARGO_TARGET_DIR+x}");
    expect(sidecarSource).toContain(
      '$CARGO_TARGET_DIR" != "$repo_dir/target"'
    );
    expect(sidecarSource).toContain('export CARGO_TARGET_DIR="$repo_dir/target"');
    expect(sidecarSource).toContain("${CARGO_BUILD_TARGET_DIR+x}");
    expect(sidecarSource).toContain("--remap-path-prefix=$repo_dir=woof-source");
    expect(sidecarSource).toContain(
      "--remap-path-prefix=$build_account_home=rust-build-home"
    );

    const desktopPackage = JSON.parse(
      readFileSync(resolve(process.cwd(), "package.json"), "utf8")
    );
    expect(desktopPackage.scripts["tauri:dev"]).toContain(
      "../../scripts/stage-sidecars.sh debug"
    );
    expect(desktopPackage.scripts["tauri:build"]).toContain(
      "../../scripts/stage-sidecars.sh debug"
    );
    expect(desktopPackage.scripts["tauri:build:pre-staged"]).not.toContain(
      "stage-sidecars.sh"
    );

    const testDirectory = mkdtempSync(join(tmpdir(), "woof-release-environment-"));
    const startupHook = join(testDirectory, "startup-hook.sh");
    const marker = join(testDirectory, "inherited-environment-ran");
    writeFileSync(
      startupHook,
      '[ -z "${WOOF_RELEASE_TEST_MARKER:-}" ] || /usr/bin/touch "$WOOF_RELEASE_TEST_MARKER"\n',
      { mode: 0o600 }
    );
    try {
      const result = spawnSync(releaseScript, ["--help"], {
        encoding: "utf8",
        env: {
          ...process.env,
          BASH_ENV: startupHook,
          ENV: startupHook,
          SHELLOPTS: "xtrace",
          PS4: '$(/usr/bin/touch "$WOOF_RELEASE_TEST_MARKER") ',
          PERL5LIB: testDirectory,
          PERL5OPT: "-MReleaseEnvironmentPayload",
          CPATH: testDirectory,
          LIBRARY_PATH: testDirectory,
          COMPILER_PATH: testDirectory,
          TOOLCHAINS: "untrusted",
          WOOF_RELEASE_TEST_MARKER: marker
        }
      });
      expect(result.status, result.stderr).toBe(0);
      expect(result.stderr).toContain("usage: scripts/build-release.sh");
      expect(existsSync(marker)).toBe(false);

      const bypass = spawnSync("/bin/sh", [releaseScript, "--help"], {
        encoding: "utf8",
        env: {
          WOOF_RELEASE_CLEAN_ENV: "1",
          CPATH: testDirectory,
          LIBRARY_PATH: testDirectory,
          TOOLCHAINS: "untrusted"
        }
      });
      expect(bypass.status).toBe(1);
      expect(bypass.stderr).toContain("bypassed the clean-environment launcher");
    } finally {
      rmSync(testDirectory, { recursive: true, force: true });
    }
  });

  it("binds stable identities into both single-file helpers", () => {
    const identity = JSON.parse(
      readFileSync(resolve(process.cwd(), "../../docs/contracts/identity.json"), "utf8")
    );
    expect(identity).toMatchObject({
      daemon_name: "woof_d",
      daemon_bundle_id: "com.julius.woof.daemon",
      daemon_bundle_name: "woof daemon",
      mcp_name: "woof-mcp",
      mcp_bundle_id: "com.julius.woof.mcp",
      mcp_bundle_name: "woof mcp"
    });

    for (const helper of [
      {
        directory: "woof_d",
        executable: "woof_d",
        identifier: identity.daemon_bundle_id,
        name: identity.daemon_bundle_name
      },
      {
        directory: "woof-mcp",
        executable: "woof-mcp",
        identifier: identity.mcp_bundle_id,
        name: identity.mcp_bundle_name
      }
    ]) {
      const helperRoot = resolve(process.cwd(), `../${helper.directory}`);
      const plist = readFileSync(resolve(helperRoot, "Info.plist"), "utf8");
      const buildSource = readFileSync(resolve(helperRoot, "build.rs"), "utf8");
      expect(plist).toContain(`<string>${helper.identifier}</string>`);
      expect(plist).toContain(`<string>${helper.name}</string>`);
      expect(plist).toContain(`<string>${identity.version}</string>`);
      expect(buildSource).toContain('"__TEXT".to_owned()');
      expect(buildSource).toContain('"__info_plist".to_owned()');
      expect(buildSource).toContain(
        `cargo:rustc-link-arg-bin=${helper.executable}={argument}`
      );
    }

    const releaseSource = readFileSync(
      resolve(process.cwd(), "../../scripts/build-release.sh"),
      "utf8"
    );
    expect(releaseSource).toContain(
      '"$bundle/Contents/MacOS/woof_d" sidecar com.julius.woof.daemon'
    );
    expect(releaseSource).toContain(
      '"$bundle/Contents/MacOS/woof-mcp" sidecar com.julius.woof.mcp'
    );
    expect(releaseSource).toContain(
      "Signed helper does not bind its embedded Info.plist metadata."
    );
    expect(releaseSource).toContain("codesign --verify --strict --verbose=2 \\");
    expect(releaseSource).toContain('-R="$explicit_requirement" "$signed_path"');

    const probeSource = readFileSync(
      resolve(process.cwd(), "../../scripts/verify-code-identities.sh"),
      "utf8"
    );
    expect(probeSource).toContain('--sign - "$signed_copy"');
    const executableProbeSource = probeSource
      .split("\n")
      .filter((line) => !line.trimStart().startsWith("#"))
      .join("\n");
    expect(executableProbeSource).not.toContain("--identifier");
  });

  it("keeps memory hub data commands registered in the native bridge", async () => {
    const source = readFileSync(
      resolve(process.cwd(), "src-tauri/src/lib.rs"),
      "utf8"
    );
    for (const command of [
      COMMANDS.memoryRecentActivity,
      COMMANDS.memoryWorkingMemory,
      COMMANDS.memoryWikiList,
      COMMANDS.memoryWikiPage,
      COMMANDS.memoryWikiSearch,
      COMMANDS.memoryFollowups,
      COMMANDS.memoryWorkPatterns,
      COMMANDS.captureStatus,
      COMMANDS.memoryTimeReport,
      COMMANDS.memoryTimeRules
    ]) {
      expect(source).toContain(`commands::${command}`);
    }

    const activity = await invokeCommand<{ activity: Array<{ snapshot_id: string }> }>(
      COMMANDS.memoryRecentActivity,
      { minutes: 60, limit: 12 }
    );
    const report = await invokeCommand<{ total_seconds: number }>(
      COMMANDS.memoryTimeReport,
      { period: "today" }
    );
    expect(activity.activity[0]?.snapshot_id).toBe("browser-activity-3");
    expect(report.total_seconds).toBe(8040);
    expect(EVENTS.memoryHubRefreshRequested).toBe("woof:memory-hub-refresh-requested");
    expect(EVENTS.memoryHubNavigate).toBe("woof:memory-hub-navigate");
  });

  it("bounds memory hub navigation to canonical destinations", async () => {
    const routes: string[] = [];
    const listener = (event: Event) =>
      routes.push((event as CustomEvent<{ route: string }>).detail.route);
    window.addEventListener(EVENTS.memoryHubNavigate, listener);

    await invokeCommand(COMMANDS.memoryHubOpenRoute, { route: "followups" });
    await invokeCommand(COMMANDS.memoryHubOpenRoute, { route: "workflows" });
    await expect(
      invokeCommand(COMMANDS.memoryHubOpenRoute, { route: "settings" })
    ).rejects.toThrow("invalid memory hub route");
    expect(routes).toEqual(["followups", "workflows"]);
    window.removeEventListener(EVENTS.memoryHubNavigate, listener);

    const source = readFileSync(resolve(process.cwd(), "src-tauri/src/lib.rs"), "utf8");
    expect(source).toContain(`commands::${COMMANDS.memoryHubOpenRoute}`);
  });

  it("registers the caret/edit commands for the scoped overlay windows", () => {
    const source = readFileSync(
      resolve(process.cwd(), "src-tauri/src/lib.rs"),
      "utf8"
    );
    for (const command of [
      COMMANDS.caretReady,
      COMMANDS.caretCancel,
      COMMANDS.editReady,
      COMMANDS.editClose,
      COMMANDS.editSetContentHeight,
      COMMANDS.editSetGlass
    ]) {
      expect(source).toContain(`commands::${command}`);
    }

    const caretCapability = JSON.parse(
      readFileSync(
        resolve(process.cwd(), "src-tauri/capabilities/caret-overlay.json"),
        "utf8"
      )
    );
    const editCapability = JSON.parse(
      readFileSync(
        resolve(process.cwd(), "src-tauri/capabilities/edit-overlay.json"),
        "utf8"
      )
    );
    expect(caretCapability.windows).toEqual(["caret-overlay"]);
    expect(caretCapability.permissions).toContain("caret-overlay-commands");
    expect(editCapability.windows).toEqual(["edit-mode"]);
    expect(editCapability.permissions).toContain("edit-overlay-commands");
  });
});
