<script lang="ts">
  import { onMount } from "svelte";
  import { Check, Mic, Sparkles, Square, X } from "lucide-svelte";
  import Mascot from "./Mascot.svelte";
  import {
    COMMANDS,
    EVENTS,
    type CaretFadeoutPayload,
    type CaretInitPayload,
    type CaretStatusPayload,
    type InlineRefusedPayload,
    type TranscriptionLevelPayload,
    transcriptionLevelFromPayload
  } from "$lib/contracts/ipc";
  import { invokeCommand, listenEvent } from "$lib/contracts/bridge";

  type CaretMode = "prompt" | "listening" | "processing" | "done" | "error";

  let mode = $state<CaretMode>("prompt");
  let level = $state(0.28);
  let title = $state("What should I change?");
  let detail = $state("Selection ready");
  let sessionId = $state(0);
  let visible = $state(false);
  let leaving = $state(false);

  function showError(detailText: string): void {
    mode = "error";
    title = "Couldn’t continue";
    detail = detailText;
  }

  async function cancel(): Promise<void> {
    if (!visible || sessionId === 0) return;
    await invokeCommand(COMMANDS.caretCancel, { sessionId });
  }

  onMount(() => {
    const unlisteners: Array<() => void> = [];
    let disposed = false;
    const listeners = [
      listenEvent<CaretInitPayload>(EVENTS.caretInit, (payload) => {
        if (!Number.isSafeInteger(payload?.session_id) || payload.session_id <= 0) return;
        if (sessionId > 0 && payload.session_id < sessionId) return;
        const newSession = payload.session_id !== sessionId;
        sessionId = payload.session_id;
        if (newSession) {
          mode = "prompt";
          level = 0.28;
          title = "What should I change?";
        }
        detail = payload.status || "Selection ready";
        leaving = false;
        visible = true;
      }),
      listenEvent<CaretStatusPayload>(EVENTS.caretStatus, (payload) => {
        if (payload?.session_id !== sessionId || typeof payload.text !== "string") return;
        detail = payload.text;
      }),
      listenEvent<TranscriptionLevelPayload>(EVENTS.transcriptionLevel, (payload) => {
        level = transcriptionLevelFromPayload(payload);
        mode = "listening";
        title = "Listening…";
        detail = "Release Right Option when you’re done";
      }),
      listenEvent(EVENTS.transcriptionProcessing, () => {
        mode = "processing";
        title = "Working on it…";
        detail = "Keeping the tone and meaning";
      }),
      listenEvent(EVENTS.transcriptionDone, () => {
        mode = "done";
        title = "Ready to paste";
        detail = "Your clipboard will be restored";
      }),
      listenEvent(EVENTS.transcriptionCancelled, () => showError("Dictation was cancelled.")),
      listenEvent(EVENTS.transcriptionFailed, () =>
        showError("Check microphone, Input Monitoring, and OpenAI settings.")
      ),
      listenEvent(EVENTS.transcriptionOverflow, () =>
        showError("Dictation exceeded its safe size limit.")
      ),
      listenEvent(EVENTS.transcriptionLimit, () => {
        mode = "processing";
        title = "Finishing dictation…";
        detail = "The recording limit was reached";
      }),
      listenEvent<InlineRefusedPayload>(EVENTS.inlineRefused, (payload) => {
        if (payload?.session_id !== undefined && payload.session_id !== sessionId) return;
        const reason = payload?.reason ?? "";
        showError(
          reason === "secure-input"
            ? "Secure keyboard input is active."
            : reason === "permission-denied" || reason === "accessibility-permission"
              ? "Accessibility and Input Monitoring are required."
              : reason === "delivery-unconfirmed"
                ? "Couldn’t confirm the draft was inserted. Review the composer before retrying."
              : reason === "delivery-failed"
                ? "Couldn’t insert the draft. The chat was left untouched."
              : "The focused text is protected or unavailable."
        );
      }),
      listenEvent<CaretFadeoutPayload>(EVENTS.caretFadeout, (payload) => {
        if (payload?.session_id !== sessionId) return;
        leaving = true;
        visible = false;
      })
    ];
    void Promise.all(listeners).then((resolved) => {
      if (disposed) {
        resolved.forEach((unlisten) => unlisten());
        return;
      }
      unlisteners.push(...resolved);
      return invokeCommand(COMMANDS.caretReady);
    });
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  });
</script>

