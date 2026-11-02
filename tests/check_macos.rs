#![cfg(target_os = "macos")]

use std::{
    fs,
    os::unix::fs::symlink,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn application_info(pid: u32) -> String {
    let output = Command::new("/usr/bin/lsappinfo")
        .args(["info", &format!("#{pid}")])
        .output()
        .expect("read the checker's Launch Services registration");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn check_stays_out_of_the_dock_from_a_bundle_and_cli_symlink() {
    let id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    let scratch = Scratch(std::env::temp_dir().join(format!("herdr-check-{id}")));
    let contents = scratch.0.join("Check.app/Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>HerdrCheck</string>
<key>CFBundleIdentifier</key><string>dev.herdr.check-test.{id}</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#
        ),
    )
    .unwrap();
    let bundled = contents.join("MacOS/HerdrCheck");
    fs::copy(env!("CARGO_BIN_EXE_herdr-app"), &bundled).unwrap();
    let signed = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(contents.parent().unwrap())
        .output()
        .unwrap();
    assert!(
        signed.status.success(),
        "{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    let cli = scratch.0.join("herdr-app");
    symlink(&bundled, &cli).unwrap();
    let mod_dir = scratch.0.join("mod");
    fs::create_dir(&mod_dir).unwrap();

    for executable in [&bundled, &cli] {
        for fail in [false, true] {
            let render = if fail {
                "throw new Error('dock-check-error');"
            } else {
                "return div().child('dock-check-probe');"
            };
            fs::write(
                mod_dir.join("main.js"),
                format!(
                    r#"
import {{ View, div }} from "gpui-kit";
export default class Probe extends View {{ render() {{ {render} }} }}
"#
                ),
            )
            .unwrap();
            let stdout = scratch.0.join("stdout");
            let stderr = scratch.0.join("stderr");
            let mut child = Command::new(executable)
                .arg("check")
                .arg(&mod_dir)
                .arg("--print-spec")
                .env("HERDR_APP_HOME", scratch.0.join("home"))
                .env("HC_SESSION", format!("herdr-check-{id}"))
                .stdin(Stdio::null())
                .stdout(fs::File::create(&stdout).unwrap())
                .stderr(fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut observed_accessory = false;
            let mut foreground = String::new();
            let status = loop {
                let registration = application_info(child.id());
                // Launch Services first reports the bundle's default type while
                // !signalled. AppKit must apply the checker policy before launch
                // completes, and keep it for the rest of the process lifetime.
                let launched = !registration.contains("!signalled");
                observed_accessory |= launched && registration.contains("type=\"UIElement\"");
                if launched && registration.contains("type=\"Foreground\"") {
                    foreground = registration;
                }
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("isolated checker did not exit within 30 seconds");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let out = fs::read_to_string(&stdout).unwrap();
            let err = fs::read_to_string(&stderr).unwrap();
            assert_eq!(
                status.code(),
                Some(i32::from(fail)),
                "{status}: {out}\n{err}"
            );
            if fail {
                assert!(
                    err.contains("dock-check-error") && err.contains("check failed:"),
                    "{err}"
                );
            } else {
                assert!(
                    out.contains("dock-check-probe") && out.contains("check passed:"),
                    "{out}"
                );
            }
            assert!(
                foreground.is_empty(),
                "{} registered in the Dock (accessory observed: {observed_accessory}): {foreground}",
                executable.display()
            );
            assert!(
                observed_accessory,
                "never observed the running checker as a UIElement"
            );
            assert!(
                !application_info(child.id()).contains("type=\"Foreground\""),
                "checker left a foreground registration after exit"
            );
        }
    }
}
