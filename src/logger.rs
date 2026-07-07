//! Logging and profiling long-running operations utilities.
//!
//! - **`log_event`**: writes a timestamped message to both `app.log` (located next to the executable)
//!   and `stderr`. It is used throughout the application for diagnostic and monitoring purposes.
//!
//! - **`Timer`**: a simple struct that measures elapsed time. It automatically logs a start event,
//!   supports logging of intermediate steps with the current elapsed time, and logs a finish event
//!   when dropped or explicitly finalized. This is useful for profiling long-running operations
//!   (e.g., starting/stopping a child process).
//!
//! # Examples
//!
//! ```no_run
//! let timer = Timer::new("Operation");
//! // ... do some work ...
//! timer.log("step 1");
//! // ... more work ...
//! timer.finish();
//! ```
//!
//! All logged messages use the local system time with millisecond precision.
use std::time::Instant;
use chrono::Local;

/// Logs a timestamped message to both a file and standard error out.
///
/// This function writes the given message to:
/// - `app.log`, located in the same directory as the current executable (if the path can be determined).
///   The log file is opened in append mode, and if it does not exist, it is created automatically.
/// - Standard error (`stderr`) as a plain text line, without any timestamp prefix.
///
/// The timestamp uses the local system time with millisecond precision (format: `YYYY-MM-DD HH:MM:SS.mmm`).
///
/// If the executable path cannot be resolved, or the log file cannot be opened (e.g., due to permission
/// issues or missing parent directory), the function silently falls back to only printing to `stderr`.
/// No errors are propagated; the function is designed to be infallible for logging purposes.
///
/// # Arguments
/// - `msg` – The log message to record.
///
/// # Examples
///
/// ```
/// log_event("Application started successfully");
/// ```
///
/// # Notes
/// - This function is used internally by the `Timer` struct for timing events.
/// - The log file is always written to the directory containing the executable, which may be read‑only
///   in some environments (e.g., Program Files). Ensure the application has write permissions.
pub fn log_event(msg: &str) {
    use std::io::Write;
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            let log_path = dir.join("app.log");
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        }
    }
    eprintln!("{}", msg);
}

/// Timer struct for timing events.
///
/// # Fields
/// - `name` – A label for the timer, used in log messages.
/// - `start` – The instant when the timer was created.
pub struct Timer {
    name: String,
    start: Instant,
}

/// Implementation Timer struct for timing events.
///
/// # Examples
///
/// ```no_run
/// let timer = Timer::new("Data loading");
/// // ... some work ...
/// timer.log("Query executed");
/// // ... more work ...
/// timer.finish();
/// ```
///
/// Sample log output:
/// ```text
/// [2025-01-01 12:00:00.000] ⏱️ Data loading START
/// [2025-01-01 12:00:01.234] ⏱️ Data loading: Query executed at 1234.567ms
/// [2025-01-01 12:00:02.345] ⏱️ Data loading FINISH: 2345.678ms
/// ```
impl Timer {

    /// Creates a new timer with the given identifying name and logs a start event.
    ///
    /// The timer immediately captures the current instant. A log message `"⏱️ {name} START"`
    /// is written via [`log_event`]. The name is stored for subsequent log entries.
    ///
    /// # Arguments
    /// * `name` – A label identifying this timer (e.g., "Download", "Startup").
    ///
    /// # Examples
    /// ```
    /// let timer = Timer::new("Processing");
    /// ```
    pub fn new(name: &str) -> Self {
        let start = Instant::now();
        log_event(&format!("⏱️ {} START", name));
        Self { name: name.to_string(), start }
    }

    /// Logs an intermediate step with the current elapsed time.
    ///
    /// This method writes a message like `"⏱️ {name}: {step} at {elapsed}ms"` to the log.
    /// The elapsed time is measured from the timer's creation.
    ///
    /// # Arguments
    /// * `step` – A description of the step being logged (e.g., "Cache loaded", "Request sent").
    ///
    /// # Examples
    /// ```
    /// let timer = Timer::new("Data fetch");
    /// timer.log("Request started");
    /// // ... perform request ...
    /// timer.log("Response received");
    /// ```
    pub fn log(&self, step: &str) {
        let elapsed = self.start.elapsed();
        log_event(&format!("⏱️ {}: {} at {:.3}ms", self.name, step, elapsed.as_secs_f64() * 1000.0));
    }

    /// Finishes the timer and logs the total elapsed time.
    ///
    /// This method consumes the timer and writes a final message
    /// `"⏱️ {name} FINISH: {total}ms"`. It is typically called at the end of the operation
    /// being measured.
    ///
    /// # Examples
    /// ```
    /// let timer = Timer::new("Task");
    /// // ... do work ...
    /// timer.finish();
    /// ```
    ///
    /// # Note
    /// After calling `finish`, the timer is moved and cannot be used further.
    pub fn finish(self) {
        let elapsed = self.start.elapsed();
        log_event(&format!("⏱️ {} FINISH: {:.3}ms", self.name, elapsed.as_secs_f64() * 1000.0));
    }
}