use std::sync::Arc;

use anyhow::{Context as _, Result};
use arc_swap::ArcSwap;
use koharu_config::Config;
use koharu_scene::Snapshot;

use crate::{
    Committer, PipelineConfig, PipelineError, Report, Request, ResourceSnapshot,
    execution::Execution, resources::ResourceMonitor, stage_runner::StageRunner,
};

#[derive(Clone)]
pub struct Pipeline {
    current: Arc<ArcSwap<StageRunner>>,
    resources: Arc<ResourceMonitor>,
    execution: Arc<tokio::sync::Mutex<()>>,
    config: Config<PipelineConfig>,
    translator: koharu_translator::Translator,
    device: koharu_ml::Device,
}

impl Pipeline {
    pub fn load(device: koharu_ml::Device) -> Result<Self> {
        Self::from_config(
            PipelineConfig::load()?,
            koharu_translator::ProvidersConfig::load()?,
            device,
        )
    }

    #[tracing::instrument(skip_all)]
    pub fn from_config(
        config: Config<PipelineConfig>,
        providers: Config<koharu_translator::ProvidersConfig>,
        device: koharu_ml::Device,
    ) -> Result<Self> {
        let translator = koharu_translator::Translator::from_config(device.clone(), providers)?;
        let resources = ResourceMonitor::new(&device);
        let runner = {
            let value = config.read()?;
            StageRunner::new(&value, translator.clone(), &device, resources.clone())?
        };
        let pipeline = Self {
            current: Arc::new(ArcSwap::from_pointee(runner)),
            resources,
            execution: Arc::new(tokio::sync::Mutex::new(())),
            config,
            translator,
            device,
        };
        let watched = pipeline.clone();
        let _watcher = tokio::runtime::Handle::try_current()
            .context("pipeline requires a Tokio runtime")?
            .spawn(async move {
                let mut changes = watched.config.subscribe();
                while changes.changed().await.is_ok() {
                    if let Err(error) = watched.refresh() {
                        tracing::error!(%error, "failed to reload pipeline");
                    }
                }
            });
        Ok(pipeline)
    }

    /// Rebuilds the stage runner from the live configuration, immediately.
    ///
    /// The watcher is asynchronous, so a caller that writes run-scoped values and then starts a job
    /// cannot assume the runner has caught up: `execute` would load the previous one and run with the
    /// previous instructions, silently. Everything that writes per-run values therefore rebuilds
    /// through here first, and the watcher is left to pick up configuration the user changed.
    ///
    /// Rebuilding shares the translator with the running runner, so a local model that is already
    /// loaded stays loaded.
    #[tracing::instrument(skip_all)]
    pub fn refresh(&self) -> Result<()> {
        let value = self.config.read()?;
        let runner = StageRunner::new(
            &value,
            self.translator.clone(),
            &self.device,
            self.resources.clone(),
        )
        .context("failed to rebuild the pipeline from its configuration")?;
        self.current.store(Arc::new(runner));
        Ok(())
    }

    pub fn subscribe_resources(&self) -> tokio::sync::watch::Receiver<ResourceSnapshot> {
        self.resources.start();
        self.resources.subscribe()
    }

    #[tracing::instrument(skip_all)]
    pub async fn execute(
        &self,
        snapshot: Snapshot,
        request: Request,
        committer: &mut dyn Committer,
    ) -> std::result::Result<Report, PipelineError> {
        let _execution = self.execution.lock().await;
        Execution::new(
            self.current.load_full(),
            self.resources.clone(),
            snapshot,
            request,
            committer,
        )?
        .run()
        .await
    }
}
