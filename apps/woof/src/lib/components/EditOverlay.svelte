<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowUp, Check, Command, CornerDownLeft, Sparkles, X } from "lucide-svelte";
  import Mascot from "./Mascot.svelte";
  import {
    COMMANDS,
    EVENTS,
    type EditInitPayload,
    type EditStatePayload,
    type TranscriptionItemPayload,
    transcriptionItemFromPayload
  } from "$lib/contracts/ipc";
  import { invokeCommand, listenEvent } from "$lib/contracts/bridge";

  const FOCUS_DELAY_MS = 60;
  const IDLE_CLOSE_MS = 30_000;
  const TRANSCRIPTION_FALLBACK_MS = 8_000;

  let instruction = $state("");
  let submitting = $state(false);
  let scope = $state<"selection" | "draft">("selection");
  let editor = $state<HTMLTextAreaElement>();
  let error = $state("");
  let visible = $state(false);
  let leaving = $state(false);
  let glass = $state(false);
  let transcribing = $state(false);
  let transcript = $state("");
  let transcriptItems = $state<Array<{ item_id: string; text: string }>>([]);
  let nativeStateSeen = false;
  let idleTimer: number | null = null;
  let focusTimer: number | null = null;
  let transcriptionFallbackTimer: number | null = null;
  let heightFrame: number | null = null;

  function refusalMessage(reason: string): string {
    switch (reason) {
      case "secure-input":
        return "Inline rewriting is unavailable while secure keyboard input is active.";
      case "protected-content":
        return "This protected field cannot be rewritten.";
      case "permission-denied":
        return "Accessibility or Input Monitoring permission is required.";
      case "not-editable":
        return "The focused element is not editable.";
      default:
        return "Woof could not access the focused text.";
    }
  }

  function clearTimer(timer: number | null): void {
    if (timer !== null) window.clearTimeout(timer);
  }

  function resetIdleTimer(): void {
    clearTimer(idleTimer);
    idleTimer = null;
    if (!visible || submitting || transcribing) return;
    idleTimer = window.setTimeout(() => {
      idleTimer = null;
      void close("timeout");
    }, IDLE_CLOSE_MS);
  }

  function focusEditor(): void {
    clearTimer(focusTimer);
    focusTimer = window.setTimeout(() => {
      focusTimer = null;
      editor?.focus();
    }, FOCUS_DELAY_MS);
  }

  function reportContentHeight(): void {
    if (heightFrame !== null) return;
    heightFrame = window.requestAnimationFrame(() => {
      heightFrame = null;
      const inputHeight = editor?.scrollHeight ?? 20;
      const height = inputHeight + 20 + (error ? 26 : 0);
      void invokeCommand(COMMANDS.editSetContentHeight, { height }).catch(() => undefined);
    });
  }

  function updateTranscriptItem(payload: TranscriptionItemPayload): void {
    const item = transcriptionItemFromPayload(payload);
    if (!item) return;
    const index = transcriptItems.findIndex((candidate) => candidate.item_id === item.item_id);
    transcriptItems =
      index === -1
        ? [...transcriptItems, item]
        : transcriptItems.map((candidate, candidateIndex) =>
            candidateIndex === index ? item : candidate
          );
    transcript = transcriptItems
      .map((candidate) => candidate.text.trim())
      .filter(Boolean)
      .join(" ");
    reportContentHeight();
  }

  function finishTranscription(): void {
    transcribing = false;
    clearTimer(transcriptionFallbackTimer);
    if (transcript.trim()) {
      submitting = true;
      transcriptionFallbackTimer = window.setTimeout(() => {
        transcriptionFallbackTimer = null;
        if (submitting && !nativeStateSeen && !transcribing) {
          submitting = false;
          transcript = "";
          transcriptItems = [];
          resetIdleTimer();
          reportContentHeight();
        }
      }, TRANSCRIPTION_FALLBACK_MS);
    } else {
      resetIdleTimer();
    }
  }

  function failTranscription(message: string): void {
    transcribing = false;
    submitting = false;
    transcript = "";
    transcriptItems = [];
    clearTimer(transcriptionFallbackTimer);
    error = message;
    resetIdleTimer();
    reportContentHeight();
  }

  async function submit(): Promise<void> {
    const clean = instruction.trim();
    if (submitting) return;
    if (!clean) {
      return;
    }
    submitting = true;
    error = "";
    try {
      await invokeCommand(COMMANDS.editSubmit, {
        instruction: clean,
        scope
      });
    } catch (cause) {
      submitting = false;
      error = cause instanceof Error ? cause.message : String(cause || "The rewrite failed.");
      resetIdleTimer();
      reportContentHeight();
    }
  }

  async function close(reason: "blur" | "esc" | "timeout" | "button"): Promise<void> {
    clearTimer(idleTimer);
    await invokeCommand(COMMANDS.editClose, { reason }).catch(() => undefined);
  }

  onMount(() => {
    const unlisteners: Array<() => void> = [];
    let disposed = false;
    const listeners = [
      listenEvent<EditInitPayload>(EVENTS.editInit, (payload) => {
        glass = payload?.glass ?? false;
        visible = true;
        leaving = false;
        submitting = false;
        transcribing = false;
        instruction = "";
        transcript = "";
        transcriptItems = [];
        error = "";
        nativeStateSeen = false;
        focusEditor();
        resetIdleTimer();
        reportContentHeight();
      }),
      listenEvent<EditStatePayload>(EVENTS.editState, (payload) => {
        submitting = payload?.state === "thinking";
        nativeStateSeen = submitting;
        if (!submitting) {
          error = payload?.error ?? "";
          instruction = "";
          transcript = "";
          transcriptItems = [];
          focusEditor();
        }
        resetIdleTimer();
        reportContentHeight();
      }),
      listenEvent(EVENTS.editFadeout, () => {
        leaving = true;
        visible = false;
        clearTimer(idleTimer);
      }),
      listenEvent<{ scope?: "selection" | "draft" }>(EVENTS.editContext, (payload) => {
        if (payload?.scope === "selection" || payload?.scope === "draft") scope = payload.scope;
      }),
      listenEvent<{ reason?: string }>(EVENTS.inlineRefused, (payload) => {
        error = refusalMessage(payload?.reason ?? "");
        reportContentHeight();
      }),
      listenEvent(EVENTS.transcriptionStart, () => {
        if (!visible) return;
        transcribing = true;
        transcript = "";
        transcriptItems = [];
        error = "";
        clearTimer(idleTimer);
      }),
      listenEvent<TranscriptionItemPayload>(EVENTS.transcriptionPartial, (payload) => {
        if (!visible) return;
        updateTranscriptItem(payload);
      }),
      listenEvent<TranscriptionItemPayload>(EVENTS.transcriptionItemCompleted, (payload) => {
        if (!visible) return;
        updateTranscriptItem(payload);
      }),
      listenEvent(EVENTS.transcriptionDone, finishTranscription),
      listenEvent(EVENTS.transcriptionCancelled, () => failTranscription("Dictation cancelled.")),
      listenEvent(EVENTS.transcriptionFailed, () =>
        failTranscription("Dictation could not be completed.")
      ),
      listenEvent(EVENTS.transcriptionOverflow, () =>
        failTranscription("Dictation exceeded its safe size limit.")
      )
    ];
    const onKeydown = (event: KeyboardEvent) => {
      resetIdleTimer();
      if (event.key === "Escape" || (event.metaKey && event.key === ".")) {
        event.preventDefault();
        if (transcribing) {
          void invokeCommand(COMMANDS.transcriptionCancel).catch(() => undefined);
        }
        void close("esc");
      }
    };
    const onBlur = () => {
      if (visible && !submitting && !transcribing) void close("blur");
    };
    const colorScheme = window.matchMedia("(prefers-color-scheme: dark)");
    const reportGlass = () => {
      void invokeCommand(COMMANDS.editSetGlass, { dark: colorScheme.matches }).catch(
        () => undefined
      );
    };
    window.addEventListener("keydown", onKeydown);
    window.addEventListener("blur", onBlur);
    colorScheme.addEventListener("change", reportGlass);
    reportGlass();
    void Promise.all(listeners).then((resolved) => {
      if (disposed) {
        resolved.forEach((unlisten) => unlisten());
        return;
      }
      unlisteners.push(...resolved);
      return invokeCommand(COMMANDS.editReady);
    });
    return () => {
      disposed = true;
      window.removeEventListener("keydown", onKeydown);
      window.removeEventListener("blur", onBlur);
      colorScheme.removeEventListener("change", reportGlass);
      clearTimer(idleTimer);
      clearTimer(focusTimer);
      clearTimer(transcriptionFallbackTimer);
      if (transcribing) {
        void invokeCommand(COMMANDS.transcriptionCancel).catch(() => undefined);
      }
      if (heightFrame !== null) window.cancelAnimationFrame(heightFrame);
      unlisteners.forEach((unlisten) => unlisten());
    };
  });
