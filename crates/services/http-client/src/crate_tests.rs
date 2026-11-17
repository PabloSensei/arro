//! The crate as the panel uses it: a file of requests, the environments next to it and
//! the prepared request.

use std::fs;
use std::path::Path;

use crate::{Variables, is_request_file, load_environments, parse, prepare};

#[test]
fn the_extensions_of_a_request_file_are_the_two_of_the_client() {
    assert!(is_request_file(Path::new("api.http")));
    assert!(is_request_file(Path::new("API.HTTP")));
    assert!(is_request_file(Path::new("api.rest")));
    assert!(!is_request_file(Path::new("api.http.txt")));
    assert!(!is_request_file(Path::new("api.json")));
}

#[test]
fn the_sample_file_prepares_from_its_environment() {
    let root = test_support::scratch("http-client-sample");
    fs::create_dir_all(&root).unwrap();
    let file = root.join("api.http");
    fs::write(
        &file,
        "### Get owners\nGET {{host}}/owners?lastName=Davis\nAccept: application/json\n\n\
         ### Create\nPOST {{host}}/api/owners\nContent-Type: application/json\n\n\
         {\"firstName\": \"Ann\", \"lastName\": \"Lee\"}\n",
    )
    .unwrap();
    fs::write(
        root.join("http-client.env.json"),
        r#"{"dev":{"host":"http://localhost:8080"}}"#,
    )
    .unwrap();

    let text = fs::read_to_string(&file).unwrap();
    let parsed = parse(&text);
    assert_eq!(parsed.requests.len(), 2);

    let environments = load_environments(&file);
    let values = environments.values("dev");
    let prepared = prepare(&parsed.requests[0], &root, &Variables::for_request(values)).unwrap();
    assert_eq!(prepared.url, "http://localhost:8080/owners?lastName=Davis");
    assert_eq!(prepared.method.as_str(), "GET");
    assert_eq!(prepared.label(), "Get owners");
}
