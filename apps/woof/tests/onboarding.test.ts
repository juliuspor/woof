import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import Onboarding from "../src/lib/components/Onboarding.svelte";
import * as bridge from "../src/lib/contracts/bridge";
import { COMMANDS, EVENTS } from "../src/lib/contracts/ipc";

afterEach(() => {
  vi.restoreAllMocks();
  cleanup();
  window.localStorage.clear();
});

async function openLocalCaptureStep(): Promise<void> {
  await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
}

describe("onboarding permissions", () => {
  it("requires explicit Accessibility trust before local capture can be enabled", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Onboarding);
    await openLocalCaptureStep();

    expect(
      screen.getByText(/Accessibility is required for local capture and stays under your control/i)
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Set up Input Monitoring (optional)" })
    ).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Allow Accessibility for woof" }));
    expect(invoke).toHaveBeenCalledWith(COMMANDS.requestAccessibility);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Enable woof in Accessibility, then return here."
    );
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
  });

  it("reveals the capture service for manual Accessibility registration", async () => {
    window.localStorage.setItem(
      "woof:command:accessibility_status",
      JSON.stringify({
        app_trusted: true,
        capture_service_trusted: false,
        capture_service_operational: false,
        ready: false,
        next_request: "capture-service"
      })
    );
    window.localStorage.setItem(
      "woof:command:request_accessibility",
      JSON.stringify({
        app_trusted: true,
        capture_service_trusted: false,
        capture_service_operational: false,
        ready: false,
        next_request: "capture-service"
      })
    );
    render(Onboarding);
    await openLocalCaptureStep();

    const request = await screen.findByRole("button", {
      name: "Reveal capture service to add manually"
    });
    await fireEvent.click(request);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Finder selected woof_d. In Accessibility, click +, add that file, and enable it."
    );
  });

  it("completes local capture setup without Input Monitoring, microphone, or an API key", async () => {
    window.localStorage.setItem("woof:command:accessibility_trusted", "true");
    const invoke = vi.spyOn(bridge, "invokeCommand");
    const completed = vi.fn();
    window.addEventListener(EVENTS.onboardingComplete, completed, { once: true });
    render(Onboarding);
    await openLocalCaptureStep();

    expect(await screen.findByText("Local capture is ready")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Continue without shortcuts" })).toBeEnabled();
    await fireEvent.click(screen.getByRole("button", { name: "Continue without shortcuts" }));

    expect(screen.getByText("Optional: Voice")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Allow microphone (optional)" })).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Not now" }));

    expect(screen.getByText("Optional: Connect OpenAI")).toBeInTheDocument();
    expect(screen.getByLabelText("OpenAI API key (optional)")).toHaveValue("");
    await fireEvent.click(screen.getByRole("button", { name: "Not now" }));

    expect(screen.getByText("Optional shortcuts")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    await fireEvent.click(screen.getByRole("button", { name: "Open memory hub" }));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith(COMMANDS.finishOnboarding));
    expect(completed).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls.some(([command]) => command === COMMANDS.requestInputMonitoring)).toBe(false);
    expect(
      invoke.mock.calls.some(
        ([command, args]) => command === COMMANDS.microphoneStatus && args?.request === true
      )
    ).toBe(false);
    expect(invoke.mock.calls.some(([command]) => command === COMMANDS.setOpenAiApiKey)).toBe(false);
  });

  it("uses the privacy-safe skip command without enabling capture", async () => {
    const invoke = vi.spyOn(bridge, "invokeCommand");
    render(Onboarding);

    await fireEvent.click(screen.getByRole("button", { name: "Skip onboarding" }));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith(COMMANDS.skipOnboarding));
    expect(invoke.mock.calls.some(([command]) => command === COMMANDS.finishOnboarding)).toBe(false);
  });

  it("returns to Accessibility without completing when final trust is lost", async () => {
    vi.spyOn(bridge, "invokeCommand").mockImplementation(async (command) => {
      if (command === COMMANDS.finishOnboarding) {
        throw new Error("Accessibility changed before local capture could start");
      }
      if (command === COMMANDS.accessibilityStatus) {
        return {
          app_trusted: true,
          capture_service_trusted: true,
          capture_service_operational: true,
          ready: true,
          next_request: null
        };
      }
      if (command === COMMANDS.accessibilityTrusted) return true;
      if (command === COMMANDS.microphoneStatus) return "not-determined";
      return false;
    });
    const completed = vi.fn();
    window.addEventListener(EVENTS.onboardingComplete, completed, { once: true });
    render(Onboarding);
    await openLocalCaptureStep();

    await fireEvent.click(await screen.findByRole("button", { name: "Continue without shortcuts" }));
    await fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    await fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    await fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    await fireEvent.click(screen.getByRole("button", { name: "Open memory hub" }));

    expect(await screen.findByText("Local capture")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Accessibility changed before local capture started"
    );
    expect(completed).not.toHaveBeenCalled();
  });
});
