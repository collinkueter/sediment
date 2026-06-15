import { type ClaudeCodeStatus, type CopilotStatus, tauri } from "@/lib/tauri";
import { useEffect, useRef, useState } from "react";

type Engine = "claude-code" | "copilot";

/// Settings overlay. Two sections:
///  - Conversation engine: which agentic CLI runs the conversation (ADR-0009).
///  - Model storage: where the local embedding model is kept.
export function SettingsModal({
  onClose,
  onModelConfigChanged,
}: {
  onClose: () => void;
  onModelConfigChanged: () => void;
}) {
  const [modelsDir, setModelsDir] = useState<string | null>(null);
  const [initialModelsDir, setInitialModelsDir] = useState<string | null>(null);
  const [engine, setEngine] = useState<Engine>("claude-code");
  const [claudeCodeModel, setClaudeCodeModel] = useState("sonnet");
  const [copilotModel, setCopilotModel] = useState("");
  const [claudeCode, setClaudeCode] = useState<ClaudeCodeStatus | null>(null);
  const [copilot, setCopilot] = useState<CopilotStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    Promise.all([tauri.getModelsDir(), tauri.getConversationEngine()])
      .then(([dir, ce]) => {
        setModelsDir(dir);
        setInitialModelsDir(dir);
        setEngine((ce.engine as Engine) ?? "claude-code");
        setClaudeCodeModel(ce.claude_code_model ?? "sonnet");
        setCopilotModel(ce.copilot_model ?? "");
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
    // CLI detection is ~1s each — let both resolve independently.
    tauri
      .detectClaudeCode()
      .then(setClaudeCode)
      .catch(() => {
        setClaudeCode({
          installed: false,
          binary_path: null,
          logged_in: false,
          auth_method: null,
          subscription_type: null,
          email: null,
        });
      });
    tauri
      .detectCopilot()
      .then(setCopilot)
      .catch(() => {
        setCopilot({ installed: false, binary_path: null });
      });
  }, []);

  // Dialog accessibility: close on Escape, focus the first focusable element
  // on mount, and trap Tab/Shift+Tab so focus wraps within the dialog.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const focusables = () =>
      Array.from(
        dialog.querySelectorAll<HTMLElement>(
          'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
      );
    // Focus the first focusable on mount so keyboard users land inside.
    focusables()[0]?.focus();
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key !== "Tab") return;
      const items = focusables();
      const first = items[0];
      const last = items[items.length - 1];
      if (!first || !last) return;
      const active = document.activeElement as HTMLElement | null;
      if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const modelsDirChanged = modelsDir !== initialModelsDir;

  async function chooseFolder() {
    try {
      const dir = await tauri.pickDirectory();
      if (dir) setModelsDir(dir);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  async function save() {
    setSaving(true);
    setError(null);
    try {
      if (modelsDirChanged) {
        await tauri.setModelsDir(modelsDir);
      }
      const model =
        engine === "claude-code" ? claudeCodeModel.trim() || "sonnet" : copilotModel.trim() || null;
      await tauri.setConversationEngine(engine, model);
      if (modelsDirChanged) {
        onModelConfigChanged();
      }
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-6">
      <div
        ref={dialogRef}
        // biome-ignore lint/a11y/useSemanticElements: native <dialog> requires the imperative showModal API and brings its own focus / top-layer model — we keep a styled div + ARIA so the visibility-via-prop pattern (`{settingsOpen && …}`) stays intact.
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
        className="max-h-[85vh] w-full max-w-md space-y-5 overflow-auto rounded-lg border border-zinc-200 bg-white p-6 shadow-lg dark:border-zinc-800 dark:bg-zinc-900"
      >
        <div className="flex items-center justify-between">
          <h2
            id="settings-title"
            className="text-sm font-semibold text-zinc-900 dark:text-zinc-100"
          >
            Settings
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close settings"
            className="rounded px-1.5 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
          >
            ✕
          </button>
        </div>

        <section className="space-y-3">
          <div>
            <h3 className="text-xs font-semibold text-zinc-700 dark:text-zinc-300">
              Conversation engine
            </h3>
            <p className="mt-0.5 text-[11px] text-zinc-500 dark:text-zinc-400">
              Which agentic CLI runs the conversation. The conversation and your note content go to
              that CLI under your own subscription — they leave your machine. Note search and your
              formation stay on-device.
            </p>
          </div>

          {loading ? (
            <p className="text-xs text-zinc-400 dark:text-zinc-500">Loading…</p>
          ) : (
            <div className="space-y-1.5">
              {/* Claude Code */}
              <button
                type="button"
                onClick={() => setEngine("claude-code")}
                className={`block w-full rounded-md border px-3 py-2 text-left ${
                  engine === "claude-code"
                    ? "border-zinc-900 bg-zinc-100 dark:border-zinc-100 dark:bg-zinc-800"
                    : "border-zinc-200 hover:bg-zinc-50 dark:border-zinc-800 dark:hover:bg-zinc-800"
                }`}
              >
                <div className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
                  Claude Code
                </div>
                <div className="text-xs text-zinc-500 dark:text-zinc-400">
                  Runs the agent on your Claude Pro/Max subscription via the local Claude Code CLI.
                  No API key needed.
                </div>
              </button>
              <p
                className={`pl-3 text-[11px] ${
                  claudeCode?.installed && claudeCode.logged_in
                    ? "text-emerald-600 dark:text-emerald-400"
                    : "text-zinc-500 dark:text-zinc-400"
                }`}
              >
                {claudeCode === null
                  ? "Checking for Claude Code…"
                  : !claudeCode.installed
                    ? "Not installed. Install Claude Code from claude.com/claude-code."
                    : !claudeCode.logged_in
                      ? "Installed — run `claude` in a terminal and sign in."
                      : `Connected as ${claudeCode.email} · ${claudeCode.subscription_type} subscription.`}
              </p>

              {engine === "claude-code" && (
                <div className="space-y-1.5 pl-3 pt-1">
                  <label className="block text-xs text-zinc-600 dark:text-zinc-400">
                    Model
                    <input
                      type="text"
                      value={claudeCodeModel}
                      onChange={(e) => setClaudeCodeModel(e.target.value)}
                      placeholder="sonnet"
                      className="mt-1 block w-full rounded-md border border-zinc-300 bg-white px-2 py-1.5 text-sm text-zinc-900 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-100"
                    />
                  </label>
                </div>
              )}

              {/* GitHub Copilot */}
              <button
                type="button"
                onClick={() => setEngine("copilot")}
                className={`block w-full rounded-md border px-3 py-2 text-left ${
                  engine === "copilot"
                    ? "border-zinc-900 bg-zinc-100 dark:border-zinc-100 dark:bg-zinc-800"
                    : "border-zinc-200 hover:bg-zinc-50 dark:border-zinc-800 dark:hover:bg-zinc-800"
                }`}
              >
                <div className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
                  GitHub Copilot
                </div>
                <div className="text-xs text-zinc-500 dark:text-zinc-400">
                  Runs the agent via your GitHub Copilot subscription. Each turn draws on your
                  Copilot premium-request quota.
                </div>
              </button>
              <p
                className={`pl-3 text-[11px] ${
                  copilot?.installed
                    ? "text-emerald-600 dark:text-emerald-400"
                    : "text-zinc-500 dark:text-zinc-400"
                }`}
              >
                {copilot === null
                  ? "Checking for the Copilot CLI…"
                  : !copilot.installed
                    ? "Not installed. Install with `npm install -g @github/copilot`."
                    : "Installed — run `copilot login` if turns fail."}
              </p>

              {engine === "copilot" && (
                <div className="space-y-1.5 pl-3 pt-1">
                  <label className="block text-xs text-zinc-600 dark:text-zinc-400">
                    Model
                    <input
                      type="text"
                      value={copilotModel}
                      onChange={(e) => setCopilotModel(e.target.value)}
                      placeholder="claude-haiku-4.5"
                      className="mt-1 block w-full rounded-md border border-zinc-300 bg-white px-2 py-1.5 text-sm text-zinc-900 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-100"
                    />
                  </label>
                  <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                    Examples: claude-haiku-4.5, gpt-5-mini. Leave blank for the Copilot default.
                  </p>
                </div>
              )}
            </div>
          )}

          {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}
        </section>

        <section className="space-y-3 border-t border-zinc-200 pt-5 dark:border-zinc-800">
          <div>
            <h3 className="text-xs font-semibold text-zinc-700 dark:text-zinc-300">
              Model storage
            </h3>
            <p className="mt-0.5 text-[11px] text-zinc-500 dark:text-zinc-400">
              Where the local embedding model is kept. A shared folder lets the Ollama daemon
              Sediment starts store models in one place across formations.
            </p>
          </div>

          {loading ? (
            <p className="text-xs text-zinc-400 dark:text-zinc-500">Loading…</p>
          ) : (
            <>
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 truncate rounded-md border border-zinc-300 bg-zinc-50 px-2 py-1.5 text-[11px] text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300">
                  {modelsDir ?? "Default — Ollama's own storage location"}
                </code>
                <button
                  type="button"
                  onClick={() => void chooseFolder()}
                  className="shrink-0 rounded-md border border-zinc-300 px-2 py-1.5 text-[11px] text-zinc-700 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800"
                >
                  Choose…
                </button>
                {modelsDir && (
                  <button
                    type="button"
                    onClick={() => setModelsDir(null)}
                    className="shrink-0 rounded-md px-2 py-1.5 text-[11px] text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
                  >
                    Use default
                  </button>
                )}
              </div>
              <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                Ollama models move to this folder only when Sediment starts the Ollama server — an
                Ollama already running keeps its current location until next launch.
              </p>
            </>
          )}
        </section>

        {(() => {
          // Warn — but don't block — when the user's chosen engine isn't ready.
          // The user may want to save the choice now and finish the install
          // separately; turns simply fail until they do.
          const selectedStatus = engine === "claude-code" ? claudeCode : copilot;
          const selectedReady =
            engine === "claude-code"
              ? !!claudeCode?.installed && !!claudeCode.logged_in
              : !!copilot?.installed;
          if (selectedStatus === null || selectedReady) return null;
          return (
            <p className="rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-[11px] text-amber-800 dark:border-amber-800/60 dark:bg-amber-950/40 dark:text-amber-300">
              This engine isn't ready — turns will fail until you sign in / install.
            </p>
          );
        })()}

        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={saving}
            className="rounded-md px-3 py-1.5 text-xs text-zinc-500 hover:bg-zinc-100 disabled:opacity-40 dark:text-zinc-400 dark:hover:bg-zinc-800"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={saving || loading}
            className="rounded-md bg-zinc-900 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900"
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
