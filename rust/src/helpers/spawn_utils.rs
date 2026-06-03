use tokio::runtime::Handle;
use tokio::task::JoinHandle;

#[cfg(feature = "tokio-console")]
pub fn spawn_named<F>(name: &str, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    use tracing::Instrument;
    let span = tracing::trace_span!("task");
    tokio::task::Builder::new()
        .name(name)
        .spawn(future.instrument(span))
        .expect(&format!(
            "Something went wrong trying to build the task {name}."
        ))
}

#[cfg(not(feature = "tokio-console"))]
pub fn spawn_named<F>(_name: &str, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(future)
}

#[cfg(feature = "tokio-console")]
pub fn spawn_named_on<F>(name: &str, runtime: &Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    use tracing::Instrument;

    let span = tracing::trace_span!("task");
    tokio::task::Builder::new()
        .name(name)
        .spawn_on(future.instrument(span), runtime)
        .expect(&format!(
            "Something went wrong trying to build the task {name}."
        ))
}

#[cfg(not(feature = "tokio-console"))]
pub fn spawn_named_on<F>(_name: &str, runtime: &Handle, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime.spawn(future)
}
