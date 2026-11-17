//! Prints the address of the mock debug adapter and serves one session.
//!
//! `lang-fixture-mock-dap` is meant for a harness that starts it and reads the first
//! line of its output: `LISTENING host:port`.

fn main() {
    let server = match lang_fixture::MockDap::start() {
        Ok(server) => server,
        Err(error) => {
            eprintln!("lang-fixture: cannot listen: {error}");
            std::process::exit(1);
        }
    };
    println!("LISTENING {}", server.address());
    let _ = std::io::Write::flush(&mut std::io::stdout());
    server.join();
}
