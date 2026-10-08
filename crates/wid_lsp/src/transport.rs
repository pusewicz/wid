//! JSON-RPC 2.0 over a byte stream, each message framed by a
//! `Content-Length` header, as LSP sends it over stdio.
//!
//! A reader thread turns the stream into [`Incoming`] messages for the
//! server's loop. A message that can't be read (a bad header, a body that
//! isn't JSON, JSON that isn't a request, a notification or a response)
//! becomes [`Incoming::Invalid`], which the server answers with an error;
//! only the end of the stream or a failed read stops the reader.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::mpsc::{Receiver, channel};

use serde_json::{Value, json};

/// JSON-RPC's and LSP's error codes.
pub(crate) mod code {
    /// The body isn't JSON, or the header can't be read.
    pub(crate) const PARSE_ERROR: i64 = -32700;
    /// The JSON isn't a request, a notification or a response.
    pub(crate) const INVALID_REQUEST: i64 = -32600;
    /// No such request.
    pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
    /// The parameters don't have the request's shape.
    pub(crate) const INVALID_PARAMS: i64 = -32602;
    /// The server failed.
    pub(crate) const INTERNAL_ERROR: i64 = -32603;
    /// A request came before `initialize`.
    pub(crate) const SERVER_NOT_INITIALIZED: i64 = -32002;
    /// The client cancelled the request.
    pub(crate) const REQUEST_CANCELLED: i64 = -32800;
    /// The request was well-formed but couldn't be answered.
    pub(crate) const REQUEST_FAILED: i64 = -32803;
}

/// An error to answer a request with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RpcError {
    /// One of [`code`].
    pub(crate) code: i64,
    /// What went wrong, for the client's log.
    pub(crate) message: String,
}

impl RpcError {
    /// An error with `code` and `message`.
    pub(crate) fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into() }
    }
}

/// A message from the client.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Incoming {
    /// A request, which gets one response with its `id`.
    Request {
        /// A number or a string, echoed in the response.
        id: Value,
        /// The method.
        method: String,
        /// The parameters; `null` when there are none.
        params: Value,
    },
    /// A notification, which gets no response.
    Notification {
        /// The method.
        method: String,
        /// The parameters; `null` when there are none.
        params: Value,
    },
    /// A response to a request the server sent. The server sends none that
    /// it waits for, so these are ignored.
    Response,
    /// Something that isn't a message: the error to answer with, for the
    /// request's `id` when it has a usable one and `null` otherwise.
    Invalid {
        /// The id to answer.
        id: Value,
        /// Why the message can't be read.
        error: RpcError,
    },
}

/// Why a frame couldn't be read.
#[derive(Debug)]
pub(crate) enum FrameError {
    /// The headers are malformed or lack a `Content-Length`, or the body
    /// isn't UTF-8. The stream stays usable: the next frame starts after
    /// this one's headers (and body, when its length was known).
    Malformed(String),
    /// Reading failed, or the stream ended inside a frame.
    Io(io::Error),
}

/// Reads one frame's body; `Ok(None)` when the stream ends between frames.
/// Header names are matched without regard to case, header lines may end
/// with `\n` alone, and blank lines between frames are skipped.
pub(crate) fn read_frame(input: &mut impl BufRead) -> Result<Option<String>, FrameError> {
    let mut length: Option<usize> = None;
    let mut problem: Option<String> = None;
    let mut started = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line).map_err(FrameError::Io)? == 0 {
            return match started {
                true => Err(FrameError::Io(io::ErrorKind::UnexpectedEof.into())),
                false => Ok(None),
            };
        }
        let text = line.strip_suffix(b"\n").unwrap_or(&line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        if text.is_empty() {
            if started {
                break;
            }
            continue;
        }
        started = true;
        match std::str::from_utf8(text).ok().and_then(|t| t.split_once(':')) {
            Some((name, value)) if name.trim().eq_ignore_ascii_case("content-length") => {
                match value.trim().parse::<usize>() {
                    Ok(n) => length = Some(n),
                    Err(_) => problem = Some(format!("invalid Content-Length `{}`", value.trim())),
                }
            }
            Some(_) => {}
            None => problem = Some(format!("malformed header line `{}`", String::from_utf8_lossy(text))),
        }
    }
    let Some(length) = length else {
        return Err(FrameError::Malformed(problem.unwrap_or_else(|| "the message has no Content-Length".into())));
    };
    let mut body = Vec::new();
    input.take(length as u64).read_to_end(&mut body).map_err(FrameError::Io)?;
    if body.len() < length {
        return Err(FrameError::Io(io::ErrorKind::UnexpectedEof.into()));
    }
    if let Some(problem) = problem {
        return Err(FrameError::Malformed(problem));
    }
    String::from_utf8(body).map(Some).map_err(|_| FrameError::Malformed("the message isn't UTF-8".into()))
}

