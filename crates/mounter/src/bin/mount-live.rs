//! Drive the whole mount path against the real system.
//!
//! Every earlier bug on this path was invisible to unit tests and only
//! appeared against the live daemon: polkit declaring an object path as `s`,
//! and the missing `ALLOW_INTERACTIVE_AUTHORIZATION` flag that made a correct
//! agent silently never fire. So the backend gets a live probe too.
//!
//! ```text
//! cargo run -p mounter --bin mount-live -- --cancel
//! cargo run -p mounter --bin mount-live -- --device /dev/sdb1 --label STICK
//! ```
//!
//! With no `--device` it names one that does not exist, so the mount cannot
//! succeed and only the authentication path is exercised. `--cancel` declines
//! the prompt, which makes the run unattended.

use mounter::prompting_backend::{PasswordPrompt, PromptingBackend};
use volumes::mount_backend::MountBackend;
use volumes::mount_point::MountPoint;
use volumes::mount_request::MountRequest;
use std::io::{BufRead, Write};

/// What the run was asked to do.
struct ProbeOptions {
    device: String,
    label: String,
    fs_type: String,
    cancel: bool,
}

impl ProbeOptions {
    /// Read the options from the command line.
    fn from_args() -> Self {
        let args: Vec<String> = std::env::args().collect();
        Self {
            device: Self::value(&args, "--device")
                .unwrap_or_else(|| String::from("/dev/does-not-exist")),
            label: Self::value(&args, "--label").unwrap_or_else(|| String::from("cozy-probe")),
            fs_type: Self::value(&args, "--fs").unwrap_or_else(|| String::from("vfat")),
            cancel: args.iter().any(|arg| arg == "--cancel"),
        }
    }

    /// The value following `flag`, if it was given.
    fn value(args: &[String], flag: &str) -> Option<String> {
        let at = args.iter().position(|arg| arg == flag)?;
        args.get(at + 1).cloned()
    }

    /// The mount request these options describe.
    fn request(&self) -> Option<MountRequest> {
        let user = std::env::var("USER").unwrap_or_else(|_| String::from("user"));
        let point = MountPoint::for_user(&user, Some(&self.label), &self.device)?;
        let uid = dbus::sys::DbusSys::effective_uid();
        Some(MountRequest::new(
            self.device.clone(),
            self.fs_type.clone(),
            point,
            uid,
            uid,
        ))
    }
}

/// Answers prompts from the terminal, or declines them.
struct TerminalPrompt {
    cancel: bool,
}

impl PasswordPrompt for TerminalPrompt {
    fn ask(&mut self, prompt: &str, echo: bool) -> Option<String> {
        println!("\n  PROMPT RECEIVED: {prompt:?} (echo: {echo})");
        if self.cancel {
            println!("  --cancel given, so declining without sending a password");
            return None;
        }
        print!("  {prompt}[this probe echoes; the browser will not] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']).to_owned();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    }

    fn notify(&mut self, message: &str) {
        println!("  helper says: {message}");
    }
}

/// The probe run.
struct MountProbe;

impl MountProbe {
    fn run() {
        let options = ProbeOptions::from_args();
        let Some(request) = options.request() else {
            println!("could not build a mount request");
            return;
        };

        println!("mount request:");
        println!("  device: {}", request.device());
        println!("  type:   {}", request.fs_type());
        println!("  where:  {}", request.mount_point().as_str());
        println!("  owner:  uid {} gid {}", request.uid(), request.gid());

        let prompt = TerminalPrompt {
            cancel: options.cancel,
        };
        let Ok(mut backend) = PromptingBackend::connect(prompt) else {
            println!("\nno system bus");
            return;
        };
        println!("\nbackend available: {}", backend.is_available());

        println!("\nattempting the mount...");
        let outcome = backend.mount(&request);
        println!("\noutcome: {outcome:?}");
        match outcome.message() {
            Some(message) => println!("  shown to the user: {message}"),
            None if outcome.is_mounted() => println!("  mounted, so nothing is shown"),
            None => println!("  nothing is shown: the user chose this"),
        }
        if let Some(auth) = backend.last_authentication() {
            println!("  authentication ended as: {auth:?}");
        } else {
            println!("  no password was ever requested");
        }
    }
}

fn main() {
    MountProbe::run();
}
