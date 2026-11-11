//! What the CLI covers for each registry kind (RFC 0040 §4.7, §6.6).
//!
//! An exhaustive `match` over `RegistryKind` per capability, with no wildcard
//! arm: a new kind does not compile until somebody writes its cells. The tests
//! below are the gate — every **yes** and **print** cell names a test
//! `<kind>_<capability>` in `cli/tests/`, and the table in the CLI reference
//! pages is generated from here (`task docs:cli-matrix`).

use batlehub_core::entities::{FetchSupport, RegistryKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only `publish` reads a cell at run time; the gate and the docs table read all six"
    )
)]
pub enum Capability {
    Configure,
    Publish,
    Download,
    Consult,
    Lifecycle,
    Check,
}

/// One cell. There is no fourth value (§4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Implemented, with what it reads or writes.
    Done(&'static str),
    /// The CLI prints what to run: running it needs root, or belongs to the
    /// native client.
    Print(&'static str),
    /// The server cannot do it, and why.
    NotApplicable(&'static str),
}

use Cell::{Done, NotApplicable, Print};
use RegistryKind as K;

pub fn cell(kind: RegistryKind, cap: Capability) -> Cell {
    match cap {
        Capability::Configure => configure(kind),
        Capability::Publish => publish(kind),
        Capability::Download => match kind.fetchable_by_version() {
            FetchSupport::ByVersion(_) => Done("by coordinate, checksum verified"),
            // The version has no single artifact: its files are downloaded by
            // their `/proxy/…` path until `package show` can name one (§17).
            FetchSupport::None(why) => NotApplicable(why),
        },
        Capability::Consult => consult(kind),
        Capability::Lifecycle => lifecycle(kind),
        Capability::Check => check(kind),
    }
}

fn configure(kind: RegistryKind) -> Cell {
    match kind {
        K::Cargo => Done("`~/.cargo/config.toml`"),
        K::Npm => Done("`~/.npmrc`, `~/.yarnrc.yml`"),
        K::Pypi => Done("`pip.conf`, `uv.toml`"),
        K::Goproxy => Done("`go env` file"),
        K::Maven => Done("`~/.m2/settings.xml`"),
        K::Conda => Done("`~/.condarc`"),
        K::Composer => Done("composer `config.json`"),
        K::Rubygems => Done("`~/.gemrc`"),
        K::Nuget => Done("`NuGet.Config`"),
        K::Terraform => Done("`~/.terraformrc`"),
        K::Galaxy => Done("`~/.ansible.cfg`"),
        K::Nix => Done("user `nix.conf`"),
        K::Github | K::Forgejo | K::Gitlab | K::Generic => Done("mise"),
        K::Nodedist => Done("env + mise"),
        K::MiseVersions => Done("mise"),
        K::Rustup | K::Sdkman => Done("env"),
        K::Openvsx | K::VscodeMarketplace | K::JetbrainsMarketplace => Done("`setup ide`"),
        K::Helm => Print("`helm repo add`: helm rewrites its own repository file"),
        K::Deb | K::Rpm | K::Pacman | K::Apk => Print("the sources file is root's"),
        K::Devfile => Print("Che's `CheCluster` is cluster configuration"),
        K::Jetbrains => NotApplicable(
            "serves IDE downloads by path; `setup ide` configures the plugin marketplace",
        ),
    }
}

fn publish(kind: RegistryKind) -> Cell {
    match kind {
        K::Cargo => Done("`.crate`"),
        K::Npm => Done("`.tgz`"),
        K::Pypi => Done("wheel, sdist"),
        K::Goproxy => Done("module directory → one `{version}.zip`"),
        K::Maven => Done("`.jar` + `--pom`, or `target/`"),
        K::Conda => Done("`.conda`, `.tar.bz2`"),
        K::Composer => Done("`.zip`"),
        K::Rubygems => Done("`.gem`"),
        K::Nuget => Done("`.nupkg`"),
        K::Terraform => Done("module `.tar.gz`; provider directory of `_<os>_<arch>.zip`"),
        K::Helm => Done("chart `.tgz`"),
        K::Galaxy => Done("collection `.tar.gz`"),
        K::Devfile => Done("`devfile.yaml` or its directory"),
        K::Openvsx | K::VscodeMarketplace => Done("`.vsix`"),
        K::JetbrainsMarketplace => Done("plugin `.zip`/`.jar`"),
        K::Deb => Done("`.deb` + `--distribution`/`--component`"),
        K::Rpm => Done("`.rpm`"),
        K::Pacman => Done("`.pkg.tar.zst`"),
        K::Apk => Done("`.apk`"),
        K::Nix => Print("`nix copy --to` signs and uploads the closure"),
        K::Jetbrains | K::Github | K::Forgejo | K::Gitlab | K::Generic => {
            NotApplicable("proxy only")
        }
        K::Nodedist | K::Sdkman | K::Rustup => NotApplicable("a vendor's releases, mirrored"),
        K::MiseVersions => NotApplicable("a version list, mirrored"),
    }
}

fn consult(kind: RegistryKind) -> Cell {
    match kind {
        K::Cargo
        | K::Npm
        | K::Pypi
        | K::Goproxy
        | K::Maven
        | K::Conda
        | K::Composer
        | K::Rubygems
        | K::Nuget
        | K::Terraform
        | K::Helm
        | K::Galaxy
        | K::Nix
        | K::Devfile
        | K::Openvsx
        | K::VscodeMarketplace
        | K::JetbrainsMarketplace
        | K::Jetbrains
        | K::Deb
        | K::Rpm
        | K::Pacman
        | K::Apk
        | K::Github
        | K::Forgejo
        | K::Gitlab
        | K::Generic
        | K::Nodedist
        | K::Sdkman
        | K::Rustup
        | K::MiseVersions => Done("`package versions`, `package readme`"),
    }
}

fn lifecycle(kind: RegistryKind) -> Cell {
    match kind {
        K::Cargo
        | K::Npm
        | K::Pypi
        | K::Goproxy
        | K::Maven
        | K::Conda
        | K::Composer
        | K::Rubygems
        | K::Nuget
        | K::Terraform
        | K::Helm
        | K::Galaxy
        | K::Nix
        | K::Devfile
        | K::Openvsx
        | K::VscodeMarketplace
        | K::JetbrainsMarketplace
        | K::Deb
        | K::Rpm
        | K::Pacman
        | K::Apk => Done("the generic lifecycle endpoint"),
        K::Jetbrains | K::Github | K::Forgejo | K::Gitlab | K::Generic => {
            NotApplicable("proxy only: nothing published here to yank")
        }
        K::Nodedist | K::Sdkman | K::Rustup => {
            NotApplicable("a vendor's releases: nothing published here to yank")
        }
        K::MiseVersions => NotApplicable("a version list: nothing published here to yank"),
    }
}

/// `check`: the lockfiles `batlehub_core::lockfile` reads, plus `mise.lock`.
fn check(kind: RegistryKind) -> Cell {
    match kind {
        K::Cargo => Done("`Cargo.lock`"),
        K::Npm => Done("`package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`"),
        K::Pypi => Done("`uv.lock`, `poetry.lock`, pinned `requirements*.txt`"),
        K::Goproxy => Done("`go.sum`"),
        K::Conda => Done("`conda-lock.yml`"),
        K::Composer => Done("`composer.lock`"),
        K::Rubygems => Done("`Gemfile.lock`"),
        K::Nuget => Done("`packages.lock.json`"),
        K::Terraform => Done("`.terraform.lock.hcl`"),
        K::Helm => Done("`Chart.lock`"),
        K::Github | K::Forgejo => Done("`mise.lock`"),
        K::Gitlab => NotApplicable("`mise plan` maps no GitLab download to a registry yet"),
        K::Nodedist => Done("`.nvmrc`"),
        K::Sdkman => Done("`.sdkmanrc`"),
        K::Rustup => Done("`rust-toolchain.toml`"),
        K::Maven => NotApplicable("Maven has no lockfile"),
        K::Galaxy => NotApplicable("`requirements.yml` pins no exact version"),
        K::Nix => NotApplicable("`flake.lock` pins sources; the cache serves store paths by hash"),
        K::Generic => NotApplicable("a mirrored file has no version to ask about"),
        K::MiseVersions => NotApplicable("`mise.lock` names assets, never this listing"),
        K::Devfile | K::Openvsx | K::VscodeMarketplace | K::JetbrainsMarketplace | K::Jetbrains => {
            NotApplicable("editors keep no lockfile")
        }
        K::Deb | K::Rpm | K::Pacman | K::Apk => {
            NotApplicable("the system's package manager keeps no lockfile")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Capability {
        const ALL: [Capability; 6] = [
            Capability::Configure,
            Capability::Publish,
            Capability::Download,
            Capability::Consult,
            Capability::Lifecycle,
            Capability::Check,
        ];

        fn as_str(self) -> &'static str {
            match self {
                Capability::Configure => "configure",
                Capability::Publish => "publish",
                Capability::Download => "download",
                Capability::Consult => "consult",
                Capability::Lifecycle => "lifecycle",
                Capability::Check => "check",
            }
        }
    }

    /// The name a cell's test carries: `<kind>_<capability>`, `-` read as `_`.
    fn test_name(kind: RegistryKind, cap: Capability) -> String {
        format!("{}_{}", kind.as_str().replace('-', "_"), cap.as_str())
    }

    /// The markdown table, as both CLI reference pages carry it.
    fn matrix_markdown() -> String {
        let mut out = String::from(
            "| Kind | Configure | Publish | Download | Consult | Lifecycle | Check |\n\
         | --- | --- | --- | --- | --- | --- | --- |\n",
        );
        for kind in RegistryKind::ALL {
            out.push_str(&format!("| {} |", kind.as_str()));
            for cap in Capability::ALL {
                let text = match cell(*kind, cap) {
                    Done(what) => format!("yes — {what}"),
                    Print(why) => format!("print — {why}"),
                    NotApplicable(why) => format!("n.a. — {why}"),
                };
                out.push_str(&format!(" {} |", text.replace('|', "\\|")));
            }
            out.push('\n');
        }
        out
    }

    const START: &str = "<!-- cli-matrix:start (generated by `task docs:cli-matrix`) -->\n";
    const END: &str = "<!-- cli-matrix:end -->";

    fn tests_source() -> String {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let mut all = String::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") {
                all.push_str(&std::fs::read_to_string(path).unwrap());
            }
        }
        all
    }

    /// The gate (§6.6): every **yes** and **print** cell names an integration test
    /// that exists, as `fn <name>(` or as a row of a `*_case!` table.
    #[test]
    fn every_claimed_cell_has_its_test() {
        let source = tests_source();
        let mut missing = Vec::new();
        for kind in RegistryKind::ALL {
            for cap in Capability::ALL {
                if matches!(cell(*kind, cap), NotApplicable(_)) {
                    continue;
                }
                let name = test_name(*kind, cap);
                // `fn <name>(`, or a row of a `*_case!` table: `<name>, "<kind>"`.
                let row =
                    regex::Regex::new(&format!(r#"(^|[\s{{(])(fn {name}\(|{name},\s*")"#)).unwrap();
                let declared = row.is_match(&source);
                if !declared {
                    missing.push(name);
                }
            }
        }
        assert!(
            missing.is_empty(),
            "cells claimed with no test in cli/tests/: {}",
            missing.join(", ")
        );
    }

    /// The reference pages carry the matrix as generated; `BLESS=1` (what
    /// `task docs:cli-matrix` sets) rewrites it.
    #[test]
    fn the_reference_pages_carry_the_matrix() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let table = matrix_markdown();
        for page in ["docs/use/cli.md", "docs/fr/use/cli.md"] {
            let path = root.join(page);
            let text = std::fs::read_to_string(&path).unwrap();
            let (Some(a), Some(b)) = (text.find(START), text.find(END)) else {
                panic!("{page} has no cli-matrix markers");
            };
            let current = &text[a + START.len()..b];
            if current == table {
                continue;
            }
            if std::env::var_os("BLESS").is_some() {
                let next = format!("{}{START}{table}{}", &text[..a], &text[b..]);
                std::fs::write(&path, next).unwrap();
            } else {
                panic!("{page}'s matrix is stale: run `task docs:cli-matrix`");
            }
        }
    }

    #[test]
    fn test_names_read_the_kind_with_underscores() {
        assert_eq!(
            test_name(RegistryKind::VscodeMarketplace, Capability::Publish),
            "vscode_marketplace_publish"
        );
    }
}
