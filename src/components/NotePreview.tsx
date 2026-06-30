import { useFormationStore } from "@/lib/store";
import { type Backlink, tauri } from "@/lib/tauri";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useEffect, useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Rendered markdown preview — Obsidian-style. The agent edits notes on disk
 * as raw markdown (ADR-0009 §5), and this component is what the user reads.
 *
 * Two link kinds are intercepted:
 * - `[[Wiki Link]]` (Obsidian syntax, pre-processed to `wiki:` URLs below)
 *   resolves to a note in the active formation; clicking opens it inline.
 * - External `http(s)://...` links open in the OS default browser via the
 *   Tauri shell plugin — never inside the app.
 *
 * Task-list checkboxes render visually (GFM via remark-gfm) but are
 * read-only in V1 — flipping them needs to round-trip through the file, which
 * is the agent's job for daily-note `## Checklist` items.
 */

const WIKI_PROTOCOL = "wiki:";

interface FrontmatterEntry {
  key: string;
  value: string;
}

/**
 * Split a leading YAML frontmatter block (`---\n…\n---`) off the source. Parses
 * only flat `key: value` scalar lines — nested/blank/list values are skipped,
 * which keeps the chip row honest without pulling in a YAML dependency. Returns
 * the stripped body and the scalar entries. Defensive: anything it can't parse
 * leaves the original source untouched so rendering never breaks.
 */
function splitFrontmatter(src: string): { body: string; entries: FrontmatterEntry[] } {
  if (!src.startsWith("---\n") && src !== "---") {
    return { body: src, entries: [] };
  }
  const match = src.match(/^---\n([\s\S]*?)\n---\n?/);
  if (!match) {
    return { body: src, entries: [] };
  }
  const [block, inner] = match;
  const entries: FrontmatterEntry[] = [];
  for (const rawLine of (inner ?? "").split("\n")) {
    const line = rawLine.trimEnd();
    // Skip blanks, comments, and list items — only flat scalars become chips.
    if (!line.trim() || line.trimStart().startsWith("#") || line.trimStart().startsWith("-")) {
      continue;
    }
    const colon = line.indexOf(":");
    if (colon <= 0) continue;
    const key = line.slice(0, colon).trim();
    let value = line.slice(colon + 1).trim();
    // Strip surrounding quotes from a quoted scalar.
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    if (!key || !value) continue;
    entries.push({ key, value });
  }
  return { body: src.slice((block ?? "").length), entries };
}

/**
 * Pre-process Obsidian wiki-link syntax into standard markdown links carrying
 * a `wiki:` protocol, so `react-markdown` can intercept them through its
 * `components.a` hook. Supports both `[[Name]]` and `[[Name|Display]]` forms.
 */
function preprocessWikiLinks(src: string): string {
  return src.replace(/\[\[([^\]|\n]+)(?:\|([^\]\n]+))?\]\]/g, (_match, target, display) => {
    const t = String(target).trim();
    const text = (display ? String(display) : t).trim();
    return `[${text}](${WIKI_PROTOCOL}${encodeURIComponent(t)})`;
  });
}

/**
 * Resolve a wiki target string to a formation-relative note path, or `null`
 * if no matching note exists. Tries (1) the exact relative path, (2) the same
 * with `.md` appended, (3) a case-insensitive basename match (the typical
 * `[[Keaton]]` → `People/Keaton.md` case).
 */
