/**
 * An example of what a package-addressed registry's warm field takes, per
 * type — the coordinate *its* clients use.
 *
 * Every package-addressed registry showed `lodash, react@18.0.0`, so a cargo
 * or Go registry was offered an npm package as the example of its own input:
 * the same defect §6.1 fixed for paths, one field over. Read from
 * `EXAMPLE_COORDINATES`, the table Access Check already uses, so the two
 * forms never offer one ecosystem two different packages; an unlisted type
 * falls back to the generic `name@version` shape rather than to another
 * ecosystem's package.
 */
import { EXAMPLE_COORDINATES } from "./registryTypes";

/** Extension galleries warm an extension by its id, without a version. */
const NAME_ONLY = new Set(["openvsx", "vscode-marketplace", "jetbrains-marketplace"]);

export function packageExampleFor(registryType: string | undefined): string {
  const c = registryType ? EXAMPLE_COORDINATES[registryType] : undefined;
  if (!c) return "name, name@1.2.3";
  return NAME_ONLY.has(registryType ?? "") ? c.name : `${c.name}@${c.version}`;
}
