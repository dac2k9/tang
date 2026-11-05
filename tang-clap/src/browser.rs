//! The plugin's window is a page in a browser: pressing the host's button for a plugin's window opens it, on the address that this instance serves. The program that opens the page is the system's own (`open`, `xdg-open`, the
//! shell's handler for a web address on Windows), which is started and left to itself.

use std::process::{Child, Command};

/// The program that is run with the address as its one argument instead of the system's, if a person says one: `TANG_BROWSER=firefox` for the browser that they want it opened in, whatever the system says (the tests have a program that writes the address down).
const PROGRAM_VARIABLE: &str = "TANG_BROWSER";

/// Open `url` in the person's browser. An error if the program could not be started.
pub fn open(url: &str) -> std::io::Result<()> {
    let child = command(url).spawn()?;
    wait_for(child);
    Ok(())
}

/// What is run to open `url`: the program that `TANG_BROWSER` names, or the system's own.
fn command(url: &str) -> Command {
    match std::env::var_os(PROGRAM_VARIABLE).filter(|program| !program.is_empty()) {
        Some(program) => {
            let mut command = Command::new(program);
            command.arg(url);
            command
        }
        None => system_command(url),
    }
}

#[cfg(target_os = "windows")]
fn system_command(url: &str) -> Command {
    use std::os::windows::process::CommandExt;
    // (`rundll32` is the shell's handler for a web address, and has no console; none is flashed up in the host's face, and the flag says so as well.)
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = Command::new("rundll32");
    command.args(["url.dll,FileProtocolHandler", url]).creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(target_os = "macos")]
fn system_command(url: &str) -> Command {
    let mut command = Command::new("open");
    command.arg(url);
    command
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_command(url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    command
}

/// The program is waited for on a thread of its own: one that is not leaves a process that has ended in the system's table of processes until this one ends, which is the host's, for as long as the project is open.
fn wait_for(mut child: Child) {
    let _ = std::thread::Builder::new().name("tang-browser".to_string()).spawn(move || {
        let _ = child.wait();
    });
}
