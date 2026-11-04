//! `package yank|unyank|delete|pin|unpin|deprecate|undeprecate|unlist|relist|block|unblock
//! <coord>…` (RFC 0040 §4.4, §4.9): one verb, any number of versions, across
//! registries. Replaces `version …`, `admin bulk …` and `admin deprecate …`.

use anyhow::Result;
use serde::Serialize;

use crate::api::BatleHubClient;
use crate::confirm::confirm;
use crate::coordinate::Coordinate;
use crate::exit::{CliError, ExitCode};
use crate::render::{ctx, data};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Yank,
    Unyank,
    Delete,
    Pin,
    Unpin,
    Deprecate,
    Undeprecate,
    Unlist,
    Relist,
    Block,
    Unblock,
}

impl Verb {
    fn name(self) -> &'static str {
        match self {
            Self::Yank => "yank",
            Self::Unyank => "unyank",
            Self::Delete => "delete",
            Self::Pin => "pin",
            Self::Unpin => "unpin",
            Self::Deprecate => "deprecate",
            Self::Undeprecate => "undeprecate",
            Self::Unlist => "unlist",
            Self::Relist => "relist",
            Self::Block => "block",
            Self::Unblock => "unblock",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Self::Yank => "Yanked",
            Self::Unyank => "Unyanked",
            Self::Delete => "Deleted",
            Self::Pin => "Pinned",
            Self::Unpin => "Unpinned",
            Self::Deprecate => "Deprecated",
            Self::Undeprecate => "Undeprecated",
            Self::Unlist => "Unlisted",
            Self::Relist => "Relisted",
            Self::Block => "Blocked",
            Self::Unblock => "Unblocked",
        }
    }

    /// §4.5: deletes, yanks, unlists and blocks ask first.
    fn destructive(self) -> bool {
        matches!(self, Self::Yank | Self::Delete | Self::Unlist | Self::Block)
    }

    /// The verbs that have a bulk endpoint: one request per registry.
    fn bulk(self) -> bool {
        matches!(self, Self::Yank | Self::Unyank | Self::Delete)
    }
}

#[derive(Serialize)]
struct Failure {
    coordinate: String,
    error: String,
}

#[derive(Serialize)]
struct Outcome {
    processed: usize,
    succeeded: Vec<String>,
    failed: Vec<Failure>,
}

/// What a run did, item by item.
#[derive(Default)]
struct Tally {
    succeeded: Vec<String>,
    failed: Vec<Failure>,
    /// The single-item error is returned as-is, so its exit code (403 → 4)
    /// survives instead of becoming a partial failure.
    only_error: Option<anyhow::Error>,
}

/// One request per registry, for the verbs that have a bulk endpoint.
async fn run_bulk(
    verb: Verb,
    items: &[(String, String, String)],
    client: &BatleHubClient,
    tally: &mut Tally,
) {
    let mut registries: Vec<&str> = items.iter().map(|(r, _, _)| r.as_str()).collect();
    registries.sort_unstable();
    registries.dedup();
    for reg in registries {
        let pkgs: Vec<(String, String)> = items
            .iter()
            .filter(|(r, _, _)| r == reg)
            .map(|(_, n, v)| (n.clone(), v.clone()))
            .collect();
        let result = match verb {
            Verb::Yank => client.bulk_yank(reg, pkgs.clone()).await,
            Verb::Unyank => client.bulk_unyank(reg, pkgs.clone()).await,
            _ => client.bulk_delete(reg, pkgs.clone()).await,
        };
        match result {
            Ok(res) => {
                for (n, v) in &pkgs {
                    let coordinate = format!("{reg}/{n}@{v}");
                    // The server's owner arm reports a NuGet id lower-cased
                    // (its canonical form): matched exactly, a failure read
                    // as a success.
                    match res
                        .failed
                        .iter()
                        .find(|f| f.name.eq_ignore_ascii_case(n) && &f.version == v)
                    {
                        Some(f) => tally.failed.push(Failure {
                            coordinate,
                            error: f.error.clone(),
                        }),
                        None => tally.succeeded.push(coordinate),
                    }
                }
            }
            Err(e) => {
                for (n, v) in &pkgs {
                    tally.failed.push(Failure {
                        coordinate: format!("{reg}/{n}@{v}"),
                        error: format!("{e:#}"),
                    });
                }
                tally.only_error = Some(e);
            }
        }
    }
}

