//! Requests over HTTP, as `wasi:http`'s outgoing handler makes them: what is at a URL asked for, its answer's status,
//! headers and body read as it comes. Only what the host asks is here - a `GET`, with headers of its own. The
//! system's asks the network (`ureq`); [`Served`] answers from what a test gives it, and refuses the rest.

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::Arc;

/// What is asked: what is at a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    /// Each header, its name and value.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// What is at `url`, asked with no headers of its own.
    #[must_use]
    pub fn get(url: &str) -> Request {
        Request { url: url.to_string(), headers: Vec::new() }
    }

    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Request {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// An answer: its status, its headers, and its body to read.
pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    pub body: Box<dyn Read + Send>,
}

impl Response {
    #[must_use]
    pub fn new(status: u16, headers: Vec<(String, String)>, body: Box<dyn Read + Send>) -> Response {
        Response { status, headers, body }
    }

    /// A header's value, by its name in any case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(named, _)| named.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response").field("status", &self.status).field("headers", &self.headers).finish_non_exhaustive()
    }
}

/// What asks over HTTP.
pub trait Http: Send + Sync {
    /// Ask, and have the answer as it begins: any status, an error's too.
    ///
    /// # Errors
    /// Nothing answered: the name is not found, the connection failed or was refused.
    fn send(&self, request: &Request) -> io::Result<Response>;
}

/// The system's: the network, by `ureq`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemHttp;

impl Http for SystemHttp {
    fn send(&self, request: &Request) -> io::Result<Response> {
        let mut asked = ureq::get(&request.url);
        for (name, value) in &request.headers {
            asked = asked.set(name, value);
        }
        // An answer whatever its status: whoever asked decides what one is to it.
        let answer = match asked.call() {
            Ok(answer) | Err(ureq::Error::Status(_, answer)) => answer,
            Err(e) => return Err(io::Error::other(e.to_string())),
        };
        let names = answer.headers_names();
        let headers = names.into_iter().filter_map(|name| Some((answer.header(&name)?.to_string(), name)));
        let headers = headers.map(|(value, name)| (name, value)).collect();
        Ok(Response { status: answer.status(), headers, body: Box::new(answer.into_reader()) })
    }
}

/// What a test serves: each URL's bytes, given whole (`200`); every other is refused, as where there is no network -
/// all of them by one served nothing (`Served::default()`), a run that reaches no network.
#[derive(Clone, Debug, Default)]
pub struct Served {
    files: Arc<HashMap<String, Arc<[u8]>>>,
}

impl Served {
    /// What is served at `url`, besides the rest.
    #[must_use]
    pub fn with(mut self, url: &str, bytes: &[u8]) -> Served {
        Arc::make_mut(&mut self.files).insert(url.to_string(), Arc::from(bytes));
        self
    }
}

impl Http for Served {
    fn send(&self, request: &Request) -> io::Result<Response> {
        let Some(bytes) = self.files.get(&request.url) else {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "downloads are off in this run"));
        };
        let length = vec![("Content-Length".to_string(), bytes.len().to_string())];
        Ok(Response { status: 200, headers: length, body: Box::new(io::Cursor::new(bytes.to_vec())) })
    }
}
