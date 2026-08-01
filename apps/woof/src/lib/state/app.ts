import { derived, writable } from "svelte/store";
import type {
  CaptureState,
  CompanionMode,
  HealthState,
  WikiSummary
} from "$lib/contracts/ipc";

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  pending?: boolean;
}

export interface AppState {
  companionMode: CompanionMode;
  settingsOpen: boolean;
  capture: CaptureState;
  health: HealthState;
  transcription:
    | "idle"
    | "listening"
    | "processing"
    | "done"
    | "cancelled"
    | "failed"
    | "overflow"
    | "limit";
  transcriptionLevel: number;
  messages: ChatMessage[];
  wiki: WikiSummary[];
  activeNav: "home" | "memory" | "time" | "settings";
  onboardingStep: number;
}

const initial: AppState = {
  companionMode: "collapsed",
  settingsOpen: false,
  capture: "active",
  health: "starting",
  transcription: "idle",
  transcriptionLevel: 0,
  messages: [
    {
      id: "welcome",
      role: "assistant",
      content: "I’m here. Ask what you were working on, or tell me what to help with."
    }
  ],
  wiki: [],
  activeNav: "home",
  onboardingStep: 0
};

export const appState = writable<AppState>(initial);
export const isCapturing = derived(appState, ($state) => $state.capture === "active");

export function updateState(patch: Partial<AppState>): void {
  appState.update((state) => ({ ...state, ...patch }));
}

export function addMessage(message: Omit<ChatMessage, "id">): string {
  const id = crypto.randomUUID();
  appState.update((state) => ({
    ...state,
    messages: [...state.messages, { id, ...message }]
  }));
  return id;
}

export function appendMessage(id: string, delta: string): void {
  appState.update((state) => ({
    ...state,
    messages: state.messages.map((message) =>
      message.id === id
        ? { ...message, content: `${message.content}${delta}`, pending: true }
        : message
    )
  }));
}

export function finishMessage(id: string): void {
  appState.update((state) => ({
    ...state,
    messages: state.messages.map((message) =>
      message.id === id ? { ...message, pending: false } : message
    )
  }));
}

export function resetState(): void {
  appState.set(initial);
}
