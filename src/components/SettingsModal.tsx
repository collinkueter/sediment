import { Segmented } from "@/components/Segmented";
import { Icon } from "@/components/icons";
import { useFormationStore } from "@/lib/store";
import type { ClaudeCodeStatus, CopilotModels, CopilotStatus, ModelReadiness } from "@/lib/tauri";
import { tauri } from "@/lib/tauri";
import { type Theme, useThemeStore } from "@/lib/theme";
import { useEffect, useRef, useState } from "react";

type Engine = "claude-code" | "copilot";
type SearchMode = "bundled" | "ollama" | "none";
type Tone = "stoic" | "warm" | "sassy";

const TONE_DESCRIPTIONS: Record<Tone, string> = {
  stoic: "Calm and economical — says what's needed, then gets out of your way.",
  warm: "A sharp, plainspoken friend who keeps your notes. The default.",
  sassy: "Good company with a dry, knowing edge — wit that reads the room.",
};

const SEARCH_MODE_DESCRIPTIONS: Record<SearchMode, string> = {
  bundled: "On-device semantic search — runs in the app, no Ollama. Downloads ~80 MB once.",
  ollama: "Semantic search via a local Ollama embedding model (needs the Ollama daemon).",
  none: "Keyword search — no model, fully offline.",
};

/** The Claude Code model aliases offered in the model selector. */
const CLAUDE_MODELS = ["sonnet", "opus", "haiku"];

/** Honest status line for the active note-search model, replacing the old
 *  hardcoded "all-MiniLM · Ready" placeholder. Keyword mode needs no model;
 *  the semantic providers show whether their model is installed. */
function ModelStatusRow({
  searchMode,
  readiness,
}: {
  searchMode: SearchMode;
  readiness: ModelReadiness | null;
}) {
  let detail: string;
  let ready: boolean;
  if (searchMode === "none") {
    detail = "Keyword search — no model required.";
    ready = true;
  } else if (readiness === null) {
    detail = "Checking…";
    ready = false;
  } else {
    const req = readiness.requirements[0];
    detail =
      req?.label ?? (searchMode === "bundled" ? "On-device embedding model" : "Embedding model");
    ready = readiness.all_present;
  }
  const showChip = searchMode === "none" || readiness !== null;
  return (
    <div className="flex items-center justify-between py-[11px]">
      <div className="min-w-0 flex-1 pr-4">
        <p className="text-[13.5px] font-medium text-ink">Local models</p>
        <p className="mt-0.5 truncate text-[11.5px] text-muted">{detail}</p>
      </div>
      {showChip && (
        <span
          className={[
            "inline-flex items-center gap-[5px] text-[12px] font-semibold",
            ready ? "text-sage" : "text-warn",
          ].join(" ")}
        >
          <span
            className="inline-block h-[6px] w-[6px] rounded-full bg-current"
            aria-hidden="true"
          />
          {ready ? "Ready" : "Not installed"}
        </span>
      )}
    </div>
  );
}

/** Format a Copilot premium-request multiplier for display: "0x" → "Free". */
function copilotCost(usage: string | null): string {
  if (!usage) return "";
  if (usage === "0x" || usage === "0×") return " · Free";
  return ` · ${usage.replace(/x$/i, "×")}`;
}

