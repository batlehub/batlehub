/**
 * Where a registry's `name = "…"` line sits in a TOML config, for the editor's
 * `?registry=` caret (RFC 0046 §6.3, M5).
 *
 * A line scan rather than a TOML parser: the caret needs an offset into the
 * text the operator is editing, which a parsed document no longer has. Only a
 * `name` key inside a `[[registries]]` table counts (its `[registries.*]`
 * sub-tables included), so a `name` under `[server]` or a notification
 * subscription never matches.
 *
 * shortcut: inline tables (`registries = [{ name = "npm" }]`) are not found;
 * upgrade to a span-preserving TOML parser if a config written that way needs
 * the jump.
 */
export interface RegistryLocation {
  /** Offset just after the `name = "…"` line, where the caret goes. */
  offset: number;
  /** Zero-based line of that `name` key. */
  line: number;
}

const NAME_LINE = /^\s*name\s*=\s*(["'])(.*?)\1\s*(?:#.*)?$/;

export function locateRegistry(toml: string, name: string): RegistryLocation | null {
  let inRegistry = false;
  let offset = 0;
  const lines = toml.split("\n");
  for (const [line, raw] of lines.entries()) {
    const text = raw.replace(/\r$/, "");
    const header = text.trim();
    if (header.startsWith("[")) {
      inRegistry = header === "[[registries]]" || /^\[\[?registries\./.test(header);
    } else if (inRegistry && NAME_LINE.exec(text)?.[2] === name) {
      return { offset: offset + text.length, line };
    }
    offset += raw.length + 1;
  }
  return null;
}
