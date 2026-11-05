//! The `tang` program. Everything it does is in the library (`src/lib.rs`), which the plugin that Tang is in a DAW (`tang-clap/`) is made of too; this is the one place that ends the process.

use std::io::Write;

fn main() {
    let result = tang::run();
    let code = match result {
        Ok(()) => 0,
        // (What returning the error from `main` would say.)
        Err(e) => {
            eprintln!("Error: {e:?}");
            1
        }
    };
    // `run` has stopped everything that is ours, in order. What is not is the native code of the plugins, which the scan of
    // the installed ones may still be in, on a thread of its own (a quit a moment after the start finds it there): the
    // process's ordinary exit runs what those libraries have to run at exit while that thread is in them, and the
    // program crashed on the way out about one time in twenty. The process ends here instead, at once: what it has to say is
    // flushed first, as an exit that is not this would.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    // Safety: `_exit` ends the process; nothing after it runs, and nothing that matters is left to do.
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}