/// One request for every registry: the moderation endpoints take the
/// registry per item.
async fn run_moderation(
    verb: Verb,
    items: &[(String, String, String)],
    reason: Option<&str>,
    client: &BatleHubClient,
    tally: &mut Tally,
) {
    let result = match verb {
        Verb::Block => client.bulk_block(items, reason.unwrap_or_default()).await,
        _ => client.bulk_unblock(items).await,
    };
    let failures = match result {
        Ok(res) => res["failures"].as_array().cloned().unwrap_or_default(),
        Err(e) => {
            for (r, n, v) in items {
                tally.failed.push(Failure {
                    coordinate: format!("{r}/{n}@{v}"),
                    error: format!("{e:#}"),
                });
            }
            tally.only_error = Some(e);
            return;
        }
    };
    for (r, n, v) in items {
        let coordinate = format!("{r}/{n}@{v}");
        match failures.iter().find(|f| {
            f["registry"] == r.as_str() && f["name"] == n.as_str() && f["version"] == v.as_str()
        }) {
            Some(f) => tally.failed.push(Failure {
                coordinate,
                error: f["error"].as_str().unwrap_or_default().to_owned(),
            }),
            None => tally.succeeded.push(coordinate),
        }
    }
}

/// One request per item, for the verbs with no bulk endpoint.
async fn run_each(
    verb: Verb,
    items: &[(String, String, String)],
    message: Option<&str>,
    client: &BatleHubClient,
    tally: &mut Tally,
) {
    for (reg, n, v) in items {
        let result = match verb {
            Verb::Pin => client.set_retention_pin(reg, n, v, true).await,
            Verb::Unpin => client.set_retention_pin(reg, n, v, false).await,
            Verb::Deprecate => client.deprecate_package(reg, n, v, message).await,
            Verb::Undeprecate => client.undeprecate_package(reg, n, v).await,
            Verb::Unlist => client.unlist_package(reg, n, v).await,
            _ => client.relist_package(reg, n, v).await,
        };
        let coordinate = format!("{reg}/{n}@{v}");
        match result {
            Ok(()) => tally.succeeded.push(coordinate),
            Err(e) => {
                tally.failed.push(Failure {
                    coordinate,
                    error: format!("{e:#}"),
                });
                tally.only_error = Some(e);
            }
        }
    }
}

pub async fn run(
    verb: Verb,
    coordinates: &[String],
    message: Option<&str>,
    client: &BatleHubClient,
) -> Result<()> {
    let items: Vec<(String, String, String)> =
        Coordinate::resolve_all(client, coordinates, ctx().registry.as_deref())
            .await?
            .into_iter()
            .map(Coordinate::require_version)
            .collect::<Result<_, _>>()?;
    let shown: Vec<String> = items
        .iter()
        .map(|(r, n, v)| format!("{r}/{n}@{v}"))
        .collect();
    if verb.destructive() {
        let note = (verb == Verb::Delete).then_some(
            "the artifact is dropped and the version number is spent permanently — it can \
             never be published again",
        );
        confirm(verb.name(), &shown, note)?;
    }

    let mut tally = Tally::default();
    if matches!(verb, Verb::Block | Verb::Unblock) {
        run_moderation(verb, &items, message, client, &mut tally).await;
    } else if verb.bulk() {
        run_bulk(verb, &items, client, &mut tally).await;
    } else {
        run_each(verb, &items, message, client, &mut tally).await;
    }
    let Tally {
        succeeded,
        failed,
        only_error,
    } = tally;

    if items.len() == 1 {
        if let Some(e) = only_error {
            return Err(e);
        }
    }
    let outcome = Outcome {
        processed: items.len(),
        succeeded,
        failed,
    };
    if ctx().structured() {
        data(&outcome)?;
    } else {
        for c in &outcome.succeeded {
            println!("{} {c}", verb.past());
        }
        for f in &outcome.failed {
            println!("FAILED {}: {}", f.coordinate, f.error);
        }
    }
    if outcome.failed.is_empty() {
        Ok(())
    } else if ctx().structured() {
        // The outcome above is the run's one document, failures included.
        let exit = if items.len() == 1 {
            ExitCode::Failure
        } else {
            ExitCode::Partial
        };
        Err(CliError::reported(exit).into())
    } else if items.len() == 1 {
        // A bulk endpoint that answered 200 with the one item failed.
        Err(CliError::new(ExitCode::Failure, outcome.failed[0].error.clone()).into())
    } else {
        Err(CliError::new(
            ExitCode::Partial,
            format!("{} of {} item(s) failed", outcome.failed.len(), items.len()),
        )
        .into())
    }
}
