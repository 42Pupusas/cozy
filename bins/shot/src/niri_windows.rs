use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use json_bourne::{FromJson, ToJson, parse_str, to_string};

use crate::window_selector::WindowInfo;
use crate::window_source::WindowSource;

const SOCKET_VARIABLE: &str = "NIRI_SOCKET";

pub struct NiriWindows {
    socket: PathBuf,
}

#[derive(FromJson)]
#[bourne(deny_unknown_fields = false)]
struct NiriWindow {
    id: u64,
    #[bourne(default)]
    title: Option<String>,
    #[bourne(default)]
    app_id: Option<String>,
}

#[derive(FromJson)]
#[bourne(deny_unknown_fields = false)]
struct WindowsBody {
    #[bourne(rename = "Windows")]
    windows: Vec<NiriWindow>,
}

#[derive(FromJson)]
#[bourne(deny_unknown_fields = false)]
struct WindowsReply {
    #[bourne(rename = "Ok", default)]
    ok: Option<WindowsBody>,
    #[bourne(rename = "Err", default)]
    err: Option<String>,
}

#[derive(FromJson)]
#[bourne(deny_unknown_fields = false)]
struct StatusReply {
    #[bourne(rename = "Ok", default)]
    ok: Option<String>,
    #[bourne(rename = "Err", default)]
    err: Option<String>,
}

#[derive(FromJson)]
#[bourne(deny_unknown_fields = false)]
struct CapturedBody {
    #[bourne(default)]
    path: Option<String>,
}

#[derive(FromJson)]
#[bourne(deny_unknown_fields = false)]
struct CapturedEvent {
    #[bourne(rename = "ScreenshotCaptured")]
    captured: CapturedBody,
}

#[derive(ToJson)]
struct ScreenshotWindowArguments {
    id: u64,
    write_to_disk: bool,
    show_pointer: bool,
    path: String,
}

#[derive(ToJson)]
struct ScreenshotWindowAction {
    #[bourne(rename = "ScreenshotWindow")]
    screenshot_window: ScreenshotWindowArguments,
}

#[derive(ToJson)]
struct ActionRequest {
    #[bourne(rename = "Action")]
    action: ScreenshotWindowAction,
}

impl NiriWindows {
    #[must_use]
    pub fn from_environment() -> Option<Self> {
        std::env::var_os(SOCKET_VARIABLE)
            .filter(|value| !value.is_empty())
            .map(|value| Self {
                socket: PathBuf::from(value),
            })
    }

    fn connect(&self) -> Result<UnixStream, String> {
        UnixStream::connect(&self.socket)
            .map_err(|e| format!("connecting to niri at {}: {e}", self.socket.display()))
    }

    fn exchange(stream: &mut UnixStream, request: &str) -> Result<String, String> {
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(b"\n"))
            .and_then(|()| stream.flush())
            .map_err(|e| format!("writing to niri: {e}"))?;
        let mut line = String::new();
        BufReader::new(&*stream)
            .read_line(&mut line)
            .map_err(|e| format!("reading from niri: {e}"))?;
        Ok(line)
    }

    fn temporary_path() -> PathBuf {
        std::env::temp_dir().join(format!("shot-window-{}.png", std::process::id()))
    }

    fn wait_for_capture(events: UnixStream, expected: &Path) -> Result<(), String> {
        let mut lines = BufReader::new(events);
        let mut line = String::new();
        loop {
            line.clear();
            let read = lines
                .read_line(&mut line)
                .map_err(|e| format!("waiting for niri to finish the capture: {e}"))?;
            if read == 0 {
                return Err("niri closed the event stream before finishing the capture".into());
            }
            if !line.contains("ScreenshotCaptured") {
                continue;
            }
            let Ok(event) = parse_str::<CapturedEvent>(line.trim()) else {
                continue;
            };
            return match event.captured.path {
                Some(path) if Path::new(&path) == expected => Ok(()),
                Some(_) => continue,
                None => Err("niri finished the capture without writing a file".into()),
            };
        }
    }
}

impl WindowSource for NiriWindows {
    fn windows(&self) -> Result<Vec<WindowInfo>, String> {
        let mut stream = self.connect()?;
        let line = Self::exchange(&mut stream, "\"Windows\"")?;
        let reply: WindowsReply =
            parse_str(line.trim()).map_err(|e| format!("unreadable niri reply: {e}"))?;
        if let Some(message) = reply.err {
            return Err(format!("niri refused the window query: {message}"));
        }
        let body = reply
            .ok
            .ok_or_else(|| "niri sent an empty window reply".to_string())?;
        Ok(body
            .windows
            .into_iter()
            .map(|window| WindowInfo {
                id: window.id,
                title: window.title,
                app_id: window.app_id,
            })
            .collect())
    }

