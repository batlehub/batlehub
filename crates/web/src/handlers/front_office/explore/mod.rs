use std::sync::Arc;

use actix_web::{get, web, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{ExploreFilter, ExploreSortBy, PackageFilter, PackageSource, PackageStatus},
    services::{AdminService, LocalRegistryService, ProxyService},
};

use crate::{error::AppError, extractors::AuthIdentity};

pub mod detail;
pub mod fetch;
pub mod image;
pub mod list;
pub mod readme;
pub mod refs;
pub mod stats;

pub use detail::{
    explore_package_detail, ExplorePackageDetailResponse, ExploreVersionDto, FetchOfferDto,
    FirewallDto, GateDto, PackageDetailPath, SbomDto,
};
pub use fetch::{explore_fetch_version, FetchPath, FetchResponse};
pub use image::{explore_readme_image, ReadmeImagePath};
pub use list::{explore_packages, ExploreEntryDto, ExplorePackageListResponse, ExploreQuery};
pub use readme::{explore_package_readme, ReadmePath, ReadmeQuery, ReadmeResponse};
pub use refs::{explore_forge_refs, ForgeRefDto, ForgeRefsResponse};
pub use stats::{
    explore_registry_stats, explore_upstream_search, ExploreRegistryStatsResponse, RegistryStatDto,
    UpstreamPackageDto, UpstreamSearchQuery, UpstreamSearchResponse,
};

pub fn format_dt(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

/// Order two version strings newest-first.
///
/// Re-exported from `batlehub_core::services::version_order` rather than
/// defined here: `proxy::discovery::capped` *truncates* an upstream version
/// list, and a list this sorts one way and that cuts the other way loses the
/// rows this calls newest. One function, so the two cannot drift.
pub use batlehub_core::services::newest_first;

// `default_per_page` used to live here as a constant 20. The catalog's page size
// is `[limits].packages_per_page` now — an operator's number rather than a
// literal — so the only thing a compile-time default could still do is disagree
// with it. `DEFAULT_PACKAGES_PER_PAGE` in `batlehub_core::services::hot_config`
// is what an unconfigured server answers with.
