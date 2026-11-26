#![cfg(all(unix, not(target_os = "macos"), feature = "use_zbus"))]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use souvlaki::{MediaControls, MediaPlayback, PlatformConfig};

/// The session bus can go away while the player runs. The service thread then ends with the
/// bus's error instead of a panic, an update reports it once, and the controls can be attached
/// again later.
#[test]
#[ignore = "requires dbus-daemon"]
fn a_lost_session_bus_is_reported_without_a_panic() {
    let mut bus = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("start a private session bus");
    let mut address = String::new();
    BufReader::new(bus.stdout.take().unwrap())
        .read_line(&mut address)
        .expect("read the bus address");
    // The only test of this binary, so no other thread reads the environment meanwhile.
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address.trim());

    let mut controls = MediaControls::new(PlatformConfig {
        dbus_name: "souvlaki_test_lost_bus",
        display_name: "Souvlaki test",
        hwnd: None,
    })
    .unwrap();
    controls.attach(|_| {}).expect("attach to the private bus");
    controls
        .set_playback(MediaPlayback::Playing { progress: None })
        .expect("update while the bus runs");

    bus.kill().unwrap();
    bus.wait().unwrap();

    // The update that finds the bus gone ends the service thread; a later one reports its error.
    let deadline = Instant::now() + Duration::from_secs(30);
    let error = loop {
        match controls.set_playback(MediaPlayback::Paused { progress: None }) {
            Ok(()) => {
                assert!(
                    Instant::now() < deadline,
                    "updates kept succeeding after the bus was gone"
                );
                thread::yield_now();
            }
            Err(error) => break error,
        }
    };
    assert!(
        matches!(error, souvlaki::Error::DbusError(_)),
        "the lost bus was reported as {:?}",
        error
    );
    assert!(matches!(
        controls.set_playback(MediaPlayback::Stopped),
        Err(souvlaki::Error::ThreadNotRunning)
    ));
    controls.detach().unwrap();
}
