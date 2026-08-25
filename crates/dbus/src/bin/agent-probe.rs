//! Prove the polkit agent-helper socket speaks its protocol to us, before a
//! prompt UI is designed around it.
//!
//! `/run/polkit/agent-helper.socket` is the setuid-free transport: connect,
//! send the username, send the cookie, and read the PAM conversation as
//! newline-delimited lines. This probe stops at the first prompt and never
//! sends a password.
//!
//! Run with `cargo run -p dbus --bin agent-probe`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

/// One attempt at the helper handshake.
struct AgentProbe {
    path: &'static str,
}

impl AgentProbe {
    const fn new() -> Self {
        Self {
            path: "/run/polkit/agent-helper.socket",
        }
    }

    /// The login name for our own uid, read from `/etc/passwd` so this needs
    /// no libc binding beyond what the crate already has.
    fn own_user_name() -> Option<String> {
        let uid = dbus::sys::DbusSys::effective_uid();
        let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
        for line in passwd.lines() {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _password = fields.next();
            let entry_uid: u32 = fields.next()?.parse().ok()?;
            if entry_uid == uid {
                return Some(name.to_owned());
            }
        }
        None
    }

    /// Connect and read until the helper asks something, then stop.
    fn run(&self) {
        let Some(user) = Self::own_user_name() else {
            println!("could not resolve our own user name");
            return;
        };
        println!("uid resolves to user: {user}");

        let mut sock = match UnixStream::connect(self.path) {
            Ok(sock) => sock,
            Err(err) => {
                println!("cannot connect to {}: {err}", self.path);
                println!("-> the setuid helper would be required instead");
                return;
            }
        };
        println!("connected to {}", self.path);

        // A deliberately invalid cookie: the helper runs the PAM conversation
        // first and only checks the cookie after authentication, so this is
        // enough to see whether it prompts -- and it can never authorise
        // anything, because no such request exists.
        if let Err(err) = write!(sock, "{user}\nnot-a-real-cookie\n") {
            println!("write failed: {err}");
            return;
        }
        let _ = sock.flush();

        let mut reader = BufReader::new(sock);
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => println!("helper closed the connection without speaking"),
            Ok(_) => {
                let trimmed = line.trim_end();
                println!("helper said: {trimmed}");
                println!("\n-> the socket transport works, with NO setuid spawn");
            }
            Err(err) => println!("read failed: {err}"),
        }
        println!("(stopping here; no password was sent)");
    }
}

fn main() {
    AgentProbe::new().run();
}
