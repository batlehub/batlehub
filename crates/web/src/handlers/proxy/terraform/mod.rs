use std::sync::Arc;

use actix_web::{delete, get, post, put, web, HttpRequest, HttpResponse, Responder};
use sha2::{Digest, Sha256};

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    ports::StorageMeta,
    services::{
        terraform_provider_binary_storage_key, LocalRegistryService, ProxyService,
        PublishPolicyRequest, PublishRequest, TerraformPlatform,
    },
};

use super::common::{
    append_signature_headers, collect_payload, dispatch_notification, extract_signature_headers,
    proxy_document, proxy_stream, registry_public_base, require_local_mode, require_registry_type,
    ArtifactSignature,
};
use crate::{
    error::AppError, extractors::AuthIdentity, services::NotificationService, RegistryMap,
    RegistryModeMap,
};
use batlehub_core::entities::NotificationEventType;

pub mod discovery;
pub mod modules;
pub mod providers;
mod shared;

pub use discovery::{
    terraform_discovery, terraform_discovery_host_routed, terraform_mirror_index,
    terraform_mirror_version,
};
pub use modules::{
    terraform_module_artifact, terraform_module_download, terraform_module_metadata,
    terraform_module_unyank, terraform_module_upload, terraform_module_versions,
    terraform_module_yank,
};
pub use providers::{
    terraform_provider_artifact, terraform_provider_binary_upload, terraform_provider_download,
    terraform_provider_shasums, terraform_provider_shasums_sig, terraform_provider_unyank,
    terraform_provider_upload, terraform_provider_versions, terraform_provider_yank,
};
use shared::{
    identity_for_artifact, mark_uncacheable_if_signed, off_origin_checksum_urls,
    sign_download_document, DownloadCoords,
};
pub(super) use shared::{
    require_segments, terraform_set_yanked, terraform_versions_response, TerraformYankRequest,
};