</script>

<main class:visible class:leaving class:native-glass={glass} class="edit glass">
  <header class="drag-region">
    <div><Mascot size={43} mood={submitting ? "thinking" : "calm"} /><span><b>Rewrite with woof</b><small>{scope === "selection" ? "Selection" : "Whole draft"}</small></span></div>
    <button class="no-drag" onclick={() => void close("button")} aria-label="Close"><X size={14} /></button>
  </header>

  <section>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <label>
      <span class="visually-hidden">Rewrite instruction</span>
      <textarea
        bind:this={editor}
        bind:value={instruction}
        placeholder={transcribing ? transcript || "Listening…" : submitting ? "Working on it…" : "Make this warmer, clearer, shorter…"}
        disabled={submitting || transcribing}
        oninput={() => {
          resetIdleTimer();
          reportContentHeight();
        }}
        onkeydown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            void submit();
          }
        }}
      ></textarea>
      <button class:ready={instruction.trim().length > 0} disabled={!instruction.trim() || submitting} onclick={submit}>
        {#if submitting}<span class="spinner"></span>{:else}<ArrowUp size={16} />{/if}
      </button>
    </label>
    <div class="scope">
      <button class:active={scope === "selection"} onclick={() => (scope = "selection")}>
        {#if scope === "selection"}<Check size={11} />{/if} Selection
      </button>
      <button class:active={scope === "draft"} onclick={() => (scope = "draft")}>
        {#if scope === "draft"}<Check size={11} />{/if} Whole draft
      </button>
    </div>
  </section>

  <footer>
    <span><Sparkles size={11} /> Style learns only when you explicitly ask</span>
    <span><kbd><CornerDownLeft size={10} /></kbd> rewrite</span>
    <span><kbd><Command size={10} /> .</kbd> cancel</span>
  </footer>
</main>

<style>
  .edit {
    width: 100%;
    height: 100%;
    overflow: hidden;
    border-radius: 24px;
    background:
      radial-gradient(circle at 9% 0, rgba(231, 173, 117, 0.17), transparent 35%),
      var(--glass-strong);
    animation: appear 280ms var(--ease);
    opacity: 0;
    pointer-events: none;
  }

  .edit.visible {
    opacity: 1;
    pointer-events: auto;
  }

  .edit.leaving {
    opacity: 0;
    transition: opacity 150ms ease;
  }

  .edit.native-glass {
    background:
      radial-gradient(circle at 9% 0, rgba(231, 173, 117, 0.1), transparent 35%),
      color-mix(in srgb, var(--glass-strong) 90%, transparent);
  }

  header {
    height: 59px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 12px 0 9px;
    border-bottom: 1px solid var(--line);
  }

  header > div {
    display: flex;
    align-items: center;
    gap: 5px;
  }

  header b,
  header small {
    display: block;
  }

  header b {
    font-size: 10px;
  }

  header small {
    margin-top: 3px;
    color: var(--ink-faint);
    font-size: 7px;
  }

  header button {
    width: 27px;
    height: 27px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 9px;
    color: var(--ink-muted);
    background: color-mix(in srgb, var(--ink) 5%, transparent);
    cursor: pointer;
  }

  section {
    padding: 14px 15px 10px;
  }

  .error {
    margin: 0 0 8px;
    padding: 7px 9px;
    border: 1px solid color-mix(in srgb, #c7534f 35%, transparent);
    border-radius: 10px;
    color: #9f3430;
    background: color-mix(in srgb, #c7534f 9%, transparent);
    font-size: 9px;
    line-height: 1.35;
  }

  section > label {
    min-height: 89px;
    display: grid;
    grid-template-columns: 1fr 32px;
    align-items: end;
    padding: 10px;
    border: 1px solid var(--line-strong);
    border-radius: 15px;
    background: color-mix(in srgb, var(--cream) 79%, transparent);
    box-shadow: 0 5px 18px rgba(74, 50, 40, 0.07);
  }

  textarea {
    align-self: stretch;
    resize: none;
    padding: 2px 4px;
    border: 0;
    outline: 0;
    color: var(--ink);
    background: transparent;
    font-size: 11px;
    line-height: 1.45;
    user-select: text;
  }

  label button {
    width: 29px;
    height: 29px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 9px;
    color: var(--ink-faint);
    background: color-mix(in srgb, var(--ink) 6%, transparent);
    cursor: pointer;
  }

  label button.ready {
    color: #fff8f0;
    background: var(--brown);
  }

  .scope {
    display: flex;
    gap: 6px;
    margin-top: 8px;
  }

  .scope button {
    min-width: 68px;
    height: 25px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    padding: 0 8px;
    border: 1px solid transparent;
    border-radius: 8px;
    color: var(--ink-faint);
    background: transparent;
    font-size: 7.5px;
    cursor: pointer;
  }

  .scope button.active {
    border-color: color-mix(in srgb, var(--fawn) 24%, transparent);
    color: var(--fawn-deep);
    background: color-mix(in srgb, var(--fawn) 8%, transparent);
  }

  footer {
    height: 38px;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 16px;
    border-top: 1px solid var(--line);
    color: var(--ink-faint);
    font-size: 6.5px;
  }

  footer span {
    display: flex;
    align-items: center;
    gap: 5px;
  }

  footer span:first-child {
    flex: 1;
  }

  kbd {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    padding: 3px 5px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--cream);
  }

  .spinner {
    width: 12px;
    height: 12px;
    border: 1.5px solid rgba(255, 255, 255, 0.3);
    border-top-color: white;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }

  @keyframes appear {
    from {
      transform: translateY(9px) scale(0.97);
      opacity: 0;
    }
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