/// Writes one frame.
pub(crate) fn write_frame(output: &mut impl Write, body: &str) -> io::Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(body.as_bytes())?;
    output.flush()
}

/// Reads a frame's body as a message.
pub(crate) fn parse_message(body: &str) -> Incoming {
    let invalid =
        |id: Value, message: String| Incoming::Invalid { id, error: RpcError::new(code::INVALID_REQUEST, message) };
    let value: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(e) => {
            return Incoming::Invalid {
                id: Value::Null,
                error: RpcError::new(code::PARSE_ERROR, format!("the message isn't JSON: {e}")),
            };
        }
    };
    let Value::Object(mut map) = value else {
        return invalid(Value::Null, "a message must be a JSON object".into());
    };
    let id = map.remove("id");
    let params = map.remove("params").unwrap_or(Value::Null);
    let usable = |id: &Value| matches!(id, Value::Number(_) | Value::String(_));
    match (map.remove("method"), id) {
        (Some(Value::String(method)), None) => Incoming::Notification { method, params },
        (Some(Value::String(method)), Some(id)) if usable(&id) => Incoming::Request { id, method, params },
        (Some(Value::String(_)), Some(_)) => invalid(Value::Null, "a request's id must be a number or a string".into()),
        (Some(_), id) => {
            invalid(id.filter(usable).unwrap_or(Value::Null), "a message's method must be a string".into())
        }
        (None, Some(_)) if map.contains_key("result") || map.contains_key("error") => Incoming::Response,
        (None, id) => invalid(id.filter(usable).unwrap_or(Value::Null), "the message has no method".into()),
    }
}

/// The body of a response with `result`.
pub(crate) fn response(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// The body of an error response.
pub(crate) fn error_response(id: &Value, error: &RpcError) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": error.code, "message": error.message}}).to_string()
}

/// The body of a notification.
pub(crate) fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

