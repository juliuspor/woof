<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowUp, Command, CornerDownLeft, Sparkles, X } from "lucide-svelte";
  import Mascot from "./Mascot.svelte";
  import {
    COMMANDS,
    EVENTS,
    type EditFadeoutPayload,
    type EditInitPayload,
    type EditStatePayload,
    type InlineContextState,
    type InlineEditMode,
    type InlineRefusedPayload,
    type TranscriptionItemPayload,
    transcriptionItemFromPayload
  } from "$lib/contracts/ipc";
  import { invokeCommand, listenEvent } from "$lib/contracts/bridge";

  const FOCUS_DELAY_MS = 60;
  const IDLE_CLOSE_MS = 30_000;
  const TRANSCRIPTION_FALLBACK_MS = 8_000;

  let instruction = $state("");
  let submitting = $state(false);
  let sessionId = $state(0);
  let mode = $state<InlineEditMode>("selection");
  let contextState = $state<InlineContextState>("unavailable");
  let contextReason = $state("");
  let replyPhase = $state<"reading" | "drafting" | "failed">("reading");
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
  let autoSubmittedReplySession = 0;

  function titleForMode(value: InlineEditMode): string {
    if (value === "reply") return "Draft a reply";
    if (value === "draft") return "Rewrite draft";
    return "Rewrite selection";
  }

  function subtitleForMode(value: InlineEditMode): string {
    if (value === "reply") return "Empty composer";
    if (value === "draft") return "Whole draft";
    return "Selected text";
  }

  function contextUnavailableCopy(): string {
    if (contextReason) return contextReason;
    if (mode === "reply") return "woof could not find recent on-screen context for a reply.";
    return "Recent on-screen context is unavailable; only your text will be used.";
  }

  function footerContextCopy(): string {
    if (mode === "reply") {
      return contextState === "available"
        ? "Uses recent on-screen context for this draft"
        : "No reply context was used";
    }
    return mode === "selection"
      ? "Uses selected text and local rewrite examples"
      : "Uses this draft and local rewrite examples";
  }

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
      const bodyHeight = mode === "reply" ? 72 : (editor?.scrollHeight ?? 20) + 20;
      const unavailableNote = contextState === "unavailable" ? 26 : 0;
      const height = bodyHeight + unavailableNote + (error ? 26 : 0);
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
    if (mode === "reply") return;
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

  async function submit(requestedInstruction = instruction): Promise<void> {
    const clean = requestedInstruction.trim();
    if (submitting) return;
    if (!Number.isSafeInteger(sessionId) || sessionId <= 0) return;
    if (mode === "reply" && contextState !== "available") return;
    if (mode !== "reply" && !clean) return;
    const requestedSessionId = sessionId;
    submitting = true;
    error = "";
    if (mode === "reply") replyPhase = "reading";
    try {
      await invokeCommand(COMMANDS.editSubmit, {
        sessionId: requestedSessionId,
        instruction: mode === "reply" ? "" : clean
      });
    } catch (cause) {
      if (sessionId !== requestedSessionId) return;
      submitting = false;
      if (mode === "reply") replyPhase = "failed";
      error = cause instanceof Error ? cause.message : String(cause || "The edit failed.");
      resetIdleTimer();
      reportContentHeight();
    }
  }

  function applyInit(payload: EditInitPayload): void {
    if (
      !payload ||
      !Number.isSafeInteger(payload.session_id) ||
      payload.session_id <= 0 ||
      !["reply", "selection", "draft"].includes(payload.mode) ||
      !["available", "unavailable"].includes(payload.context_state)
    ) {
      return;
    }
    if (sessionId > 0 && payload.session_id < sessionId) return;

    const sameSession = payload.session_id === sessionId;
    if (
      sameSession &&
      contextState === "available" &&
      payload.context_state === "unavailable"
    ) {
      return;
    }
    sessionId = payload.session_id;
    mode = payload.mode;
    contextState = payload.context_state;
    contextReason =
      typeof payload.context_reason === "string" ? payload.context_reason.trim().slice(0, 320) : "";
    glass = payload.glass ?? false;
    visible = true;
    leaving = false;

    if (!sameSession) {
      submitting = false;
      transcribing = false;
      instruction = "";
      transcript = "";
      transcriptItems = [];
      error = "";
      nativeStateSeen = false;
      replyPhase = "reading";
    }

    clearTimer(focusTimer);
    focusTimer = null;
    resetIdleTimer();
    reportContentHeight();

    if (mode === "reply") {
      if (contextState === "available" && autoSubmittedReplySession !== sessionId) {
        autoSubmittedReplySession = sessionId;
        void submit("");
      }
      return;
    }
    focusEditor();
  }

  async function close(reason: "blur" | "esc" | "timeout" | "button"): Promise<void> {
    clearTimer(idleTimer);
    if (!Number.isSafeInteger(sessionId) || sessionId <= 0) return;
    const closingSessionId = sessionId;
    await invokeCommand(COMMANDS.editClose, {
      sessionId: closingSessionId,
      reason
    }).catch(() => undefined);
  }

  onMount(() => {
    const unlisteners: Array<() => void> = [];
    let disposed = false;
    const listeners = [
      listenEvent<EditInitPayload>(EVENTS.editInit, applyInit),
      listenEvent<EditStatePayload>(EVENTS.editState, (payload) => {
        if (payload?.session_id !== sessionId) return;
        submitting = payload?.state === "thinking";
        nativeStateSeen = submitting;
        if (mode === "reply" && submitting) replyPhase = "drafting";
        if (!submitting) {
          error = payload?.error ?? "";
          instruction = "";
          transcript = "";
          transcriptItems = [];
          if (mode === "reply") replyPhase = "failed";
          else focusEditor();
        }
        resetIdleTimer();
        reportContentHeight();
      }),
      listenEvent<EditFadeoutPayload>(EVENTS.editFadeout, (payload) => {
        if (payload?.session_id !== sessionId) return;
        leaving = true;
        visible = false;
        clearTimer(idleTimer);
      }),
      listenEvent<InlineRefusedPayload>(EVENTS.inlineRefused, (payload) => {
        if (payload?.session_id !== undefined && payload.session_id !== sessionId) return;
        error = refusalMessage(payload?.reason ?? "");
        reportContentHeight();
      }),
      listenEvent(EVENTS.transcriptionStart, () => {
        if (!visible || mode === "reply") return;
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
    <div><Mascot size={43} mood={submitting ? "thinking" : "calm"} /><span><b>{titleForMode(mode)}</b><small>{subtitleForMode(mode)}</small></span></div>
    <button class="no-drag" onclick={() => void close("button")} aria-label="Close"><X size={14} /></button>
  </header>

  <section>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if mode === "reply"}
      {#if contextState === "available"}
        <div class:failed={replyPhase === "failed"} class="reply-status" role="status" aria-live="polite">
          <Sparkles size={16} />
          <span>
            <b>{replyPhase === "drafting" ? "Drafting your reply…" : replyPhase === "failed" ? "Reply not inserted" : "Reading recent context…"}</b>
            <small>{replyPhase === "failed" ? "Double-tap again to retry." : "woof will insert a draft into the empty composer."}</small>
          </span>
        </div>
      {:else}
        <div class="context-unavailable" role="alert">
          <b>Reply context unavailable</b>
          <span>{contextUnavailableCopy()}</span>
          <small>Keep the conversation visible, then double-tap again to retry.</small>
        </div>
      {/if}
    {:else}
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
        <button
          aria-label="Rewrite and insert"
          class:ready={instruction.trim().length > 0}
          disabled={!instruction.trim() || submitting || transcribing}
          onclick={() => void submit()}
        >
          {#if submitting}<span class="spinner"></span>{:else}<ArrowUp size={16} />{/if}
        </button>
      </label>
    {/if}
  </section>

  <footer>
    <span><Sparkles size={11} /> {footerContextCopy()}</span>
    {#if mode !== "reply"}<span><kbd><CornerDownLeft size={10} /></kbd> insert only</span>{/if}
    <span>woof never sends</span>
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

  .reply-status,
  .context-unavailable {
    min-height: 89px;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px;
    border: 1px solid var(--line-strong);
    border-radius: 15px;
    background: color-mix(in srgb, var(--cream) 79%, transparent);
    box-shadow: 0 5px 18px rgba(74, 50, 40, 0.07);
  }

  .reply-status > span,
  .context-unavailable > span,
  .reply-status b,
  .reply-status small,
  .context-unavailable b,
  .context-unavailable small {
    display: block;
  }

  .reply-status > span {
    min-width: 0;
  }

  .reply-status b,
  .context-unavailable b {
    color: var(--ink);
    font-size: 10px;
  }

  .reply-status small,
  .context-unavailable span,
  .context-unavailable small {
    margin-top: 4px;
    color: var(--ink-faint);
    font-size: 8px;
    line-height: 1.4;
  }

  .reply-status.failed {
    border-color: color-mix(in srgb, #c7534f 28%, transparent);
  }

  .context-unavailable {
    align-items: flex-start;
    flex-direction: column;
    justify-content: center;
    gap: 0;
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

  label button:disabled {
    cursor: default;
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
