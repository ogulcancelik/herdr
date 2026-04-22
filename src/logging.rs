pub(crate) fn init_file_logging(file_name: &str) {
    use std::fs::{self, OpenOptions};
    use tracing_subscriber::EnvFilter;

    let log_dir = crate::config::config_dir();
    let _ = fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(file_name);

    if let Ok(meta) = fs::metadata(&log_path) {
        if meta.len() > 5 * 1024 * 1024 {
            let _ = fs::remove_file(&log_path);
        }
    }

    let file = match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(file) => file,
        Err(_) => return,
    };

    let filter =
        EnvFilter::try_from_env("HERDR_LOG").unwrap_or_else(|_| EnvFilter::new("herdr=info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .with_target(false)
        .try_init();
}
