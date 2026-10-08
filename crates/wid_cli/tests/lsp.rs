//! `wid lsp` end to end: the built binary, spoken to over pipes with
//! JSON-RPC, as an editor speaks to it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long a test waits for any one message.
const WAIT: Duration = Duration::from_secs(20);

/// A package on disk: `main.wid` (which the editor changes before saving)
/// and `util.wid`.
fn package(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wid-lsp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the package");
    std::fs::write(dir.join("main.wid"), FIXED.replace("x/2", "x / 2")).expect("write main.wid");
    std::fs::write(dir.join("util.wid"), "# Doubles `x`.\ndef twice(x: Int) -> Int = x * 2\n").expect("write util.wid");
    dir
}

/// The buffer as the editor opens it: line 6 reads a field without `@`
/// after `é` (one UTF-16 unit, two bytes) and `😀` (two units, four
/// bytes), and `half` is defined twice.
const BROKEN: &str = "\
# A ball.
struct Ball
  pos: Int

  # Moves the ball by `by`.
  def move(by: Int) -> Int
    puts \"é😀\", pos
    @pos += by
    @pos
  end
end

def half(x: Int) -> Int = x / 2
def half(x: Int) -> Int = x / 3

def main
  b = Ball.new(pos: 1)
  b.move(2)
  puts twice(b.pos)
end
";

/// The buffer after the fixes: no errors, but `x/2` isn't formatted.
const FIXED: &str = "\
# A ball.
struct Ball
  pos: Int

  # Moves the ball by `by`.
  def move(by: Int) -> Int
    puts \"é😀\", @pos
    @pos += by
    @pos
  end
end

def half(x: Int) -> Int = x/2

def main
  b = Ball.new(pos: 1)
  b.move(2)
  puts twice(half(b.pos))
end
";

/// A running `wid lsp`.
struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    /// Notifications read while waiting for something else.
    seen: Vec<Value>,
    next_id: i64,
}

impl Client {
    fn start(dir: &Path) -> Client {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut child = Command::new(env!("CARGO_BIN_EXE_wid"))
            .arg("lsp")
            .current_dir(dir)
            .env("WID_ROOT", root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start wid lsp");
        let stdin = child.stdin.take().expect("a piped stdin");
        let stdout = child.stdout.take().expect("a piped stdout");
        let (sender, messages) = channel();
        std::thread::spawn(move || {
            let mut out = BufReader::new(stdout);
            while let Some(body) = read_frame(&mut out) {
                let value = serde_json::from_slice(&body).expect("the server writes JSON");
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Client { child, stdin, messages, seen: Vec::new(), next_id: 0 }
    }

    fn write(&mut self, body: &[u8]) {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).expect("write a header");
        self.stdin.write_all(body).expect("write a body");
        self.stdin.flush().expect("flush");
    }

    fn notify(&mut self, method: &str, params: Value) {
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string();
        self.write(body.as_bytes());
    }

    fn next(&mut self, what: &str) -> Value {
        match self.messages.recv_timeout(WAIT) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => panic!("no message from wid lsp while waiting for {what}"),
            Err(RecvTimeoutError::Disconnected) => panic!("wid lsp stopped while waiting for {what}"),
        }
    }

    /// Sends a request and returns its response, keeping the notifications
    /// that come first.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        self.write(body.as_bytes());
        self.response(json!(id), method)
    }

    fn response(&mut self, id: Value, what: &str) -> Value {
        loop {
            let message = self.next(what);
            if message.get("method").is_some() {
                self.seen.push(message);
            } else if message["id"] == id {
                return message;
            } else {
                panic!("a response to another request: {message}");
            }
        }
    }

    /// The next diagnostics published for `uri`.
    fn diagnostics(&mut self, uri: &str) -> Value {
        let mine = |m: &Value| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri;
        if let Some(i) = self.seen.iter().position(mine) {
            return self.seen.remove(i)["params"].clone();
        }
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            let message = self.next("diagnostics");
            if mine(&message) {
                return message["params"].clone();
            }
            self.seen.push(message);
        }
        panic!("no diagnostics for {uri}");
    }

    /// Sends `shutdown` and `exit`, and returns the exit status.
    fn stop(mut self) -> Option<i32> {
        let reply = self.request("shutdown", Value::Null);
        assert_eq!(reply["result"], Value::Null);
        self.notify("exit", Value::Null);
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.code();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        panic!("wid lsp didn't exit");
    }
}

/// Reads one `Content-Length` frame; `None` at the end.
fn read_frame(input: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).ok()?;
    Some(body)
}

fn uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = if text.starts_with('/') { text } else { format!("/{text}") };
    format!("file://{text}")
}

