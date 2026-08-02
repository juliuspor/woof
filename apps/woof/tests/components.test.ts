import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { get } from "svelte/store";
import { afterEach, describe, expect, it, vi } from "vitest";
import Companion from "../src/lib/components/Companion.svelte";
import CaretOverlay from "../src/lib/components/CaretOverlay.svelte";
import MemoryHub from "../src/lib/components/MemoryHub.svelte";
import EditOverlay from "../src/lib/components/EditOverlay.svelte";
import HealthBadge from "../src/lib/components/HealthBadge.svelte";
import HealthRecovery from "../src/lib/components/HealthRecovery.svelte";
import Onboarding from "../src/lib/components/Onboarding.svelte";
import SettingsPanel from "../src/lib/components/SettingsPanel.svelte";
import * as bridge from "../src/lib/contracts/bridge";
import { MOTION, WINDOWS } from "../src/lib/contracts/geometry";
import { COMMANDS, EVENTS } from "../src/lib/contracts/ipc";
import { appState, resetState, updateState } from "../src/lib/state/app";

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  cleanup();
  resetState();
  window.localStorage.clear();
});

describe("desktop surfaces", () => {
  it("starts onboarding with local-first product copy", () => {
    render(Onboarding);
    expect(screen.getByRole("heading", { name: /A second memory/i })).toBeInTheDocument();
    expect(screen.getByText(/keeps its memory local/i)).toBeInTheDocument();
    expect(screen.getByText("1 of 7")).toBeInTheDocument();
  });

  it("discloses periodic remote memory summaries before accepting an API key", async () => {
    window.localStorage.setItem("woof:command:accessibility_trusted", "true");
    window.localStorage.setItem("woof:command:input_monitoring_trusted", "true");
    window.localStorage.setItem("woof:command:microphone_status", '"authorized"');
    render(Onboarding);

    await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(screen.getByText(/Capture runs locally and does not need OpenAI/i)).toBeInTheDocument();
    expect(
      screen.getByText(/periodically sends selected captured text to OpenAI for memory summarization/i)
    ).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    await screen.findByText("Local capture is ready");
    await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByText("Microphone is on")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(
      screen.getByRole("heading", { name: /Bring your own\s+API key/i })
    ).toBeInTheDocument();
    expect(
      screen.getByText(/periodic OpenAI summaries for chronicles, wiki pages, and time rules/i)
    ).toBeInTheDocument();
    expect(screen.getByText(/chat, rewriting, and transcription when you request them/i)).toBeInTheDocument();
  });

  it("renders the companion geometry states", () => {
    const collapsed = render(Companion, { mode: "collapsed" });
    const collapsedShell = screen.getByTestId("companion-shell");
    const collapsedButton = screen.getByRole("button", { name: "Open woof" });
    expect(collapsedShell).toHaveAttribute("data-state", "collapsed");
    expect(collapsedButton).toBeEmptyDOMElement();
    expect(collapsedShell.querySelector("[role='img']")).toBeNull();
    collapsed.unmount();

    render(Companion, { mode: "hidden" });
    expect(screen.getByTestId("companion-shell")).toHaveAttribute("data-state", "hidden");
    expect(screen.queryByRole("button", { name: "Open woof" })).not.toBeInTheDocument();
  });

  it("opens and closes the passive companion on hover when enabled", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem(
      `woof:command:${COMMANDS.companionGetHoverOpen}`,
      "true"
    );
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Companion, { mode: "collapsed" });
    const shell = screen.getByTestId("companion-shell");
    await vi.advanceTimersByTimeAsync(0);

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: true }));
    await fireEvent.mouseEnter(shell);
    await vi.advanceTimersByTimeAsync(0);
    expect(invoke).toHaveBeenCalledWith(COMMANDS.companionSetState, {
      state: "expanded",
      requestId: expect.any(Number)
    });
    expect(
      invoke.mock.calls.some(([command]) => command === COMMANDS.companionOpenFocused)
    ).toBe(false);
    expect(
      invoke.mock.calls.filter(([command]) => command === COMMANDS.companionSetState)
    ).toHaveLength(1);
    expect(shell).toHaveAttribute("data-state", "expanded");
    const chat = screen.getByRole("region", { name: "woof chat" });
    expect(chat).toHaveClass("visible");
    expect(screen.getByRole("textbox", { name: "Message woof" })).not.toHaveFocus();

    await fireEvent.mouseLeave(shell);
    await vi.advanceTimersByTimeAsync(MOTION.hoverCloseDelay);
    expect(shell).toHaveAttribute("data-state", "expanded");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: false }));
    await vi.advanceTimersByTimeAsync(119);
    expect(shell).toHaveAttribute("data-state", "expanded");
    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: true }));
    await vi.advanceTimersByTimeAsync(1);
    expect(shell).toHaveAttribute("data-state", "expanded");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: false }));
    await vi.advanceTimersByTimeAsync(120);
    expect(shell).toHaveAttribute("data-state", "collapsed");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: true }));
    await vi.advanceTimersByTimeAsync(0);
    expect(shell).toHaveAttribute("data-state", "expanded");
  });

  it("opens when native pointer truth arrives before the hover preference", async () => {
    vi.useFakeTimers();
    const nativeInvoke = bridge.invokeCommand;
    let resolveHover!: (enabled: boolean) => void;
    const invoke = vi.spyOn(bridge, "invokeCommand").mockImplementation(
      (command, args = {}) => {
        if (command === COMMANDS.companionGetHoverOpen) {
          return new Promise<boolean>((resolve) => {
            resolveHover = resolve;
          });
        }
        if (command === COMMANDS.companionGetPosition) {
          return Promise.reject(new Error("position unavailable"));
        }
        return nativeInvoke(command, args);
      }
    );
    render(Companion, { mode: "collapsed" });
    await vi.advanceTimersByTimeAsync(0);

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: true }));
    await vi.advanceTimersByTimeAsync(0);
    expect(
      invoke.mock.calls.some(([command]) => command === COMMANDS.companionSetState)
    ).toBe(false);

    resolveHover(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(invoke).toHaveBeenCalledWith(COMMANDS.companionSetState, {
      state: "expanded",
      requestId: expect.any(Number)
    });
    expect(screen.getByTestId("companion-shell")).toHaveAttribute(
      "data-state",
      "expanded"
    );
  });

  it("opens from the listener-ready native pointer snapshot without a DOM hover", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem("woof:test:pointer-inside", "true");
    const invoke = vi.spyOn(bridge, "invokeCommand");

    render(Companion, { mode: "collapsed" });
    await vi.advanceTimersByTimeAsync(0);

    expect(invoke).toHaveBeenCalledWith(COMMANDS.companionPointerReady);
    expect(screen.getByTestId("companion-shell")).toHaveAttribute(
      "data-state",
      "expanded"
    );
  });

  it("keeps a manual collapse closed until the pointer leaves and re-enters", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem("woof:test:pointer-inside", "true");
    render(Companion, { mode: "collapsed" });
    await vi.advanceTimersByTimeAsync(0);
    const shell = screen.getByTestId("companion-shell");
    expect(shell).toHaveAttribute("data-state", "expanded");

    await fireEvent.click(screen.getByRole("button", { name: "Collapse" }));
    await vi.advanceTimersByTimeAsync(0);
    expect(shell).toHaveAttribute("data-state", "collapsed");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: true }));
    await vi.advanceTimersByTimeAsync(MOTION.hoverCloseDelay * 2);
    expect(shell).toHaveAttribute("data-state", "collapsed");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: false }));
    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: true }));
    await vi.advanceTimersByTimeAsync(0);
    expect(shell).toHaveAttribute("data-state", "expanded");
  });

  it("serializes a deferred hover open behind the newer collapse intent", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem("woof:test:pointer-inside", "true");
    const nativeInvoke = bridge.invokeCommand;
    let resolveOpen!: () => void;
    const invoke = vi.spyOn(bridge, "invokeCommand").mockImplementation(
      (command, args = {}) => {
        if (command === COMMANDS.companionSetState) {
          return new Promise<void>((resolve) => {
            resolveOpen = resolve;
          });
        }
        return nativeInvoke(command, args);
      }
    );
    render(Companion, { mode: "collapsed" });
    await vi.advanceTimersByTimeAsync(0);
    const shell = screen.getByTestId("companion-shell");
    expect(shell).toHaveAttribute("data-state", "expanded");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: false }));
    await vi.advanceTimersByTimeAsync(MOTION.hoverCloseDelay);
    expect(shell).toHaveAttribute("data-state", "collapsed");
    expect(
      invoke.mock.calls.some(([command]) => command === COMMANDS.companionRollup)
    ).toBe(false);

    resolveOpen();
    await vi.advanceTimersByTimeAsync(0);
    expect(
      invoke.mock.calls.some(([command]) => command === COMMANDS.companionRollup)
    ).toBe(true);
    expect(shell).toHaveAttribute("data-state", "collapsed");
  });

  it("reopens after a deferred collapse and ignores its stale completion", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem("woof:test:pointer-inside", "true");
    const nativeInvoke = bridge.invokeCommand;
    let resolveCollapse!: () => void;
    const invoke = vi.spyOn(bridge, "invokeCommand").mockImplementation(
      (command, args = {}) => {
        if (command === COMMANDS.companionRollup) {
          return new Promise<void>((resolve) => {
            resolveCollapse = resolve;
          });
        }
        return nativeInvoke(command, args);
      }
    );
    render(Companion, { mode: "collapsed" });
    await vi.advanceTimersByTimeAsync(0);
    const shell = screen.getByTestId("companion-shell");
    expect(shell).toHaveAttribute("data-state", "expanded");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: false }));
    await vi.advanceTimersByTimeAsync(MOTION.hoverCloseDelay);
    expect(shell).toHaveAttribute("data-state", "collapsed");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: true }));
    await vi.advanceTimersByTimeAsync(0);
    expect(shell).toHaveAttribute("data-state", "expanded");
    expect(
      invoke.mock.calls.filter(([command]) => command === COMMANDS.companionSetState)
    ).toHaveLength(1);

    resolveCollapse();
    await vi.advanceTimersByTimeAsync(0);
    expect(
      invoke.mock.calls.filter(([command]) => command === COMMANDS.companionSetState)
    ).toHaveLength(2);
    expect(shell).toHaveAttribute("data-state", "expanded");
  });

  it("retracts a click-opened companion after the native pointer leaves", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem(
      `woof:command:${COMMANDS.companionGetHoverOpen}`,
      "false"
    );
    window.localStorage.setItem("woof:test:pointer-inside", "true");
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Companion, { mode: "collapsed" });
    await vi.advanceTimersByTimeAsync(0);

    await fireEvent.click(screen.getByRole("button", { name: "Open woof" }), { detail: 1 });
    await vi.advanceTimersByTimeAsync(0);
    const shell = screen.getByTestId("companion-shell");
    expect(shell).toHaveAttribute("data-state", "expanded");

    window.dispatchEvent(new CustomEvent(EVENTS.companionPointer, { detail: false }));
    await vi.advanceTimersByTimeAsync(MOTION.hoverCloseDelay);
    expect(invoke).toHaveBeenCalledWith(COMMANDS.companionRollup, {
      durationMs: MOTION.panelExit,
      requestId: expect.any(Number)
    });
    expect(shell).toHaveAttribute("data-state", "collapsed");
  });

  it("focuses externally requested chat and keeps it open on pointer leave", async () => {
    vi.useFakeTimers();
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Companion, { mode: "collapsed" });
    const shell = screen.getByTestId("companion-shell");
    await Promise.resolve();

    window.dispatchEvent(new CustomEvent("woof:open-chat"));
    await vi.advanceTimersByTimeAsync(30);

    expect(invoke).toHaveBeenCalledWith(COMMANDS.companionOpenFocused, {
      requestId: expect.any(Number)
    });
    expect(shell).toHaveAttribute("data-state", "expanded");
    expect(screen.getByRole("textbox", { name: "Message woof" })).toHaveFocus();

    await fireEvent.mouseEnter(shell);
    await fireEvent.mouseLeave(shell);
    await vi.advanceTimersByTimeAsync(MOTION.hoverCloseDelay + MOTION.panelExit);
    expect(shell).toHaveAttribute("data-state", "expanded");
  });

  it("applies open-chat prefill and autosend without exposing opaque attachments", async () => {
    vi.useFakeTimers();
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Companion, { mode: "collapsed" });
    await vi.advanceTimersByTimeAsync(0);

    window.dispatchEvent(
      new CustomEvent(EVENTS.openChat, {
        detail: {
          attachment: "/private/opaque-reference",
          prefill: "Summarize this safely",
          auto_send: true,
          source: "inline"
        }
      })
    );
    await vi.advanceTimersByTimeAsync(30);

    expect(screen.getByRole("textbox", { name: "Message woof" })).toHaveFocus();
    expect(invoke).toHaveBeenCalledWith(COMMANDS.chatSend, {
      request: {
        text: "Summarize this safely",
        threadId: expect.stringMatching(
          /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/
        ),
        history: [],
        mode: "chat"
      }
    });
    expect(document.body.textContent).not.toContain("/private/opaque-reference");
    const serializedCalls = JSON.stringify(invoke.mock.calls);
    expect(serializedCalls).not.toContain("/private/opaque-reference");
  });

  it("sends bounded prior turns on one thread and resets them for a new chat", async () => {
    window.localStorage.setItem("woof:command:chat_send", JSON.stringify("First answer"));
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Companion, { mode: "expanded" });
    const textbox = screen.getByRole("textbox", { name: "Message woof" });

    await fireEvent.input(textbox, { target: { value: "First question" } });
    await fireEvent.keyDown(textbox, { key: "Enter" });
    await screen.findByText("First answer");
    await waitFor(() => expect(textbox).not.toBeDisabled());

    window.localStorage.setItem("woof:command:chat_send", JSON.stringify("Second answer"));
    await fireEvent.input(textbox, { target: { value: "Second question" } });
    await fireEvent.keyDown(textbox, { key: "Enter" });
    await screen.findByText("Second answer");
    await waitFor(() => expect(textbox).not.toBeDisabled());

    const chatCalls = invoke.mock.calls.filter(([command]) => command === COMMANDS.chatSend);
    const firstRequest = chatCalls[0]?.[1]?.request as {
      threadId: string;
      history: Array<{ role: string; content: string }>;
    };
    const secondRequest = chatCalls[1]?.[1]?.request as {
      threadId: string;
      history: Array<{ role: string; content: string }>;
    };
    expect(firstRequest.history).toEqual([]);
    expect(secondRequest.threadId).toBe(firstRequest.threadId);
    expect(secondRequest.history).toEqual([
      { role: "user", content: "First question" },
      { role: "assistant", content: "First answer" }
    ]);

    await fireEvent.click(screen.getByRole("button", { name: "New chat" }));
    window.localStorage.setItem("woof:command:chat_send", JSON.stringify("Fresh answer"));
    await fireEvent.input(textbox, { target: { value: "Fresh question" } });
    await fireEvent.keyDown(textbox, { key: "Enter" });
    await screen.findByText("Fresh answer");

    const refreshedCalls = invoke.mock.calls.filter(([command]) => command === COMMANDS.chatSend);
    const freshRequest = refreshedCalls[2]?.[1]?.request as {
      threadId: string;
      history: Array<{ role: string; content: string }>;
    };
    expect(freshRequest.threadId).not.toBe(firstRequest.threadId);
    expect(freshRequest.history).toEqual([]);
  });

  it("reconciles item-aware transcription corrections and cancels on Escape", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Companion, { mode: "expanded" });
    await Promise.resolve();

    window.dispatchEvent(
      new CustomEvent(EVENTS.transcriptionStart, { detail: { hands_free: false } })
    );
    window.dispatchEvent(
      new CustomEvent(EVENTS.transcriptionPartial, {
        detail: { item_id: "first", text: "Hello" }
      })
    );
    window.dispatchEvent(
      new CustomEvent(EVENTS.transcriptionPartial, {
        detail: { item_id: "second", text: "world" }
      })
    );
    window.dispatchEvent(
      new CustomEvent(EVENTS.transcriptionPartial, {
        detail: { item_id: "first", text: "Hallo" }
      })
    );

    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Message woof" })).toHaveValue("Hallo world")
    );
    await fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(COMMANDS.transcriptionCancel)
    );
  });

  it("cancels active transcription when the companion is torn down", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    const companion = render(Companion, { mode: "expanded" });
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.transcriptionStart, { detail: { hands_free: false } })
    );

    companion.unmount();
    expect(invoke).toHaveBeenCalledWith(COMMANDS.transcriptionCancel);
  });

  it("filters stale caret status and cancels only the active session", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(CaretOverlay);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith(COMMANDS.caretReady));

    window.dispatchEvent(
      new CustomEvent(EVENTS.caretInit, {
        detail: { session_id: 7, status: "Selection ready" }
      })
    );
    window.dispatchEvent(
      new CustomEvent(EVENTS.caretStatus, {
        detail: { session_id: 8, text: "stale status" }
      })
    );
    await Promise.resolve();
    expect(screen.queryByText("stale status")).not.toBeInTheDocument();

    window.dispatchEvent(
      new CustomEvent(EVENTS.caretStatus, {
        detail: { session_id: 7, text: "Working on it…" }
      })
    );
    expect(await screen.findByText("Working on it…")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(invoke).toHaveBeenCalledWith(COMMANDS.caretCancel, { sessionId: 7 });
  });

  it("resets a newer caret session and ignores stale init and fade events", async () => {
    const view = render(CaretOverlay);
    await waitFor(() =>
      expect(screen.getByText("What should I change?")).toBeInTheDocument()
    );
    const overlay = view.container.querySelector("main");

    window.dispatchEvent(
      new CustomEvent(EVENTS.caretInit, {
        detail: { session_id: 70, status: "Selection ready" }
      })
    );
    window.dispatchEvent(
      new CustomEvent(EVENTS.inlineRefused, {
        detail: { session_id: 70, reason: "focus-unavailable" }
      })
    );
    expect(await screen.findByText("Couldn’t continue")).toBeInTheDocument();

    window.dispatchEvent(
      new CustomEvent(EVENTS.caretInit, {
        detail: { session_id: 71, status: "Reading this chat…" }
      })
    );
    expect(await screen.findByText("What should I change?")).toBeInTheDocument();
    expect(screen.getByText("Reading this chat…")).toBeInTheDocument();
    expect(screen.queryByText("Couldn’t continue")).not.toBeInTheDocument();
    expect(overlay).toHaveClass("visible");
    expect(overlay).not.toHaveClass("error");

    window.dispatchEvent(
      new CustomEvent(EVENTS.inlineRefused, {
        detail: { session_id: 71, reason: "delivery-unconfirmed" }
      })
    );
    expect(
      await screen.findByText(
        "Couldn’t confirm the draft was inserted. Review the composer before retrying."
      )
    ).toBeInTheDocument();

    window.dispatchEvent(
      new CustomEvent(EVENTS.caretInit, {
        detail: { session_id: 70, status: "stale init" }
      })
    );
    window.dispatchEvent(
      new CustomEvent(EVENTS.caretFadeout, { detail: { session_id: 70 } })
    );
    expect(screen.queryByText("stale init")).not.toBeInTheDocument();
    expect(overlay).toHaveClass("visible");

    window.dispatchEvent(
      new CustomEvent(EVENTS.caretFadeout, { detail: { session_id: 71 } })
    );
    await waitFor(() => expect(overlay).not.toHaveClass("visible"));
  });

  it("implements rewrite focus, idle close, inert blank submit, and transcription fallback", async () => {
    vi.useFakeTimers();
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(EditOverlay);
    await vi.advanceTimersByTimeAsync(0);
    expect(invoke).toHaveBeenCalledWith(COMMANDS.editReady);

    window.dispatchEvent(
      new CustomEvent(EVENTS.editInit, {
        detail: {
          glass: true,
          session_id: 1,
          mode: "selection",
          context_state: "unavailable",
          context_reason: "Recent context could not be read."
        }
      })
    );
    expect(screen.getByText("Rewrite selection")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Selection" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Whole draft" })).not.toBeInTheDocument();
    const editor = screen.getByRole("textbox", { name: "Rewrite instruction" });
    await vi.advanceTimersByTimeAsync(59);
    expect(editor).not.toHaveFocus();
    await vi.advanceTimersByTimeAsync(1);
    expect(editor).toHaveFocus();

    window.dispatchEvent(
      new CustomEvent(EVENTS.transcriptionStart, { detail: { hands_free: false } })
    );
    window.dispatchEvent(
      new CustomEvent(EVENTS.transcriptionPartial, {
        detail: { item_id: "instruction", text: "make it clear" }
      })
    );
    window.dispatchEvent(new CustomEvent(EVENTS.transcriptionDone));
    await vi.advanceTimersByTimeAsync(0);
    expect(editor).toBeDisabled();
    await vi.advanceTimersByTimeAsync(7_999);
    expect(editor).toBeDisabled();
    await vi.advanceTimersByTimeAsync(1);
    expect(editor).not.toBeDisabled();

    const callsBeforeBlankSubmit = invoke.mock.calls.length;
    await fireEvent.keyDown(editor, { key: "Enter" });
    expect(invoke).toHaveBeenCalledTimes(callsBeforeBlankSubmit);

    window.dispatchEvent(
      new CustomEvent(EVENTS.editInit, {
        detail: {
          session_id: 2,
          mode: "draft",
          context_state: "available"
        }
      })
    );
    expect(await screen.findByText("Rewrite draft")).toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(invoke).toHaveBeenCalledWith(COMMANDS.editClose, {
      sessionId: 2,
      reason: "timeout"
    });
  });

  it("automatically drafts an available contextual reply once per session without sending", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(EditOverlay);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith(COMMANDS.editReady));

    const init = {
      glass: false,
      session_id: 41,
      mode: "reply" as const,
      context_state: "available" as const,
      context_reason: null
    };
    window.dispatchEvent(new CustomEvent(EVENTS.editInit, { detail: init }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(COMMANDS.editSubmit, {
        sessionId: 41,
        instruction: ""
      })
    );
    expect(await screen.findByText("Draft a reply")).toBeInTheDocument();
    expect(screen.getByText("Drafting your reply…")).toBeInTheDocument();
    expect(screen.getByText(/insert a draft into the empty composer/i)).toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.getByText("woof never sends")).toBeInTheDocument();

    window.dispatchEvent(new CustomEvent(EVENTS.editInit, { detail: init }));
    await Promise.resolve();
    expect(
      invoke.mock.calls.filter(([command]) => command === COMMANDS.editSubmit)
    ).toEqual([[COMMANDS.editSubmit, { sessionId: 41, instruction: "" }]]);
    window.dispatchEvent(
      new CustomEvent(EVENTS.editInit, {
        detail: { ...init, context_state: "unavailable", context_reason: "stale" }
      })
    );
    expect(screen.queryByText("Reply context unavailable")).not.toBeInTheDocument();
    expect(invoke.mock.calls.some(([command]) => command === COMMANDS.chatSend)).toBe(false);
  });

  it("keeps an unavailable contextual reply inert and explains how to retry", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(EditOverlay);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith(COMMANDS.editReady));

    window.dispatchEvent(
      new CustomEvent(EVENTS.editInit, {
        detail: {
          session_id: 42,
          mode: "reply",
          context_state: "unavailable",
          context_reason: "No recent conversation was visible."
        }
      })
    );

    expect(await screen.findByText("Draft a reply")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("No recent conversation was visible.");
    expect(screen.getByRole("alert")).toHaveTextContent(/double-tap again to retry/i);
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(invoke.mock.calls.some(([command]) => command === COMMANDS.editSubmit)).toBe(false);
  });

  it("uses Return only to insert rewrites and leaves Shift-Return to the textarea", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(EditOverlay);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith(COMMANDS.editReady));
    window.dispatchEvent(
      new CustomEvent(EVENTS.editInit, {
        detail: {
          session_id: 43,
          mode: "draft",
          context_state: "available"
        }
      })
    );

    const editor = screen.getByRole("textbox", { name: "Rewrite instruction" });
    await fireEvent.input(editor, { target: { value: "make it concise" } });
    const propagates = ["bub", "bles"].join("");
    const shiftedReturn = new KeyboardEvent("keydown", {
      key: "Enter",
      shiftKey: true,
      [propagates]: true,
      cancelable: true
    } as KeyboardEventInit);
    expect(editor.dispatchEvent(shiftedReturn)).toBe(true);
    expect(shiftedReturn.defaultPrevented).toBe(false);
    expect(invoke.mock.calls.some(([command]) => command === COMMANDS.editSubmit)).toBe(false);

    const plainReturn = new KeyboardEvent("keydown", {
      key: "Enter",
      [propagates]: true,
      cancelable: true
    } as KeyboardEventInit);
    expect(editor.dispatchEvent(plainReturn)).toBe(false);
    expect(plainReturn.defaultPrevented).toBe(true);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(COMMANDS.editSubmit, {
        sessionId: 43,
        instruction: "make it concise"
      })
    );
    expect(invoke.mock.calls.some(([command]) => command === COMMANDS.chatSend)).toBe(false);
    expect(screen.getByText("woof never sends")).toBeInTheDocument();
  });

  it("ignores edit state and fade events from stale sessions", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    const view = render(EditOverlay);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith(COMMANDS.editReady));
    window.dispatchEvent(
      new CustomEvent(EVENTS.editInit, {
        detail: {
          session_id: 51,
          mode: "draft",
          context_state: "available"
        }
      })
    );
    await screen.findByText("Rewrite draft");
    const editor = screen.getByRole("textbox", { name: "Rewrite instruction" });
    const main = view.container.querySelector("main");
    await waitFor(() => expect(main).toHaveClass("visible"));

    window.dispatchEvent(
      new CustomEvent(EVENTS.editState, {
        detail: { session_id: 50, state: "error", error: "stale failure" }
      })
    );
    window.dispatchEvent(
      new CustomEvent(EVENTS.editFadeout, { detail: { session_id: 50 } })
    );
    expect(screen.queryByText("stale failure")).not.toBeInTheDocument();
    expect(editor).not.toBeDisabled();
    expect(main).toHaveClass("visible");

    window.dispatchEvent(
      new CustomEvent(EVENTS.editState, {
        detail: { session_id: 51, state: "thinking" }
      })
    );
    await waitFor(() => expect(editor).toBeDisabled());
  });

  it("keeps settings mounted through the close transition", async () => {
    vi.useFakeTimers();
    render(Companion, { mode: "expanded" });
    await vi.advanceTimersByTimeAsync(MOTION.expandedBodyDelay);

    await fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.getByTestId("companion-settings")).toHaveAttribute("data-state", "open");

    await fireEvent.click(screen.getByRole("button", { name: "Return to chat" }));
    const closingSettings = screen.getByTestId("companion-settings");
    expect(closingSettings).toHaveAttribute("data-state", "closing");
    expect(closingSettings).toHaveClass("closing");
    expect(closingSettings.style.transitionDuration).toBe(`${MOTION.settingsClose}ms`);

    await vi.advanceTimersByTimeAsync(MOTION.settingsClose - 1);
    expect(screen.getByTestId("companion-settings")).toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(1);
    expect(screen.queryByTestId("companion-settings")).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "woof chat" })).toBeInTheDocument();
  });

  it("loads chat suggestions and working memory through live commands", async () => {
    window.localStorage.setItem(
      "woof:command:generate_chat_suggestions",
      JSON.stringify(["Recall the customer decision"])
    );
    render(Companion, { mode: "expanded" });

    expect(
      await screen.findByRole("button", { name: "Recall the customer decision" })
    ).toBeInTheDocument();
    expect(screen.queryByText("woof launch checklist")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Memory" }));
    expect(await screen.findByRole("region", { name: "Working memory" })).toBeInTheDocument();
    expect(await screen.findByText("woof — desktop")).toBeInTheDocument();
  });

  it("renders and dismisses daemon-supplied nudges without hard-coded content", async () => {
    const firstNudgeId = "0194f3cb-16d8-7f10-a922-4379a7c54d31";
    const secondNudgeId = "0194f3cb-16d8-7f10-a922-4379a7c54d32";
    render(Companion, { mode: "expanded" });
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent("woof:nudge-ready", {
        detail: {
          nudge_id: firstNudgeId,
          title: "A local follow-up is due",
          body: "Review the decision captured in Notes.",
          deep_link: "woof://chat?prompt=Review%20the%20decision"
        }
      })
    );
    window.dispatchEvent(
      new CustomEvent("woof:nudge-ready", {
        detail: {
          nudge_id: secondNudgeId,
          title: "A second reminder is due",
          body: "Review the second decision.",
          deep_link: "woof://chat?prompt=Review%20the%20second%20decision"
        }
      })
    );

    expect(await screen.findByRole("complementary", { name: "woof nudge" })).toBeInTheDocument();
    expect(screen.getByText("A local follow-up is due")).toBeInTheDocument();
    expect(screen.queryByText("A second reminder is due")).not.toBeInTheDocument();
    window.dispatchEvent(
      new CustomEvent(EVENTS.notificationStatus, { detail: { status: "denied" } })
    );
    expect(await screen.findByText("System notifications are disabled.")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Dismiss nudge" }));
    await waitFor(() =>
      expect(screen.queryByText("A local follow-up is due")).not.toBeInTheDocument()
    );
    expect(await screen.findByText("A second reminder is due")).toBeInTheDocument();
    expect(
      JSON.parse(
        window.localStorage.getItem("woof:mutation:companion_dismiss_nudge") ?? "{}"
      )
    ).toEqual({ nudgeId: firstNudgeId });
  });

  it("routes only canonical nudge deep links into memory hub destinations", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    const companion = render(Companion, { mode: "expanded" });
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.nudgeReady, {
        detail: {
          nudge_id: "0194f3cb-16d8-7f10-a922-4379a7c54d33",
          title: "Follow-ups are ready",
          body: "Review open items.",
          deep_link: "woof://memory-hub/followups"
        }
      })
    );

    await fireEvent.click(await screen.findByRole("button", { name: "Open" }));
    expect(invoke).toHaveBeenCalledWith(COMMANDS.companionOpenNudge, {
      nudgeId: "0194f3cb-16d8-7f10-a922-4379a7c54d33"
    });
    companion.unmount();

    render(MemoryHub);
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.memoryHubNavigate, { detail: { route: "followups" } })
    );
    expect(await screen.findByRole("heading", { name: "Follow-ups" })).toBeInTheDocument();
    expect(await screen.findByText("Review the launch decision")).toBeInTheDocument();

    window.dispatchEvent(
      new CustomEvent(EVENTS.memoryHubNavigate, { detail: { route: "workflows" } })
    );
    expect(await screen.findByRole("heading", { name: "Workflows" })).toBeInTheDocument();
    expect(await screen.findByText("Review launch decisions")).toBeInTheDocument();
  });

  it("keeps a reminder visible when native dismissal is not confirmed", async () => {
    window.localStorage.setItem(
      `woof:command:${COMMANDS.companionDismissNudge}`,
      JSON.stringify({ dismissed: false })
    );
    render(Companion, { mode: "expanded" });
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.nudgeReady, {
        detail: {
          nudge_id: "0194f3cb-16d8-7f10-a922-4379a7c54d35",
          title: "Durable reminder",
          body: "Keep this visible until dismissal is persisted.",
          deep_link: "woof://chat?prompt=Keep%20this%20visible"
        }
      })
    );

    await fireEvent.click(await screen.findByRole("button", { name: "Dismiss nudge" }));
    expect(await screen.findByText("This reminder could not be dismissed.")).toBeInTheDocument();
    expect(screen.getByText("Durable reminder")).toBeInTheDocument();
  });

  it("resolves and dismisses open follow-ups only after native confirmation", async () => {
    render(MemoryHub);
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.memoryHubNavigate, { detail: { route: "followups" } })
    );
    expect(await screen.findByText("Review the launch decision")).toBeInTheDocument();

    await fireEvent.click(
      screen.getByRole("button", { name: "Resolve Review the launch decision" })
    );
    await waitFor(() =>
      expect(screen.queryByText("Review the launch decision")).not.toBeInTheDocument()
    );
    expect(
      JSON.parse(
        window.localStorage.getItem("woof:mutation:memory_followup_set_status") ?? "{}"
      )
    ).toEqual({ flagId: 7, status: "resolved" });
  });

  it("keeps a follow-up visible when native status update is not confirmed", async () => {
    window.localStorage.setItem(
      `woof:command:${COMMANDS.memoryFollowupSetStatus}`,
      JSON.stringify({ updated: false })
    );
    render(MemoryHub);
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.memoryHubNavigate, { detail: { route: "followups" } })
    );
    expect(await screen.findByText("Review the launch decision")).toBeInTheDocument();

    await fireEvent.click(
      screen.getByRole("button", { name: "Dismiss Review the launch decision" })
    );
    expect(await screen.findByText("This follow-up could not be updated."))
      .toBeInTheDocument();
    expect(screen.getByText("Review the launch decision")).toBeInTheDocument();
  });

  it("keeps an accepted workflow as a local pattern without implying automation", async () => {
    render(MemoryHub);
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.memoryHubNavigate, { detail: { route: "workflows" } })
    );

    expect(await screen.findByText("3 recurrences across 2 days · Browser"))
      .toBeInTheDocument();
    expect(screen.getByText("Launch review")).toBeInTheDocument();
    expect(
      screen.getByText("Keep pattern saves a detection in local memory. It never runs actions or automation.")
    ).toBeInTheDocument();

    await fireEvent.click(
      screen.getByRole("button", { name: "Keep pattern Review launch decisions" })
    );
    expect(await screen.findByText("Kept locally — no automation runs."))
      .toBeInTheDocument();
    expect(
      JSON.parse(
        window.localStorage.getItem("woof:mutation:memory_work_pattern_set_status") ?? "{}"
      )
    ).toEqual({
      workflowId: "0192f3cb-16d8-7f10-a922-4379a7c54d31",
      status: "accepted"
    });
  });

  it("keeps a proposed workflow visible when native dismissal is not confirmed", async () => {
    window.localStorage.setItem(
      `woof:command:${COMMANDS.memoryWorkPatternSetStatus}`,
      JSON.stringify({ updated: false })
    );
    render(MemoryHub);
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.memoryHubNavigate, { detail: { route: "workflows" } })
    );
    expect(await screen.findByText("Review launch decisions")).toBeInTheDocument();

    await fireEvent.click(
      screen.getByRole("button", { name: "Dismiss Review launch decisions" })
    );
    expect(await screen.findByText("This pattern could not be updated."))
      .toBeInTheDocument();
    expect(screen.getByText("Review launch decisions")).toBeInTheDocument();
  });

  it("does not expose an action for unknown local nudge targets", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Companion, { mode: "expanded" });
    await Promise.resolve();
    window.dispatchEvent(
      new CustomEvent(EVENTS.nudgeReady, {
        detail: {
          nudge_id: "0194f3cb-16d8-7f10-a922-4379a7c54d34",
          title: "Unknown target",
          body: "This route is not supported.",
          deep_link: "woof://memory-hub/private"
        }
      })
    );

    expect(await screen.findByText("Unknown target")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open" })).not.toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith(COMMANDS.memoryHubOpenRoute, expect.anything());
  });

  it("shows the dotted resize grip only on the expanded shell", () => {
    const collapsed = render(Companion, { mode: "collapsed" });
    expect(screen.queryByTestId("companion-resize-grip")).not.toBeInTheDocument();
    collapsed.unmount();

    render(Companion, { mode: "expanded" });
    const grip = screen.getByTestId("companion-resize-grip");
    expect(grip).toHaveAttribute("aria-hidden", "true");
    expect(grip).toHaveStyle({
      width: `${WINDOWS.companion.resizeGripSize}px`,
      height: `${WINDOWS.companion.resizeGripSize}px`
    });
    expect(grip.querySelectorAll("i")).toHaveLength(6);
  });

  it("restores the paused capture UI and explains when native resume fails", async () => {
    const nativeInvoke = bridge.invokeCommand;
    const invoke = vi.spyOn(bridge, "invokeCommand").mockImplementation(
      (command, args = {}) => {
        if (command === COMMANDS.captureResume) {
          return Promise.reject(new Error("capture resume failed"));
        }
        return nativeInvoke(command, args);
      }
    );
    updateState({ capture: "paused" });
    render(Companion, { mode: "expanded" });

    await fireEvent.click(screen.getByRole("button", { name: "Resume" }));

    await waitFor(() => {
      expect(screen.getByText("capture resume failed")).toBeInTheDocument();
    });
    expect(invoke).toHaveBeenCalledWith(COMMANDS.captureResume);
  });

  it("does not accept an unverified resume event as active capture", async () => {
    updateState({ capture: "paused", health: "healthy" });
    render(Companion, { mode: "expanded" });
    await Promise.resolve();

    window.dispatchEvent(new CustomEvent(EVENTS.capturePaused, { detail: false }));
    expect(await screen.findByText("Capture is starting")).toBeInTheDocument();

    window.dispatchEvent(
      new CustomEvent(EVENTS.captureChanged, { detail: { state: "active" } })
    );
    expect(await screen.findByText("Capture is starting")).toBeInTheDocument();

    window.dispatchEvent(
      new CustomEvent(EVENTS.captureChanged, {
        detail: { state: "permission-revoked" }
      })
    );
    expect(
      await screen.findByText("Accessibility permission is needed")
    ).toBeInTheDocument();
    expect(screen.queryByText("Capture is starting")).not.toBeInTheDocument();
  });

  it("keeps chat failures separate from daemon and capture health", async () => {
    const nativeInvoke = bridge.invokeCommand;
    vi.spyOn(bridge, "invokeCommand").mockImplementation((command, args = {}) => {
      if (command === COMMANDS.chatSend) {
        return Promise.reject(new Error("chat request unavailable"));
      }
      return nativeInvoke(command, args);
    });
    updateState({ health: "healthy", capture: "active" });
    render(Companion, { mode: "expanded" });

    const textbox = screen.getByRole("textbox", { name: "Message woof" });
    await fireEvent.input(textbox, { target: { value: "What changed?" } });
    await fireEvent.keyDown(textbox, { key: "Enter" });

    expect(
      await screen.findByText("I couldn’t complete that request. Please try again.")
    ).toBeInTheDocument();
    expect(get(appState).health).toBe("healthy");
    expect(get(appState).capture).toBe("active");
  });

  it("describes local health without exposing implementation secrets", () => {
    render(HealthBadge, { state: "healthy" });
    expect(screen.getByText("All local systems ready")).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/token|api key|captured text/i);
  });

  it("updates recovery UI from the canonical native health payload", async () => {
    render(HealthRecovery, { state: "offline" });
    expect(screen.getByRole("heading", { name: "The local service is offline" }))
      .toBeInTheDocument();
    await Promise.resolve();

    window.dispatchEvent(
      new CustomEvent(EVENTS.healthChanged, { detail: { state: "healthy" } })
    );

    expect(await screen.findByRole("heading", { name: "Everything is ready" }))
      .toBeInTheDocument();
  });

  it("surfaces a safe, dismissible database recovery notice", async () => {
    window.localStorage.setItem(
      `woof:command:${COMMANDS.captureStatus}`,
      JSON.stringify({
        paused: false,
        capturing: true,
        runtime: { running: true, permission: "granted" },
        database_recovery: { occurred: true, reason: "corrupt" }
      })
    );
    render(HealthRecovery, { state: "healthy" });

    expect(
      await screen.findByRole("heading", { name: "Local memory started fresh" })
    ).toBeInTheDocument();
    expect(screen.getByText(/isolated copy follows your retention setting/i)).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/\/Users\/|database\.sqlite/i);
    await fireEvent.click(screen.getByRole("button", { name: "Got it" }));
  });

  it("loads memory hub activity, wiki, capture, and time through the browser bridge", async () => {
    render(MemoryHub);
    expect(await screen.findByText("woof launch checklist")).toBeInTheDocument();
    expect(screen.getByText("Woof is noticing locally.")).toBeInTheDocument();
    expect(screen.getByText("woof — desktop")).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Memory" }));
    expect(await screen.findByText("A private local memory companion for macOS.")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: /woof.*A private local memory companion/i }));
    expect(await screen.findByText(/captures visible interface text locally/i)).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Time" }));
    expect(await screen.findByText("Today’s allocation")).toBeInTheDocument();
    expect(screen.getByText("2h 14m")).toBeInTheDocument();
    expect(screen.getByText("Time rules")).toBeInTheDocument();
  });

  it("never presents denied Accessibility capture as active or healthy", async () => {
    window.localStorage.setItem(
      `woof:command:${COMMANDS.captureStatus}`,
      JSON.stringify({
        paused: false,
        capturing: true,
        runtime: {
          running: true,
          permission: "denied",
          last_capture_at: null,
          last_error: "permission_denied"
        }
      })
    );
    render(MemoryHub);

    expect(
      await screen.findByText("Accessibility permission is needed.")
    ).toBeInTheDocument();
    expect(screen.getByText("Accessibility needed")).toBeInTheDocument();
    expect(screen.getByText("Local service needs attention")).toBeInTheDocument();
    expect(screen.queryByText("Noticing locally")).not.toBeInTheDocument();
    expect(screen.queryByText("Woof is noticing locally.")).not.toBeInTheDocument();
  });

  it("renders the persisted local profile and current greeting as text", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 7, 1, 20, 0, 0));
    const contactName = 'Ada <img src=x onerror="alert(1)">';
    window.localStorage.setItem(
      `woof:command:${COMMANDS.loadContactInfo}`,
      JSON.stringify({ name: contactName, company: "Analytical Engine" })
    );

    render(MemoryHub);
    await vi.advanceTimersByTimeAsync(0);

    expect(screen.getByText(contactName)).toBeInTheDocument();
    expect(screen.getByText("A", { selector: ".avatar" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Good evening." })).toBeInTheDocument();
    expect(document.querySelector(".profile img")).toBeNull();
    expect(screen.queryByText("Julius")).not.toBeInTheDocument();
  });

  it("edits and persists daemon-backed capture blacklist rules", async () => {
    render(SettingsPanel, { embedded: true });

    await fireEvent.click(screen.getByRole("button", { name: "Privacy" }));
    await fireEvent.click(screen.getByRole("button", { name: /Capture blacklist/i }));
    expect(await screen.findByText("No custom exclusions")).toBeInTheDocument();

    await fireEvent.input(screen.getByRole("textbox", { name: "New rule pattern" }), {
      target: { value: "Private Notes" }
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Add capture blacklist rule" })
    );

    expect(screen.getByRole("textbox", { name: "Rule pattern 1" })).toHaveValue(
      "Private Notes"
    );
    await fireEvent.click(screen.getByRole("button", { name: "Save blacklist" }));
    expect(await screen.findByText("Saved")).toBeInTheDocument();

    expect(
      JSON.parse(
        window.localStorage.getItem("woof:mutation:set_capture_blacklist") ?? "{}"
      )
    ).toEqual({
      blacklist: [{ kind: "app_name", pattern: "Private Notes" }]
    });
  });

  it("requires explicit confirmation before deleting all local memory", async () => {
    window.localStorage.setItem(
      "woof:command:memory_delete_all",
      JSON.stringify({ status: "deleted", deleted_rows: 12, vector_index: { indexed: 0 } })
    );
    render(SettingsPanel, { embedded: true, dock: true });

    await fireEvent.click(screen.getByRole("button", { name: "Delete all data" }));
    expect(
      screen.getByRole("alertdialog", {
        name: "Permanently delete woof’s local memory?"
      })
    ).toBeInTheDocument();
    expect(screen.queryByText(/permanently deleted/i)).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Delete permanently" }));
    expect(
      await screen.findByText("Local memory and identity were permanently deleted.")
    ).toBeInTheDocument();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("never labels unavailable capture as active in settings", async () => {
    window.localStorage.setItem("woof:command:capture_is_paused", "false");
    window.localStorage.setItem(
      "woof:command:capture_status",
      JSON.stringify({
        paused: false,
        capturing: false,
        runtime: {
          running: true,
          permission: "denied",
          last_error: "permission_denied"
        }
      })
    );

    render(SettingsPanel, { embedded: true, dock: true });
    await fireEvent.click(screen.getByRole("button", { name: "Memory" }));

    expect(
      await screen.findByText("Unavailable — Accessibility permission is required.")
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Active — visible accessibility text is stored locally.")
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("shows the reason when resuming capture from settings fails", async () => {
    window.localStorage.setItem("woof:command:capture_is_paused", "true");
    window.localStorage.setItem(
      "woof:command:capture_status",
      JSON.stringify({
        paused: true,
        capturing: false,
        runtime: { running: true, permission: "denied", last_error: "accessibility" }
      })
    );
    const nativeInvoke = bridge.invokeCommand;
    vi.spyOn(bridge, "invokeCommand").mockImplementation((command, args = {}) => {
      if (command === COMMANDS.captureResume) {
        return Promise.reject(
          new Error("Accessibility is not ready in the local capture service")
        );
      }
      return nativeInvoke(command, args);
    });

    render(SettingsPanel, { embedded: true, dock: true });
    await fireEvent.click(screen.getByRole("button", { name: "Memory" }));
    await fireEvent.click(await screen.findByRole("button", { name: "Resume" }));

    expect(
      await screen.findByText("Accessibility is not ready in the local capture service")
    ).toHaveAttribute("role", "alert");
  });

  it("loads and saves local nudge notification preferences", async () => {
    window.localStorage.setItem("woof:command:get_nudges_enabled", "false");
    render(SettingsPanel, { embedded: true, dock: true });

    await fireEvent.click(screen.getByRole("button", { name: "Notifications" }));
    const toggle = await screen.findByRole("checkbox", { name: /Local nudges/i });
    expect(toggle).not.toBeChecked();
    await fireEvent.click(toggle);
    await waitFor(() => expect(toggle).toBeChecked());
    expect(
      JSON.parse(window.localStorage.getItem("woof:mutation:set_nudges_enabled") ?? "{}")
    ).toEqual({ enabled: true });
    expect(
      screen.getByRole("button", { name: "Open macOS notification settings" })
    ).toBeInTheDocument();
  });

  it("creates, lists, and deletes explicit local reminders", async () => {
    render(SettingsPanel, { embedded: true, dock: true });
    await fireEvent.click(screen.getByRole("button", { name: "Notifications" }));

    await fireEvent.input(await screen.findByLabelText("Label"), {
      target: { value: "Daily launch review" }
    });
    await fireEvent.input(screen.getByLabelText("Reminder"), {
      target: { value: "Review open launch decisions." }
    });
    await fireEvent.change(screen.getByLabelText("Reminder schedule"), {
      target: { value: "daily" }
    });
    await fireEvent.input(screen.getByLabelText("Daily reminder time"), {
      target: { value: "09:15" }
    });
    await fireEvent.click(screen.getByRole("button", { name: "Add reminder" }));

    expect(await screen.findByText("Daily launch review")).toBeInTheDocument();
    expect(screen.getByText(/Daily at/i)).toBeInTheDocument();
    expect(
      JSON.parse(
        window.localStorage.getItem("woof:mutation:scheduled_reminder_create") ?? "{}"
      )
    ).toEqual({
      reminder: {
        label: "Daily launch review",
        prompt: "Review open launch decisions.",
        scheduleKind: "daily",
        hour: 9,
        minute: 15
      }
    });

    await fireEvent.click(
      screen.getByRole("button", { name: "Delete reminder Daily launch review" })
    );
    await waitFor(() =>
      expect(screen.queryByText("Daily launch review")).not.toBeInTheDocument()
    );
  });

  it("loads and saves an explicit local data-retention policy", async () => {
    render(SettingsPanel, { embedded: true, dock: true });
    await fireEvent.click(screen.getByRole("button", { name: "Data retention" }));

    const selector = await screen.findByRole("combobox", { name: "Data retention" });
    expect(selector).toHaveValue("keep_forever");
    await fireEvent.change(selector, { target: { value: "30" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save retention" }));

    expect(await screen.findByRole("status")).toHaveTextContent("Saved");
    expect(
      JSON.parse(window.localStorage.getItem("woof:mutation:set_data_retention") ?? "{}")
    ).toEqual({ retention: { mode: "days", days: 30 } });
  });

  it("refreshes visible dock permissions every four seconds and cancels polling", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem("woof:command:accessibility_trusted", "false");
    window.localStorage.setItem("woof:command:input_monitoring_trusted", "false");
    window.localStorage.setItem("woof:command:microphone_status", '"not-determined"');

    const panel = render(SettingsPanel, { embedded: true, dock: true });
    await vi.advanceTimersByTimeAsync(0);

    const accessibility = screen.getByRole("button", { name: /Accessibility/i });
    const microphone = screen.getByRole("button", { name: /Microphone/i });
    const inputMonitoring = screen.getByRole("button", { name: /Input Monitoring/i });
    expect(within(accessibility).getByText("Not granted")).toBeInTheDocument();
    expect(within(microphone).getByText("Not requested")).toBeInTheDocument();
    expect(within(inputMonitoring).getByText("Not granted")).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(1);

    window.localStorage.setItem("woof:command:accessibility_trusted", "true");
    window.localStorage.setItem("woof:command:input_monitoring_trusted", "true");
    window.localStorage.setItem("woof:command:microphone_status", '"authorized"');
    await vi.advanceTimersByTimeAsync(3_999);
    expect(within(accessibility).getByText("Not granted")).toBeInTheDocument();

    await vi.advanceTimersByTimeAsync(1);
    expect(within(accessibility).getByText("Granted")).toBeInTheDocument();
    expect(within(microphone).getByText("Granted")).toBeInTheDocument();
    expect(within(inputMonitoring).getByText("Granted")).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Appearance" }));
    expect(vi.getTimerCount()).toBe(0);

    await fireEvent.click(screen.getByRole("button", { name: "Privacy" }));
    expect(vi.getTimerCount()).toBe(1);
    panel.unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not call two granted Accessibility entries not granted while the service restarts", async () => {
    window.localStorage.setItem(
      "woof:command:accessibility_status",
      JSON.stringify({
        app_trusted: true,
        capture_service_trusted: true,
        capture_service_operational: false,
        ready: false,
        next_request: null
      })
    );

    render(SettingsPanel, { embedded: true, dock: true });

    const accessibility = await screen.findByRole("button", { name: /Accessibility/i });
    await waitFor(() => {
      expect(within(accessibility).getByText("Restart needed")).toBeInTheDocument();
      expect(within(accessibility).getByText("woof granted · capture service granted"))
        .toBeInTheDocument();
    });
  });

  it("renders every dock settings destination without placeholder claims", async () => {
    render(SettingsPanel, { embedded: true, dock: true });
    expect(screen.getByText("Input Monitoring")).toBeInTheDocument();
    expect(screen.getAllByText("Not granted").length).toBeGreaterThan(0);

    const headings = [
      "Account",
      "Appearance",
      "Identity",
      "Memory",
      "Notifications",
      "Community",
      "MCP",
      "Bug report",
      "Shortcuts",
      "Tutorials",
      "Release notes"
    ];
    for (const heading of headings) {
      await fireEvent.click(screen.getByRole("button", { name: heading }));
      expect(screen.getByRole("heading", { name: heading })).toBeInTheDocument();
    }

    await fireEvent.click(screen.getByRole("button", { name: "MCP" }));
    expect(
      await screen.findByText(/\/Applications\/woof\.app\/Contents\/MacOS\/woof-mcp/)
    ).toBeInTheDocument();
    expect(screen.queryByText(/"command": "woof-mcp"/)).not.toBeInTheDocument();

    expect(document.body.textContent).not.toContain(
      "This section is part of woof’s local companion settings."
    );
    await fireEvent.click(screen.getByRole("button", { name: "Account" }));
    expect(screen.getByText("gpt-5.6-terra")).toBeInTheDocument();
  });

  it("persists identity and shortcut settings through their native contracts", async () => {
    window.localStorage.setItem(
      `woof:command:${COMMANDS.recordSecondaryShortcut}`,
      JSON.stringify({ meta: true, shift: true, alt: false, control: false, key: "w" })
    );
    render(SettingsPanel, { embedded: true, dock: true });

    await fireEvent.click(screen.getByRole("button", { name: "Identity" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Save identity" })).toBeEnabled();
    });
    await fireEvent.input(screen.getByLabelText("Your name"), {
      target: { value: "Julius" }
    });
    await fireEvent.input(screen.getByLabelText(/Company or project/), {
      target: { value: "woof" }
    });
    await fireEvent.click(screen.getByRole("button", { name: "Save identity" }));
    expect(await screen.findByRole("button", { name: "Saved" })).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Shortcuts" }));
    await waitFor(() => {
      expect(screen.getByLabelText("Hold to talk")).toHaveValue("fn");
    });
    expect(screen.getAllByRole("option").map((option) => option.getAttribute("value")))
      .toEqual([
        "fn", "left_option", "right_option", "left_command", "right_command",
        "left_shift", "right_shift", "left_control", "right_control",
        "fn", "left_option", "right_option", "left_command", "right_command",
        "left_shift", "right_shift", "left_control", "right_control"
      ]);
    await fireEvent.click(screen.getByRole("button", { name: "Record secondary shortcut" }));
    await waitFor(() =>
      expect(screen.getByLabelText("Secondary shortcut")).toHaveValue("⌘ ⇧ W")
    );
    await fireEvent.click(screen.getByRole("button", { name: "Save shortcuts" }));
    await waitFor(() => {
      expect(JSON.parse(window.localStorage.getItem("woof:mutation:set_secondary_shortcut") ?? "{}"))
        .toEqual({
        chord: { meta: true, shift: true, alt: false, control: false, key: "w" }
      });
    });
    expect(JSON.parse(window.localStorage.getItem("woof:mutation:set_modifier_keys") ?? "{}"))
      .toEqual({ woofKey: "right_option", transcriptionKey: "fn" });
  });

  it("rejects an inline and hold-to-talk modifier collision before native mutation", async () => {
    render(SettingsPanel, { embedded: true, dock: true });

    await fireEvent.click(screen.getByRole("button", { name: "Shortcuts" }));
    const inlineModifier = await screen.findByLabelText("Inline help and companion");
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Save shortcuts" })).toBeEnabled();
    });
    await fireEvent.change(inlineModifier, { target: { value: "fn" } });

    expect(
      await screen.findByText("Inline help and hold to talk must use different modifier keys.")
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Hold to talk")).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByRole("button", { name: "Save shortcuts" })).toBeDisabled();
    expect(window.localStorage.getItem("woof:mutation:set_modifier_keys")).toBeNull();
  });

  it("shows a startup shortcut registration failure as disabled", async () => {
    window.localStorage.setItem("woof:command:get_secondary_shortcut_enabled", "false");
    window.localStorage.setItem(
      "woof:command:get_secondary_shortcut_error",
      JSON.stringify("The secondary shortcut conflicts with another app.")
    );
    render(SettingsPanel, { embedded: true, dock: true });

    await fireEvent.click(screen.getByRole("button", { name: "Shortcuts" }));

    expect(
      await screen.findByText("The secondary shortcut conflicts with another app.")
    ).toBeInTheDocument();
    expect(
      screen.getByRole("checkbox", { name: "Secondary shortcut enabled" })
    ).not.toBeChecked();
  });

  it("renders the configured bindings instead of shortcut defaults", async () => {
    window.localStorage.setItem("woof:command:get_woof_modifier_key", '"left_shift"');
    window.localStorage.setItem("woof:command:get_transcription_modifier_key", '"right_control"');
    window.localStorage.setItem(
      "woof:command:get_secondary_shortcut",
      JSON.stringify({ meta: true, shift: false, alt: true, control: false, key: "k" })
    );
    render(SettingsPanel, { embedded: true });

    await fireEvent.click(screen.getByRole("button", { name: "Shortcuts" }));

    expect(await screen.findAllByText("Left Shift", { selector: "kbd" })).toHaveLength(2);
    expect(screen.getByText("Right Control", { selector: "kbd" })).toBeInTheDocument();
    expect(screen.getByText("⌘ ⌥ K", { selector: "kbd" })).toBeInTheDocument();
  });
});
