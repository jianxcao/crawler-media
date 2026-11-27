mod client;
mod connector;
mod credentials;
mod error;
mod ip;
mod policy;
mod url_norm;

pub use client::{GuardedMethod, GuardedRequest, GuardedResponse, NetGuardClient};
pub use connector::{
    DnsResolver, PinnedResolver, PinnedTcpConnector, ResolvedTarget, SystemDnsResolver,
    resolve_and_pin,
};
pub use credentials::CredentialSlot;
pub use error::NetGuardError;
pub use ip::{
    canonicalize_ip, has_unspecified_or_zero_ip, is_benchmark_fake_ip, is_cloud_metadata_host,
    is_cloud_metadata_ip, is_global_unicast,
};
pub use policy::{
    NetGuardLimits, OriginScope, PrivateEndpointException, TargetPolicy, path_matches_prefix,
};
pub use url_norm::{NormalizedTarget, Origin, Scheme, normalize_url};
