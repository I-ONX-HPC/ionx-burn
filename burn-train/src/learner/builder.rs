use super::log::install_file_logger;
use super::Learner;
use crate::checkpoint::{AsyncCheckpointer, FileCheckpointer};
use crate::components::LearnerComponentsMarker;
use crate::learner::base::TrainingInterrupter;
use crate::logger::{FileMetricLogger, MetricLogger};
use crate::metric::callback::{
    default_renderer, MetricWrapper, Metrics, MetricsCallback, MetricsRenderer,
};
use crate::metric::{Adaptor, Metric};
use crate::{AsyncTrainerCallback, LearnerCheckpointer};
use burn_core::lr_scheduler::LrScheduler;
use burn_core::module::ADModule;
use burn_core::optim::Optimizer;
use burn_core::record::FileRecorder;
use burn_core::tensor::backend::ADBackend;

/// Struct to configure and create a [learner](Learner).
pub struct LearnerBuilder<B, T, V, M, O, S>
where
    T: Send + Sync + 'static,
    V: Send + Sync + 'static,
    B: ADBackend,
    M: ADModule<B>,
    O: Optimizer<M, B>,
    S: LrScheduler,
{
    // Not that complex and very convenient when the traits are
    // already constrained correctly. Extracting in another type
    // would be more complex.
    #[allow(clippy::type_complexity)]
    checkpointers: Option<(
        AsyncCheckpointer<M::Record>,
        AsyncCheckpointer<O::Record>,
        AsyncCheckpointer<S::Record>,
    )>,
    num_epochs: usize,
    checkpoint: Option<usize>,
    directory: String,
    grad_accumulation: Option<usize>,
    devices: Vec<B::Device>,
    metric_logger_train: Option<Box<dyn MetricLogger + 'static>>,
    metric_logger_valid: Option<Box<dyn MetricLogger + 'static>>,
    renderer: Option<Box<dyn MetricsRenderer + 'static>>,
    metrics: Metrics<T, V>,
    interrupter: TrainingInterrupter,
    log_to_file: bool,
}

