use std::collections::hash_map::RandomState;
use std::ffi::OsString;
use std::hash::BuildHasher;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub const ASKPASS_PORT: &str = "IDE_ASKPASS_PORT";
pub const ASKPASS_TOKEN: &str = "IDE_ASKPASS_TOKEN";
/// The user may take a while to type a password, but not forever.
const ANSWER_PATIENCE: Duration = Duration::from_secs(300);

type Answer = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Answers the questions git asks through `ide-askpass`, like a password for a remote.
/// Listens on loopback only, and a question without the token gets no answer.
pub struct AskpassServer {
    port: u16,
    token: String,
}

impl AskpassServer {
    /// `answer` gets the prompt and returns what to type, `None` to refuse. It runs on a
    /// thread of its own, a dialog in it does not hold up git calls that need no answer.
    pub fn start(
        answer: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let token = new_token();
        let answer: Arc<Answer> = Arc::new(answer);
        let expected = token.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (answer, expected) = (answer.clone(), expected.clone());
                thread::spawn(move || serve(stream, &expected, answer.as_ref()));
            }
        });
        Ok(Self { port, token })
    }

    /// What git needs to find the helper, for [`crate::GitRunner::with_env`].
    pub fn env(&self, helper: &Path) -> Vec<(OsString, OsString)> {
        let helper = helper.as_os_str().to_owned();
        vec![
            (ASKPASS_PORT.into(), self.port.to_string().into()),
            (ASKPASS_TOKEN.into(), self.token.clone().into()),
            ("GIT_ASKPASS".into(), helper.clone()),
            ("SSH_ASKPASS".into(), helper),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
        ]
    }
}

fn new_token() -> String {
    let state = RandomState::new();
    let parts = [
        state.hash_one(std::process::id()),
        state.hash_one(std::time::SystemTime::now()),
    ];
    format!("{:016x}{:016x}", parts[0], parts[1])
}

/// One question: the token line, the prompt line. The reply is `1` and the answer, or `0`.
fn serve(stream: TcpStream, expected: &str, answer: &Answer) {
    let _ = stream.set_read_timeout(Some(ANSWER_PATIENCE));
    let mut reader = BufReader::new(&stream);
    let (mut token, mut prompt) = (String::new(), String::new());
    if reader.read_line(&mut token).is_err() || reader.read_line(&mut prompt).is_err() {
        return;
    }
    let reply = match token.trim_end() == expected {
        true => answer(prompt.trim_end()),
        false => None,
    };
    let reply = reply.map_or_else(|| "0\n".to_owned(), |text| format!("1\n{text}\n"));
    let _ = (&stream).write_all(reply.as_bytes());
}

/// The helper side: asks the server named in the environment. `None` when nobody answers.
pub fn ask(prompt: &str) -> Option<String> {
    let port: u16 = std::env::var(ASKPASS_PORT).ok()?.parse().ok()?;
    let token = std::env::var(ASKPASS_TOKEN).ok()?;
    let prompt = prompt.replace(['\r', '\n'], " ");
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).ok()?;
    stream
        .write_all(format!("{token}\n{prompt}\n").as_bytes())
        .ok()?;
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).ok()?;
    if status.trim_end() != "1" {
        return None;
    }
    let mut answer = String::new();
    reader.read_line(&mut answer).ok()?;
    Some(answer.trim_end_matches(['\r', '\n']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask_with(port: u16, token: &str, prompt: &str) -> Option<String> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream
            .write_all(format!("{token}\n{prompt}\n").as_bytes())
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        let mut answer = String::new();
        reader.read_line(&mut answer).unwrap();
        (status.trim_end() == "1").then(|| answer.trim_end().to_owned())
    }

    #[test]
    fn a_question_with_the_token_gets_the_answer() {
        let server = AskpassServer::start(|prompt| Some(format!("answer to {prompt}"))).unwrap();

        let reply = ask_with(server.port, &server.token, "Password for 'x':");

        assert_eq!(reply.as_deref(), Some("answer to Password for 'x':"));
    }

    #[test]
    fn a_question_without_the_token_gets_nothing() {
        let server = AskpassServer::start(|_| Some("secret".into())).unwrap();

        assert_eq!(ask_with(server.port, "wrong", "Password:"), None);
    }

    #[test]
    fn a_refusal_comes_through() {
        let server = AskpassServer::start(|_| None).unwrap();

        assert_eq!(ask_with(server.port, &server.token, "Password:"), None);
    }

    #[test]
    fn the_environment_names_the_helper_for_git_and_ssh() {
        let server = AskpassServer::start(|_| None).unwrap();

        let env = server.env(Path::new("C:/ide/ide-askpass.exe"));

        let names: Vec<_> = env
            .iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        for name in ["GIT_ASKPASS", "SSH_ASKPASS", ASKPASS_PORT, ASKPASS_TOKEN] {
            assert!(names.iter().any(|found| found == name), "{name}");
        }
    }
}
