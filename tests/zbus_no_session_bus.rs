#![cfg(all(unix, not(target_os = "macos"), feature = "use_zbus"))]

use souvlaki::{MediaControls, MediaPlayback, PlatformConfig};

/// A machine without a desktop session, a container, or a minimal window manager session has no
/// session bus. `attach` must say so, and nothing may panic.
#[test]
fn attach_without_a_session_bus_is_an_error() {
    // The only test of this binary, so no other thread reads the environment meanwhile.
    std::env::set_var(
        "DBUS_SESSION_BUS_ADDRESS",
        "unix:path=/nonexistent/souvlaki-test-bus",
    );

    let mut controls = MediaControls::new(PlatformConfig {
        dbus_name: "souvlaki_test_no_bus",
        display_name: "Souvlaki test",
        hwnd: None,
    })
    .unwrap();

    let attached = controls.attach(|_| {});
    assert!(
        matches!(attached, Err(souvlaki::Error::DbusError(_))),
        "attach without a session bus returned {:?}",
        attached
    );
    // No service thread was left behind: there is none to update and none that panicked.
    assert!(matches!(
        controls.set_playback(MediaPlayback::Stopped),
        Err(souvlaki::Error::ThreadNotRunning)
    ));
    controls.detach().unwrap();
}
