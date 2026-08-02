<script lang="ts">
  import { onMount } from "svelte";
  import {
    Accessibility,
    ArrowLeft,
    ArrowRight,
    Check,
    Command,
    KeyRound,
    LockKeyhole,
    Mic,
    ShieldCheck,
    Sparkles
  } from "lucide-svelte";
  import Mascot from "./Mascot.svelte";
  import { COMMANDS, EVENTS, type AccessibilityStatus } from "$lib/contracts/ipc";
  import { invokeCommand } from "$lib/contracts/bridge";

  let step = $state(0);
  let accessGranted = $state(false);
  let accessibilityStatus = $state<AccessibilityStatus>({
    app_trusted: false,
    capture_service_trusted: false,
    capture_service_operational: false,
    ready: false,
    next_request: "app"
  });
  let inputMonitoringGranted = $state(false);
  let microphoneGranted = $state(false);
  let apiKey = $state("");
  let saving = $state(false);
  let error = $state("");

  const pages = [
    {
      eyebrow: "Meet woof",
      title: "A second memory,\nright on your Mac.",
      body: "woof notices the work already on your screen, helps you find it later, and keeps its memory local."
    },
    {
      eyebrow: "Private by design",
      title: "Your memory stays\nyours.",
      body: "Capture runs locally and does not need OpenAI. After you connect a key, woof periodically sends selected captured text to OpenAI for memory summarization."
    },
    {
      eyebrow: "Local capture",
      title: "Let woof notice\nwhat you’re doing.",
      body: "Accessibility is required for local capture and stays under your control. Input Monitoring is optional and only enables the Option shortcuts. woof never records screenshots and refuses secure fields."
    },
    {
      eyebrow: "Optional: Voice",
      title: "Talk when typing\ngets in the way.",
      body: "Enable microphone access for hold-to-talk and inline dictation, or set it up later in Settings. Audio is streamed for transcription and is not stored."
    },
    {
      eyebrow: "Optional: Connect OpenAI",
      title: "Bring your own\nAPI key.",
      body: "Leave this blank to keep using local capture. Your key stays in macOS Keychain. Connecting it enables periodic OpenAI summaries for chronicles, wiki pages, and time rules, plus chat, rewriting, and transcription when you request them."
    },
    {
      eyebrow: "Optional shortcuts",
      title: "Double-tap Option\nto call woof.",
      body: "After you enable Input Monitoring, use Option shortcuts for rewriting, the companion, and dictation. You can enable them later in Settings."
    },
    {
      eyebrow: "Ready",
      title: "Welcome to\nwoof.",
      body: "woof will build a useful local memory quietly. You can pause capture at any time from the menu bar or companion."
    }
  ] as const;

  let page = $derived(pages[step]);
  let normalizedApiKey = $derived(apiKey.trim());
  let canContinue = $derived(
    step === 2
      ? accessGranted
      : step === 4
        ? normalizedApiKey.length === 0 || normalizedApiKey.length > 12
        : true
  );
  let continueLabel = $derived(
    saving
      ? "Saving…"
      : step === pages.length - 1
        ? "Open memory hub"
        : step === 2 && accessGranted && !inputMonitoringGranted
          ? "Continue without shortcuts"
          : (step === 3 && !microphoneGranted) || (step === 4 && normalizedApiKey.length === 0)
            ? "Not now"
            : "Continue"
  );
  let accessibilityRequestLabel = $derived(
    accessibilityStatus.ready
      ? "Local capture is ready"
      : accessibilityStatus.next_request === "app"
        ? "Allow Accessibility for woof"
        : accessibilityStatus.next_request === "capture-service"
          ? "Reveal capture service to add manually"
          : "Restart woof to apply Accessibility"
  );

  function applyAccessibilityStatus(status: AccessibilityStatus): void {
    accessibilityStatus = status;
    accessGranted = status.ready;
  }

  async function refreshPermissions(): Promise<void> {
    const [accessibility, inputMonitoring, microphone] = await Promise.all([
      invokeCommand<AccessibilityStatus>(COMMANDS.accessibilityStatus).catch(() => ({
        app_trusted: false,
        capture_service_trusted: false,
        capture_service_operational: false,
        ready: false,
        next_request: "app" as const
      })),
      invokeCommand<boolean>(COMMANDS.inputMonitoringTrusted).catch(() => false),
      invokeCommand<string>(COMMANDS.microphoneStatus).catch(() => "not-determined")
    ]);
    applyAccessibilityStatus(accessibility);
    inputMonitoringGranted = inputMonitoring;
    microphoneGranted = microphone === "authorized" || microphone === "granted";
  }

  async function requestAccess(): Promise<void> {
    error = "";
    try {
      const status = await invokeCommand<AccessibilityStatus>(COMMANDS.requestAccessibility);
      applyAccessibilityStatus(status);
      if (!status.ready) {
        error = status.next_request === "app"
          ? "Enable woof in Accessibility, then return here."
          : status.next_request === "capture-service"
            ? "Finder selected woof_d. In Accessibility, click +, add that file, and enable it."
            : "Both entries are enabled. Quit and reopen woof so macOS applies the change.";
      }
    } catch {
      error = "woof couldn’t check the next Accessibility step.";
    }
  }

  async function requestMicrophone(): Promise<void> {
    error = "";
    try {
      const status = await invokeCommand<string>(COMMANDS.microphoneStatus, { request: true });
      microphoneGranted = status === "authorized" || status === "granted";
      if (!microphoneGranted) {
        error = status === "denied" || status === "restricted"
          ? "Microphone remains off. You can enable it later in System Settings."
          : "Finish the microphone prompt, or continue without voice features.";
      }
    } catch {
      error = "Microphone permission wasn’t granted.";
    }
  }

  async function requestInputMonitoring(): Promise<void> {
    error = "";
    inputMonitoringGranted = await invokeCommand<boolean>(
      COMMANDS.requestInputMonitoring
    ).catch(() => false);
    if (!inputMonitoringGranted) {
      await invokeCommand(COMMANDS.openInputMonitoringSettings).catch(() => undefined);
      error = "Input Monitoring remains off. You can enable it later in Settings.";
    }
  }

  onMount(() => {
    void refreshPermissions();
    const interval = window.setInterval(() => void refreshPermissions(), 2_000);
    const refreshWhenVisible = () => {
      if (document.visibilityState === "visible") void refreshPermissions();
    };
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      window.clearInterval(interval);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
    };
  });

  async function skipFlow(): Promise<void> {
    if (saving) return;
    saving = true;
    error = "";
    try {
      await invokeCommand(COMMANDS.skipOnboarding);
    } catch {
      error = "woof couldn’t safely skip setup. Try again.";
    } finally {
      saving = false;
    }
  }

  async function continueFlow(): Promise<void> {
    if (!canContinue || saving) return;
    error = "";
    if (step === 4 && normalizedApiKey.length > 0) {
      saving = true;
      try {
        await invokeCommand(COMMANDS.setOpenAiApiKey, { apiKey: normalizedApiKey });
        apiKey = "";
      } catch {
        error = "That key could not be saved to Keychain.";
        saving = false;
        return;
      }
      saving = false;
    }
    if (step < pages.length - 1) {
      step += 1;
      return;
    }
    saving = true;
    try {
      await invokeCommand(COMMANDS.finishOnboarding);
      window.dispatchEvent(new CustomEvent(EVENTS.onboardingComplete));
    } catch {
      await refreshPermissions();
      step = 2;
      error = "Accessibility changed before local capture started. Check both woof entries, then try again.";
    } finally {
      saving = false;
    }
  }