/// Settings overlay. Sections:
///  - Conversation engine: which agentic CLI runs the conversation (ADR-0009).
///  - Appearance: theme picker.
///  - Formation: model storage path + local model status.
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
  const [copilotModels, setCopilotModels] = useState<CopilotModels | null>(null);
  const [copilotModelsLoading, setCopilotModelsLoading] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searchMode, setSearchMode] = useState<SearchMode>("ollama");
  const [ollamaUrl, setOllamaUrl] = useState("");
  const [initialOllamaUrl, setInitialOllamaUrl] = useState("");
  const [ollamaUrlError, setOllamaUrlError] = useState<string | null>(null);
  const [tone, setTone] = useState<Tone>("warm");
  const [readiness, setReadiness] = useState<ModelReadiness | null>(null);
  const [importing, setImporting] = useState(false);
  const [importMsg, setImportMsg] = useState<string | null>(null);
  const dialogRef = useRef<HTMLDivElement>(null);

  // Honest readout of the active note-search model. Loaded separately from the
  // form (an Ollama check can spin up the daemon) so it never blocks the dialog.
  function refreshReadiness() {
    tauri
      .checkModelReadiness()
      .then(setReadiness)
      .catch(() => setReadiness(null));
  }

  const { theme, setTheme } = useThemeStore();
  const formationPath = useFormationStore((s) => s.formationPath);
  const pickFormation = useFormationStore((s) => s.pick);

  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only load; refreshReadiness reads no reactive value and the setters are stable.
  useEffect(() => {
    Promise.all([
      tauri.getModelsDir(),
      tauri.getConversationEngine(),
      tauri.getEmbeddingProvider(),
      tauri.getAgentTone(),
      tauri.getOllamaUrl(),
    ])
      .then(([dir, ce, provider, agentTone, oUrl]) => {
        setModelsDir(dir);
        setInitialModelsDir(dir);
        setEngine((ce.engine as Engine) ?? "claude-code");
        setClaudeCodeModel(ce.claude_code_model ?? "sonnet");
        setCopilotModel(ce.copilot_model ?? "");
        setSearchMode(provider === "none" ? "none" : provider === "bundled" ? "bundled" : "ollama");
        setTone(agentTone === "stoic" || agentTone === "sassy" ? agentTone : "warm");
        setOllamaUrl(oUrl ?? "");
        setInitialOllamaUrl(oUrl ?? "");
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
    refreshReadiness();
  }, []);

  // Discover the Copilot account's models live when the Copilot engine is in
  // view and installed (ADR-0012). Best-effort: on failure we keep the free-text
  // fallback. Spawns a short-lived `copilot --acp` handshake — no prompt is sent.
  useEffect(() => {
    if (engine !== "copilot" || !copilot?.installed || copilotModels) return;
    setCopilotModelsLoading(true);
    tauri
      .listCopilotModels()
      .then(setCopilotModels)
      .catch(() => setCopilotModels(null))
      .finally(() => setCopilotModelsLoading(false));
  }, [engine, copilot?.installed, copilotModels]);

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

  // The search mode applies immediately (independent of Save): it changes
  // whether launches require the embedding model, so re-running the readiness
  // check via onModelConfigChanged keeps the setup gate in sync.
  async function changeSearchMode(mode: SearchMode) {
    const previous = searchMode;
    setSearchMode(mode);
    setError(null);
    try {
      await tauri.setEmbeddingProvider(mode);
      // Re-run the launch-time readiness check. If on-device is selected but the
      // model files aren't installed, this surfaces the setup screen (Download /
      // Import) instead of letting indexing and search fail silently later. The
      // Ollama gate works the same way; keyword needs no model.
      onModelConfigChanged();
      // Switching to a semantic provider invalidates the prior vectors. If its
      // model is already installed, re-embed now so search returns results; if
      // not, the setup screen re-indexes after install. Keyword uses no vectors.
      if (mode !== "none") {
        const r = await tauri.checkModelReadiness();
        setReadiness(r);
        if (r.all_present) {
          tauri
            .indexFormation(true)
            .catch((e) => console.warn("re-index after search-mode switch failed:", e));
        }
      } else {
        refreshReadiness();
      }
    } catch (e) {
      setSearchMode(previous);
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  // BYO model (on-device): the user downloaded the embedding-model files
  // themselves and points at the folder. The app validates and installs them
  // into its own model dir, then uses them — no network. Switches to the
  // on-device provider if needed, then re-indexes so search uses the new vectors.
  async function importModelFolder() {
    setImportMsg(null);
    let dir: string | null = null;
    try {
      dir = await tauri.pickDirectory();
    } catch (e) {
      setImportMsg(e instanceof Error ? e.message : String(e));
      return;
    }
    if (!dir) return;
    setImporting(true);
    try {
      if (searchMode !== "bundled") {
        await tauri.setEmbeddingProvider("bundled");
        setSearchMode("bundled");
      }
      await tauri.importBundledModel(dir);
      refreshReadiness();
      onModelConfigChanged();
      tauri
        .indexFormation(true)
        .catch((e) => console.warn("re-index after model import failed:", e));
      setImportMsg("Model imported.");
    } catch (e) {
      setImportMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setImporting(false);
    }
  }

  // Tone applies immediately (independent of Save): it only changes reply
  // wording, takes effect on the next turn, and recycles the warm Copilot
  // session server-side — nothing else in the form depends on it.
  async function changeTone(next: Tone) {
    const prev = tone;
    setTone(next);
    try {
      await tauri.setAgentTone(next);
    } catch (e) {
      setTone(prev);
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  // The Ollama endpoint applies immediately (independent of Save), like the
  // search mode: it reconfigures the live sidecar, so re-running readiness keeps
  // the setup gate honest against whatever the new endpoint reports.
  const ollamaUrlChanged = ollamaUrl.trim() !== initialOllamaUrl.trim();
  async function saveOllamaUrl() {
    const trimmed = ollamaUrl.trim();
    setOllamaUrlError(null);
    try {
      await tauri.setOllamaUrl(trimmed === "" ? null : trimmed);
      setInitialOllamaUrl(trimmed);
      onModelConfigChanged();
      refreshReadiness();
    } catch (e) {
      setOllamaUrlError(e instanceof Error ? e.message : String(e));
    }
  }

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

  // Derived engine ready state
  const claudeCodeReady = !!claudeCode?.installed && !!claudeCode.logged_in;
  const copilotReady = !!copilot?.installed;
  const copilotDefaultName =
    copilotModels?.available.find((m) => m.modelId === copilotModels.currentModelId)?.name ?? null;
  const selectedStatus = engine === "claude-code" ? claudeCode : copilot;
  const selectedReady = engine === "claude-code" ? claudeCodeReady : copilotReady;
  const showWarning = selectedStatus !== null && !selectedReady;

  // Status chip text/color for each engine
  function claudeCodeStatusLabel(): string {
    if (claudeCode === null) return "Checking…";
    if (!claudeCode.installed) return "Not installed";
    if (!claudeCode.logged_in) return "Not signed in";
    const parts = ["Installed · authenticated"];
    if (claudeCode.email) parts.push(claudeCode.email);
    if (claudeCode.subscription_type) parts.push(claudeCode.subscription_type);
    return parts.join(" · ");
  }

  function copilotStatusLabel(): string {
    if (copilot === null) return "Checking…";
    if (!copilot.installed) return "Not detected";
    return "Installed";
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-6">
      <button
        type="button"
        aria-label="Close settings"
        className="absolute inset-0 cursor-default bg-black/40"
        onClick={onClose}
      />
      <div
        ref={dialogRef}
        // biome-ignore lint/a11y/useSemanticElements: native <dialog> requires the imperative showModal API and brings its own focus / top-layer model — we keep a styled div + ARIA so the visibility-via-prop pattern (`{settingsOpen && …}`) stays intact.
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
        className="relative max-h-[84vh] w-[min(660px,94vw)] overflow-y-auto rounded-2xl border border-line-strong bg-raised shadow-2xl"
      >
        {/* Sticky header */}
        <div className="sticky top-0 z-10 flex items-center justify-between border-b border-line bg-raised px-[22px] py-[18px]">
          <h2 id="settings-title" className="font-serif text-[19px] font-semibold text-ink">
            Settings
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close settings"
            className="grid h-[30px] w-[30px] place-items-center rounded-lg border-none bg-transparent text-muted hover:bg-bg-sunk hover:text-ink"
          >
            <Icon.X className="h-[18px] w-[18px]" />
          </button>
        </div>

        {/* Body */}
        <div className="px-[22px] pb-6 pt-5">
          {/* ── Conversation engine ── */}
          <section className="mb-[26px]">
            <p className="mb-[11px] text-[11px] font-bold uppercase tracking-[.06em] text-faint">
              Conversation engine
            </p>

            {loading ? (
              <p className="text-xs text-muted">Loading…</p>
            ) : (
              <>
                <div className="grid grid-cols-2 gap-3">
                  {/* Claude Code card */}
                  <button
                    type="button"
                    onClick={() => setEngine("claude-code")}
                    className={[
                      "relative cursor-pointer rounded-[13px] border-[1.5px] p-[15px] text-left transition-colors",
                      engine === "claude-code"
                        ? "border-accent bg-accent-tint"
                        : "border-line hover:border-line-strong",
                    ].join(" ")}
                  >
                    {/* Selected check badge */}
                    {engine === "claude-code" && (
                      <span className="absolute right-3 top-3 grid h-5 w-5 place-items-center rounded-full bg-accent text-white">
                        <Icon.Check className="h-3 w-3" strokeWidth={3} />
                      </span>
                    )}
                    <div className="flex items-center gap-[9px] text-[14px] font-semibold text-ink">
                      <span
                        className="grid h-[25px] w-[25px] shrink-0 place-items-center rounded-[7px] text-[12px] font-bold text-white bg-[linear-gradient(150deg,var(--accent),var(--accent-ink))]"
                        aria-hidden="true"
                      >
                        C
                      </span>
                      Claude Code
                    </div>
                    <p className="mt-2 text-[12px] leading-relaxed text-muted">
                      Drives your own installed, own-authenticated Claude Code binary. Nothing
                      leaves your machine but the turn.
                    </p>
                    <span
                      className={[
                        "mt-[10px] inline-flex items-center gap-[5px] text-[11px] font-semibold",
                        claudeCodeReady ? "text-sage" : "text-muted",
                      ].join(" ")}
                    >
                      <span
                        className="inline-block h-[6px] w-[6px] rounded-full bg-current"
                        aria-hidden="true"
                      />
                      {claudeCodeStatusLabel()}
                    </span>
                  </button>

                  {/* GitHub Copilot card */}
                  <button
                    type="button"
                    onClick={() => setEngine("copilot")}
                    className={[
                      "relative cursor-pointer rounded-[13px] border-[1.5px] p-[15px] text-left transition-colors",
                      engine === "copilot"
                        ? "border-accent bg-accent-tint"
                        : "border-line hover:border-line-strong",
                    ].join(" ")}
                  >
                    {engine === "copilot" && (
                      <span className="absolute right-3 top-3 grid h-5 w-5 place-items-center rounded-full bg-accent text-white">
                        <Icon.Check className="h-3 w-3" strokeWidth={3} />
                      </span>
                    )}
                    <div className="flex items-center gap-[9px] text-[14px] font-semibold text-ink">
                      <span
                        className="grid h-[25px] w-[25px] shrink-0 place-items-center rounded-[7px] bg-[#24292f] text-[11px] font-bold text-white"
                        aria-hidden="true"
                      >
                        GH
                      </span>
                      GitHub Copilot
                    </div>
                    <p className="mt-2 text-[12px] leading-relaxed text-muted">
                      Uses your Copilot CLI subscription as the engine. The agent persona is
                      identical either way.
                    </p>
                    <span
                      className={[
                        "mt-[10px] inline-flex items-center gap-[5px] text-[11px] font-semibold",
                        copilotReady ? "text-sage" : "text-muted",
                      ].join(" ")}
                    >
                      <span
                        className="inline-block h-[6px] w-[6px] rounded-full bg-current"
                        aria-hidden="true"
                      />
                      {copilotStatusLabel()}
                    </span>
                  </button>
                </div>

                {/* Model selector — shown below the grid for the selected engine */}
                {engine === "claude-code" && (
                  <label className="mt-3 block text-[11px] text-ink-soft">
                    Model
                    <select
                      value={claudeCodeModel}
                      onChange={(e) => setClaudeCodeModel(e.target.value)}
                      className="mt-1 block w-full rounded-lg border border-line bg-surface px-2 py-1.5 text-[12px] text-ink focus:border-accent focus:outline-none"
                    >
                      <option value="sonnet">Sonnet — balanced (recommended)</option>
                      <option value="opus">Opus — most capable</option>
                      <option value="haiku">Haiku — fastest</option>
                      {!CLAUDE_MODELS.includes(claudeCodeModel) && (
                        <option value={claudeCodeModel}>{claudeCodeModel} (custom)</option>
                      )}
                    </select>
                  </label>
                )}

                {engine === "copilot" && (
                  <label htmlFor="copilot-model" className="mt-3 block text-[11px] text-ink-soft">
                    Model
                    {copilotModels && copilotModels.available.length > 0 ? (
                      <select
                        id="copilot-model"
                        value={copilotModel}
                        onChange={(e) => setCopilotModel(e.target.value)}
                        className="mt-1 block w-full rounded-lg border border-line bg-surface px-2 py-1.5 text-[12px] text-ink focus:border-accent focus:outline-none"
                      >
                        <option value="">
                          Copilot default{copilotDefaultName ? ` (${copilotDefaultName})` : ""}
                        </option>
                        {copilotModels.available.map((m) => (
                          <option key={m.modelId} value={m.modelId} disabled={!m.enabled}>
                            {m.name}
                            {copilotCost(m.usage)}
                            {m.enabled ? "" : " · unavailable"}
                          </option>
                        ))}
                        {copilotModel !== "" &&
                          !copilotModels.available.some((m) => m.modelId === copilotModel) && (
                            <option value={copilotModel}>{copilotModel} (custom)</option>
                          )}
                      </select>
                    ) : (
                      <input
                        id="copilot-model"
                        list="copilot-models"
                        value={copilotModel}
                        onChange={(e) => setCopilotModel(e.target.value)}
                        placeholder="Copilot default"
                        className="mt-1 block w-full rounded-lg border border-line bg-surface px-2 py-1.5 font-mono text-[12px] text-ink placeholder:text-faint focus:border-accent focus:outline-none"
                      />
                    )}
                    {!copilotModels && (
                      <datalist id="copilot-models">
                        <option value="claude-haiku-4.5" />
                        <option value="gpt-5-mini" />
                      </datalist>
                    )}
                    <span className="mt-1 block text-[11px] text-muted">
                      {copilotModelsLoading
                        ? "Reading your account's models…"
                        : copilotModels && copilotModels.available.length > 0
                          ? "Live from your Copilot account — cost is the premium-request multiplier."
                          : "Leave blank for the Copilot default."}
                    </span>
                  </label>
                )}

                {/* Tone — a parameter of the one behaviour prompt; applies next turn */}
                <div className="mt-3 flex items-center justify-between gap-4 border-t border-line pt-3">
                  <div className="min-w-0 flex-1">
                    <p className="text-[13.5px] font-medium text-ink">Tone</p>
                    <p className="mt-0.5 text-[11.5px] text-muted">{TONE_DESCRIPTIONS[tone]}</p>
                  </div>
                  <Segmented<Tone>
                    value={tone}
                    onChange={(t) => void changeTone(t)}
                    ariaLabel="Agent tone"
                    options={[
                      { value: "stoic", label: "Stoic" },
                      { value: "warm", label: "Warm" },
                      { value: "sassy", label: "Sassy" },
                    ]}
                  />
                </div>
              </>
            )}

            {error && <p className="mt-2 text-xs text-danger">{error}</p>}
          </section>

          {/* ── Appearance ── */}
          <section className="mb-[26px]">
            <p className="mb-[11px] text-[11px] font-bold uppercase tracking-[.06em] text-faint">
              Appearance
            </p>
            <div className="flex items-center justify-between border-b border-line py-[11px]">
              <div>
                <p className="text-[13.5px] font-medium text-ink">Theme</p>
                <p className="mt-0.5 text-[11.5px] text-muted">
                  Paper for daylight, Strata for night
                </p>
              </div>
              <Segmented<Theme>
                value={theme}
                onChange={setTheme}
                ariaLabel="Theme"
                options={[
                  { value: "paper", label: "Paper" },
                  { value: "strata", label: "Strata" },
                ]}
              />
            </div>
          </section>

          {/* ── Note search ── */}
          <section className="mb-[26px]">
            <p className="mb-[11px] text-[11px] font-bold uppercase tracking-[.06em] text-faint">
              Note search
            </p>
            <div className="flex items-center justify-between gap-4 border-b border-line py-[11px]">
              <div className="min-w-0 flex-1">
                <p className="text-[13.5px] font-medium text-ink">Retrieval</p>
                <p className="mt-0.5 text-[11.5px] text-muted">
                  {SEARCH_MODE_DESCRIPTIONS[searchMode]} Switching re-indexes your notes.
                </p>
              </div>
              <Segmented<SearchMode>
                value={searchMode}
                onChange={(m) => void changeSearchMode(m)}
                ariaLabel="Note search mode"
                options={[
                  { value: "bundled", label: "On-device" },
                  { value: "ollama", label: "Ollama" },
                  { value: "none", label: "Keyword" },
                ]}
              />
            </div>

            {/* Custom Ollama endpoint — run the model yourself (Docker/Podman/
                remote). For locked-down networks where direct model downloads are
                blocked but a container image can be pulled through approved channels. */}
            {searchMode === "ollama" && (
              <div className="py-[11px]">
                <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0 flex-1">
                    <p className="text-[13.5px] font-medium text-ink">Ollama endpoint</p>
                    <p className="mt-0.5 text-[11.5px] text-muted">
                      Leave blank to auto-manage a local daemon. Or run Ollama yourself — e.g. in
                      Docker/Podman — and point Sediment at it.
                    </p>
                  </div>
                </div>
                <div className="mt-2 flex items-center gap-2">
                  <input
                    type="text"
                    value={ollamaUrl}
                    onChange={(e) => setOllamaUrl(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && ollamaUrlChanged) void saveOllamaUrl();
                    }}
                    placeholder="http://localhost:11434"
                    spellCheck={false}
                    autoCapitalize="off"
                    autoCorrect="off"
                    className="min-w-0 flex-1 rounded-md border border-line bg-surface px-2.5 py-1.5 font-mono text-[12px] text-ink placeholder:text-faint focus:border-accent-ink focus:outline-none"
                  />
                  <button
                    type="button"
                    onClick={() => void saveOllamaUrl()}
                    disabled={!ollamaUrlChanged}
                    className="shrink-0 rounded-md border border-line px-3 py-1.5 text-[12px] font-semibold text-accent-ink hover:bg-raised disabled:cursor-default disabled:opacity-40"
                  >
                    Save
                  </button>
                  {ollamaUrl.trim() !== "" && (
                    <button
                      type="button"
                      onClick={() => setOllamaUrl("")}
                      className="shrink-0 text-[12px] text-muted hover:text-ink-soft"
                    >
                      Clear
                    </button>
                  )}
                </div>
                {ollamaUrlError && (
                  <p className="mt-1.5 text-[11.5px] text-warn">{ollamaUrlError}</p>
                )}
              </div>
            )}

            {/* On-device: bring-your-own model. Point Sediment at a folder of
                model files you downloaded yourself; it installs and uses them
                (the offline path — no download). */}
            {searchMode === "bundled" && (
              <div className="py-[11px]">
                <div className="flex items-center justify-between gap-4">
                  <div className="min-w-0 flex-1">
                    <p className="text-[13.5px] font-medium text-ink">Model files</p>
                    <p className="mt-0.5 text-[11.5px] text-muted">
                      Sediment downloads the model for you. Or, if you downloaded it yourself,
                      import the folder and Sediment uses those files — no network needed.
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={() => void importModelFolder()}
                    disabled={importing}
                    className="shrink-0 rounded-md border border-line px-3 py-1.5 text-[12px] font-semibold text-accent-ink hover:bg-raised disabled:cursor-default disabled:opacity-40"
                  >
                    {importing ? "Importing…" : "Import folder…"}
                  </button>
                </div>
                {importMsg && <p className="mt-1.5 text-[11.5px] text-muted">{importMsg}</p>}
              </div>
            )}
          </section>

          {/* ── Formation ── */}
          <section className="mb-[26px]">
            <p className="mb-[11px] text-[11px] font-bold uppercase tracking-[.06em] text-faint">
              Formation
            </p>

            {loading ? (
              <p className="text-xs text-muted">Loading…</p>
            ) : (
              <>
                {/* Formation location (the notes folder) */}
                <div className="flex items-center justify-between border-b border-line py-[11px]">
                  <div className="min-w-0 flex-1 pr-4">
                    <p className="text-[13.5px] font-medium text-ink">Location</p>
                    <p className="mt-0.5 truncate font-mono text-[11.5px] text-muted">
                      {formationPath ?? "No formation open"}
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={() => void pickFormation()}
                    className="shrink-0 text-[13px] font-semibold text-accent-ink hover:underline"
                  >
                    Switch…
                  </button>
                </div>

                {/* Model storage (the embedding-model cache directory) */}
                <div className="flex items-center justify-between border-b border-line py-[11px]">
                  <div className="min-w-0 flex-1 pr-4">
                    <p className="text-[13.5px] font-medium text-ink">Model storage</p>
                    <p className="mt-0.5 truncate font-mono text-[11.5px] text-muted">
                      {modelsDir ?? "Default — Ollama's own storage location"}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <button
                      type="button"
                      onClick={() => void chooseFolder()}
                      className="text-[13px] font-semibold text-accent-ink hover:underline"
                    >
                      Change…
                    </button>
                    {modelsDir && (
                      <button
                        type="button"
                        onClick={() => setModelsDir(null)}
                        className="text-[12px] text-muted hover:text-ink-soft"
                      >
                        Use default
                      </button>
                    )}
                  </div>
                </div>

                {/* Local models status — reflects the active search mode and
                    whether its model is actually installed (no hardcoding). */}
                <ModelStatusRow searchMode={searchMode} readiness={readiness} />
              </>
            )}
          </section>

          {/* Engine not ready warning */}
          {showWarning && (
            <p className="mb-4 flex items-center gap-2 rounded-lg border border-warn/40 bg-warn-tint px-3 py-2 text-[11.5px] text-warn">
              <Icon.Warning className="h-4 w-4 flex-none" />
              This engine isn't ready — turns will fail until you sign in / install.
            </p>
          )}

          {/* Footer actions */}
          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              disabled={saving}
              className="rounded-lg px-3 py-1.5 text-xs text-muted hover:bg-bg-sunk hover:text-ink-soft disabled:opacity-40"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={() => void save()}
              disabled={saving || loading}
              className="rounded-lg bg-accent px-3 py-1.5 text-xs font-semibold text-white hover:bg-accent-ink disabled:opacity-40"
            >
              {saving ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
