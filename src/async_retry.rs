use std::future::Future;
use std::marker::Tuple;
use std::ops::AsyncFnMut;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use crate::backoff::BackoffBuilder;
use crate::Backoff;
use crate::DefaultSleeper;
use crate::Sleeper;

/// Retryable will add retry support for functions that produces a futures with results.
///
/// That means all types that implement `FnMut() -> impl Future<Output = Result<T, E>>`
/// will be able to use `retry`.
///
/// For example:
///
/// - Functions without extra args:
///
/// ```ignore
/// async fn fetch() -> Result<String> {
///     Ok(reqwest::get("https://www.rust-lang.org").await?.text().await?)
/// }
/// ```
///
/// - Closures
///
/// ```ignore
/// || async {
///     let x = reqwest::get("https://www.rust-lang.org")
///         .await?
///         .text()
///         .await?;
///
///     Err(anyhow::anyhow!(x))
/// }
/// ```
///
/// # Examples
///
/// For more examples, please see: [https://docs.rs/backon/#examples](https://docs.rs/backon/#examples)
///
pub trait Retryable<
    'a,
    B: BackoffBuilder,
    T,
    E,
    Args: Tuple,
    FutureFn: AsyncFnMut<Args, Output = Result<T, E>> + 'a,
>
{
    /// Generate a new retry
    fn retry(self, builder: &B, args: Args) -> Retry<'a, B::Backoff, T, E, Args, FutureFn>;
}

impl<'a, B, T, E, Args, FutureFn> Retryable<'a, B, T, E, Args, FutureFn> for FutureFn
where
    B: BackoffBuilder,
    Args: Tuple,
    FutureFn: AsyncFnMut<Args, Output = Result<T, E>> + 'a,
{
    fn retry(self, builder: &B, args: Args) -> Retry<'a, B::Backoff, T, E, Args, FutureFn> {
        Retry::new(self, builder.build(), args)
    }
}

/// Retry struct generated by [`Retryable`].
pub struct Retry<
    'a,
    B: Backoff,
    T,
    E,
    Args: Tuple,
    FutureFn: AsyncFnMut<Args, Output = Result<T, E>> + 'a,
    SF: Sleeper = DefaultSleeper,
    RF = fn(&E) -> bool,
    NF = fn(&E, Duration),
> {
    backoff: B,
    retryable: RF,
    notify: NF,
    sleep_fn: SF,
    args: Args,

    state: State<T, E, FutureFn::CallRefFuture<'a>, SF::Sleep>,
    future_fn: FutureFn,
}

impl<'a, B, T, E, Args, FutureFn> Retry<'a, B, T, E, Args, FutureFn>
where
    B: Backoff,
    Args: Tuple,
    FutureFn: AsyncFnMut<Args, Output = Result<T, E>> + 'a,
{
    /// Create a new retry.
    ///
    /// This API is only available when `tokio-sleep` feature is enabled.
    fn new(future_fn: FutureFn, backoff: B, args: Args) -> Self {
        Retry {
            backoff,
            retryable: |_: &E| true,
            notify: |_: &E, _: Duration| {},
            args,
            future_fn,
            sleep_fn: DefaultSleeper::default(),
            state: State::Idle,
        }
    }
}