</script>

<main class="onboarding glass">
  <header class="drag-region">
    <div class="wordmark"><span class="mark"></span>woof</div>
    <div class="progress" aria-label={`Step ${step + 1} of ${pages.length}`}>
      {#each pages as _, index}
        <span class:active={index <= step}></span>
      {/each}
    </div>
    <button
      class="skip no-drag"
      disabled={saving}
      onclick={skipFlow}
      aria-label="Skip onboarding"
    >
      Skip
    </button>
  </header>

  <section class="content" data-step={step}>
    <div class="copy">
      <div class="eyebrow">{page.eyebrow}</div>
      <h1>{page.title}</h1>
      <p>{page.body}</p>

      {#if step === 2}
        <div class="permission-stack">
          <button class:done={accessGranted} class="permission-button" onclick={requestAccess}>
            {#if accessGranted}<Check size={18} />{:else}<Accessibility size={18} />{/if}
            {accessibilityRequestLabel}
          </button>
          <button class:done={inputMonitoringGranted} class="permission-button" onclick={requestInputMonitoring}>
            {#if inputMonitoringGranted}<Check size={18} />{:else}<Command size={18} />{/if}
            {inputMonitoringGranted ? "Input Monitoring is on" : "Set up Input Monitoring (optional)"}
          </button>
        </div>
      {:else if step === 3}
        <button class:done={microphoneGranted} class="permission-button" onclick={requestMicrophone}>
          {#if microphoneGranted}<Check size={18} />{:else}<Mic size={18} />{/if}
          {microphoneGranted ? "Microphone is on" : "Allow microphone (optional)"}
        </button>
      {:else if step === 4}
        <label class="key-field">
          <KeyRound size={18} />
          <span class="visually-hidden">OpenAI API key (optional)</span>
          <input
            type="password"
            aria-label="OpenAI API key (optional)"
            autocomplete="off"
            bind:value={apiKey}
            placeholder="Optional API key"
            spellcheck="false"
          />
          <span class="keychain"><LockKeyhole size={13} /> Keychain</span>
        </label>
      {:else if step === 5}
        <div class="shortcut">
          <span class="key"><Command size={15} /> option</span>
          <span class="tap">× 2</span>
          <span class="meaning">inline help</span>
        </div>
      {/if}

      {#if error}<p class="error" role="alert">{error}</p>{/if}
    </div>

    <div class="scene" aria-hidden="true">
      <div class="halo"></div>
      {#if step === 0}
        <div class="memory-card card-one"><span>9:41</span><b>project notes</b></div>
        <div class="memory-card card-two"><span>Yesterday</span><b>that decision</b></div>
        <Mascot size={210} mood="happy" />
      {:else if step === 1}
        <div class="privacy-orbit orbit-one"><ShieldCheck size={26} /></div>
        <div class="privacy-orbit orbit-two"><LockKeyhole size={22} /></div>
        <Mascot size={210} />
        <div class="local-pill"><span></span> stored locally</div>
      {:else if step === 2}
        <div class="access-window">
          <div class="window-bar"><i></i><i></i><i></i></div>
          <div class="line wide"></div><div class="line"></div><div class="line short"></div>
          <div class="focus-ring"></div>
        </div>
        <Mascot size={150} mood="thinking" />
      {:else if step === 3}
        <div class="wave">
          {#each [24, 45, 70, 38, 62, 87, 50, 31, 68, 42] as height}
            <i style:height={`${height}%`}></i>
          {/each}
        </div>
        <Mascot size={190} mood="listening" />
      {:else if step === 4}
        <div class="keychain-card">
          <LockKeyhole size={34} />
          <b>macOS Keychain</b>
          <span>com.julius.woof.openai</span>
        </div>
        <Mascot size={145} />
      {:else if step === 5}
        <div class="editor-demo">
          <div class="fake-copy">Can you make this sound warmer<span class="caret"></span></div>
          <div class="inline-prompt"><Sparkles size={15} /> What should I change?</div>
        </div>
        <Mascot size={140} mood="thinking" />
      {:else}
        <div class="memory-floor"></div>
        <Mascot size={235} mood="happy" />
        <div class="ready-sparkle one">✦</div>
        <div class="ready-sparkle two">✦</div>
        <div class="ready-sparkle three">✦</div>
      {/if}
    </div>
  </section>

  <footer>
    <button
      class="back"
      disabled={step === 0 || saving}
      onclick={() => (step = Math.max(0, step - 1))}
      aria-label="Previous step"
    >
      <ArrowLeft size={18} />
    </button>
    <p>{step + 1} of {pages.length}</p>
    <button
      class="continue"
      class:disabled={!canContinue || saving}
      disabled={!canContinue || saving}
      onclick={continueFlow}
    >
      {continueLabel}
      {#if step === pages.length - 1}<Sparkles size={17} />{:else}<ArrowRight size={18} />{/if}
    </button>
  </footer>
</main>

<style>
  .onboarding {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    border-radius: 24px;
    background:
      radial-gradient(circle at 77% 25%, rgba(232, 174, 117, 0.22), transparent 34%),
      var(--glass-strong);
  }

  header {
    position: relative;
    z-index: 3;
    height: 72px;
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    padding: 0 28px;
    border-bottom: 1px solid var(--line);
  }

  .wordmark {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 15px;
    font-weight: 760;
    letter-spacing: -0.02em;
  }

  .mark {
    width: 20px;
    height: 20px;
    border-radius: 8px 8px 10px 10px;
    background: var(--fawn);
    box-shadow: inset 0 -5px 0 rgba(74, 50, 40, 0.2);
  }

  .progress {
    display: flex;
    gap: 6px;
  }

  .progress span {
    width: 22px;
    height: 4px;
    border-radius: 999px;
    background: var(--line-strong);
    transition: background 280ms var(--ease), transform 280ms var(--ease);
  }

  .progress span.active {
    background: var(--fawn-deep);
    transform: scaleX(1.06);
  }

  .skip {
    justify-self: end;
    padding: 7px 9px;
    border: 0;
    background: transparent;
    color: var(--ink-muted);
    font-size: 12px;
    cursor: pointer;
  }

  .content {
    height: calc(100% - 142px);
    display: grid;
    grid-template-columns: 0.93fr 1.07fr;
    animation: enter 360ms var(--ease);
  }

  .copy {
    align-self: center;
    max-width: 400px;
    padding: 18px 12px 18px 58px;
  }

  h1 {
    max-width: 430px;
    margin: 15px 0 17px;
    white-space: pre-line;
    color: var(--ink);
    font-size: clamp(38px, 5vw, 50px);
    line-height: 0.99;
    letter-spacing: -0.052em;
    font-weight: 735;
  }

  .copy > p {
    max-width: 350px;
    margin: 0;
    color: var(--ink-muted);
    font-size: 15px;
    line-height: 1.56;
  }

  .permission-button,
  .key-field,
  .shortcut {
    width: 100%;
    max-width: 354px;
    height: 52px;
    margin-top: 24px;
    border: 1px solid var(--line-strong);
    border-radius: 15px;
    background: color-mix(in srgb, var(--cream-solid) 74%, transparent);
    box-shadow: 0 5px 18px rgba(74, 50, 40, 0.08);
  }

  .permission-button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 9px;
    color: var(--cream);
    border-color: transparent;
    background: var(--brown);
    font-size: 13px;
    font-weight: 680;
    cursor: pointer;
    transition: transform 160ms var(--spring), background 160ms ease;
  }

  .permission-button:hover {
    transform: translateY(-1px);
  }

  .permission-button.done {
    color: #fff;
    background: var(--sage);
  }

  .permission-stack {
    width: 100%;
    max-width: 354px;
    display: grid;
    gap: 8px;
    margin-top: 20px;
  }

  .permission-stack .permission-button {
    margin-top: 0;
  }

  .key-field {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 13px;
  }

  .key-field input {
    min-width: 0;
    flex: 1;
    border: 0;
    outline: 0;
    color: var(--ink);
    background: transparent;
    font-size: 13px;
    user-select: text;
  }

  .keychain {
    display: flex;
    align-items: center;
    gap: 4px;
    color: var(--ink-faint);
    font-size: 9px;
    font-weight: 650;
  }

  .shortcut {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 13px;
  }

  .key {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 7px 10px;
    border: 1px solid var(--line-strong);
    border-radius: 9px;
    background: var(--cream);
    box-shadow: 0 2px 0 var(--line-strong);
    font-size: 12px;
    font-weight: 670;
  }

  .tap,
  .meaning {
    color: var(--ink-muted);
    font-size: 12px;
  }

  .meaning {
    margin-left: auto;
  }

  .error {
    margin-top: 10px !important;
    color: var(--rose) !important;
    font-size: 11px !important;
  }

  .scene {
    position: relative;
    display: grid;
    place-items: center;
    overflow: hidden;
  }

  .halo {
    position: absolute;
    width: 390px;
    height: 390px;
    border-radius: 50%;
    background:
      radial-gradient(circle at center, rgba(232, 174, 117, 0.21), rgba(232, 174, 117, 0.04) 58%, transparent 70%);
  }

  .memory-card {
    position: absolute;
    z-index: 0;
    width: 145px;
    height: 78px;
    padding: 14px;
    border: 1px solid rgba(255, 255, 255, 0.63);
    border-radius: 17px;
    background: rgba(255, 250, 242, 0.68);
    box-shadow: var(--shadow-tight);
    transform: rotate(-7deg);
  }

  .memory-card span,
  .memory-card b {
    display: block;
  }

  .memory-card span {
    margin-bottom: 8px;
    color: #98745e;
    font-size: 9px;
    text-transform: uppercase;
  }

  .memory-card b {
    color: #4a3228;
    font-size: 12px;
  }

  .card-one {
    left: 48px;
    top: 105px;
  }

  .card-two {
    right: 43px;
    bottom: 86px;
    transform: rotate(8deg);
  }

  .privacy-orbit {
    position: absolute;
    z-index: 3;
    display: grid;
    place-items: center;
    width: 56px;
    height: 56px;
    border-radius: 20px;
    color: var(--sage);
    background: var(--glass-strong);
    box-shadow: var(--shadow-tight);
  }

  .orbit-one {
    transform: translate(-140px, -96px) rotate(-7deg);
  }

  .orbit-two {
    transform: translate(136px, 96px) rotate(8deg);
  }

  .local-pill {
    position: absolute;
    z-index: 4;
    bottom: 76px;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 8px 12px;
    border-radius: 99px;
    color: var(--ink-muted);
    background: var(--glass-strong);
    box-shadow: var(--shadow-tight);
    font-size: 10px;
    font-weight: 640;
  }

  .local-pill span {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--sage);
  }

  .access-window {
    position: absolute;
    width: 282px;
    height: 206px;
    padding: 72px 30px 20px;
    border-radius: 18px;
    background: rgba(255, 250, 242, 0.76);
    box-shadow: var(--shadow);
    transform: translate(-28px, -14px) rotate(-2deg);
  }

  .window-bar {
    position: absolute;
    inset: 0 0 auto;
    display: flex;
    gap: 6px;
    height: 34px;
    padding: 13px;
    border-bottom: 1px solid rgba(74, 50, 40, 0.08);
  }

  .window-bar i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #e5c09b;
  }

  .line {
    width: 76%;
    height: 8px;
    margin-bottom: 16px;
    border-radius: 99px;
    background: #dbc9b4;
  }

  .line.wide {
    width: 100%;
  }

  .line.short {
    width: 54%;
  }

  .focus-ring {
    position: absolute;
    inset: 58px 20px 43px;
    border: 2px solid var(--fawn);
    border-radius: 12px;
  }

  .access-window + :global(.mascot) {
    position: absolute;
    right: 43px;
    bottom: 62px;
  }

  .wave {
    position: absolute;
    width: 350px;
    height: 142px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 9px;
    opacity: 0.5;
  }

  .wave i {
    width: 7px;
    border-radius: 99px;
    background: var(--fawn);
    animation: wave 1s ease-in-out infinite alternate;
  }

  .wave i:nth-child(2n) {
    animation-delay: -0.4s;
  }

  .wave i:nth-child(3n) {
    animation-delay: -0.7s;
  }

  .keychain-card {
    position: absolute;
    z-index: 0;
    width: 300px;
    height: 180px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    border: 1px solid rgba(255, 255, 255, 0.6);
    border-radius: 25px;
    color: var(--fawn-deep);
    background: rgba(255, 250, 242, 0.67);
    box-shadow: var(--shadow);
    transform: translateY(-25px);
  }

  .keychain-card b {
    color: var(--brown);
    font-size: 14px;
  }

  .keychain-card span {
    color: #9a8171;
    font-family: ui-monospace, monospace;
    font-size: 9px;
  }

  .keychain-card + :global(.mascot) {
    position: absolute;
    right: 58px;
    bottom: 49px;
  }

  .editor-demo {
    position: absolute;
    width: 350px;
    height: 170px;
    padding: 58px 30px;
    border-radius: 22px;
    color: #60483a;
    background: rgba(255, 250, 242, 0.75);
    box-shadow: var(--shadow);
    transform: translate(-12px, -35px);
  }

  .fake-copy {
    font-size: 13px;
  }

  .caret {
    display: inline-block;
    width: 1.5px;
    height: 16px;
    margin-left: 2px;
    vertical-align: -3px;
    background: var(--fawn-deep);
    animation: blink 0.76s infinite;
  }

  .inline-prompt {
    position: absolute;
    left: 62px;
    bottom: -25px;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 12px 16px;
    border-radius: 15px;
    color: #fff;
    background: var(--brown);
    box-shadow: var(--shadow-tight);
    font-size: 11px;
  }

  .editor-demo + :global(.mascot) {
    position: absolute;
    right: 30px;
    bottom: 48px;
  }

  .memory-floor {
    position: absolute;
    width: 330px;
    height: 86px;
    bottom: 55px;
    border-radius: 50%;
    background: rgba(166, 103, 66, 0.12);
    filter: blur(5px);
  }

  .ready-sparkle {
    position: absolute;
    color: var(--fawn-bright);
    font-size: 24px;
    animation: twinkle 1.4s ease-in-out infinite alternate;
  }

  .ready-sparkle.one {
    transform: translate(-137px, -110px);
  }

  .ready-sparkle.two {
    transform: translate(137px, -70px) scale(0.7);
    animation-delay: -0.6s;
  }

  .ready-sparkle.three {
    transform: translate(-154px, 52px) scale(0.52);
    animation-delay: -0.9s;
  }

  footer {
    position: absolute;
    z-index: 5;
    inset: auto 0 0;
    height: 70px;
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    padding: 0 28px;
    border-top: 1px solid var(--line);
    background: color-mix(in srgb, var(--cream) 60%, transparent);
  }

  footer p {
    color: var(--ink-faint);
    font-size: 10px;
  }

  .back,
  .continue {
    border: 0;
    cursor: pointer;
  }

  .back {
    width: 36px;
    height: 36px;
    display: grid;
    place-items: center;
    border-radius: 12px;
    background: transparent;
  }

  .back:disabled {
    opacity: 0;
    pointer-events: none;
  }

  .continue {
    justify-self: end;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 9px;
    min-width: 125px;
    height: 40px;
    padding: 0 17px;
    border-radius: 13px;
    color: #fff8ef;
    background: var(--brown);
    box-shadow: 0 7px 20px rgba(74, 50, 40, 0.18);
    font-size: 12px;
    font-weight: 680;
    transition: transform 160ms var(--spring), opacity 160ms ease;
  }

  .continue:hover:not(:disabled) {
    transform: translateY(-1px) scale(1.01);
  }

  .continue.disabled {
    opacity: 0.36;
    cursor: default;
  }

  @keyframes enter {
    from {
      transform: translateX(15px);
      opacity: 0;
    }
  }

  @keyframes wave {
    to {
      transform: scaleY(0.4);
      opacity: 0.45;
    }
  }

  @keyframes blink {
    50% {
      opacity: 0;
    }
  }

  @keyframes twinkle {
    to {
      transform: translate(var(--x, 0), var(--y, 0)) scale(1.25) rotate(12deg);
      opacity: 0.45;
    }
  }
</style>
