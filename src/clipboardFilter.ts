/**
 * Clipboard popup tab filter (v0.9.4: All | Text | Images).
 *
 * Pure module — no Tauri imports — so the filtering rules are unit
 * tested under node (see the v0.9.4 smoke report). The popup
 * (clipboard.tsx) imports this; the rules here are the single source
 * of truth for what each tab shows.
 */

export type ClipboardTab = "all" | "text" | "image";

/** Tab order left to right; also the Ctrl+Tab cycle order. */
export const CLIPBOARD_TABS: readonly ClipboardTab[] = ["all", "text", "image"];

/** Labels as drawn in the segmented control. */
export const CLIPBOARD_TAB_LABELS: Record<ClipboardTab, string> = {
  all: "All",
  text: "Text",
  image: "Images",
};

export interface TabFilterEntry {
  kind: "text" | "image";
  preview: string;
  imagePath: string | null;
}

/**
 * Anything unexpected (an older settings payload, a hand-edited file)
 * falls back to the mixed list — the popup must never render empty
 * because of a tab value it doesn't understand.
 */
export function normalizeTab(tab: unknown): ClipboardTab {
  return tab === "text" || tab === "image" ? tab : "all";
}

/**
 * Tab first, then the existing search rules: images carry no searchable
 * text, so they match a bare "image" query and are hidden by anything
 * else. Image rows whose PNG is gone from disk are dropped — a blank
 * row would be worse than no row.
 */
export function filterClipboardEntries<E extends TabFilterEntry>(
  entries: E[],
  tab: ClipboardTab,
  query: string
): E[] {
  const inTab =
    tab === "all" ? entries : entries.filter((e) => e.kind === tab);
  const alive = inTab.filter((e) => e.kind !== "image" || e.imagePath);
  const q = query.trim().toLowerCase();
  if (!q) return alive;
  return alive.filter((e) =>
    e.kind === "image"
      ? "image".includes(q)
      : e.preview.toLowerCase().includes(q)
  );
}

/**
 * Empty-state line when a tab has no rows but other tabs do (and no
 * search query is active). Returns "" for "all" — that case can't
 * produce rows-missing-in-tab, so the caller falls back to its
 * generic empty copy.
 */
export function emptyTabCopy(tab: ClipboardTab): string {
  switch (tab) {
    case "text":
      return "No text copied yet.";
    case "image":
      return "No images copied yet.";
    default:
      return "";
  }
}
