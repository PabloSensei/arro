//! The client against a local server: headers, a body, a redirect, a timeout and a stop.

use std::time::Duration;

use crate::execute::{Cancel, Error, execute};
use crate::parse::Method;
use crate::test_server::TestServer;
use crate::variables::PreparedRequest;

fn request(method: Method, url: String) -> PreparedRequest {
    PreparedRequest {
        name: None,
        method,
        url,
        headers: Vec::new(),
        body: None,
        line: 0,
    }
}

#[test]
fn a_get_sends_its_headers_and_reads_the_answer() {
    let server = TestServer::start();
    let mut get = request(Method::Get, server.url("/get?lastName=Davis"));
    get.headers = vec![("Accept".to_owned(), "application/json".to_owned())];

    let response = execute(&get, Duration::from_secs(5), &Cancel::new()).expect("the answer");

    assert_eq!(response.status, 200);
    assert_eq!(response.reason, "OK");
    assert_eq!(response.header("x-server"), Some("test"));
    assert!(response.text().contains("Davis"), "{}", response.text());
    let received = server.received();
    assert_eq!(received[0].method, "GET");
    assert_eq!(received[0].path, "/get?lastName=Davis");
    assert_eq!(received[0].header("accept"), Some("application/json"));
}

#[test]
fn a_post_sends_its_body() {
    let server = TestServer::start();
    let mut post = request(Method::Post, server.url("/post"));
    post.headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
    post.body = Some(br#"{"firstName":"Ann"}"#.to_vec());

    let response = execute(&post, Duration::from_secs(5), &Cancel::new()).expect("the answer");

    assert_eq!(response.status, 201);
    assert_eq!(response.text(), r#"{"firstName":"Ann"}"#);
    let received = server.received();
    assert_eq!(received[0].method, "POST");
    assert_eq!(received[0].body, br#"{"firstName":"Ann"}"#);
    assert_eq!(received[0].header("content-type"), Some("application/json"));
}

#[test]
fn a_redirect_is_followed() {
    let server = TestServer::start();
    let get = request(Method::Get, server.url("/redirect"));

    let response = execute(&get, Duration::from_secs(5), &Cancel::new()).expect("the answer");

    assert_eq!(response.status, 200, "the client followed the redirect");
    assert!(response.text().contains("owners"), "{}", response.text());
    let paths: Vec<String> = server
        .received()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert_eq!(paths, ["/redirect", "/get"]);
}

#[test]
fn a_server_that_does_not_answer_times_out() {
    let server = TestServer::start();
    let get = request(Method::Get, server.url("/slow"));

    let error = execute(&get, Duration::from_millis(200), &Cancel::new())
        .expect_err("the answer is too late");

    assert_eq!(error, Error::Timeout);
}

#[test]
fn a_request_in_flight_can_be_stopped() {
    let server = TestServer::start();
    let get = request(Method::Get, server.url("/trickle"));
    let cancel = Cancel::new();
    let stopped = cancel.clone();
    let started = std::time::Instant::now();
    let worker = std::thread::spawn(move || execute(&get, Duration::from_secs(10), &cancel));
    std::thread::sleep(Duration::from_millis(150));
    stopped.cancel();
    let error = worker
        .join()
        .expect("the worker ends")
        .expect_err("stopped");
    assert_eq!(error, Error::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the stop did not wait for the body"
    );
}
