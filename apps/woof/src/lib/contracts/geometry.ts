/**
 * Desktop geometry and motion contract.
 *
 * All values are physical design pixels or milliseconds. Keeping the contract
 * here lets layout tests tune a single source rather than allowing incidental
 * CSS values to drift across windows.
 */
export const WINDOWS = {
  companion: {
    label: "companion-chat",
    collapsed: { width: 260, height: 32 },
    expanded: { width: 588, height: 440 },
    cornerRadius: 16,
    resizeGripSize: 18,
    topInset: 0,
    dockSnapDistance: 42
  },
  chat: {
    label: "companion-chat",
    width: 588,
    minHeight: 440,
    defaultHeight: 440,
    maxHeight: 440,
    cornerRadius: 16
  },
  memoryHub: {
    label: "memory-hub",
    width: 1060,
    height: 720,
    minWidth: 880,
    minHeight: 620,
    cornerRadius: 18
  },
  onboarding: {
    label: "onboarding",
    width: 920,
    height: 680,
    cornerRadius: 24
  },
  caret: {
    label: "caret-overlay",
    width: 320,
    height: 68,
    caretGap: 9,
    cornerRadius: 18
  },
  edit: {
    label: "edit-mode",
    width: 440,
    height: 248,
    cornerRadius: 24
  },
  permission: {
    label: "permission",
    width: 480,
    height: 420,
    cornerRadius: 24
  }
} as const;

export const MOTION = {
  micro: 120,
  hoverCloseDelay: 250,
  companionMorph: 180,
  expandedBodyDelay: 200,
  expandedContentFade: 220,
  collapsedContentFade: 180,
  collapsedFadeDelay: 80,
  settingsClose: 240,
  panelEnter: 180,
  panelExit: 180,
  caretFade: 150,
  caretHold: 850,
  messageEnter: 260,
  onboardingStep: 360,
  glowBreath: 2400,
  notificationBounce: 440,
  dragSettle: 380,
  healthPulse: 1600,
  transcriptionLevelSmoothing: 90,
  streamingCursorBlink: 760
} as const;

export const CURVES = {
  standard: "cubic-bezier(0.22, 1, 0.36, 1)",
  enter: "cubic-bezier(0.16, 1, 0.3, 1)",
  exit: "cubic-bezier(0.4, 0, 1, 1)",
  spring: "cubic-bezier(0.34, 1.56, 0.64, 1)",
  gentle: "cubic-bezier(0.25, 0.8, 0.25, 1)"
} as const;

export const GLASS = {
  blur: 28,
  saturation: 1.22,
  panelOpacity: 0.82,
  elevatedOpacity: 0.9,
  borderOpacity: 0.56,
  shadow: "0 18px 60px rgba(52, 35, 24, 0.22)",
  tightShadow: "0 8px 28px rgba(52, 35, 24, 0.18)"
} as const;

export const SCREENSHOT_VIEWPORTS = {
  onboarding: { width: WINDOWS.onboarding.width, height: WINDOWS.onboarding.height },
  memoryHub: { width: WINDOWS.memoryHub.width, height: WINDOWS.memoryHub.height },
  companion: {
    width: WINDOWS.companion.collapsed.width,
    height: WINDOWS.companion.collapsed.height
  },
  collapsed: { width: WINDOWS.companion.collapsed.width, height: WINDOWS.companion.collapsed.height },
  chat: { width: WINDOWS.chat.width, height: WINDOWS.chat.defaultHeight },
  caret: { width: WINDOWS.caret.width, height: WINDOWS.caret.height },
  edit: { width: WINDOWS.edit.width, height: WINDOWS.edit.height }
} as const;