<main class:visible class:leaving class:processing={mode === "processing"} class:listening={mode === "listening"} class:done={mode === "done"} class:error={mode === "error"} class="caret glass">
  <div class="mascot"><Mascot size={53} mood={mode === "listening" ? "listening" : mode === "processing" ? "thinking" : mode === "done" ? "happy" : "calm"} /></div>
  <div class="copy">
    <div class="title">
      {#if mode === "prompt"}<Sparkles size={12} />
      {:else if mode === "listening"}<Mic size={12} />
      {:else if mode === "processing"}<span class="spinner"></span>
      {:else if mode === "done"}<Check size={12} />
      {:else if mode === "error"}<X size={12} />{/if}
      <b>{title}</b>
    </div>
    <span>{detail}</span>
  </div>
  {#if mode === "listening"}
    <div class="wave" aria-label="Voice level">
      {#each [0.45, 0.78, 1, 0.67, 0.36] as factor}
        <i style:height={`${Math.max(4, level * factor * 23)}px`}></i>
      {/each}
    </div>
    <button onclick={() => invokeCommand(COMMANDS.transcriptionFinalize)} aria-label="Stop">
      <Square size={9} fill="currentColor" />
    </button>
  {:else}
    <button onclick={cancel} aria-label="Dismiss"><X size={13} /></button>
  {/if}
</main>

<style>
  .caret {
    width: 100%;
    height: 100%;
    display: grid;
    grid-template-columns: 58px 1fr auto 30px;
    align-items: center;
    padding: 5px 10px 5px 5px;
    border-radius: 18px;
    background: var(--glass-strong);
    animation: appear 220ms var(--spring);
    opacity: 0;
    pointer-events: none;
  }

  .caret.visible {
    opacity: 1;
    pointer-events: auto;
  }

  .caret.leaving {
    opacity: 0;
    transition: opacity 150ms ease;
  }

  .caret::after {
    content: "";
    position: absolute;
    left: 35px;
    bottom: -6px;
    width: 13px;
    height: 13px;
    border-right: 1px solid rgba(255, 255, 255, 0.56);
    border-bottom: 1px solid rgba(255, 255, 255, 0.56);
    background: var(--glass-strong);
    transform: rotate(45deg);
  }

  .mascot {
    align-self: end;
    height: 57px;
  }

  .title {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--fawn-deep);
  }

  .copy b,
  .copy span {
    display: block;
  }

  .copy b {
    color: var(--ink);
    font-size: 10px;
  }

  .copy > span {
    margin-top: 4px;
    color: var(--ink-faint);
    font-size: 7.5px;
  }

  .caret > button {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 8px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--ink) 5%, transparent);
    cursor: pointer;
  }

  .listening {
    border-color: color-mix(in srgb, var(--fawn) 35%, rgba(255, 255, 255, 0.56));
  }

  .listening > button {
    color: #fff8ef;
    background: var(--brown);
  }

  .done {
    border-color: color-mix(in srgb, var(--sage) 32%, rgba(255, 255, 255, 0.56));
  }

  .done .title {
    color: var(--sage);
  }

  .error {
    border-color: color-mix(in srgb, #c7534f 38%, rgba(255, 255, 255, 0.56));
  }

  .error .title {
    color: #a23b37;
  }

  .wave {
    width: 35px;
    height: 25px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 2px;
  }

  .wave i {
    width: 3px;
    min-height: 4px;
    border-radius: 99px;
    background: var(--fawn-deep);
    transition: height 90ms linear;
  }

  .spinner {
    width: 11px;
    height: 11px;
    border: 1.5px solid color-mix(in srgb, var(--fawn-deep) 25%, transparent);
    border-top-color: var(--fawn-deep);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }

  @keyframes appear {
    from {
      transform: translateY(7px) scale(0.96);
      opacity: 0;
    }
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
