//! What the orchestration asks of SpacetimeDB over HTTP, as the identity that
//! owns the databases: make an identity, publish a module as a new database,
//! delete one, read the metrics. (Everything a match does goes over the
//! WebSocket connection instead.)
//!
//! Plain HTTP only: the server is on this machine or its own network, as the
//! gateway's connection to it is.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// A SpacetimeDB to administer, by its `http://host:port` address.
#[derive(Debug, Clone)]
pub struct Admin {
    host: String,
    port: u16,
}

/// An identity on the server and the token that proves it.
#[derive(Debug, Clone)]
pub struct Account {
    /// Hex.
    pub identity: String,
    pub token: String,
}

impl Admin {
    pub fn new(url: &str) -> Result<Admin, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("{url:?}: only http:// addresses are supported for the server's own SpacetimeDB"))?;
        let authority = rest.trim_end_matches('/');
        let (host, port) = authority.rsplit_once(':').ok_or_else(|| format!("{url:?} has no port"))?;
        let port = port.parse().map_err(|_| format!("{url:?}: {port:?} is not a port"))?;
        Ok(Admin { host: host.to_string(), port })
    }

    /// Whether the server answers.
    pub fn ping(&self) -> bool {
        matches!(self.request("GET", "/v1/ping", None, &[]), Ok((200, _)))
    }

    /// A new identity on the server.
    pub fn new_identity(&self) -> Result<Account, String> {
        let (status, body) = self.request("POST", "/v1/identity", None, &[])?;
        if status != 200 {
            return Err(format!("making an identity: {status} {body}"));
        }
        Ok(Account { identity: json_field(&body, "identity")?, token: json_field(&body, "token")? })
    }

    /// Publish a module as the database `name` (a new one, or an update of
    /// the one the token owns); the database's identity, hex.
    pub fn publish(&self, name: &str, wasm: &[u8], token: &str) -> Result<String, String> {
        let (status, body) = self.request("PUT", &format!("/v1/database/{name}"), Some(token), wasm)?;
        if status != 200 {
            return Err(format!("publishing {name}: {status} {}", body.trim()));
        }
        json_field(&body, "database_identity")
    }

    /// Delete the database `name` and everything in it.
    pub fn delete(&self, name: &str, token: &str) -> Result<(), String> {
        let (status, body) = self.request("DELETE", &format!("/v1/database/{name}"), Some(token), &[])?;
        // a database already gone is as good as deleted
        if status == 200 || status == 404 {
            Ok(())
        } else {
            Err(format!("deleting {name}: {status} {}", body.trim()))
        }
    }

    /// The Prometheus metrics.
    pub fn metrics(&self) -> Result<String, String> {
        match self.request("GET", "/v1/metrics", None, &[])? {
            (200, body) => Ok(body),
            (status, body) => Err(format!("metrics: {status} {}", body.trim())),
        }
    }

    fn request(&self, method: &str, path: &str, bearer: Option<&str>, body: &[u8]) -> Result<(u16, String), String> {
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|e| format!("{}:{}: {e}", self.host, self.port))?;
        stream.set_read_timeout(Some(Duration::from_secs(120))).map_err(|e| e.to_string())?;
        let auth = bearer.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        let head = format!(
            "{method} {path} HTTP/1.0\r\nHost: {}\r\n{auth}Content-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
            self.host,
            body.len()
        );
        stream.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
        stream.write_all(body).map_err(|e| e.to_string())?;
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).map_err(|e| e.to_string())?;
        let answer = String::from_utf8_lossy(&answer);
        let status = answer
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("{method} {path}: no HTTP answer"))?;
        let body = answer.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_string();
        Ok((status, body))
    }
}

/// The string value of `"name"` in a flat JSON answer.
fn json_field(body: &str, name: &str) -> Result<String, String> {
    let key = format!("\"{name}\":\"");
    let at = body.find(&key).ok_or_else(|| format!("no {name} in {body}"))? + key.len();
    let rest = &body[at..];
    let end = rest.find('"').ok_or_else(|| format!("no end of {name} in {body}"))?;
    Ok(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_needs_http_and_a_port() {
        assert!(Admin::new("http://127.0.0.1:3000").is_ok());
        assert!(Admin::new("http://localhost:3000/").is_ok());
        assert!(Admin::new("https://example.org:3000").is_err());
        assert!(Admin::new("http://127.0.0.1").is_err());
    }

    #[test]
    fn json_fields_are_found_in_a_publish_answer() {
        let body = r#"{"Success":{"domain":"m1","database_identity":"abc123","op":"created"}}"#;
        assert_eq!(json_field(body, "database_identity").unwrap(), "abc123");
        assert!(json_field(body, "nope").is_err());
    }
}
