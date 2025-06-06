use futures::{
    future::FutureExt,
    stream::{FuturesUnordered, StreamExt},
};
#[cfg(feature = "metrics")]
use std::sync::Arc;
use std::{future::Future, time::Duration};
use tracing::{trace_span, Instrument};

use crate::errors::{RequestAttemptError, RequestError};
#[cfg(feature = "metrics")]
use crate::observability::metrics::Metrics;
use crate::response::Coordinator;

/// Context is passed as an argument to `SpeculativeExecutionPolicy` methods
#[non_exhaustive]
pub struct Context {
    #[cfg(feature = "metrics")]
    pub metrics: Arc<Metrics>,
}

/// The policy that decides if the driver will send speculative queries to the
/// next targets when the current target takes too long to respond.
pub trait SpeculativeExecutionPolicy: std::fmt::Debug + Send + Sync {
    /// The maximum number of speculative executions that will be triggered
    /// for a given request (does not include the initial request)
    fn max_retry_count(&self, context: &Context) -> usize;

    /// The delay between each speculative execution
    fn retry_interval(&self, context: &Context) -> Duration;
}

/// A SpeculativeExecutionPolicy that schedules a given number of speculative
/// executions, separated by a fixed delay.
#[derive(Debug, Clone)]
pub struct SimpleSpeculativeExecutionPolicy {
    /// The maximum number of speculative executions that will be triggered
    /// for a given request (does not include the initial request)
    pub max_retry_count: usize,

    /// The delay between each speculative execution
    pub retry_interval: Duration,
}

/// A policy that triggers speculative executions when the request to the current
/// target is above a given percentile.
#[cfg(feature = "metrics")]
#[derive(Debug, Clone)]
pub struct PercentileSpeculativeExecutionPolicy {
    /// The maximum number of speculative executions that will be triggered
    /// for a given request (does not include the initial request)
    pub max_retry_count: usize,

    /// The percentile that a request's latency must fall into to be considered
    /// slow (ex: 99.0)
    pub percentile: f64,
}

impl SpeculativeExecutionPolicy for SimpleSpeculativeExecutionPolicy {
    fn max_retry_count(&self, _: &Context) -> usize {
        self.max_retry_count
    }

    fn retry_interval(&self, _: &Context) -> Duration {
        self.retry_interval
    }
}

#[cfg(feature = "metrics")]
impl SpeculativeExecutionPolicy for PercentileSpeculativeExecutionPolicy {
    fn max_retry_count(&self, _: &Context) -> usize {
        self.max_retry_count
    }

    fn retry_interval(&self, context: &Context) -> Duration {
        let interval = context.metrics.get_latency_percentile_ms(self.percentile);
        let ms = match interval {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    "Failed to get latency percentile ({}), defaulting to 100 ms",
                    e
                );
                100
            }
        };
        Duration::from_millis(ms)
    }
}

/// Checks if a result created in a speculative execution branch can be ignored.
///
/// We should ignore errors such that their presence when executing the request
/// on one node, does not imply that the same error will appear during retry on some other node.
fn can_be_ignored<ResT>(result: &Result<ResT, RequestError>) -> bool {
    match result {
        Ok(_) => false,
        // Do not remove this lint!
        // It's there for a reason - we don't want new variants
        // automatically fall under `_` pattern when they are introduced.
        #[deny(clippy::wildcard_enum_match_arm)]
        Err(e) => match e {
            // This error should not appear it. Anyway, if it possibly could
            // in the future, it should not be ignored.
            RequestError::EmptyPlan => false,

            // Request execution timed out.
            RequestError::RequestTimeout(_) => false,

            // Can try on another node.
            RequestError::ConnectionPoolError { .. } => true,

            RequestError::LastAttemptError(e) => {
                // Do not remove this lint!
                // It's there for a reason - we don't want new variants
                // automatically fall under `_` pattern when they are introduced.
                #[deny(clippy::wildcard_enum_match_arm)]
                match e {
                    // Errors that will almost certainly appear for other nodes as well
                    RequestAttemptError::SerializationError(_)
                    | RequestAttemptError::CqlRequestSerialization(_)
                    | RequestAttemptError::BodyExtensionsParseError(_)
                    | RequestAttemptError::CqlResultParseError(_)
                    | RequestAttemptError::CqlErrorParseError(_)
                    | RequestAttemptError::UnexpectedResponse(_)
                    | RequestAttemptError::RepreparedIdChanged { .. }
                    | RequestAttemptError::RepreparedIdMissingInBatch
                    | RequestAttemptError::NonfinishedPagingState => false,

                    // Errors that can be ignored
                    RequestAttemptError::BrokenConnectionError(_)
                    | RequestAttemptError::UnableToAllocStreamId => true,

                    // Handle DbErrors
                    RequestAttemptError::DbError(db_error, _) => db_error.can_speculative_retry(),
                }
            }
        },
    }
}

const EMPTY_PLAN_ERROR: RequestError = RequestError::EmptyPlan;

pub(crate) async fn execute<QueryFut, ResT>(
    policy: &dyn SpeculativeExecutionPolicy,
    context: &Context,
    query_runner_generator: impl Fn(bool) -> QueryFut,
) -> Result<(ResT, Coordinator), RequestError>
where
    QueryFut: Future<Output = Option<Result<(ResT, Coordinator), RequestError>>>,
{
    let mut retries_remaining = policy.max_retry_count(context);
    let retry_interval = policy.retry_interval(context);

    let mut async_tasks = FuturesUnordered::new();
    async_tasks.push(
        query_runner_generator(false)
            .instrument(trace_span!("Speculative execution: original query")),
    );

    let sleep = tokio::time::sleep(retry_interval).fuse();
    tokio::pin!(sleep);

    let mut last_error = None;
    loop {
        futures::select! {
            _ = &mut sleep => {
                if retries_remaining > 0 {
                    async_tasks.push(query_runner_generator(true).instrument(trace_span!("Speculative execution", retries_remaining = retries_remaining)));
                    retries_remaining -= 1;

                    // reset the timeout
                    sleep.set(tokio::time::sleep(retry_interval).fuse());
                }
            }
            res = async_tasks.select_next_some() => {
                if let Some(r) = res {
                    if !can_be_ignored(&r) {
                        return r;
                    } else {
                        last_error = Some(r)
                    }
                }
                if async_tasks.is_empty() && retries_remaining == 0 {
                    return last_error.unwrap_or({
                        Err(EMPTY_PLAN_ERROR)
                    });
                }
            }
        }
    }
}