impl<'a, B, T, E, Args, FutureFn, SF, RF, NF> Retry<'a, B, T, E, Args, FutureFn, SF, RF, NF>
where
    B: Backoff,
    Args: Tuple,
    FutureFn: AsyncFnMut<Args, Output = Result<T, E>> + 'a,
    SF: Sleeper,
    RF: FnMut(&E) -> bool,
    NF: FnMut(&E, Duration),
{
    /// Set the sleeper for retrying.
    ///
    /// If not specified, we use the default sleeper that enabled by feature flag.
    ///
    /// The sleeper should implement the [`Sleeper`] trait. The simplest way is to use a closure that returns a `Future<Output=()>`.
    ///
    /// ```no_run
    /// use anyhow::Result;
    /// use backon::ExponentialBuilder;
    /// use backon::Retryable;
    /// use std::future::ready;
    ///
    /// async fn fetch() -> Result<String> {
    ///     Ok(reqwest::get("https://www.rust-lang.org")
    ///         .await?
    ///         .text()
    ///         .await?)
    /// }
    ///
    /// #[tokio::main(flavor = "current_thread")]
    /// async fn main() -> Result<()> {
    ///     let content = fetch
    ///         .retry(&ExponentialBuilder::default())
    ///         .sleep(|_| ready(()))
    ///         .await?;
    ///     println!("fetch succeeded: {}", content);
    ///
    ///     Ok(())
    /// }
    /// ```
    pub fn sleep<SN: Sleeper>(
        self,
        sleep_fn: SN,
    ) -> Retry<'a, B, T, E, Args, FutureFn, SN, RF, NF> {
        Retry {
            backoff: self.backoff,
            retryable: self.retryable,
            notify: self.notify,
            future_fn: self.future_fn,
            args: self.args,
            sleep_fn,
            state: State::Idle,
        }
    }

    /// Set the conditions for retrying.
    ///
    /// If not specified, we treat all errors as retryable.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use anyhow::Result;
    /// use backon::ExponentialBuilder;
    /// use backon::Retryable;
    ///
    /// async fn fetch() -> Result<String> {
    ///     Ok(reqwest::get("https://www.rust-lang.org")
    ///         .await?
    ///         .text()
    ///         .await?)
    /// }
    ///
    /// #[tokio::main(flavor = "current_thread")]
    /// async fn main() -> Result<()> {
    ///     let content = fetch
    ///         .retry(&ExponentialBuilder::default())
    ///         .when(|e| e.to_string() == "EOF")
    ///         .await?;
    ///     println!("fetch succeeded: {}", content);
    ///
    ///     Ok(())
    /// }
    /// ```
    pub fn when<RN: FnMut(&E) -> bool>(
        self,
        retryable: RN,
    ) -> Retry<'a, B, T, E, Args, FutureFn, SF, RN, NF> {
        Retry {
            backoff: self.backoff,
            retryable,
            notify: self.notify,
            future_fn: self.future_fn,
            args: self.args,
            sleep_fn: self.sleep_fn,
            state: self.state,
        }
    }

    /// Set to notify for everything retrying.
    ///
    /// If not specified, this is a no-op.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::time::Duration;
    ///
    /// use anyhow::Result;
    /// use backon::ExponentialBuilder;
    /// use backon::Retryable;
    ///
    /// async fn fetch() -> Result<String> {
    ///     Ok(reqwest::get("https://www.rust-lang.org")
    ///         .await?
    ///         .text()
    ///         .await?)
    /// }
    ///
    /// #[tokio::main(flavor = "current_thread")]
    /// async fn main() -> Result<()> {
    ///     let content = fetch
    ///         .retry(&ExponentialBuilder::default())
    ///         .notify(|err: &anyhow::Error, dur: Duration| {
    ///             println!("retrying error {:?} with sleeping {:?}", err, dur);
    ///         })
    ///         .await?;
    ///     println!("fetch succeeded: {}", content);
    ///
    ///     Ok(())
    /// }
    /// ```
    pub fn notify<NN: FnMut(&E, Duration)>(
        self,
        notify: NN,
    ) -> Retry<'a, B, T, E, Args, FutureFn, SF, RF, NN> {
        Retry {
            backoff: self.backoff,
            retryable: self.retryable,
            notify,
            sleep_fn: self.sleep_fn,
            args: self.args,
            future_fn: self.future_fn,
            state: self.state,
        }
    }
}

/// State maintains internal state of retry.
///
/// # Notes
///
/// `tokio::time::Sleep` is a very struct that occupy 640B, so we wrap it
/// into a `Pin<Box<_>>` to avoid this enum too large.
#[derive(Default)]
enum State<T, E, Fut: Future<Output = Result<T, E>>, SleepFut: Future<Output = ()>> {
    #[default]
    Idle,
    Polling(Fut),
    Sleeping(SleepFut),
}

impl<'a, B, T, E, Args, FutureFn, SF, RF, NF> Future
    for Retry<'a, B, T, E, Args, FutureFn, SF, RF, NF>
