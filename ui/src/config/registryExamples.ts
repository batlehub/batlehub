/**
 * Example coordinates per registry type: package names and versions as each
 * ecosystem's own clients write them. Data, kept verbatim in every locale —
 * the i18n audit skips this file for the same reason it skips the setup
 * snippets in `registryTypes.ts`.
 */

/**
 * An example of what a package-addressed registry's warm field takes, per
 * type — the coordinate *its* clients use.
 *
 * Every package-addressed registry showed `lodash, react@18.0.0`, so a cargo
 * or Go registry was offered an npm package as the example of its own input:
 * the same defect §6.1 fixed for paths, one field over. Data, not markup, so
 * a new type adds a line here; an unlisted type falls back to the generic
 * `name@version` shape rather than to another ecosystem's package.
 */
const PACKAGE_EXAMPLES: Record<string, string> = {
  npm: "lodash, react@18.3.1",
  cargo: "serde, tokio@1.40.0",
  pypi: "requests, django@5.1.2",
  conda: "numpy, pandas@2.2.3",
  composer: "monolog/monolog@3.7.0",
  maven: "org.slf4j:slf4j-api@2.0.16",
  nuget: "Newtonsoft.Json@13.0.3",
  rubygems: "rails, rack@3.1.8",
  goproxy: "github.com/gorilla/mux@v1.8.1",
  terraform: "hashicorp/aws@5.70.0",
  github: "owner/repo@v1.2.3",
  gitlab: "group/project@v1.2.3",
  forgejo: "owner/repo@v1.2.3",
  openvsx: "rust-lang.rust-analyzer",
  "vscode-marketplace": "ms-python.python",
  "jetbrains-marketplace": "org.rust.lang",
  galaxy: "community.general@9.5.0",
  helm: "ingress-nginx@4.11.3",
};

export function packageExampleFor(registryType: string | undefined): string {
  return (registryType && PACKAGE_EXAMPLES[registryType]) || "name, name@1.2.3";
}
