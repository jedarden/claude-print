use claude_print::hook::HookInstaller;
use claude_print::pty::PtySpawner;
use std::ffi::CString;
use std::io::Read;

/// Locate the mock-claude binary compiled alongside the test binary.
/// Test binaries live at `target/<profile>/deps/`; other bins at `target/<profile>/`.
fn mock_claude_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent() // deps/
        .and_then(|p| p.parent()) // target/<profile>/
        .expect("unexpected test binary path");
    profile_dir.join("mock-claude")
}

#[test]
fn test_pty_spawns_tty() {
    let installer = HookInstaller::new().expect("HookInstaller::new");
    let fifo_path = installer.fifo_path.clone();

    // Read from the FIFO in a background thread so mock-claude's write doesn't block.
    let reader = std::thread::spawn(move || {
        let mut file = std::fs::File::open(&fifo_path).expect("open stop.fifo");
        let mut buf = String::new();
        file.read_to_string(&mut buf).ok();
        buf
    });

    let bin = mock_claude_bin();
    let cmd = CString::new(bin.to_str().expect("binary path is utf-8")).expect("CString");
    let fifo_arg =
        CString::new(installer.fifo_path.to_str().expect("fifo path is utf-8")).expect("CString");

    let spawner = PtySpawner::spawn(&cmd, &[fifo_arg]).expect("PtySpawner::spawn");
    let exit_code = spawner.relay().expect("relay");

    let fifo_content = reader.join().expect("reader thread");
    assert!(
        fifo_content.contains("session_id"),
        "expected Stop JSON payload in FIFO content, got: {fifo_content:?}",
    );
    assert_eq!(
        exit_code, 0,
        "mock-claude should detect a controlling TTY (isatty(0) returned false)",
    );
}