impl<B, T, V, M, O, S> LearnerBuilder<B, T, V, M, O, S>
where
    B: ADBackend,
    T: Send + Sync + 'static,
    V: Send + Sync + 'static,
    M: ADModule<B> + core::fmt::Display + 'static,
    O: Optimizer<M, B>,
    S: LrScheduler,
{
    /// Creates a new learner builder.
    ///
    /// # Arguments
    ///
    /// * `directory` - The directory to save the checkpoints.
    pub fn new(directory: &str) -> Self {
        Self {
            num_epochs: 1,
            checkpoint: None,
            checkpointers: None,
            directory: directory.to_string(),
            grad_accumulation: None,
            devices: vec![B::Device::default()],
            metric_logger_train: None,
            metric_logger_valid: None,
            metrics: Metrics::new(),
            renderer: None,
            interrupter: TrainingInterrupter::new(),
            log_to_file: true,
        }
    }

    /// Replace the default metric loggers with the provided ones.
    ///
    /// # Arguments
    ///
    /// * `logger_train` - The training logger.
    /// * `logger_valid` - The validation logger.
    pub fn metric_loggers<MT, MV>(mut self, logger_train: MT, logger_valid: MV) -> Self
    where
        MT: MetricLogger + 'static,
        MV: MetricLogger + 'static,
    {
        self.metric_logger_train = Some(Box::new(logger_train));
        self.metric_logger_valid = Some(Box::new(logger_valid));
        self
    }

    /// Replace the default CLI renderer with a custom one.
    ///
    /// # Arguments
    ///
    /// * `renderer` - The custom renderer.
    pub fn renderer<MR>(mut self, renderer: MR) -> Self
    where
        MR: MetricsRenderer + 'static,
    {
        self.renderer = Some(Box::new(renderer));
        self
    }

    /// Register a training metric.
    pub fn metric_train<Me: Metric + 'static>(mut self, metric: Me) -> Self
    where
        T: Adaptor<Me::Input>,
    {
        self.metrics
            .train
            .push(Box::new(MetricWrapper::new(metric)));
        self
    }

    /// Register a validation metric.
    pub fn metric_valid<Me: Metric + 'static>(mut self, metric: Me) -> Self
    where
        V: Adaptor<Me::Input>,
    {
        self.metrics
            .valid
            .push(Box::new(MetricWrapper::new(metric)));
        self
    }

    /// Enable gradients accumulation.
    ///
    /// # Notes
    ///
    /// When you enable gradients accumulation, the gradients object used by the optimizer will be
    /// the sum of all gradients generated by each backward pass. It might be a good idea to
    /// reduce the learning to compensate.
    ///
    /// The effect is similar to increasing the `batch size` and the `learning rate` by the `accumulation`
    /// amount.
    pub fn grads_accumulation(mut self, accumulation: usize) -> Self {
        self.grad_accumulation = Some(accumulation);
        self
    }

    /// Register a training metric and displays it on a plot.
    ///
    /// # Notes
    ///
    /// Only [numeric](crate::metric::Numeric) metric can be displayed on a plot.
    /// If the same metric is also registered for the [validation split](Self::metric_valid_plot),
    /// the same graph will be used for both.
    pub fn metric_train_plot<Me>(mut self, metric: Me) -> Self
    where
        Me: Metric + crate::metric::Numeric + 'static,
        T: Adaptor<Me::Input>,
    {
        self.metrics
            .train_numeric
            .push(Box::new(MetricWrapper::new(metric)));
        self
    }

    /// Register a validation metric and displays it on a plot.
    ///
    /// # Notes
    ///
    /// Only [numeric](crate::metric::Numeric) metric can be displayed on a plot.
    /// If the same metric is also registered for the [training split](Self::metric_train_plot),
    /// the same graph will be used for both.
    pub fn metric_valid_plot<Me: Metric + crate::metric::Numeric + 'static>(
        mut self,
        metric: Me,
    ) -> Self
    where
        V: Adaptor<Me::Input>,
    {
        self.metrics
            .valid_numeric
            .push(Box::new(MetricWrapper::new(metric)));
        self
    }

    /// The number of epochs the training should last.
    pub fn num_epochs(mut self, num_epochs: usize) -> Self {
        self.num_epochs = num_epochs;
        self
    }

    /// Run the training loop on multiple devices.
    pub fn devices(mut self, devices: Vec<B::Device>) -> Self {
        self.devices = devices;
        self
    }

    /// The epoch from which the training must resume.
    pub fn checkpoint(mut self, checkpoint: usize) -> Self {
        self.checkpoint = Some(checkpoint);
        self
    }

    /// Provides a handle that can be used to interrupt training.
    pub fn interrupter(&self) -> TrainingInterrupter {
        self.interrupter.clone()
    }

    /// By default, Rust logs are captured and written into
    /// `experiment.log`. If disabled, standard Rust log handling
    /// will apply.
    pub fn log_to_file(mut self, enabled: bool) -> Self {
        self.log_to_file = enabled;
        self
    }

    /// Register a checkpointer that will save the [optimizer](Optimizer) and the
    /// [model](ADModule).
    ///
    /// The number of checkpoints to be keep should be set to a minimum of two to be safe, since
    /// they are saved and deleted asynchronously and a crash during training might make a
    /// checkpoint non-usable.
    pub fn with_file_checkpointer<FR>(mut self, num_keep: usize, recorder: FR) -> Self
    where
        FR: FileRecorder + 'static,
        O::Record: 'static,
        M::Record: 'static,
        S::Record: 'static,
    {
        let checkpointer_model = FileCheckpointer::new(
            recorder.clone(),
            format!("{}/checkpoint", self.directory).as_str(),
            "model",
            num_keep,
        );
        let checkpointer_optimizer = FileCheckpointer::new(
            recorder.clone(),
            format!("{}/checkpoint", self.directory).as_str(),
            "optim",
            num_keep,
        );
        let checkpointer_scheduler = FileCheckpointer::new(
            recorder,
            format!("{}/checkpoint", self.directory).as_str(),
            "scheduler",
            num_keep,
        );

        self.checkpointers = Some((
            AsyncCheckpointer::new(checkpointer_model),
            AsyncCheckpointer::new(checkpointer_optimizer),
            AsyncCheckpointer::new(checkpointer_scheduler),
        ));

        self
    }

    /// Create the [learner](Learner) from a [model](ADModule) and an [optimizer](Optimizer).
    /// The [learning rate scheduler](LrScheduler) can also be a simple
    /// [learning rate](burn_core::LearningRate).
    #[allow(clippy::type_complexity)] // The goal for the builder is to handle all types and
                                      // creates a clean learner.
    pub fn build(
        self,
        model: M,
        optim: O,
        lr_scheduler: S,
    ) -> Learner<
        LearnerComponentsMarker<
            B,
            S,
            M,
            O,
            AsyncCheckpointer<M::Record>,
            AsyncCheckpointer<O::Record>,
            AsyncCheckpointer<S::Record>,
            AsyncTrainerCallback<T, V>,
        >,
    >
    where
        M::Record: 'static,
        O::Record: 'static,
        S::Record: 'static,
    {
        if self.log_to_file {
            self.init_logger();
        }
        let renderer = self.renderer.unwrap_or_else(|| {
            Box::new(default_renderer(self.interrupter.clone(), self.checkpoint))
        });
        let directory = &self.directory;
        let logger_train = self.metric_logger_train.unwrap_or_else(|| {
            Box::new(FileMetricLogger::new(format!("{directory}/train").as_str()))
        });
        let logger_valid = self.metric_logger_valid.unwrap_or_else(|| {
            Box::new(FileMetricLogger::new(format!("{directory}/valid").as_str()))
        });
        let callback = AsyncTrainerCallback::new(MetricsCallback::new(
            renderer,
            self.metrics,
            logger_train,
            logger_valid,
        ));

        let checkpointer = self
            .checkpointers
            .map(|(model, optim, scheduler)| LearnerCheckpointer::new(model, optim, scheduler));

        Learner {
            model,
            optim,
            lr_scheduler,
            checkpointer,
            num_epochs: self.num_epochs,
            callback,
            checkpoint: self.checkpoint,
            grad_accumulation: self.grad_accumulation,
            devices: self.devices,
            interrupter: self.interrupter,
        }
    }

    fn init_logger(&self) {
        let file_path = format!("{}/experiment.log", self.directory);
        install_file_logger(file_path.as_str());
    }
}
