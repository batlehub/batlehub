/**
 * RFC 0043 decision 1: a release blocked on a forge registry is still offered
 * by mise's versions host, which names it by mise's tool and version, not by
 * the forge's repository and tag. The block dialog offers the matching block
 * on each `mise-versions` registry, pre-filled from this guess and editable:
 * only the admin knows the tool name, so the server relies on none of it.
 */

/** Registry types whose releases a mise tool can be installed from. */
export const MISE_BACKEND_TYPES: readonly string[] = ["github", "gitlab", "forgejo"];

/** The mise block a forge block most likely means. */
export function miseBlockGuess(
  packageName: string,
  tag: string,
): { tool: string; version: string } {
  const tool = packageName.split("/").pop() ?? packageName;
  let version = tag;
  if (version.startsWith(`${tool}-`)) version = version.slice(tool.length + 1);
  else if (/^v\d/.test(version)) version = version.slice(1);
  return { tool, version };
}
