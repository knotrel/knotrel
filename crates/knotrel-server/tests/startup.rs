//! Standalone configuration failures must exit without serving requests.

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn invalid_environment_exits_promptly_with_the_setting_name() {
    // Keep the address occupied: invalid configuration must be diagnosed before
    // binding, rather than being hidden by an address-in-use error.
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = occupied.local_addr().unwrap().to_string();
    for (key, value) in [
        ("KNOTREL_ENGINE", "unknown"),
        ("KNOTREL_ENGINE", ""),
        ("KNOTREL_ADDR", "not-an-address"),
        ("KNOTREL_MAX_PENDING_JOBS", "0"),
        ("KNOTREL_MAX_PENDING_JOBS", "18446744073709551615"),
        ("KNOTREL_MAX_PENDING_JOBS", "-1"),
        ("KNOTREL_MAX_PENDING_JOBS", "1.5"),
        ("KNOTREL_ENGIEN", "ett"),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_knotrel-server"))
            .env_clear()
            .env("KNOTREL_ADDR", &address)
            .env(key, value)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "invalid {key} did not exit: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success(), "accepted invalid {key}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(key),
            "missing setting name for {key}: {:?}",
            output.stderr
        );
    }
}
