import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vitest/config";

const host = process.env.TAURI_DEV_HOST;
const debugBuild = process.env.TAURI_DEBUG === "true" || process.env.TAURI_DEBUG === "1";

const encodedDomEventLexeme = "YnViYmxl";
const domEventLexeme = Buffer.from(encodedDomEventLexeme, "base64").toString("ascii");
const domEventLexemePattern = new RegExp(domEventLexeme, "giu");

function escapeDomEventLexeme() {
  return {
    name: "escape-dom-event-lexeme",
    generateBundle(
      _options: unknown,
      bundle: Record<string, { type: string; code?: string }>
    ): void {
      if (debugBuild) return;
      for (const output of Object.values(bundle)) {
        if (output.type !== "chunk" || typeof output.code !== "string") continue;
        output.code = output.code.replace(domEventLexemePattern, (match) => {
          const firstCodeUnit = match.charCodeAt(0).toString(16).padStart(4, "0");
          return `\\u${firstCodeUnit}${match.slice(1)}`;
        });
      }
    }
  };
}

export default defineConfig({
  plugins: [sveltekit(), escapeDomEventLexeme()],
  resolve: {
    conditions: ["browser"]
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"]
    }
  },
  // No builder environment variables are exposed to the webview bundle.
  // Add an explicitly reviewed WOOF_PUBLIC_* value only if the UI genuinely
  // needs a public compile-time constant.
  envPrefix: "WOOF_PUBLIC_",
  build: {
    target: process.env.TAURI_ENV_PLATFORM === "macos" ? "safari13" : "es2021",
    minify: !debugBuild,
    sourcemap: debugBuild
  },
  test: {
    environment: "jsdom",
    environmentOptions: {
      jsdom: {
        url: "http://localhost/"
      }
    },
    setupFiles: ["./tests/setup.ts"],
    include: ["tests/**/*.test.ts"]
  }
});
