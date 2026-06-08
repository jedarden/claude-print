use std::io::Write;

fn main() {
    let fifo_path = std::env::args()
        .nth(1)
        .expect("usage: mock-claude <fifo-path>");

    // Simulate the stop hook by writing "stop" to the FIFO.
    // O_WRONLY on a FIFO blocks until a reader opens the other end.
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&fifo_path) {
        let _ = file.write_all(b"stop\n");
    }

    // Exit 0 if stdin is a controlling TTY (login_tty succeeded), 1 otherwise.
    let has_tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    std::process::exit(if has_tty { 0 } else { 1 });
}
