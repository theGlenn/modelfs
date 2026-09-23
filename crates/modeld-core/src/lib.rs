#[cfg(target_os = "macos")]
pub mod apfs;
pub mod artifact;
#[cfg(target_os = "macos")]
pub mod consolidate;
pub mod dedup;
pub mod digest;

#[doc(inline)]
pub use artifact::{Artifact, FileId, Format, ProviderKind};
#[doc(inline)]
pub use digest::{Algorithm, Digest, DigestError};