where
    B: Backoff,
    Args: Tuple,
    FutureFn: AsyncFnMut<Args, Output = Result<T, E>> + 'a,
    SF: Sleeper,
    RF: FnMut(&E) -> bool,
    NF: FnMut(&E, Duration),
{
    type Output = Result<T, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: This is safe because we don't move the `Retry` struct itself,
        // only its internal state.
        //
        // We do the exactly same thing like `pin_project` but without depending on it directly.
        let this = unsafe { self.get_unchecked_mut() };

        loop {
            match &mut this.state {
                State::Idle => {
                    let fut = (this.future_fn).async_call_mut(this.args);
                    this.state = State::Polling(fut);
                    continue;
                }
                State::Polling(fut) => {
                    // Safety: This is safe because we don't move the `Retry` struct and this fut,
                    // only its internal state.
                    //
                    // We do the exactly same thing like `pin_project` but without depending on it directly.
                    let mut fut = unsafe { Pin::new_unchecked(fut) };

                    match ready!(fut.as_mut().poll(cx)) {
                        Ok(v) => return Poll::Ready(Ok(v)),
                        Err(err) => {
                            // If input error is not retryable, return error directly.
                            if !(this.retryable)(&err) {
                                return Poll::Ready(Err(err));
                            }
                            match this.backoff.next() {
                                None => return Poll::Ready(Err(err)),
                                Some(dur) => {
                                    (this.notify)(&err, dur);
                                    this.state = State::Sleeping(this.sleep_fn.sleep(dur));
                                    continue;
                                }
                            }
                        }
                    }
                }
                State::Sleeping(sl) => {
                    // Safety: This is safe because we don't move the `Retry` struct and this fut,
                    // only its internal state.
                    //
                    // We do the exactly same thing like `pin_project` but without depending on it directly.
                    let mut sl = unsafe { Pin::new_unchecked(sl) };

                    ready!(sl.as_mut().poll(cx));
                    this.state = State::Idle;
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg(any(feature = "tokio-sleep", feature = "gloo-timers-sleep"))]
mod tests {
    use std::{future::ready, time::Duration};
    use tokio::sync::Mutex;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    #[cfg(not(target_arch = "wasm32"))]
    use tokio::test;

    use super::*;
    use crate::exponential::ExponentialBuilder;

    async fn always_error(x: usize) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("test_query meets error"))
    }

    #[test]
    async fn test_async_retry() -> anyhow::Result<()> {
        let result = always_error.retry((1,)).await;

        assert!(result.is_err());
        assert_eq!("test_query meets error", result.unwrap_err().to_string());
        Ok(())
    }

    // #[test]
    // async fn test_retry_with_sleep() -> anyhow::Result<()> {
    //     let result = always_error
    //         .retry(&ExponentialBuilder::default().with_min_delay(Duration::from_millis(1)))
    //         .sleep(|_| ready(()))
    //         .await;

    //     assert!(result.is_err());
    //     assert_eq!("test_query meets error", result.unwrap_err().to_string());
    //     Ok(())
    // }

    // #[test]
    // async fn test_retry_with_not_retryable_error() -> anyhow::Result<()> {
    //     let error_times = Mutex::new(0);

    //     let f = || async {
    //         let mut x = error_times.lock().await;
    //         *x += 1;
    //         Err::<(), anyhow::Error>(anyhow::anyhow!("not retryable"))
    //     };

    //     let backoff = ExponentialBuilder::default().with_min_delay(Duration::from_millis(1));
    //     let result = f
    //         .retry(&backoff)
    //         // Only retry If error message is `retryable`
    //         .when(|e| e.to_string() == "retryable")
    //         .await;

    //     assert!(result.is_err());
    //     assert_eq!("not retryable", result.unwrap_err().to_string());
    //     // `f` always returns error "not retryable", so it should be executed
    //     // only once.
    //     assert_eq!(*error_times.lock().await, 1);
    //     Ok(())
    // }

    // #[test]
    // async fn test_retry_with_retryable_error() -> anyhow::Result<()> {
    //     let error_times = Mutex::new(0);

    //     let f = || async {
    //         let mut x = error_times.lock().await;
    //         *x += 1;
    //         Err::<(), anyhow::Error>(anyhow::anyhow!("retryable"))
    //     };

    //     let backoff = ExponentialBuilder::default().with_min_delay(Duration::from_millis(1));
    //     let result = f
    //         .retry(&backoff)
    //         // Only retry If error message is `retryable`
    //         .when(|e| e.to_string() == "retryable")
    //         .await;

    //     assert!(result.is_err());
    //     assert_eq!("retryable", result.unwrap_err().to_string());
    //     // `f` always returns error "retryable", so it should be executed
    //     // 4 times (retry 3 times).
    //     assert_eq!(*error_times.lock().await, 4);
    //     Ok(())
    // }

    // #[test]
    // async fn test_fn_mut_when_and_notify() -> anyhow::Result<()> {
    //     let mut calls_retryable: Vec<()> = vec![];
    //     let mut calls_notify: Vec<()> = vec![];

    //     let f = || async { Err::<(), anyhow::Error>(anyhow::anyhow!("retryable")) };

    //     let backoff = ExponentialBuilder::default().with_min_delay(Duration::from_millis(1));
    //     let result = f
    //         .retry(&backoff)
    //         .when(|_| {
    //             calls_retryable.push(());
    //             true
    //         })
    //         .notify(|_, _| {
    //             calls_notify.push(());
    //         })
    //         .await;

    //     assert!(result.is_err());
    //     assert_eq!("retryable", result.unwrap_err().to_string());
    //     // `f` always returns error "retryable", so it should be executed
    //     // 4 times (retry 3 times).
    //     assert_eq!(calls_retryable.len(), 4);
    //     assert_eq!(calls_notify.len(), 3);
    //     Ok(())
    // }
}
