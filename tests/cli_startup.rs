use std::process::Command;

#[test]
fn informational_flags_exit_without_configuration_or_state() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing-config.toml");
    for flag in ["--version", "--help"] {
        // These are also the first native package smokes. An explicit missing
        // config makes accidental service/state startup fail instead of using
        // the developer's real installation.
        let output = Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
            .arg("--config")
            .arg(&missing)
            .arg(flag)
            .env("HOME", temp.path())
            .env("XDG_CONFIG_HOME", temp.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{flag}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        if flag == "--version" {
            assert_eq!(
                stdout.trim(),
                concat!("herdr-white-noise-bridge ", env!("CARGO_PKG_VERSION"))
            );
        } else {
            assert!(stdout.contains("Usage:"), "{stdout}");
        }
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}

#[test]
fn missing_configuration_exits_with_an_error_not_a_signal() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-white-noise-bridge"))
        .arg("--config")
        .arg(temp.path().join("missing-config.toml"))
        .arg("status")
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path())
        .output()
        .unwrap();
    // Cover entry into the async command body as well as clap's early exits.
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}
