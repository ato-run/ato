//! Bridge the owned asynchronous Native handles into the existing synchronous
//! lease evaluator. The lease's authenticated control callback remains authority.
use super::*;

pub struct NativeLeaseRun {
    runtime: Option<tokio::runtime::Runtime>,
    run: Option<OwnedNativeTerminalRun>,
    owner: Option<watch::Sender<bool>>,
}
impl NativeLeaseRun {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        spec: &RuntimeLaunchSpecV1,
        selected_ref: &str,
        descriptor: &SessionSurfaceDescriptor,
        context: &ResolvedRuntimeLaunchContext,
        owner: &OciOwner,
        config: NativeLeaseSurfaceConfig,
        pixel_helper: Option<super::pixel::PixelHelper>,
        check_startup: &mut dyn FnMut() -> Result<()>,
    ) -> std::result::Result<Self, super::super::lease::StartFailure> {
        check_startup()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .context("start owned Native executor")?;
        let (alive, owner_rx) = watch::channel(true);
        let result = runtime.block_on(async {
            let start = async {
                match descriptor {
                    SessionSurfaceDescriptor::Terminal { .. } => start_terminal(spec, selected_ref, descriptor, context, owner, config, owner_rx).await,
                    SessionSurfaceDescriptor::PixelStream { .. } => super::pixel::start_pixel(spec, selected_ref, descriptor, context, owner, config,
                        pixel_helper.context("native_surface_unavailable: Pixel helper is not owned")?, None, owner_rx).await,
                    _ => Err(anyhow::anyhow!("native_surface_unavailable: unsupported Native Surface").into()),
                }
            };
            tokio::pin!(start);
            let mut control = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    biased;
                    _ = control.tick() => {
                        if let Err(error) = tokio::task::block_in_place(&mut *check_startup) {
                            let _ = alive.send(false);
                            let mut failed = match start.await {
                                Err(failed) => failed,
                                Ok(run) => {
                                    let completion = run.stop().await;
                                    super::super::lease::StartFailure { error: anyhow::anyhow!("native owner cancelled"), stop: completion.stop, process: None }
                                }
                            };
                            failed.error = error;
                            return Err(failed);
                        }
                    }
                    result = &mut start => return result,
                }
            }
        });
        match result {
            Ok(run) => Ok(Self {
                runtime: Some(runtime),
                run: Some(run),
                owner: Some(alive),
            }),
            Err(error) => {
                runtime.shutdown_timeout(Duration::from_secs(3));
                Err(error)
            }
        }
    }
    fn run(&self) -> &OwnedNativeTerminalRun {
        self.run.as_ref().expect("owned Native Run")
    }
    pub fn surface_addr(&self) -> SocketAddr {
        self.run().surface_addr()
    }
    pub fn observation_addr(&self) -> SocketAddr {
        self.run().observation_addr()
    }
    pub fn container_id(&self) -> &str {
        self.run().container_id()
    }
    pub fn execution_evidence(&self) -> &super::super::lease::RuntimeExecutionEvidence {
        self.run().execution_evidence()
    }
    pub fn is_finished(&self) -> bool {
        self.run().is_finished()
    }
    pub fn stop(mut self) -> NativeCompletion {
        self.stop_owned()
    }
    fn stop_owned(&mut self) -> NativeCompletion {
        let result = self
            .runtime
            .as_ref()
            .expect("Native executor")
            .block_on(self.run.take().expect("Native owner").stop());
        self.owner.take();
        self.runtime
            .take()
            .unwrap()
            .shutdown_timeout(Duration::from_secs(3));
        result
    }
}
impl Drop for NativeLeaseRun {
    fn drop(&mut self) {
        if self.run.is_some() {
            let _ = self.stop_owned();
        }
    }
}