    fn capture_png(&self, window: &WindowInfo, include_cursor: bool) -> Result<Vec<u8>, String> {
        let path = Self::temporary_path();
        let path_text = path
            .to_str()
            .ok_or_else(|| "temporary path is not valid UTF-8".to_string())?
            .to_string();

        let mut events = self.connect()?;
        let subscribed = Self::exchange(&mut events, "\"EventStream\"")?;
        let subscription: StatusReply = parse_str(subscribed.trim())
            .map_err(|e| format!("unreadable niri event stream reply: {e}"))?;
        if let Some(message) = subscription.err {
            return Err(format!("niri refused the event stream: {message}"));
        }

        let request = to_string(&ActionRequest {
            action: ScreenshotWindowAction {
                screenshot_window: ScreenshotWindowArguments {
                    id: window.id,
                    write_to_disk: true,
                    show_pointer: include_cursor,
                    path: path_text,
                },
            },
        })
        .map_err(|e| format!("encoding niri request: {e}"))?;
        let mut control = self.connect()?;
        let answer = Self::exchange(&mut control, &request)?;
        let status: StatusReply =
            parse_str(answer.trim()).map_err(|e| format!("unreadable niri reply: {e}"))?;
        if let Some(message) = status.err {
            return Err(format!(
                "niri could not capture window {}: {message}",
                window.id
            ));
        }
        if status.ok.is_none() {
            return Err("niri sent an empty reply to the capture request".into());
        }

        Self::wait_for_capture(events, &path)?;
        let png = std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()));
        let _ = std::fs::remove_file(&path);
        png
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionRequest, CapturedEvent, ScreenshotWindowAction, ScreenshotWindowArguments};
    use super::{StatusReply, WindowsReply};
    use json_bourne::{parse_str, to_string};

    #[test]
    fn the_window_list_reply_ignores_fields_this_tool_does_not_read() {
        let json = r#"{"Ok":{"Windows":[{"id":3,"title":"t","app_id":"foot","pid":10,"workspace_id":1,"is_focused":true,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[1,1]},"focus_timestamp":{"secs":1,"nanos":2}},{"id":4,"title":null,"app_id":null}]}}"#;
        let reply: WindowsReply = parse_str(json).expect("parses");
        let windows = reply.ok.expect("ok").windows;
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].id, 3);
        assert_eq!(windows[0].app_id.as_deref(), Some("foot"));
        assert!(windows[1].title.is_none());
    }

    #[test]
    fn an_error_reply_carries_niris_message() {
        let reply: WindowsReply = parse_str(r#"{"Err":"nope"}"#).expect("parses");
        assert!(reply.ok.is_none());
        assert_eq!(reply.err.as_deref(), Some("nope"));
    }

    #[test]
    fn a_handled_action_reply_is_an_ok_status() {
        let reply: StatusReply = parse_str(r#"{"Ok":"Handled"}"#).expect("parses");
        assert_eq!(reply.ok.as_deref(), Some("Handled"));
    }

    #[test]
    fn the_capture_request_matches_niris_wire_shape() {
        let json = to_string(&ActionRequest {
            action: ScreenshotWindowAction {
                screenshot_window: ScreenshotWindowArguments {
                    id: 7,
                    write_to_disk: true,
                    show_pointer: false,
                    path: "/tmp/a.png".into(),
                },
            },
        })
        .expect("serializes");
        assert_eq!(
            json,
            r#"{"Action":{"ScreenshotWindow":{"id":7,"write_to_disk":true,"show_pointer":false,"path":"/tmp/a.png"}}}"#
        );
    }

    #[test]
    fn a_captured_event_exposes_its_path() {
        let event: CapturedEvent =
            parse_str(r#"{"ScreenshotCaptured":{"path":"/tmp/a.png"}}"#).expect("parses");
        assert_eq!(event.captured.path.as_deref(), Some("/tmp/a.png"));
        let none: CapturedEvent =
            parse_str(r#"{"ScreenshotCaptured":{"path":null}}"#).expect("parses");
        assert!(none.captured.path.is_none());
    }

    #[test]
    fn unrelated_events_do_not_parse_as_captures() {
        assert!(parse_str::<CapturedEvent>(r#"{"WindowClosed":{"id":1}}"#).is_err());
    }
}
