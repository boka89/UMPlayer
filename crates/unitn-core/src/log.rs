//! Tiny logging hook. The app installs a sink (file + stderr); without one,
//! messages go to stderr.

use std::sync::OnceLock;

type Sink = Box<dyn Fn(&str) + Send + Sync>;
static SINK: OnceLock<Sink> = OnceLock::new();

/// Installs the global log sink (only the first call has an effect).
pub fn set_sink(sink: impl Fn(&str) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

pub fn log(message: impl AsRef<str>) {
    match SINK.get() {
        Some(sink) => sink(message.as_ref()),
        None => eprintln!("unitn: {}", message.as_ref()),
    }
}
