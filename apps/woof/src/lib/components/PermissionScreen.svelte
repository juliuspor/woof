<script lang="ts">
  import { Accessibility, ArrowUpRight, Check, Command, Mic, RotateCw, ShieldCheck } from "lucide-svelte";
  import Mascot from "./Mascot.svelte";
  import { COMMANDS, type AccessibilityStatus } from "$lib/contracts/ipc";
  import { invokeCommand } from "$lib/contracts/bridge";

  let accessibility = $state(false);
  let accessibilityStatus = $state<AccessibilityStatus>({
    app_trusted: false,
    capture_service_trusted: false,
    capture_service_operational: false,
    ready: false,
    next_request: "app"
  });
  let microphone = $state(false);
  let inputMonitoring = $state(false);
  let checking = $state(false);

  async function refresh(): Promise<void> {
    checking = true;
    const [ax, input, mic] = await Promise.all([
      invokeCommand<AccessibilityStatus>(COMMANDS.accessibilityStatus).catch(() => ({
        app_trusted: false,
        capture_service_trusted: false,
        capture_service_operational: false,
        ready: false,
        next_request: "app" as const
      })),
      invokeCommand<boolean>(COMMANDS.inputMonitoringTrusted).catch(() => false),
      invokeCommand<string>(COMMANDS.microphoneStatus).catch(() => "denied")
    ]);
    accessibilityStatus = ax;
    accessibility = ax.ready;
    inputMonitoring = input;
    microphone = mic === "authorized" || mic === "granted";
    checking = false;
  }

  async function openAccessibility(): Promise<void> {
    accessibilityStatus = await invokeCommand<AccessibilityStatus>(
      COMMANDS.requestAccessibility
    ).catch(() => accessibilityStatus);
    accessibility = accessibilityStatus.ready;
    window.setTimeout(refresh, 800);
  }

  async function openMicrophone(): Promise<void> {
    await invokeCommand(COMMANDS.microphoneStatus, { request: true });
    window.setTimeout(refresh, 800);
  }

  async function openInputMonitoring(): Promise<void> {
    inputMonitoring = await invokeCommand<boolean>(COMMANDS.requestInputMonitoring).catch(
      () => false
    );
    if (!inputMonitoring) {
      await invokeCommand(COMMANDS.openInputMonitoringSettings).catch(() => undefined);
    }
    window.setTimeout(refresh, 800);
  }

  $effect(() => {
    void refresh();
    const interval = window.setInterval(() => void refresh(), 2_000);
    return () => window.clearInterval(interval);
  });
</script>

<main class="permission glass">
  <header class="drag-region">
    <span>woof permissions</span>
    <button class="no-drag" onclick={refresh} aria-label="Check again">
      <RotateCw class={checking ? "spin" : ""} size={15} />
    </button>
  </header>
  <section>
    <div class="mascot-wrap"><Mascot size={118} mood={accessibility ? "happy" : "calm"} /></div>
    <div class="eyebrow">A small hand from macOS</div>
    <h1>Help woof see the work,<br />not your screen.</h1>
    <p>
      Accessibility exposes visible text and focus. woof stores no screenshots and
      refuses passwords or secure keyboard input.
    </p>

    <div class="permissions">
      <button class:granted={accessibility} onclick={openAccessibility}>
        <span class="icon"><Accessibility size={18} /></span>
        <span>
          <b>Accessibility</b>
          <small>
            {accessibilityStatus.next_request === "capture-service"
              ? "Next: reveal woof_d and add it with +"
              : accessibilityStatus.next_request === "app"
                ? "Next: woof"
                : "Visible interface text"}
          </small>
        </span>
        {#if accessibility}<Check size={17} />{:else}<ArrowUpRight size={16} />{/if}
      </button>
      <button class:granted={microphone} onclick={openMicrophone}>
        <span class="icon"><Mic size={18} /></span>
        <span><b>Microphone</b><small>Hold-to-talk dictation</small></span>
        {#if microphone}<Check size={17} />{:else}<ArrowUpRight size={16} />{/if}
      </button>
      <button class:granted={inputMonitoring} onclick={openInputMonitoring}>
        <span class="icon"><Command size={18} /></span>
        <span><b>Input Monitoring</b><small>Option-key shortcuts</small></span>
        {#if inputMonitoring}<Check size={17} />{:else}<ArrowUpRight size={16} />{/if}
      </button>
    </div>

    <div class="privacy"><ShieldCheck size={14} /> You can revoke either permission at any time.</div>
  </section>
</main>

<style>
  .permission {
    width: 100%;
    height: 100%;
    overflow: hidden;
    border-radius: 24px;
    background:
      radial-gradient(circle at 50% 0%, rgba(231, 173, 117, 0.18), transparent 36%),
      var(--glass-strong);
  }

  header {
    height: 50px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 18px;
    border-bottom: 1px solid var(--line);
    color: var(--ink-muted);
    font-size: 11px;
    font-weight: 650;
  }

  header button {
    width: 29px;
    height: 29px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 9px;
    color: var(--ink-muted);
    background: transparent;
    cursor: pointer;
  }

  section {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 10px 35px 26px;
    text-align: center;
  }

  .mascot-wrap {
    height: 112px;
  }

  h1 {
    margin: 8px 0 10px;
    font-size: 26px;
    line-height: 1.06;
    letter-spacing: -0.042em;
  }

  section > p {
    max-width: 370px;
    margin: 0;
    color: var(--ink-muted);
    font-size: 11px;
    line-height: 1.5;
  }

  .permissions {
    width: 100%;
    display: grid;
    grid-template-columns: 1fr;
    gap: 7px;
    margin: 12px 0 10px;
  }

  .permissions button {
    min-width: 0;
    height: 48px;
    display: grid;
    grid-template-columns: 34px 1fr 17px;
    align-items: center;
    gap: 9px;
    padding: 8px 11px;
    border: 1px solid var(--line);
    border-radius: 16px;
    color: var(--ink);
    background: color-mix(in srgb, var(--cream-solid) 72%, transparent);
    text-align: left;
    cursor: pointer;
  }

  .permissions button.granted {
    border-color: color-mix(in srgb, var(--sage) 30%, transparent);
    color: var(--sage);
    background: color-mix(in srgb, var(--sage) 8%, var(--cream-solid));
  }

  .icon {
    width: 34px;
    height: 34px;
    display: grid;
    place-items: center;
    border-radius: 11px;
    color: var(--fawn-deep);
    background: color-mix(in srgb, var(--fawn) 14%, transparent);
  }

  .permissions b,
  .permissions small {
    display: block;
  }

  .permissions b {
    font-size: 11px;
  }

  .permissions small {
    margin-top: 3px;
    overflow: hidden;
    color: var(--ink-faint);
    font-size: 8.5px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .privacy {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--ink-faint);
    font-size: 9px;
  }

  :global(.spin) {
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
