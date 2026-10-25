//! IDE detection + editor-specific registry setup suggestions.
//!
//! Where [`crate::api::setup`] scans for *package manifests* (Cargo.toml,
//! package.json, …) and tells you how to point that project's package manager
//! at BatleHub, this module answers a different question: *which editor are you
//! using right now, and how do I point its extension/plugin ecosystem at the
//! proxy?*
//!
//! Detection is best-effort and side-effect free: it reads a handful of
//! environment variables the editors set for their integrated terminals, plus
//! the presence of well-known per-user config directories. The classification
//! is split from the environment/filesystem probing ([`IdeSignals`]) so it can
//! be unit-tested without a real editor installed.

use crate::api::registry::RegistryInfo;

/// A recognised editor/IDE family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeKind {
    /// Microsoft VS Code (and forks sharing its config, e.g. Cursor).
    VsCode,
    /// VSCodium / Code - OSS (open-source builds, default to OpenVSX).
    VsCodium,
    /// Any JetBrains IDE (IntelliJ, GoLand, PyCharm, WebStorm, …).
    JetBrains,
}

impl IdeKind {
    /// Display name shown in the TUI list / CLI output.
    pub fn label(self) -> &'static str {
        match self {
            IdeKind::VsCode => "VS Code",
            IdeKind::VsCodium => "VSCodium / Code - OSS",
            IdeKind::JetBrains => "JetBrains IDE",
        }
    }

    /// Registry types this editor's extension/plugin ecosystem can be served
    /// from, in preference order. The first type that a configured registry
    /// actually exists for is the one embedded in the instructions.
    fn registry_types(self) -> &'static [&'static str] {
        match self {
            // VSCodium defaults to OpenVSX; VS Code's built-in gallery is the
            // Microsoft marketplace, but either proxy type serves both editors.
            IdeKind::VsCode => &["vscode-marketplace", "openvsx"],
            IdeKind::VsCodium => &["openvsx", "vscode-marketplace"],
            IdeKind::JetBrains => &["jetbrains-marketplace"],
        }
    }

    /// Placeholder used in instructions when no registry of a matching type is
    /// configured on the server yet.
    fn registry_placeholder(self) -> &'static str {
        match self {
            IdeKind::VsCode => "<vscode-marketplace-registry>",
            IdeKind::VsCodium => "<openvsx-registry>",
            IdeKind::JetBrains => "<jetbrains-marketplace-registry>",
        }
    }
}

/// A single editor detection with ready-to-follow setup instructions.
pub struct IdeSetup {
    /// The recognised editor family.
    pub kind: IdeKind,
    /// The registry type the instructions target (e.g. `"openvsx"`).
    pub registry_type: &'static str,
    /// The concrete configured registry name embedded in the instructions, or a
    /// `<…>` placeholder when the server has no registry of `registry_type`.
    pub registry_name: String,
    /// Whether `registry_name` is a real configured registry (`true`) or a
    /// placeholder the user still needs to create (`false`).
    pub registry_configured: bool,
    /// Base URL the instructions point at — the registry's own host when it has
    /// one, otherwise `{server}/proxy/{registry_name}`. Also the host a
    /// `~/.netrc` entry has to name.
    pub base_url: String,
    /// Human-readable reason this editor was detected (env var / config path).
    pub detected_via: String,
    /// Multi-line setup instructions shown in the detail pane / CLI output.
    pub instructions: String,
}

/// Raw signals gathered from the environment, kept separate from
/// classification so tests can construct them directly.
#[derive(Debug, Default, Clone)]
pub struct IdeSignals {
    /// `$TERM_PROGRAM` — VS Code sets this to `"vscode"` in its terminal.
    pub term_program: Option<String>,
    /// Any `VSCODE_*` runtime variable is set (running in VS Code's terminal).
    pub vscode_env: bool,
    /// A VS Code per-user config/marker directory exists on disk.
    pub vscode_dir: bool,
    /// A VSCodium / Code - OSS per-user config directory exists on disk.
    pub vscodium_dir: bool,
    /// `$TERMINAL_EMULATOR` — JetBrains sets this to `"JetBrains-JediTerm"`.
    pub terminal_emulator: Option<String>,
    /// Any JetBrains runtime variable is set (running in a JetBrains terminal).
    pub jetbrains_env: bool,
    /// A JetBrains per-user config directory exists on disk.
    pub jetbrains_dir: bool,
    /// A `.idea/` directory is present in the current working directory.
    pub idea_project: bool,
}

impl IdeSignals {
    /// Gather signals from the real environment and filesystem.
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok();
        let env_set = |k: &str| std::env::var_os(k).is_some();
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let cwd = std::env::current_dir().ok();

