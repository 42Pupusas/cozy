use std::process::Command;

use json_bourne::parse_str;
use panel_volume::pactl::{PactlDevice, PactlStream};

#[test]
#[ignore = "needs a running audio server"]
fn every_live_listing_parses() {
    for (what, device) in [
        ("sinks", true),
        ("sources", true),
        ("sink-inputs", false),
        ("source-outputs", false),
    ] {
        let out = Command::new("pactl")
            .args(["--format=json", "list", what])
            .output()
            .expect("run pactl");
        let text = String::from_utf8(out.stdout).expect("utf-8");
        let error = if device {
            parse_str::<Vec<PactlDevice>>(&text).err()
        } else {
            parse_str::<Vec<PactlStream>>(&text).err()
        };
        assert!(error.is_none(), "{what}: {error:?}\n{text}");
    }
}