fn initialize(client: &mut Client, dir: &Path, encodings: &[&str]) -> Value {
    let capabilities = json!({
        "general": {"positionEncodings": encodings},
        "textDocument": {
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
            "publishDiagnostics": {"relatedInformation": true},
        },
    });
    let params = json!({"processId": null, "rootUri": uri(dir), "capabilities": capabilities});
    let reply = client.request("initialize", params);
    client.notify("initialized", json!({}));
    reply
}

fn range(a: (u32, u32), b: (u32, u32)) -> Value {
    json!({"start": {"line": a.0, "character": a.1}, "end": {"line": b.0, "character": b.1}})
}

fn at(uri: &str, line: u32, character: u32) -> Value {
    json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}})
}

#[test]
fn an_editing_session() {
    let dir = package("session");
    let main = uri(&dir.join("main.wid"));
    let util = uri(&dir.join("util.wid"));
    let mut client = Client::start(&dir);
    let reply = initialize(&mut client, &dir, &["utf-16"]);
    let capabilities = &reply["result"]["capabilities"];
    assert_eq!(capabilities["positionEncoding"], "utf-16");
    assert_eq!(capabilities["textDocumentSync"]["change"], 1, "full sync");
    for provider in ["hoverProvider", "definitionProvider", "documentFormattingProvider", "documentSymbolProvider"] {
        assert_eq!(capabilities[provider], true, "{provider}");
    }

    // The unsaved buffer is what gets checked: the file on disk is fine.
    let item = json!({"uri": main, "languageId": "wid", "version": 1, "text": BROKEN});
    client.notify("textDocument/didOpen", json!({"textDocument": item}));
    let published = client.diagnostics(&main);
    assert_eq!(published["version"], 1);
    let diags = published["diagnostics"].as_array().expect("a list");
    let codes: Vec<&str> = diags.iter().map(|d| d["code"].as_str().unwrap_or("")).collect();
    assert_eq!(codes, ["E0201", "E0317"]);
    let field = &diags[0];
    // `pos` after `é😀`: column 16 in UTF-16 units (15 characters, 19 bytes).
    assert_eq!(field["range"], range((6, 16), (6, 19)));
    assert_eq!(field["severity"], 1);
    assert_eq!(field["source"], "wid");
    let message = field["message"].as_str().expect("a message");
    assert!(message.starts_with("undefined name `pos`\nnot found in this scope\nnote: "), "{message}");
    assert!(message.contains("\nhelp: read the field with `@pos`"), "{message}");
    assert!(field["codeDescription"]["href"].as_str().is_some_and(|h| h.ends_with("docs/errors/E0201.md")));
    let twice = &diags[1];
    assert_eq!(twice["range"], range((13, 4), (13, 8)));
    let related = &twice["relatedInformation"][0];
    assert_eq!(related["message"], "first definition");
    assert_eq!(related["location"]["uri"], main.as_str());
    assert_eq!(related["location"]["range"], range((12, 4), (12, 8)));
    // Every file of the package is published, without errors here.
    assert_eq!(client.diagnostics(&util)["diagnostics"], json!([]));

    // The machine-applicable fix is a preferred quick fix.
    let context = json!({"diagnostics": [], "only": ["quickfix"]});
    let params = json!({"textDocument": {"uri": main}, "range": range((6, 17), (6, 17)), "context": context});
    let actions = client.request("textDocument/codeAction", params);
    let actions = actions["result"].as_array().expect("a list of actions");
    assert_eq!(actions.len(), 1, "{actions:?}");
    let fix = &actions[0];
    assert_eq!(fix["kind"], "quickfix");
    assert_eq!(fix["isPreferred"], true);
    assert!(fix["title"].as_str().is_some_and(|t| t.starts_with("read the field with `@pos`")));
    assert_eq!(fix["diagnostics"][0]["code"], "E0201");
    assert_eq!(fix["edit"]["changes"][main.as_str()], json!([{"range": range((6, 16), (6, 19)), "newText": "@pos"}]));
    let elsewhere =
        json!({"textDocument": {"uri": main}, "range": range((2, 0), (2, 0)), "context": {"diagnostics": []}});
    assert_eq!(client.request("textDocument/codeAction", elsewhere)["result"], json!([]));

    // A full change that fixes both publishes an empty list.
    let change = json!({"textDocument": {"uri": main, "version": 2}, "contentChanges": [{"text": FIXED}]});
    client.notify("textDocument/didChange", change);
    let published = client.diagnostics(&main);
    assert_eq!((&published["version"], &published["diagnostics"]), (&json!(2), &json!([])));

    // Hover and definition on a use.
    let hover = client.request("textDocument/hover", at(&main, 16, 5));
    let contents = hover["result"]["contents"]["value"].as_str().expect("markdown");
    assert_eq!(hover["result"]["contents"]["kind"], "markdown");
    assert!(contents.starts_with("```wid\ndef move(by: Int) -> Int\n```"), "{contents}");
    assert!(contents.contains("Moves the ball by `by`."), "{contents}");
    assert_eq!(hover["result"]["range"], range((16, 4), (16, 8)));
    let definition = client.request("textDocument/definition", at(&main, 16, 5));
    assert_eq!(definition["result"], json!({"uri": main, "range": range((5, 6), (5, 10))}));
    // Across files, and with the cursor just past the name.
    let definition = client.request("textDocument/definition", at(&main, 17, 12));
    assert_eq!(definition["result"], json!({"uri": util, "range": range((1, 4), (1, 9))}));
    // A position after `é😀` on the same line: `pos` in `@pos`.
    let hover = client.request("textDocument/hover", at(&main, 6, 18));
    let contents = hover["result"]["contents"]["value"].as_str().expect("markdown");
    assert!(contents.starts_with("```wid\npos: Int\n```"), "{contents}");
    let start = &hover["result"]["range"]["start"];
    assert_eq!((&start["line"], &start["character"]), (&json!(6), &json!(16)));

    // Symbols, nested under their type.
    let symbols = client.request("textDocument/documentSymbol", json!({"textDocument": {"uri": main}}));
    let names: Vec<&str> =
        symbols["result"].as_array().expect("a list").iter().map(|s| s["name"].as_str().unwrap_or("")).collect();
    assert_eq!(names, ["Ball", "half", "main"]);
    let ball = &symbols["result"][0];
    assert_eq!(ball["children"][1]["name"], "move");
    assert_eq!(ball["children"][1]["selectionRange"], range((5, 6), (5, 10)));

    // Formatting: one edit for the whole document.
    let options = json!({"tabSize": 2, "insertSpaces": true});
    let edits = client.request("textDocument/formatting", json!({"textDocument": {"uri": main}, "options": options}));
    assert_eq!(edits["result"], json!([{"range": range((0, 0), (19, 0)), "newText": FIXED.replace("x/2", "x / 2")}]));

    // A malformed message and an unknown request get errors, and the
    // server carries on.
    client.write(b"{\"jsonrpc\": \"2.0\", \"id\": ");
    let error = client.response(Value::Null, "the parse error");
    assert_eq!(error["error"]["code"], -32700);
    let unknown = client.request("wid/unknown", json!({}));
    assert_eq!(unknown["error"]["code"], -32601);

    // Closing the last file of a package stops checking it, and clears its
    // diagnostics.
    let change = json!({"textDocument": {"uri": main, "version": 3}, "contentChanges": [{"text": BROKEN}]});
    client.notify("textDocument/didChange", change);
    assert_eq!(client.diagnostics(&main)["diagnostics"].as_array().map(Vec::len), Some(2));
    client.notify("textDocument/didClose", json!({"textDocument": {"uri": main}}));
    assert_eq!(client.diagnostics(&main)["diagnostics"], json!([]));
    assert_eq!(client.stop(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn utf8_positions_when_the_client_offers_them() {
    let dir = package("utf8");
    let main = uri(&dir.join("main.wid"));
    let mut client = Client::start(&dir);
    let reply = initialize(&mut client, &dir, &["utf-8", "utf-16"]);
    assert_eq!(reply["result"]["capabilities"]["positionEncoding"], "utf-8");
    let item = json!({"uri": main, "languageId": "wid", "version": 1, "text": BROKEN});
    client.notify("textDocument/didOpen", json!({"textDocument": item}));
    let published = client.diagnostics(&main);
    // `pos` after `é😀`, counted in bytes.
    assert_eq!(published["diagnostics"][0]["range"], range((6, 19), (6, 22)));
    let change = json!({"textDocument": {"uri": main, "version": 2}, "contentChanges": [{"text": FIXED}]});
    client.notify("textDocument/didChange", change);
    assert_eq!(client.diagnostics(&main)["diagnostics"], json!([]));
    // `@pos` is at bytes 19 to 23.
    let hover = client.request("textDocument/hover", at(&main, 6, 21));
    let contents = hover["result"]["contents"]["value"].as_str().expect("markdown");
    assert!(contents.starts_with("```wid\npos: Int\n```"), "{contents}");
    assert_eq!(hover["result"]["range"]["start"], json!({"line": 6, "character": 19}));
    assert_eq!(client.stop(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}