        // Per-user config dirs live under different roots per OS; probing the
        // common Linux/macOS locations under $HOME covers the usual case.
        let home_has = |rel: &[&str]| {
            home.as_ref().is_some_and(|h| {
                rel.iter().any(|p| {
                    let mut path = h.clone();
                    for seg in p.split('/') {
                        path.push(seg);
                    }
                    path.exists()
                })
            })
        };

        Self {
            term_program: env("TERM_PROGRAM"),
            vscode_env: env_set("VSCODE_PID")
                || env_set("VSCODE_IPC_HOOK")
                || env_set("VSCODE_IPC_HOOK_CLI")
                || env_set("VSCODE_GIT_IPC_HANDLE")
                || env_set("VSCODE_INJECTION"),
            vscode_dir: home_has(&[
                ".config/Code",
                ".vscode",
                "Library/Application Support/Code",
            ]),
            vscodium_dir: home_has(&[
                ".config/VSCodium",
                ".config/Code - OSS",
                ".vscode-oss",
                "Library/Application Support/VSCodium",
            ]),
            terminal_emulator: env("TERMINAL_EMULATOR"),
            jetbrains_env: env_set("IDEA_INITIAL_DIRECTORY")
                || env_set("__INTELLIJ_COMMAND_HISTFILE__"),
            jetbrains_dir: home_has(&[
                ".config/JetBrains",
                "Library/Application Support/JetBrains",
            ]),
            idea_project: cwd.is_some_and(|c| c.join(".idea").is_dir()),
        }
    }
}

/// Classify gathered signals into detected editors, each with the reason it was
/// detected. Order is stable: VSCodium, VS Code, then JetBrains. A machine may
/// legitimately have more than one editor installed, so this returns all hits.
pub fn classify(sig: &IdeSignals) -> Vec<(IdeKind, String)> {
    let mut out: Vec<(IdeKind, String)> = Vec::new();
    let in_vscode_term = sig.term_program.as_deref() == Some("vscode") || sig.vscode_env;

    // VSCodium is checked before VS Code so a Codium-only machine reports the
    // OpenVSX-first mapping rather than being masked by a shared terminal var.
    if sig.vscodium_dir {
        out.push((IdeKind::VsCodium, "~/.config/VSCodium".to_string()));
    }
    if in_vscode_term {
        let via = if sig.term_program.as_deref() == Some("vscode") {
            "$TERM_PROGRAM=vscode".to_string()
        } else {
            "VSCODE_* environment".to_string()
        };
        out.push((IdeKind::VsCode, via));
    } else if sig.vscode_dir {
        out.push((IdeKind::VsCode, "~/.config/Code".to_string()));
    }

    let jetbrains_term = sig.terminal_emulator.as_deref() == Some("JetBrains-JediTerm");
    if jetbrains_term || sig.jetbrains_env {
        out.push((IdeKind::JetBrains, "JetBrains terminal".to_string()));
    } else if sig.idea_project {
        out.push((IdeKind::JetBrains, "./.idea project".to_string()));
    } else if sig.jetbrains_dir {
        out.push((IdeKind::JetBrains, "~/.config/JetBrains".to_string()));
    }

    out
}

/// Detect editors from the real environment and build setup suggestions,
/// substituting a concrete configured registry name for each editor's type when
/// one exists in `registries`.
pub fn detect_ides(server_url: &str, registries: &[RegistryInfo]) -> Vec<IdeSetup> {
    let server = server_url.trim_end_matches('/');
    classify(&IdeSignals::from_env())
        .into_iter()
        .map(|(kind, detected_via)| build_setup(kind, detected_via, server, registries))
        .collect()
}

/// Pick the first configured registry whose type matches one of `types`
/// (preference order preserved).
fn pick_registry<'a>(registries: &'a [RegistryInfo], types: &[&str]) -> Option<&'a str> {
    types.iter().find_map(|t| {
        registries
            .iter()
            .find(|r| r.registry_type == *t)
            .map(|r| r.name.as_str())
    })
}

