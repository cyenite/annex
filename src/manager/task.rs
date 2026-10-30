use gtk::{gio, glib};

/// Runs `work` on a worker thread and hands its result to `done` back on the UI thread.
/// annex_core blocks on docker, adb and downloads, which would freeze the window otherwise.
pub fn spawn<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    done: impl FnOnce(anyhow::Result<T>) + 'static,
) {
    let handle = gio::spawn_blocking(work);
    glib::spawn_future_local(async move {
        let result = handle
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("the operation crashed")));
        done(result);
    });
}