/// Starts a thread that reads messages from `input` until it ends, or
/// until an `exit` notification.
pub(crate) fn spawn_reader(input: impl Read + Send + 'static) -> io::Result<Receiver<Incoming>> {
    let (sender, receiver) = channel();
    std::thread::Builder::new().name("wid-lsp-reader".into()).spawn(move || {
        let mut input = BufReader::new(input);
        loop {
            let message = match read_frame(&mut input) {
                Ok(Some(body)) => parse_message(&body),
                Ok(None) => break,
                Err(FrameError::Malformed(problem)) => {
                    Incoming::Invalid { id: Value::Null, error: RpcError::new(code::PARSE_ERROR, problem) }
                }
                Err(FrameError::Io(e)) => {
                    crate::log(&format!("reading from the client failed: {e}"));
                    break;
                }
            };
            let exit = matches!(&message, Incoming::Notification { method, .. } if method == "exit");
            if sender.send(message).is_err() || exit {
                break;
            }
        }
    })?;
    Ok(receiver)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::{Value, json};

    use super::{FrameError, Incoming, code, parse_message, read_frame, write_frame};

    fn frames(text: &str) -> Vec<Result<String, String>> {
        let mut input = Cursor::new(text.as_bytes().to_vec());
        let mut out = Vec::new();
        loop {
            match read_frame(&mut input) {
                Ok(Some(body)) => out.push(Ok(body)),
                Ok(None) => break,
                Err(FrameError::Malformed(m)) => out.push(Err(m)),
                Err(FrameError::Io(e)) => {
                    out.push(Err(format!("io: {e}")));
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn frames_are_read_by_their_length() {
        let text = "Content-Length: 2\r\n\r\n{}Content-Length: 4\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\nnull";
        assert_eq!(frames(text), [Ok("{}".to_string()), Ok("null".to_string())]);
        // Lenient: any case, `\n` alone, blank lines between frames.
        assert_eq!(frames("\r\ncontent-length:  3\n\n[1]\n"), [Ok("[1]".to_string())]);
        // The length counts bytes, not characters: six bytes end inside
        // the emoji, and its last byte starts a frame the stream ends in.
        let cut = [Err("the message isn't UTF-8".to_string()), Err("io: unexpected end of file".to_string())];
        assert_eq!(frames("Content-Length: 6\r\n\r\n\"é😀"), cut);
        assert_eq!(frames("Content-Length: 8\r\n\r\n\"é😀\""), [Ok("\"é😀\"".to_string())]);
    }

    #[test]
    fn a_bad_frame_is_reported_and_the_next_one_read() {
        let text = "Content-Length: x\r\n\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(frames(text), [Err("invalid Content-Length `x`".to_string()), Ok("{}".to_string())]);
        let text = "Content-Type: a\r\n\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(frames(text), [Err("the message has no Content-Length".to_string()), Ok("{}".to_string())]);
        // A malformed header with a usable length: the body is skipped.
        let text = "garbage\r\nContent-Length: 3\r\n\r\nabcContent-Length: 2\r\n\r\n{}";
        assert_eq!(frames(text), [Err("malformed header line `garbage`".to_string()), Ok("{}".to_string())]);
        assert_eq!(frames("Content-Length: 5\r\n\r\nab"), [Err("io: unexpected end of file".to_string())]);
        assert_eq!(frames(""), []);
    }

    #[test]
    fn frames_round_trip() {
        let mut out = Vec::new();
        write_frame(&mut out, "{\"a\":\"é\"}").expect("writes to memory");
        assert_eq!(out, b"Content-Length: 10\r\n\r\n{\"a\":\"\xc3\xa9\"}");
        let text = String::from_utf8(out).expect("UTF-8");
        assert_eq!(frames(&text), [Ok("{\"a\":\"é\"}".to_string())]);
    }

    #[test]
    fn messages_are_classified() {
        let request = parse_message(r#"{"jsonrpc":"2.0","id":7,"method":"shutdown"}"#);
        assert_eq!(request, Incoming::Request { id: json!(7), method: "shutdown".into(), params: Value::Null });
        let request = parse_message(r#"{"jsonrpc":"2.0","id":"a","method":"x","params":{"k":1}}"#);
        assert_eq!(request, Incoming::Request { id: json!("a"), method: "x".into(), params: json!({"k": 1}) });
        let note = parse_message(r#"{"jsonrpc":"2.0","method":"exit"}"#);
        assert_eq!(note, Incoming::Notification { method: "exit".into(), params: Value::Null });
        assert_eq!(parse_message(r#"{"jsonrpc":"2.0","id":1,"result":null}"#), Incoming::Response);
        let error = |m: Incoming| match m {
            Incoming::Invalid { id, error } => (id, error.code),
            other => panic!("not invalid: {other:?}"),
        };
        assert_eq!(error(parse_message("{nope")), (Value::Null, code::PARSE_ERROR));
        assert_eq!(error(parse_message("[]")), (Value::Null, code::INVALID_REQUEST));
        assert_eq!(error(parse_message(r#"{"id":3}"#)), (json!(3), code::INVALID_REQUEST));
        assert_eq!(error(parse_message(r#"{"id":3,"method":4}"#)), (json!(3), code::INVALID_REQUEST));
        assert_eq!(error(parse_message(r#"{"id":[1],"method":"x"}"#)), (Value::Null, code::INVALID_REQUEST));
    }
}