fn build_setup(
    kind: IdeKind,
    detected_via: String,
    server: &str,
    registries: &[RegistryInfo],
) -> IdeSetup {
    let server = server.trim_end_matches('/');
    let matched = pick_registry(registries, kind.registry_types());
    let registry_configured = matched.is_some();
    let registry_name = matched
        .map(str::to_string)
        .unwrap_or_else(|| kind.registry_placeholder().to_string());
    // The URL the editor talks to: the registry's own host when it has one,
    // else the `/proxy/{name}` subpath — on a host-routed registry the server
    // prefixes that path itself, so writing it here would double it.
    let base = matched
        .and_then(|name| registries.iter().find(|r| r.name == name))
        .map(|r| r.base_url(server))
        .unwrap_or_else(|| format!("{server}/proxy/{registry_name}"));
    // The instruction's registry *type* is the one we matched, or the editor's
    // primary preference when nothing is configured yet.
    let registry_type = matched
        .and_then(|name| {
            registries
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.registry_type.as_str())
        })
        .and_then(|t| kind.registry_types().iter().copied().find(|k| *k == t))
        .unwrap_or_else(|| kind.registry_types()[0]);

    let instructions = match kind {
        IdeKind::VsCode | IdeKind::VsCodium => vscode_instructions(
            kind,
            server,
            &base,
            &registry_name,
            registry_type,
            registry_configured,
        ),
        IdeKind::JetBrains => {
            jetbrains_instructions(server, &base, &registry_name, registry_configured)
        }
    };

    IdeSetup {
        kind,
        registry_type,
        registry_name,
        registry_configured,
        detected_via,
        instructions,
        base_url: base,
    }
}

fn hint_if_placeholder(server: &str, registry_type: &str, configured: bool) -> String {
    if configured {
        String::new()
    } else {
        format!(
            "\n\
             No `{registry_type}` registry is configured yet. Create one, then\n\
             replace the placeholder above with its name. See:\n\
             {server}/  →  Setup Guide  ·  or  batlehub-cli registry suggest\n"
        )
    }
}

/// `registry_type` is the type [`build_setup`] actually resolved — the matched
/// registry's type when one is configured, the editor's primary preference
/// otherwise. Recomputing it from `kind` here would mislabel the fallback case
/// (a VS Code user with only an `openvsx` registry configured).
fn vscode_instructions(
    kind: IdeKind,
    server: &str,
    base: &str,
    reg: &str,
    registry_type: &str,
    configured: bool,
) -> String {
    // vscode-marketplace and openvsx share the same VSIX download route, so the
    // instructions are identical apart from which type we resolved.
    format!(
        "IDE          : {ide}\n\
         Registry     : {registry_type}  ({reg})\n\
         \n\
         Install a single extension directly (always works):\n\
         curl -L {base}/<publisher>.<name>/<version>/vsix \\\n\
           -o ext.vsix\n\
         code --install-extension ext.vsix   # or: codium --install-extension\n\
         \n\
         Point the editor's gallery at the proxy (VSCodium / Code - OSS,\n\
         ~/.config/VSCodium/User/product.json — restart after editing):\n\
         {{\n\
           \"extensionGallery\": {{\n\
             \"serviceUrl\": \"{base}/vscode/gallery\",\n\
             \"itemUrl\":    \"{base}/vscode/item\"\n\
           }}\n\
         }}\n\
         Note: gallery override needs a build that honours product.json;\n\
         VSIX proxying (above) is always supported.\n\
         The editor sends no credentials to its gallery — the registry needs\n\
         anonymous read, or put the credentials in ~/.netrc (below).\n\
         {hint}",
        ide = kind.label(),
        hint = hint_if_placeholder(server, registry_type, configured),
    )
}

