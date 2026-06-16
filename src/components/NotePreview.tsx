import { useFormationStore } from "@/lib/store";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useMemo } from "react";
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

export function NotePreview({ source }: { source: string }) {
  const notes = useFormationStore((s) => s.notes);
  const openNote = useFormationStore((s) => s.openNote);

  const { body, entries } = useMemo(() => splitFrontmatter(source), [source]);
  const processed = useMemo(() => preprocessWikiLinks(body), [body]);

  return (
    <div className="px-6 py-7">
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
                // Semantically a click-action, not a navigation — a button is
                // the correct element. CSS keeps the link-like appearance.
                return (
                  <button
                    type="button"
                    className={broken ? "wikilink-broken" : "wikilink"}
                    title={
                      broken
                        ? `No note found for [[${decodeURIComponent(target)}]]`
                        : (resolved as string)
                    }
                    onClick={() => {
                      if (resolved) {
                        openNote(resolved).catch((err) => console.error("openNote failed:", err));
                      }
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
    </div>
  );
}
