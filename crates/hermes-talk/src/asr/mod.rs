mod bailian;
#[cfg(all(feature = "rockchip", target_arch = "aarch64"))]
pub mod rk_asr;
#[cfg(feature = "sherpa-asr-tts")]
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
#[cfg(feature = "sherpa-asr-tts")]
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
    #[cfg(feature = "sherpa-asr-tts")]
    Sherpa,
    #[cfg(all(feature = "rockchip", target_arch = "aarch64"))]
    Rockchip,
}

impl AsrBackend {
    pub fn from_config(asr_cfg: &AsrConfig) -> Self {
        match classify_talk_backend(&asr_cfg.backend) {
            TalkBackendKind::Cloud => AsrBackend::Bailian,
            TalkBackendKind::Sherpa => resolve_sherpa_named_asr(),
            TalkBackendKind::LocalHardware => resolve_local_asr(),
        }
    }
}

#[cfg(feature = "sherpa-asr-tts")]
fn resolve_sherpa_named_asr() -> AsrBackend {
    AsrBackend::Sherpa
}

#[cfg(all(
    not(feature = "sherpa-asr-tts"),
    feature = "rockchip",
    target_arch = "aarch64"
))]
fn resolve_sherpa_named_asr() -> AsrBackend {
    AsrBackend::Rockchip
}

#[cfg(all(
    not(feature = "sherpa-asr-tts"),
    not(all(feature = "rockchip", target_arch = "aarch64"))
))]
fn resolve_sherpa_named_asr() -> AsrBackend {
    AsrBackend::Bailian
}

#[cfg(all(feature = "rockchip", target_arch = "aarch64"))]
fn resolve_local_asr() -> AsrBackend {
    AsrBackend::Rockchip
}

#[cfg(all(
    feature = "sherpa-asr-tts",
    not(all(feature = "rockchip", target_arch = "aarch64"))
))]
fn resolve_local_asr() -> AsrBackend {
    AsrBackend::Sherpa
}

#[cfg(all(
    not(feature = "sherpa-asr-tts"),
    not(all(feature = "rockchip", target_arch = "aarch64"))
))]
fn resolve_local_asr() -> AsrBackend {
    AsrBackend::Bailian
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
        #[cfg(feature = "sherpa-asr-tts")]
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
