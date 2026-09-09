//! The VS Code gallery wire protocol: constants, the request DTO, and the
//! parse of a raw `extensionquery` into something this server can act on.
//!
//! Nothing here touches actix or a service — it is the vocabulary the rest of
//! the module speaks.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `filterType` values an editor sends in `filters[].criteria[]`.
pub mod filter_type {
    pub const TAG: u32 = 1;
    pub const EXTENSION_ID: u32 = 4;
    pub const CATEGORY: u32 = 5;
    pub const EXTENSION_NAME: u32 = 7;
    pub const TARGET: u32 = 8;
    pub const FEATURED: u32 = 9;
    pub const SEARCH_TEXT: u32 = 10;
    pub const EXCLUDE_WITH_FLAGS: u32 = 12;
}

/// The `flags` bitmask, which selects how much of each extension to return.
pub mod flag {
    pub const INCLUDE_VERSIONS: u32 = 0x1;
    pub const INCLUDE_FILES: u32 = 0x2;
    pub const INCLUDE_CATEGORY_AND_TAGS: u32 = 0x4;
    pub const INCLUDE_VERSION_PROPERTIES: u32 = 0x10;
    pub const INCLUDE_ASSET_URI: u32 = 0x80;
    pub const INCLUDE_STATISTICS: u32 = 0x100;
    pub const INCLUDE_LATEST_VERSION_ONLY: u32 = 0x200;
}

/// `files[].assetType` values, and the `{assetType}` path segment of the asset
/// route.
pub mod asset_type {
    pub const VSIX_PACKAGE: &str = "Microsoft.VisualStudio.Services.VSIXPackage";
    pub const MANIFEST: &str = "Microsoft.VisualStudio.Code.Manifest";
    pub const DETAILS: &str = "Microsoft.VisualStudio.Services.Content.Details";
    pub const CHANGELOG: &str = "Microsoft.VisualStudio.Services.Content.Changelog";
    pub const LICENSE: &str = "Microsoft.VisualStudio.Services.Content.License";
    pub const ICON: &str = "Microsoft.VisualStudio.Services.Icons.Default";

    /// Every type the gallery advertises, in the order they appear in `files`.
    pub const ALL: &[&str] = &[VSIX_PACKAGE, MANIFEST, DETAILS, CHANGELOG, LICENSE, ICON];
    /// The signature archive beside the package (RFC 0020): what the editor's
    /// `canInstall` looks for before it enables Install. Advertised only for
    /// a version this registry signed or whose upstream did.
    pub const SIGNATURE: &str = "Microsoft.VisualStudio.Services.VsixSignature";
    /// The key the signature verifies under, Open VSX's asset for it.
    pub const PUBLIC_KEY: &str = "Microsoft.VisualStudio.Services.PublicKey";
}

/// `versions[].properties[].key` values the editor reads.
pub mod property_key {
    /// `engines.vscode` — the editor refuses a version whose engine it does not
    /// satisfy, so this is the one property that changes installability.
    pub const ENGINE: &str = "Microsoft.VisualStudio.Code.Engine";
    pub const EXTENSION_PACK: &str = "Microsoft.VisualStudio.Code.ExtensionPack";
    pub const EXTENSION_DEPENDENCIES: &str = "Microsoft.VisualStudio.Code.ExtensionDependencies";
    pub const PRE_RELEASE: &str = "Microsoft.VisualStudio.Code.PreRelease";
}

/// The only `Target` value this server answers for.
pub const TARGET_VSCODE: &str = "Microsoft.VisualStudio.Code";

/// The `Accept` the marketplace protocol requires, on both the request an
/// editor sends us and the one we forward upstream.
pub const GALLERY_API_ACCEPT: &str = "application/json;api-version=3.0-preview.1";

/// An editor never asks for more than this in one page; a client that does is
/// clamped rather than allowed to ask the registry to materialise everything.
const MAX_PAGE_SIZE: usize = 200;
const DEFAULT_PAGE_SIZE: usize = 50;

// ── The request as it arrives ────────────────────────────────────────────────

