// Copyright (c) 2023 Alibaba Cloud
//
// SPDX-License-Identifier: Apache-2.0
//

//! Abstraction for KBCs as a KMS plugin.

#[cfg(feature = "kbs")]
mod cc_kbc;

#[cfg(feature = "sev")]
mod sev;

mod offline_fs;

use std::sync::Arc;

use async_trait::async_trait;
use lazy_static::lazy_static;
pub use resource_uri::ResourceUri;
use tokio::sync::Mutex;

use crate::{Annotations, Error, Getter, Result};

enum RealClient {
    #[cfg(feature = "kbs")]
    Cc(cc_kbc::CcKbc),
    #[cfg(feature = "sev")]
    Sev(sev::OnlineSevKbc),
    OfflineFs(offline_fs::OfflineFsKbc),
}

impl RealClient {
    async fn new() -> Result<Self> {
        let (kbc, _kbs_host) = get_aa_params_from_cmdline().await?;
        let c = match &kbc[..] {
            #[cfg(feature = "kbs")]
            "cc_kbc" => RealClient::Cc(cc_kbc::CcKbc::new(&_kbs_host).await?),
            #[cfg(feature = "sev")]
            "online_sev_kbc" => RealClient::Sev(sev::OnlineSevKbc::new(&_kbs_host).await?),
            "offline_fs_kbc" => RealClient::OfflineFs(offline_fs::OfflineFsKbc::new().await?),
            others => return Err(Error::KbsClientError(format!("unknown kbc name {others}, only support `cc_kbc`(feature `kbs`), `online_sev_kbc` (feature `sev`) and `offline_fs_kbc`."))),
        };

        Ok(c)
    }
}

lazy_static! {
    static ref KBS_CLIENT: Arc<Mutex<Option<RealClient>>> = Arc::new(Mutex::new(None));
}

#[async_trait]
pub trait Kbc: Send + Sync {
    async fn get_resource(&mut self, _rid: ResourceUri) -> Result<Vec<u8>>;
}

/// A fake KbcClient to carry the [`Getter`] semantics. The real `new()`
/// and `get_resource()` will happen to the static variable [`KBS_CLIENT`].
///
/// Why we use a static variable here is the initialization of kbc is not
/// idempotent. For example online-sev-kbc will delete a file on local
/// filesystem, so we should try to reuse the online-sev-kbc created at the
/// first time.
pub struct KbcClient;

#[async_trait]
impl Getter for KbcClient {
    async fn get_secret(&mut self, name: &str, _annotations: &Annotations) -> Result<Vec<u8>> {
        let resource_uri = ResourceUri::try_from(name)
            .map_err(|_| Error::KbsClientError(format!("illegal kbs resource uri: {name}")))?;
        let real_client = KBS_CLIENT.clone();
        let mut client = real_client.lock().await;

        if client.is_none() {
            let c = RealClient::new().await?;
            *client = Some(c);
        }

        let client = client.as_mut().expect("must be initialized");

        match client {
            #[cfg(feature = "kbs")]
            RealClient::Cc(c) => c.get_resource(resource_uri).await,
            #[cfg(feature = "sev")]
            RealClient::Sev(c) => c.get_resource(resource_uri).await,
            RealClient::OfflineFs(c) => c.get_resource(resource_uri).await,
        }
    }
}

impl KbcClient {
    pub async fn new() -> Result<Self> {
        let client = KBS_CLIENT.clone();
        let mut client = client.lock().await;
        if client.is_none() {
            let c = RealClient::new().await?;
            *client = Some(c);
        }

        Ok(KbcClient {})
    }
}

async fn get_aa_params_from_cmdline() -> Result<(String, String)> {
    use tokio::fs;
    let cmdline = fs::read_to_string("/proc/cmdline")
        .await
        .map_err(|e| Error::KbsClientError(format!("read kernel cmdline failed: {e}")))?;
    let aa_kbc_params = cmdline
        .split_ascii_whitespace()
        .find(|para| para.starts_with("agent.aa_kbc_params="))
        .ok_or(Error::KbsClientError(
            "no `agent.aa_kbc_params` provided in kernel commandline!".into(),
        ))?
        .strip_prefix("agent.aa_kbc_params=")
        .expect("must have a prefix")
        .split("::")
        .collect::<Vec<&str>>();

    if aa_kbc_params.len() != 2 {
        return Err(Error::KbsClientError(
            "Illegal `agent.aa_kbc_params` format provided in kernel commandline.".to_string(),
        ));
    }

    Ok((aa_kbc_params[0].to_string(), aa_kbc_params[1].to_string()))
}
