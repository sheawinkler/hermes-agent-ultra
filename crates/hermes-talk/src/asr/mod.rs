mod bailian;
#[cfg(all(feature = "rockchip", target_arch = "aarch64"))]
pub mod rk_asr;
mod sherpa_asr;
mod types;

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::backends::{TalkBackendKind, classify_talk_backend};
use crate::config::{AsrConfig, DashscopeConfig};
use crate::error::Result;

pub use bailian::BailianAsr;
pub use types::AsrEvent;

#[cfg(all(feature = "rockchip", target_arch = "aarch64"))]
pub use rk_asr::RockchipAsr;
pub use sherpa_asr::SherpaAsr;

#[async_trait]
pub trait AsrEngine: Send + Sync {
    async fn send_audio(&self, pcm: Vec<u8>) -> Result<()>;
    async fn pause(&self) -> Result<()>;
    async fn resume(&self) -> Result<()>;
    async fn set_gate(&self, on: bool) -> Result<()>;
    async fn reconnect(&self) -> Result<()>;
    async fn finish_utterance(&self) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrBackend {
    Bailian,
    Sherpa,
    #[cfg(all(feature = "rockchip", target_arch = "aarch64"))]
    Rockchip,
}

impl AsrBackend {
    pub fn from_config(asr_cfg: &AsrConfig) -> Self {
        match classify_talk_backend(&asr_cfg.backend) {
            TalkBackendKind::Cloud => AsrBackend::Bailian,
            TalkBackendKind::Sherpa => AsrBackend::Sherpa,
            TalkBackendKind::LocalHardware => {
                #[cfg(all(feature = "rockchip", target_arch = "aarch64"))]
                {
                    AsrBackend::Rockchip
                }
                #[cfg(not(all(feature = "rockchip", target_arch = "aarch64")))]
                {
                    AsrBackend::Sherpa
                }
            }
        }
    }
}

pub async fn create_asr(
    dashscope: &DashscopeConfig,
    asr_cfg: &AsrConfig,
    start_paused: bool,
    backend: AsrBackend,
) -> Result<(Arc<dyn AsrEngine>, mpsc::Receiver<AsrEvent>)> {
    match backend {
        AsrBackend::Bailian => {
            let (client, rx) = BailianAsr::connect(dashscope, asr_cfg, start_paused).await?;
            Ok((Arc::new(client) as Arc<dyn AsrEngine>, rx))
        }
        AsrBackend::Sherpa => {
            let sherpa_cfg = asr_cfg.effective_sherpa();
            let (client, rx) =
                SherpaAsr::connect(&sherpa_cfg, asr_cfg.sample_rate, start_paused).await?;
            Ok((Arc::new(client) as Arc<dyn AsrEngine>, rx))
        }
        #[cfg(all(feature = "rockchip", target_arch = "aarch64"))]
        AsrBackend::Rockchip => {
            let rockchip_cfg = asr_cfg.local.as_ref().ok_or_else(|| {
                crate::error::DemoError::Config(
                    "asr.local config required when backend = \"local\"".into(),
                )
            })?;
            let (client, rx) = RockchipAsr::connect(rockchip_cfg, start_paused).await?;
            Ok((Arc::new(client) as Arc<dyn AsrEngine>, rx))
        }
    }
}
