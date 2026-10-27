//! `batlehub-cli completion <shell>` — clap_complete's scripts, with the two
//! gaps it leaves for this binary closed.
//!
//! - **bash**: clap_complete 4.6 names the root dispatch `batlehub__cli`
//!   (`-` → `__`) but every subcommand arm `batlehub__subcmd__cli__…`
//!   (`-` → `__subcmd__`), so no arm ever matches and nothing below the first
//!   word completes. Any binary with a `-` in its name is hit; ours is.
//! - **fish**: the generator emits named options only, never a positional's
//!   values (documented in `clap_complete::aot::Fish`), so
//!   `completion <TAB>` offered file names instead of the shells.

use clap::{Command, CommandFactory};
use clap_complete::{generate, Shell};

use super::Cli;

const BIN: &str = "batlehub-cli";

pub fn script(shell: Shell) -> String {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    generate(shell, &mut cmd, BIN, &mut buf);
    let script = String::from_utf8_lossy(&buf).into_owned();
    match shell {
        Shell::Bash => script.replace(&BIN.replace('-', "__subcmd__"), &BIN.replace('-', "__")),
        Shell::Fish => {
            let mut script = script;
            push_fish_positionals(&mut script, &cmd, &[]);
            script
        }
        _ => script,
    }
}

/// One `complete` line per positional with a closed set of values, under the
/// same condition helpers the generated script defines.
fn push_fish_positionals(out: &mut String, cmd: &Command, path: &[&str]) {
    if let Some((top, rest)) = path.split_first() {
        let mut condition = format!("__fish_{}_using_subcommand {top}", BIN.replace('-', "_"));
        if let Some(last) = rest.last() {
            condition.push_str(&format!("; and __fish_seen_subcommand_from {last}"));
        }
        for arg in cmd.get_positionals().filter(|a| !a.is_hide_set()) {
            let values: Vec<String> = arg
                .get_possible_values()
                .iter()
                .filter(|v| !v.is_hide_set())
                .map(|v| v.get_name().to_owned())
                .collect();
            if !values.is_empty() {
                out.push_str(&format!(
                    "complete -c {BIN} -n \"{condition}\" -f -a \"{}\"\n",
                    values.join(" ")
                ));
            }
        }
    }
    for sub in cmd.get_subcommands() {
        let mut next = path.to_vec();
        next.push(sub.get_name());
        push_fish_positionals(out, sub, &next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every state the bash word loop can reach has an arm in the dispatch
    /// that follows it — the property clap_complete's naming broke.
    #[test]
    fn bash_dispatch_has_an_arm_for_every_subcommand() {
        let script = script(Shell::Bash);
        let states: std::collections::BTreeSet<&str> = script
            .lines()
            .filter_map(|l| l.trim().strip_prefix("cmd=\"")?.strip_suffix('"'))
            .filter(|s| !s.is_empty())
            .collect();
        assert!(states.len() > 100, "{}", states.len());
        let arms: std::collections::BTreeSet<&str> = script
            .lines()
            .filter_map(|l| l.strip_prefix("        ")?.strip_suffix(')'))
            .collect();
        let missing: Vec<_> = states.difference(&arms).collect();
        assert!(missing.is_empty(), "no arm for {missing:?}");
        assert!(arms.contains("batlehub__cli__subcmd__setup__subcmd__apply"));
    }

    #[test]
    fn fish_completes_the_shell_names() {
        let script = script(Shell::Fish);
        assert!(
            script.contains(
                "complete -c batlehub-cli -n \"__fish_batlehub_cli_using_subcommand completion\" \
                 -f -a \"bash elvish fish powershell zsh\""
            ),
            "{script}"
        );
        // The helper the added lines call is the generated one.
        assert!(script.contains("function __fish_batlehub_cli_using_subcommand"));
    }

    #[test]
    fn every_shell_generates() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Elvish,
            Shell::PowerShell,
        ] {
            assert!(script(shell).contains("setup"), "{shell}");
        }
    }
}
