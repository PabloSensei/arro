//! Git and ssh start this with a prompt and read the answer from stdout. The question goes
//! to the IDE that started git, never to a terminal.

use std::process::ExitCode;

fn main() -> ExitCode {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    match vcs::ask(&prompt) {
        Some(answer) => {
            println!("{answer}");
            ExitCode::SUCCESS
        }
        None => ExitCode::FAILURE,
    }
}