fn jetbrains_instructions(server: &str, base: &str, reg: &str, configured: bool) -> String {
    format!(
        "IDE          : JetBrains IDE (IntelliJ / GoLand / PyCharm / …)\n\
         Registry     : jetbrains-marketplace  ({reg})\n\
         \n\
         Additive custom plugin repository (recommended):\n\
         Settings → Plugins → ⚙ → Manage Plugin Repositories… → add:\n\
         {base}/updatePlugins.xml\n\
         \n\
         Full replacement of plugins.jetbrains.com\n\
         (Help → Edit Custom Properties… → idea.properties):\n\
         idea.plugins.host={base}\n\
         Restart the IDE to apply.\n\
         \n\
         IDE installer archives use a separate `jetbrains` (proxy) registry:\n\
         {server}/proxy/<jetbrains-registry>/jetbrains/idea/idea-2026.1.tar.gz\n\
         {hint}",
        hint = hint_if_placeholder(server, "jetbrains-marketplace", configured),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(name: &str, ty: &str) -> RegistryInfo {
        RegistryInfo {
            name: name.to_owned(),
            registry_type: ty.to_owned(),
            mode: "proxy".to_owned(),
            public_url: None,
            upstream: None,
            installable: None,
        }
    }

    fn hosted_reg(name: &str, ty: &str, public_url: &str) -> RegistryInfo {
        RegistryInfo {
            public_url: Some(public_url.to_owned()),
            upstream: None,
            installable: None,
            ..reg(name, ty)
        }
    }

    #[test]
    fn classify_detects_vscode_from_term_program() {
        let sig = IdeSignals {
            term_program: Some("vscode".to_owned()),
            ..Default::default()
        };
        let hits = classify(&sig);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, IdeKind::VsCode);
        assert!(hits[0].1.contains("TERM_PROGRAM"));
    }

    #[test]
    fn classify_detects_vscode_from_env_flag() {
        let sig = IdeSignals {
            vscode_env: true,
            ..Default::default()
        };
        let hits = classify(&sig);
        assert_eq!(
            hits,
            vec![(IdeKind::VsCode, "VSCODE_* environment".to_owned())]
        );
    }

    #[test]
    fn classify_detects_jetbrains_from_terminal_emulator() {
        let sig = IdeSignals {
            terminal_emulator: Some("JetBrains-JediTerm".to_owned()),
            ..Default::default()
        };
        let hits = classify(&sig);
        assert_eq!(
            hits,
            vec![(IdeKind::JetBrains, "JetBrains terminal".to_owned())]
        );
    }

    #[test]
    fn classify_detects_jetbrains_from_idea_project() {
        let sig = IdeSignals {
            idea_project: true,
            ..Default::default()
        };
        let hits = classify(&sig);
        assert_eq!(hits[0].0, IdeKind::JetBrains);
        assert!(hits[0].1.contains(".idea"));
    }

    #[test]
    fn classify_detects_vscodium_before_vscode() {
        let sig = IdeSignals {
            vscodium_dir: true,
            term_program: Some("vscode".to_owned()),
            ..Default::default()
        };
        let hits = classify(&sig);
        // Both surface, VSCodium first.
        assert_eq!(hits[0].0, IdeKind::VsCodium);
        assert!(hits.iter().any(|(k, _)| *k == IdeKind::VsCode));
    }

    #[test]
    fn classify_empty_signals_detects_nothing() {
        assert!(classify(&IdeSignals::default()).is_empty());
    }

    #[test]
    fn build_setup_uses_configured_registry_name() {
        let regs = vec![reg("my-openvsx", "openvsx"), reg("npmjs", "npm")];
        let setup = build_setup(IdeKind::VsCodium, "test".to_owned(), "http://h:8080", &regs);
        assert!(setup.registry_configured);
        assert_eq!(setup.registry_name, "my-openvsx");
        assert_eq!(setup.registry_type, "openvsx");
        assert!(setup.instructions.contains("proxy/my-openvsx/"));
        // No "not configured" hint when a registry exists.
        assert!(!setup.instructions.contains("No `"));
    }

    #[test]
    fn build_setup_falls_back_to_placeholder_and_hint() {
        let setup = build_setup(IdeKind::JetBrains, "test".to_owned(), "http://h:8080/", &[]);
        assert!(!setup.registry_configured);
        assert_eq!(setup.registry_name, "<jetbrains-marketplace-registry>");
        assert_eq!(setup.registry_type, "jetbrains-marketplace");
        assert!(setup.instructions.contains("updatePlugins.xml"));
        assert!(setup
            .instructions
            .contains("No `jetbrains-marketplace` registry"));
        // Trailing slash on server URL must be trimmed before it reaches URLs.
        assert!(!setup.instructions.contains("h:8080//"));
    }

    /// A host-routed registry is addressed on its own subdomain, without the
    /// `/proxy/{name}` prefix that host adds itself.
    #[test]
    fn build_setup_uses_the_registry_subdomain_when_advertised() {
        let regs = vec![hosted_reg(
            "ovsx1",
            "openvsx",
            "https://ovsx1.batlehub.example.com/",
        )];
        let setup = build_setup(
            IdeKind::VsCodium,
            "test".to_owned(),
            "https://batlehub.example.com",
            &regs,
        );
        assert_eq!(setup.base_url, "https://ovsx1.batlehub.example.com");
        assert!(setup
            .instructions
            .contains("\"serviceUrl\": \"https://ovsx1.batlehub.example.com/vscode/gallery\""));
        assert!(
            !setup.instructions.contains("/proxy/ovsx1"),
            "the registry host prefixes /proxy/{{name}} itself: {}",
            setup.instructions
        );
    }

    #[test]
    fn vscode_prefers_marketplace_when_only_that_type_exists() {
        let regs = vec![reg("ms", "vscode-marketplace")];
        let setup = build_setup(IdeKind::VsCode, "d".to_owned(), "http://h", &regs);
        assert_eq!(setup.registry_type, "vscode-marketplace");
        assert_eq!(setup.registry_name, "ms");
    }

    #[test]
    fn detect_ides_trims_trailing_slash_and_matches_classify() {
        // Exercises the public entry point against the ambient environment: it
        // must never panic and must produce well-formed instructions for any
        // editors that happen to be detected in the test environment.
        let regs = vec![reg("ovsx", "openvsx")];
        for setup in detect_ides("http://host:8080/", &regs) {
            assert!(!setup.instructions.contains("host:8080//"));
            assert!(!setup.registry_name.is_empty());
        }
    }
}