function resolveWikiTarget(target: string, notes: { relative_path: string }[]): string | null {
  const decoded = decodeURIComponent(target);
  const exact = notes.find(
    (n) => n.relative_path === decoded || n.relative_path === `${decoded}.md`,
  );
  if (exact) return exact.relative_path;

  const lower = decoded.toLowerCase();
  const byBase = notes.find((n) => {
    const base = n.relative_path.replace(/^.*\//, "").replace(/\.md$/i, "").toLowerCase();
    return base === lower;
  });
  return byBase?.relative_path ?? null;
}

export function NotePreview({
  source,
  notePath,
  onSpeakerClick,
}: {
  source: string;
  notePath?: string;
  /** When set (Meeting notes), a transcript speaker label `**Name:**` renders as a
   *  button that calls this — letting you fix a misattribution right where you see
   *  it, jumping to that speaker's card in the panel (ADR-0017 §6). */
  onSpeakerClick?: (name: string) => void;
}) {
  const notes = useFormationStore((s) => s.notes);
  const openNote = useFormationStore((s) => s.openNote);
  const refreshNotes = useFormationStore((s) => s.refreshNotes);

  const { body, entries } = useMemo(() => splitFrontmatter(source), [source]);
  const processed = useMemo(() => preprocessWikiLinks(body), [body]);

  // Backlinks — notes that [[wiki-link]] to this one.
  const [backlinks, setBacklinks] = useState<Backlink[]>([]);
  useEffect(() => {
    if (!notePath) {
      setBacklinks([]);
      return;
    }
    tauri
      .noteBacklinks(notePath)
      .then(setBacklinks)
      .catch((err) => {
        console.error("noteBacklinks failed:", err);
        setBacklinks([]);
      });
  }, [notePath]);

  // The prose column is max-width 42rem. Everything in the scroll area —
  // frontmatter chips, body, backlinks — flows inside a shared centered
  // column so they share the same optical left edge at any pane width.
  return (
    <div className="px-6 py-7">
      <div className="mx-auto max-w-[42rem]">
        {entries.length > 0 && (
          <div className="note-frontmatter">
            {entries.map((entry) => (
              <span className="note-tag" key={entry.key}>
                <span className="k">{entry.key}</span> {entry.value}
              </span>
            ))}
          </div>
        )}
        <div className="note-prose">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            // react-markdown's default URL sanitiser rejects unknown protocols,
            // which strips our `wiki:` prefix to an empty href. Allow `wiki:`
            // through (and http(s)/mailto/tel as before); reject the dangerous
            // ones explicitly.
            urlTransform={(url) => {
              if (typeof url !== "string") return url;
              const lower = url.toLowerCase();
              if (lower.startsWith("javascript:") || lower.startsWith("data:")) return "";
              return url;
            }}
            components={{
              a({ href, children, ...rest }) {
                if (typeof href === "string" && href.startsWith(WIKI_PROTOCOL)) {
                  const target = href.slice(WIKI_PROTOCOL.length);
                  const resolved = resolveWikiTarget(target, notes);
                  const broken = resolved === null;
                  const linkName = decodeURIComponent(target);
                  // Semantically a click-action, not a navigation — a button is
                  // the correct element. CSS keeps the link-like appearance.
                  if (!broken) {
                    return (
                      <button
                        type="button"
                        className="wikilink"
                        title={resolved as string}
                        onClick={() => {
                          openNote(resolved as string).catch((err) =>
                            console.error("openNote failed:", err),
                          );
                        }}
                      >
                        {children}
                      </button>
                    );
                  }
                  // Broken link — create the note on click, then open it.
                  return (
                    <button
                      type="button"
                      className="wikilink-broken"
                      title={`Create note: ${linkName}`}
                      style={{ cursor: "pointer" }}
                      onClick={() => {
                        if (!linkName) return;
                        const newPath = `${linkName}.md`;
                        const newContent = `# ${linkName}\n`;
                        tauri
                          .writeNote(newPath, newContent)
                          .then(() => refreshNotes())
                          .then(() => openNote(newPath))
                          .catch((err) => console.error("create note failed:", err));
                      }}
                    >
                      {children}
                    </button>
                  );
                }
                // External http(s) — punt to the OS browser. Other schemes
                // (mailto:, internal anchors) follow default behaviour.
                if (typeof href === "string" && /^https?:\/\//i.test(href)) {
                  return (
                    <a
                      href={href}
                      onClick={(e) => {
                        e.preventDefault();
                        openExternal(href).catch((err) =>
                          console.error("open external failed:", err),
                        );
                      }}
                      {...rest}
                    >
                      {children}
                    </a>
                  );
                }
                return (
                  <a href={href} {...rest}>
                    {children}
                  </a>
                );
              },
              // Transcript speaker labels (`**Name:**`) become click-to-fix buttons
              // in a Meeting note. Scoped by `onSpeakerClick` (only passed for
              // meetings) and the trailing colon, so ordinary bold stays bold.
              strong({ children }) {
                const text = Array.isArray(children)
                  ? children.join("")
                  : typeof children === "string"
                    ? children
                    : "";
                const trimmed = text.trim();
                if (onSpeakerClick && /\S:$/.test(trimmed)) {
                  const name = trimmed.replace(/:$/, "").trim();
                  return (
                    <button
                      type="button"
                      title={`Fix speaker: ${name}`}
                      onClick={() => onSpeakerClick(name)}
                      className="cursor-pointer border-0 bg-transparent p-0 font-semibold text-ink underline decoration-faint decoration-dotted underline-offset-2 hover:decoration-accent"
                    >
                      {children}
                    </button>
                  );
                }
                return <strong>{children}</strong>;
              },
              // GFM task-list checkboxes — render but disabled in V1 (flipping a
              // box requires editing the underlying file; the agent owns that).
              input(props) {
                if (props.type === "checkbox") {
                  return <input {...props} disabled readOnly />;
                }
                return <input {...props} />;
              },
            }}
          >
            {processed}
          </ReactMarkdown>
        </div>
        {backlinks.length > 0 && (
          <div className="note-backlinks">
            <p className="mb-2 text-[10px] font-bold uppercase tracking-[.08em] text-muted">
              Linked from
            </p>
            <div className="flex flex-col gap-1">
              {backlinks.map((bl) => (
                <button
                  key={bl.path}
                  type="button"
                  aria-label={`Open note: ${bl.title}`}
                  onClick={() =>
                    openNote(bl.path).catch((err) => console.error("openNote failed:", err))
                  }
                  className="wikilink w-fit text-left text-[13.5px]"
                >
                  {bl.title}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
