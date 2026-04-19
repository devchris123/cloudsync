use indicatif::ProgressBar;

pub fn start_progress(chunk_count: u64) -> ProgressBar {
    ProgressBar::new(chunk_count)
}

pub fn progress_bar_callback(progress_bar: ProgressBar, chunk_progress: u64) {
    progress_bar.inc(chunk_progress);
}
