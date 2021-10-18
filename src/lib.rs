#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(any(feature = "v12", feature = "v13", feature = "v14")))]
compile_error!("Enable one of the metadata versions");
#[cfg(all(feature = "v12", feature = "v13", feature = "v14"))]
compile_error!("Only one metadata version can be enabled at the moment");
#[cfg(all(feature = "v12", feature = "v13"))]
compile_error!("Only one metadata version can be enabled at the moment");
#[cfg(all(feature = "v12", feature = "v14"))]
compile_error!("Only one metadata version can be enabled at the moment");
#[cfg(all(feature = "v13", feature = "v14"))]
compile_error!("Only one metadata version can be enabled at the moment");

#[macro_use]
extern crate alloc;

pub use codec;
pub use frame_metadata::RuntimeMetadataPrefixed;
pub use meta_ext::{meta_from_bytes, Metadata};
#[cfg(feature = "json")]
pub use scales::JsonValue;
#[cfg(feature = "v14")]
pub use scales::Value;

use async_trait::async_trait;
use core::{fmt, ops::Deref};
use meta_ext::{Entry, Meta};
#[cfg(feature = "std")]
use once_cell::sync::OnceCell;
#[cfg(not(feature = "std"))]
use once_cell::unsync::OnceCell;
use prelude::*;
#[cfg(feature = "v14")]
use scale_info::PortableRegistry;

mod prelude {
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
}

pub type Result<T> = core::result::Result<T, Error>;

#[cfg(any(feature = "http", feature = "http-web"))]
pub mod http;
#[cfg(feature = "ws")]
pub mod ws;

mod hasher;
mod meta_ext;
#[cfg(any(feature = "http", feature = "http-web", feature = "ws"))]
mod rpc;

/// Sube is a lightweight Substrate client with multi-backend support
/// that can use a chain's type information to auto encode/decode data
/// into human-readable formats like JSON.
///
/// ```
/// # use sube::{Sube, ws, JsonValue, Error};
/// # const CHAIN_URL: &str = "ws://localhost:24680";
/// # #[async_std::main] async fn main() -> Result<(), Error> {
/// // Create an instance of Sube from any of the available backends
/// let client: Sube<_> = ws::Backend::new_ws2(CHAIN_URL).await?.into();
/// // Query the chain storage with a path-like syntax
/// let latest_block: JsonValue = client.query("system/number").await?.into();
///
/// assert!(
///     latest_block.as_u64().unwrap() > 0,
///     "block {} is greater than 0",
///     latest_block
/// );
/// # Ok(()) }
/// ```
#[derive(Debug)]
pub struct Sube<B> {
    backend: B,
    meta: OnceCell<Metadata>,
}

impl<B: Backend> Sube<B> {
    pub fn new(backend: B) -> Self {
        Sube {
            backend,
            meta: OnceCell::new(),
        }
    }

    pub fn new_with_meta(backend: B, meta: Metadata) -> Self {
        Sube {
            backend,
            meta: meta.into(),
        }
    }

    /// Get the chain metadata and cache it for future calls
    pub async fn metadata(&self) -> Result<&Metadata> {
        match self.meta.get() {
            Some(meta) => Ok(meta),
            None => {
                let meta = self.backend.metadata().await?;
                self.meta.set(meta).expect("unset");
                Ok(self.meta.get().unwrap())
            }
        }
    }

    /// Use a path-like syntax to query storage items(e.g. `"pallet/item/keyN"`)
    #[cfg(feature = "v14")]
    pub async fn query(&self, key: &str) -> Result<Value<'_>> {
        let key = self.key_from_path(key).await?;
        let res = self.query_bytes(&key).await?;
        let reg = self.registry().await?;
        Ok(Value::new(res, key.1, reg))
    }

    pub async fn key_from_path(&self, path: &str) -> Result<StorageKey> {
        StorageKey::from_path(self.metadata().await?, path)
    }

    /// Get the type registry of the chain
    #[cfg(feature = "v14")]
    pub async fn registry(&self) -> Result<&PortableRegistry> {
        Ok(&self.metadata().await?.types)
    }
}

impl<B: Backend> From<B> for Sube<B> {
    fn from(b: B) -> Self {
        Sube::new(b)
    }
}

impl<T: Backend> Deref for Sube<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.backend
    }
}

/// Generic backend definition
#[async_trait]
pub trait Backend {
    /// Get storage items form the blockchain
    async fn query_bytes(&self, key: &StorageKey) -> Result<Vec<u8>>;

    /// Send a signed extrinsic to the blockchain
    async fn submit<T>(&self, ext: T) -> Result<()>
    where
        T: AsRef<[u8]> + Send;

    async fn metadata(&self) -> Result<Metadata>;
}

#[derive(Clone, Debug)]
pub enum Error {
    ChainUnavailable,
    BadInput,
    BadKey,
    BadMetadata,
    Decode(codec::Error),
    NoMetadataLoaded,
    Node(String),
    ParseStorageItem,
    StorageKeyNotFound,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(e) => write!(f, "{:}", e),
            _ => write!(f, "{:?}", self),
        }
    }
}

#[cfg(feature = "ws")]
impl From<async_tungstenite::tungstenite::Error> for Error {
    fn from(_err: async_tungstenite::tungstenite::Error) -> Self {
        Error::ChainUnavailable
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// Represents a key of the blockchain storage in its raw form
#[derive(Clone, Debug)]
pub struct StorageKey(Vec<u8>, u32);

impl StorageKey {
    /// Parse the StorageKey from a URL-like path
    pub fn from_path(meta: &Metadata, uri: &str) -> Result<Self> {
        let mut path = uri.trim_matches('/').split('/');
        let pallet = path.next().map(to_camel).ok_or(Error::ParseStorageItem)?;
        let item = path.next().map(to_camel).ok_or(Error::ParseStorageItem)?;
        let map_keys = path.collect::<Vec<_>>();

        log::debug!(
            "StorageKey parts: [module]={} [item]={} [keys]={:?}",
            pallet,
            item,
            map_keys,
        );

        let entry = meta
            .storage_entry(&pallet, &item)
            .ok_or(Error::StorageKeyNotFound)?;
        let key = entry
            .key(&pallet, &map_keys)
            .ok_or(Error::StorageKeyNotFound)?;

        Ok(StorageKey(key, entry.ty_id()))
    }
}

impl fmt::Display for StorageKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "0x{}", hex::encode(&self.0))
    }
}

impl AsRef<[u8]> for StorageKey {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

fn to_camel(term: &str) -> String {
    let underscore_count = term.chars().filter(|c| *c == '-').count();
    let mut result = String::with_capacity(term.len() - underscore_count);
    let mut at_new_word = true;

    for c in term.chars().skip_while(|&c| c == '-') {
        if c == '-' {
            at_new_word = true;
        } else if at_new_word {
            result.push(c.to_ascii_uppercase());
            at_new_word = false;
        } else {
            result.push(c);
        }
    }
    result
}