/// `POST …/vscode/gallery/extensionquery`.
///
/// Every field is `#[serde(default)]`: a hand-written `product.json` or a
/// non-VS-Code client that omits half of this should get results, not a `400`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtensionQueryRequest {
    pub filters: Vec<QueryFilter>,
    pub asset_types: Vec<String>,
    pub flags: u32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct QueryFilter {
    pub criteria: Vec<QueryCriterion>,
    pub page_number: Option<usize>,
    pub page_size: Option<usize>,
    pub sort_by: u32,
    pub sort_order: u32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct QueryCriterion {
    pub filter_type: u32,
    pub value: String,
}

// ── The request, interpreted ─────────────────────────────────────────────────

/// An `extensionquery` folded into the questions this server can answer.
///
/// Criteria are **AND**-ed, which is what the editor means by them. Only
/// `filters[0]` is interpreted — VS Code sends exactly one, and a second would
/// be an OR-group this server has no way to render into a single result block.
#[derive(Debug, Clone, PartialEq)]
pub struct GalleryQuery {
    /// `filterType 7` — exact `{publisher}.{name}`.
    pub extension_names: Vec<String>,
    /// `filterType 4` — the synthesized extension uuid.
    pub extension_ids: Vec<String>,
    /// `filterType 10` — free text.
    pub search_text: Option<String>,
    /// `filterType 1`.
    pub tags: Vec<String>,
    /// `filterType 5`.
    pub categories: Vec<String>,
    /// `filterType 8`.
    pub target: Option<String>,
    /// `filterType 9`.
    pub featured: bool,
    pub page_number: usize,
    pub page_size: usize,
    pub sort_by: u32,
    /// `sortOrder`: 0 = default (ascending for a name sort, descending for a
    /// recency one), 1 = ascending, 2 = descending.
    pub sort_order: u32,
    pub flags: u32,
}

/// Paging that names the first page, not the zeroth.
///
/// `Default` was derived, which gave `page_number: 0` — and `page()` computes
/// `(page_number - 1) * page_size`, so every `..Default::default()` construction
/// that did not override it produced a `usize` underflow: a panic in debug, and
/// in release a `skip(usize::MAX)` that silently answers an empty list. The
/// OpenVSX namespace listing was built that way and returned nothing at all.
///
/// The invariant `from_request` maintains (`.max(1)`, and its "page 0 is page 1"
/// test) belongs to the type rather than to one constructor, so it lives here.
/// `page_size` is defaulted for the same reason: a derived `0` makes `take(0)`
/// answer nothing, which is the same failure wearing the other field's name.
impl Default for GalleryQuery {
    fn default() -> Self {
        Self {
            extension_names: Vec::new(),
            extension_ids: Vec::new(),
            search_text: None,
            tags: Vec::new(),
            categories: Vec::new(),
            target: None,
            featured: false,
            page_number: 1,
            page_size: DEFAULT_PAGE_SIZE,
            sort_by: sort_by::RELEVANCE,
            sort_order: 0,
            flags: 0,
        }
    }
}

/// `sortBy` values an editor sends. The gallery protocol defines more; these
/// are the ones an entry list built from this proxy's own data can honour
/// without inventing a statistic it does not have.
pub mod sort_by {
    /// Whatever relevance the source produced. The default.
    pub const RELEVANCE: u32 = 0;
    /// Display name, A→Z.
    pub const TITLE: u32 = 2;
    /// Publisher name, A→Z.
    pub const PUBLISHER: u32 = 3;
    /// Most recently updated first.
    pub const LAST_UPDATED: u32 = 10;
}

impl GalleryQuery {
    pub fn from_request(req: &ExtensionQueryRequest) -> Self {
        let filter = req.filters.first();
        if req.filters.len() > 1 {
            tracing::debug!(
                count = req.filters.len(),
                "extensionquery carried more than one filter group; only the first is interpreted"
            );
        }

        let mut q = Self {
            page_number: filter.and_then(|f| f.page_number).unwrap_or(1).max(1),
            page_size: filter
                .and_then(|f| f.page_size)
                .unwrap_or(DEFAULT_PAGE_SIZE)
                .clamp(1, MAX_PAGE_SIZE),
            sort_by: filter.map(|f| f.sort_by).unwrap_or(sort_by::RELEVANCE),
            sort_order: filter.map(|f| f.sort_order).unwrap_or(0),
            flags: req.flags,
            ..Default::default()
        };

        for c in filter.map(|f| f.criteria.as_slice()).unwrap_or_default() {
            let v = c.value.trim();
            match c.filter_type {
                filter_type::EXTENSION_NAME if !v.is_empty() => {
                    q.extension_names.push(v.to_owned())
                }
                filter_type::EXTENSION_ID if !v.is_empty() => q.extension_ids.push(v.to_owned()),
                filter_type::SEARCH_TEXT if !v.is_empty() => q.search_text = Some(v.to_owned()),
                filter_type::TAG if !v.is_empty() => q.tags.push(v.to_owned()),
                filter_type::CATEGORY if !v.is_empty() => q.categories.push(v.to_owned()),
                filter_type::TARGET if !v.is_empty() => q.target = Some(v.to_owned()),
                filter_type::FEATURED => q.featured = true,
                // `ExcludeWithFlags` is almost always 4096 (Unpublished), which
                // has no counterpart here — an unlisted version is already gone
                // by the time the gallery sees it, filtered by
                // `load_visible_versions`.
                filter_type::EXCLUDE_WITH_FLAGS => {}
                other => tracing::debug!(filter_type = other, "ignoring unknown query criterion"),
            }
        }
        q
    }

    /// A query this server can only answer with nothing.
    ///
    /// `Target` naming something other than VS Code, or `Featured` — this
    /// registry hosts VS Code extensions and curates nothing, and answering
    /// either with the whole catalogue would be a claim it cannot support.
    pub fn is_unanswerable(&self) -> bool {
        self.target
            .as_deref()
            .is_some_and(|t| !t.eq_ignore_ascii_case(TARGET_VSCODE))
            || self.featured
    }

    pub fn wants(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    /// The `[skip, take)` window this page asks for.
    ///
    /// `saturating_sub` rather than `- 1`: the `Default` above keeps
    /// `page_number` at 1 and `from_request` clamps it there, but a struct
    /// literal can still write `page_number: 0` explicitly, and an arithmetic
    /// underflow here is a panic in debug and an empty answer in release. Page 0
    /// is page 1, which is what `from_request` has always said.
    pub fn page(&self) -> (usize, usize) {
        (
            self.page_number.saturating_sub(1) * self.page_size,
            self.page_size,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(json: serde_json::Value) -> GalleryQuery {
        let req: ExtensionQueryRequest = serde_json::from_value(json).expect("request parses");
        GalleryQuery::from_request(&req)
    }

    /// The body VS Code sends to resolve one known extension — an install or an
    /// update check.
    #[test]
    fn an_install_by_id_parses_as_an_exact_lookup() {
        let q = query(serde_json::json!({
            "filters": [{
                "criteria": [
                    { "filterType": 8, "value": "Microsoft.VisualStudio.Code" },
                    { "filterType": 7, "value": "ms-python.python" }
                ],
                "pageNumber": 1, "pageSize": 1, "sortBy": 0, "sortOrder": 0
            }],
            "assetTypes": [],
            "flags": 950
        }));

        assert_eq!(q.extension_names, ["ms-python.python"]);
        assert!(!q.is_unanswerable(), "the VS Code target is answerable");
        assert_eq!(q.page(), (0, 1));
    }

    #[test]
    fn a_search_parses_as_free_text() {
        let q = query(serde_json::json!({
            "filters": [{ "criteria": [{ "filterType": 10, "value": "  python  " }] }],
            "flags": 914
        }));

        assert_eq!(q.search_text.as_deref(), Some("python"), "trimmed");
        // What `source.rs` and `render.rs` actually branch on: a search names
        // no extension.
        assert!(q.extension_names.is_empty() && q.extension_ids.is_empty());
    }

    /// A body with almost nothing in it must still produce a usable query —
    /// this endpoint gets hand-written clients.
    #[test]
    fn an_empty_body_gets_defaults_rather_than_a_rejection() {
        let q = query(serde_json::json!({}));
        assert_eq!(q.page_number, 1);
        assert_eq!(q.page_size, 50);
        assert_eq!(q.page(), (0, 50));
        assert!(q.extension_names.is_empty() && q.extension_ids.is_empty());
        assert!(!q.is_unanswerable());
    }

    /// The construction the OpenVSX namespace listing uses.
    ///
    /// It builds a `GalleryQuery` as a struct literal — `search_text` and
    /// `page_size`, then `..Default::default()` — and never mentions
    /// `page_number`. With `Default` derived that was `0`, and `page()` computed
    /// `(0 - 1) * 100`: a panic under debug assertions, and under release a skip
    /// of `usize::MAX`, so `GET /proxy/{registry}/api/{namespace}` answered with
    /// an empty list however many extensions the namespace held.
    ///
    /// Asserted on `page()` rather than on the field, because the field being
    /// right is only interesting insofar as the window is.
    #[test]
    fn a_partial_struct_literal_still_asks_for_the_first_page() {
        let q = GalleryQuery {
            search_text: Some("acme".to_owned()),
            page_size: 100,
            ..Default::default()
        };
        assert_eq!(q.page(), (0, 100), "the first page skips nothing");
    }

    /// Belt and braces for the same defect: an explicit `page_number: 0` is a
    /// state `Default` no longer produces but a caller can still write.
    #[test]
    fn page_zero_written_by_hand_does_not_underflow() {
        let q = GalleryQuery {
            page_number: 0,
            page_size: 25,
            ..Default::default()
        };
        assert_eq!(q.page(), (0, 25));
    }

    #[test]
    fn paging_is_clamped_and_one_based() {
        let q = query(serde_json::json!({
            "filters": [{ "pageNumber": 3, "pageSize": 10_000 }]
        }));
        assert_eq!(q.page_size, 200, "clamped to the ceiling");
        assert_eq!(q.page(), (400, 200), "page 3 skips two full pages");

        let zero = query(serde_json::json!({ "filters": [{ "pageNumber": 0, "pageSize": 0 }] }));
        assert_eq!(zero.page_number, 1, "page 0 is page 1");
        assert_eq!(zero.page_size, 1);
    }

    /// Answering a query for a different editor with our catalogue would be a
    /// lie, and `Featured` implies a curation this registry does not do.
    #[test]
    fn a_foreign_target_or_featured_query_is_unanswerable() {
        assert!(query(serde_json::json!({
            "filters": [{ "criteria": [{ "filterType": 8, "value": "Microsoft.VisualStudio.IDE" }] }]
        }))
        .is_unanswerable());

        assert!(query(serde_json::json!({
            "filters": [{ "criteria": [{ "filterType": 9, "value": "" }] }]
        }))
        .is_unanswerable());
    }

    #[test]
    fn unknown_and_empty_criteria_are_ignored_rather_than_fatal() {
        let q = query(serde_json::json!({
            "filters": [{ "criteria": [
                { "filterType": 999, "value": "whatever" },
                { "filterType": 12,  "value": "4096" },
                { "filterType": 7,   "value": "   " }
            ]}]
        }));
        assert!(q.extension_names.is_empty(), "a blank name is not a filter");
        assert!(!q.is_unanswerable());
    }

    #[test]
    fn flags_are_readable_bit_by_bit() {
        let q = query(serde_json::json!({ "flags": 0x1 | 0x80 }));
        assert!(q.wants(flag::INCLUDE_VERSIONS));
        assert!(q.wants(flag::INCLUDE_ASSET_URI));
        assert!(!q.wants(flag::INCLUDE_STATISTICS));
    }

    #[test]
    fn criteria_accumulate_for_tags_and_categories() {
        let q = query(serde_json::json!({
            "filters": [{ "criteria": [
                { "filterType": 1, "value": "linter" },
                { "filterType": 1, "value": "python" },
                { "filterType": 5, "value": "Programming Languages" }
            ]}]
        }));
        assert_eq!(q.tags, ["linter", "python"]);
        assert_eq!(q.categories, ["Programming Languages"]);
    }
}
