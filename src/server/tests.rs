use super::local::server_args;
use super::*;
use serde_json::json;
use std::ffi::OsString;
use std::path::Path;

/// Direct adapter calls must not leak local knobs to a generic API, even when
/// every manual override was supplied. Exercise real HTTP and both response paths.
#[test]
fn remote_http_strips_all_llama_sampling_in_both_paths() {
    use std::io::{BufRead, BufReader, Read, Write};
    for streaming in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ep = RemoteEndpoint {
            base_url: format!("http://{}/", listener.local_addr().unwrap()),
            api_key: Some("test-key".into()),
            model: Some("served-name".into()),
        };
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut length = 0;
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
                headers.push_str(&line.to_ascii_lowercase());
            }
            assert!(headers.starts_with("post /v1/chat/completions "));
            assert!(headers.contains("authorization: bearer test-key"));
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let mut keys: Vec<_> = body
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            let expected = if streaming {
                vec!["max_tokens", "messages", "model", "stream", "temperature"]
            } else {
                vec!["max_tokens", "messages", "model", "temperature"]
            };
            assert_eq!(keys, expected);
            assert_eq!(body["model"], "served-name");
            assert_eq!(body["max_tokens"], 64);
            assert_eq!(body["temperature"], 0.25);
            let response = if streaming {
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n"
            } else {
                "{\"choices\":[{\"message\":{\"content\":\"ok\"}}]}"
            };
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
        });
        let result = if streaming {
            ep.ocr_image_stream(
                "Multi page parsing.",
                "data:image/png;base64,AA==",
                64,
                Some(0.25),
                Some(1.3),
                Some(1.0),
                Some(1.75),
                Some(2),
                Some(-1),
                &mut |_| true,
                &|| false,
            )
        } else {
            ep.ocr_image(
                "Multi page parsing.",
                "data:image/png;base64,AA==",
                64,
                Some(0.25),
                Some(1.3),
                Some(1.0),
                Some(1.75),
                Some(2),
                Some(-1),
            )
        };
        assert_eq!(result.unwrap().text, "ok");
        server.join().unwrap();
    }
}

/// Simulate llama-server's disconnect cancellation: a pending request must close
/// its socket (also during TTFT), then the same server must serve a fresh run.
fn cancel_stream_and_reuse_server(send_token: bool) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_server = Arc::clone(&cancelled);

    let server = std::thread::spawn(move || {
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(length) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = length.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["stream"],
                true
            );

            if attempt == 0 {
                if send_token {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n").unwrap();
                } else {
                    // Cancel before the server has sent any HTTP headers.
                    cancel_on_server.store(true, Ordering::SeqCst);
                }
                let mut byte = [0];
                match reader.read(&mut byte) {
                    Ok(0) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                    other => panic!("cancel must disconnect the active request: {other:?}"),
                }
            } else {
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"fresh run\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        }
    });

    let mut partial = String::new();
    let started = std::time::Instant::now();
    let stopped = ocr_via_stream(
        &base_url,
        None,
        None,
        "OCR",
        "data:image/png;base64,AA==",
        100,
        0.0,
        None,
        None,
        None,
        None,
        None,
        &mut |token| {
            partial.push_str(token);
            cancelled.store(true, Ordering::SeqCst);
            true
        },
        &|| cancelled.load(Ordering::SeqCst),
    );
    assert_eq!(stopped.unwrap_err().to_string(), "stopped");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(partial, if send_token { "partial" } else { "" });

    cancelled.store(false, Ordering::SeqCst);
    let next = ocr_via_stream(
        &base_url,
        None,
        None,
        "OCR",
        "data:image/png;base64,AA==",
        100,
        0.0,
        None,
        None,
        None,
        None,
        None,
        &mut |_| true,
        &|| cancelled.load(Ordering::SeqCst),
    )
    .unwrap();
    assert_eq!(next.text, "fresh run");
    assert!(!next.truncated);
    server.join().unwrap();
}

#[test]
fn cancel_during_ttft_disconnects_and_allows_another_run() {
    cancel_stream_and_reuse_server(false);
}

#[test]
fn cancel_during_stream_disconnects_and_allows_another_run() {
    cancel_stream_and_reuse_server(true);
}

/// The optional DeepSeek-OCR flags must reach the spawn args only when set,
/// and the baseline must keep the single-sequence, bounded-context startup
/// required by Unlimited-OCR-2, without spawning a real model.
#[test]
fn server_args_adds_optional_flags_only_when_set() {
    let model = Path::new("/m/model.gguf");
    let mmproj = Path::new("/m/mmproj.gguf");

    // Test paths are ASCII, so to_string_lossy round-trips losslessly here;
    // the OsString return type matters only for non-UTF8 paths in the wild.
    let stringify = |v: Vec<OsString>| -> Vec<String> {
        v.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    };

    let base = stringify(server_args(model, mmproj, 8080, None, None));
    assert_eq!(
        base,
        vec![
            "-m",
            "/m/model.gguf",
            "--mmproj",
            "/m/mmproj.gguf",
            "-np",
            "1",
            "-c",
            "32768",
            "--jinja",
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
        ]
    );
    assert!(!base
        .iter()
        .any(|a| a == "--image-max-tokens" || a == "--chat-template"));

    let full = stringify(server_args(
        model,
        mmproj,
        8080,
        Some(1280),
        Some("deepseek-ocr"),
    ));
    // Each flag appears adjacent to its value.
    assert!(full.windows(2).any(|w| w == ["--image-max-tokens", "1280"]));
    assert!(full
        .windows(2)
        .any(|w| w == ["--chat-template", "deepseek-ocr"]));
}

#[test]
fn parses_content() {
    let resp = json!({ "choices": [{ "message": { "content": "# hi" } }] });
    assert_eq!(parse_completion(&resp).unwrap(), "# hi");
}

#[test]
fn rejects_bad_shape() {
    assert!(parse_completion(&json!({})).is_err());
    assert!(parse_completion(&json!({ "choices": [] })).is_err());
    // content present but wrong type
    let bad = json!({ "choices": [{ "message": { "content": 42 } }] });
    assert!(parse_completion(&bad).is_err());
}

/// RemoteEndpoint must POST to {base}/v1/chat/completions, send the bearer
/// token, and return the assistant text. Stub HTTP server captures the
/// Authorization header + request path so we lock both the routing and auth.
#[test]
fn remote_endpoint_sends_bearer_and_parses() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let resp_body = json!({ "choices": [{ "message": { "content": "# remote ok" } }] }).to_string();
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        resp_body.len(),
        resp_body,
    );

    // Capture the request line + Authorization header from one request.
    let (tx, rx) = mpsc::channel::<(String, Option<String>)>();
    std::thread::spawn(move || {
        let Ok(s) = listener.incoming().next().unwrap() else {
            return;
        };
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut writer = s;
        let mut request_line = String::new();
        reader.read_line(&mut request_line).ok();
        let mut auth = None;
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let t = line.trim_end_matches(['\r', '\n']);
            if t.is_empty() {
                break;
            }
            let lower = t.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("authorization:") {
                auth = Some(v.trim().to_string());
            }
            if let Some(v) = lower.strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        let _ = reader.read_exact(&mut body);
        let _ = writer.write_all(http_response.as_bytes());
        let _ = tx.send((request_line.trim().to_string(), auth));
    });

    let ep = RemoteEndpoint {
        base_url: format!("http://127.0.0.1:{port}/"), // trailing slash must be trimmed
        api_key: Some("secret".to_string()),
        model: None,
    };
    let out = ep
        .ocr_image(
            "<|grounding|>x",
            "data:image/png;base64,AAAA",
            64,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("remote ocr");
    assert_eq!(out.text, "# remote ok");

    let (request_line, auth) = rx.recv().expect("stub recorded request");
    assert_eq!(request_line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(auth.as_deref(), Some("bearer secret"));
}

/// A `model` set on RemoteEndpoint must land in the request body (gateways
/// like litellm/vLLM require it); when unset, no `"model"` key is sent (a bare
/// llama-server would reject an empty/unknown model). Stub captures the body.
#[test]
fn remote_endpoint_injects_model_only_when_set() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    fn run_once(model: Option<String>) -> serde_json::Value {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();
        let resp_body = json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let Ok(s) = listener.incoming().next().unwrap() else {
                return;
            };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut writer = s;
            let mut content_length = 0usize;
            let mut first = String::new();
            reader.read_line(&mut first).ok();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(http_response.as_bytes());
            let _ = tx.send(body);
        });

        let ep = RemoteEndpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            model,
        };
        ep.ocr_image(
            "p",
            "data:image/png;base64,AAAA",
            64,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("ocr");
        let body = rx.recv().expect("stub recorded body");
        serde_json::from_slice(&body).expect("body is json")
    }

    let with = run_once(Some("my-model".to_string()));
    assert_eq!(with["model"], json!("my-model"));

    let without = run_once(None);
    assert!(
        without.get("model").is_none(),
        "model key must be absent when unset"
    );
}

/// EH-0010 acceptance: prove `ocr_via_stream` fires `on_token` once per SSE
/// `data:` chunk and assembles the full text correctly.
///
/// The stub HTTP server returns a proper SSE body with `stream: true` semantics:
///   data: {"choices":[{"delta":{"content":"Hello"}}]}
///   data: {"choices":[{"delta":{"content":" world"}}]}
///   data: [DONE]
///
/// This is the real SSE wire format that llama-server sends. The test verifies:
///   1. on_token fires exactly twice, once per chunk.
///   2. The assembled return value equals the concatenation of both chunks.
///   3. Blank lines and [DONE] are silently skipped (not counted as tokens).
#[test]
fn sse_streaming_fires_on_token() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    // Build the SSE body: two chunks then [DONE].
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\r\n",
        "data: [DONE]\r\n",
    );
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
        sse_body.len(),
        sse_body,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let http_resp_clone = http_response.clone();
    std::thread::spawn(move || {
        // Serve a single connection with the SSE response, then exit.
        if let Ok(stream) = listener.accept() {
            let (sock, _) = stream;
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut writer = sock;
            // Drain the request headers + body.
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if t.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = t.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = Read::read_exact(&mut reader, &mut body);
            let _ = writer.write_all(http_resp_clone.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let mut tokens: Vec<String> = Vec::new();
    let result = ocr_via_stream(
        &base_url,
        None,
        None,
        "test prompt",
        "data:image/png;base64,AAAA",
        64,
        0.0,
        None,
        None,
        None,
        None,
        None,
        &mut |chunk: &str| {
            tokens.push(chunk.to_string());
            true
        },
        &|| false,
    );

    assert!(result.is_ok(), "ocr_via_stream failed: {:?}", result.err());
    let assembled = result.unwrap();

    // on_token must fire exactly once per data chunk (2 chunks, not 3 — [DONE] is not a token).
    assert_eq!(
        tokens,
        vec!["Hello".to_string(), " world".to_string()],
        "on_token fired with unexpected chunks: {tokens:?}"
    );
    // Assembled text must be the concatenation of both chunks.
    assert_eq!(
        assembled.text, "Hello world",
        "assembled text mismatch: {:?}",
        assembled.text
    );
    assert!(
        !assembled.truncated,
        "no finish_reason in the stub chunks; must not be flagged truncated"
    );
}

/// Non-SSE fallback: a server that ignores `stream: true` and replies with a
/// single plain JSON completion body must still yield its text (parsed like
/// ocr_via) rather than an empty string. Regression guard for the streaming
/// switch in ocr_pages, which otherwise silently dropped such responses.
#[test]
fn stream_falls_back_to_plain_json_completion() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    // A normal (non-streaming) OpenAI chat-completion body, no `data:` framing.
    let json_body = r#"{"choices":[{"message":{"content":"plain json ok"}}]}"#;
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        json_body.len(),
        json_body,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let http_resp_clone = http_response.clone();
    std::thread::spawn(move || {
        if let Ok((sock, _)) = listener.accept() {
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut writer = sock;
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if t.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = t.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = Read::read_exact(&mut reader, &mut body);
            let _ = writer.write_all(http_resp_clone.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let mut tokens: Vec<String> = Vec::new();
    let result = ocr_via_stream(
        &base_url,
        None,
        None,
        "test prompt",
        "data:image/png;base64,AAAA",
        64,
        0.0,
        None,
        None,
        None,
        None,
        None,
        &mut |chunk: &str| {
            tokens.push(chunk.to_string());
            true
        },
        &|| false,
    );

    let assembled = result.expect("ocr_via_stream fallback failed");
    assert_eq!(assembled.text, "plain json ok", "fallback text mismatch");
    // The fallback delivers the whole body as one on_token call.
    assert_eq!(tokens, vec!["plain json ok".to_string()]);
}

/// A 2xx stream that carries a provider error chunk (`{"error":{"message":..}}`)
/// must surface as an Err carrying that message, not silently finish with the
/// partial text gathered so far. Regression guard for the bug where the SSE loop
/// only inspected `choices[0].delta.content` and let error chunks fall through,
/// making a failed run look successful with empty/partial output.
#[test]
fn sse_streaming_surfaces_provider_error() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    // A single error chunk (vLLM/OpenAI-style), then [DONE]. No content chunks.
    let sse_body = concat!(
        "data: {\"error\":{\"message\":\"Context length exceeded\"}}\r\n",
        "data: [DONE]\r\n",
    );
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
        sse_body.len(),
        sse_body,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let http_resp_clone = http_response.clone();
    std::thread::spawn(move || {
        if let Ok(stream) = listener.accept() {
            let (sock, _) = stream;
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut writer = sock;
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if t.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = t.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = Read::read_exact(&mut reader, &mut body);
            let _ = writer.write_all(http_resp_clone.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let mut tokens: Vec<String> = Vec::new();
    let result = ocr_via_stream(
        &base_url,
        None,
        None,
        "test prompt",
        "data:image/png;base64,AAAA",
        64,
        0.0,
        None,
        None,
        None,
        None,
        None,
        &mut |chunk: &str| {
            tokens.push(chunk.to_string());
            true
        },
        &|| false,
    );

    let err = result.expect_err("provider error chunk must surface as Err, not Ok");
    let msg = err.to_string();
    assert!(
        msg.contains("Context length exceeded"),
        "error message must carry the provider text, got: {msg}"
    );
    assert!(
        tokens.is_empty(),
        "no token should be emitted for an error chunk"
    );
}

/// `apply_sampling` must leave the body byte-for-byte unchanged on (None, None,
/// None) (the remote/vLLM baseline), and land each knob under its llama.cpp
/// field name when set. DRY implies the hardcoded `dry_allowed_length: 4`
/// companion. `dry_base` must reach the wire only when `dry_multiplier` is also
/// set (a base with DRY disabled is inert in llama.cpp). Pure.
#[test]
fn apply_sampling_sets_fields_only_when_set() {
    let baseline = json!({ "temperature": 0, "max_tokens": 64 });

    let mut body = baseline.clone();
    apply_sampling(&mut body, None, None, None, None, None);
    assert_eq!(body, baseline, "all-None must not touch the body");

    let mut body = baseline.clone();
    apply_sampling(&mut body, Some(1.1), Some(0.8), Some(2.0), None, None);
    assert_eq!(body["repeat_penalty"], json!(1.1f32));
    assert_eq!(body["dry_multiplier"], json!(0.8f32));
    assert_eq!(
        body["dry_allowed_length"],
        json!(4),
        "dry_allowed_length defaults to 4 when unset"
    );
    assert_eq!(body["dry_base"], json!(2.0f32));
    assert!(
        body.get("dry_penalty_last_n").is_none(),
        "dry_penalty_last_n absent when unset"
    );

    let mut body = baseline.clone();
    apply_sampling(&mut body, None, Some(0.8), Some(2.0), None, None);
    assert!(body.get("repeat_penalty").is_none());
    assert_eq!(body["dry_multiplier"], json!(0.8f32));
    assert_eq!(body["dry_allowed_length"], json!(4));
    assert_eq!(body["dry_base"], json!(2.0f32));

    // Anti-loop preset: with DRY on, an explicit allowed-length of 2 and a
    // penalty-last-n alias of -1 scans the fixed local context on the wire.
    let mut body = baseline.clone();
    apply_sampling(&mut body, None, Some(0.8), None, Some(2), Some(-1));
    assert_eq!(body["dry_allowed_length"], json!(2));
    assert_eq!(body["dry_penalty_last_n"], json!(LOCAL_CONTEXT_SIZE));

    // The two anti-loop knobs are inert without DRY: dry_multiplier None must
    // drop dry_allowed_length AND dry_penalty_last_n even when supplied.
    let mut body = baseline.clone();
    apply_sampling(&mut body, None, None, None, Some(2), Some(-1));
    assert_eq!(
        body, baseline,
        "anti-loop knobs are dropped when DRY is off"
    );

    let mut body = baseline.clone();
    apply_sampling(&mut body, Some(1.1), None, None, None, None);
    assert_eq!(body["repeat_penalty"], json!(1.1f32));
    assert!(body.get("dry_multiplier").is_none());
    assert!(body.get("dry_allowed_length").is_none());

    // Critical gating case: dry_base supplied but dry_multiplier absent must be
    // dropped, since dry_base alone is inert in llama.cpp.
    let mut body = baseline;
    apply_sampling(&mut body, Some(1.1), None, Some(2.0), None, None);
    assert_eq!(body["repeat_penalty"], json!(1.1f32));
    assert!(body.get("dry_multiplier").is_none());
    assert!(body.get("dry_allowed_length").is_none());
    assert!(
        body.get("dry_base").is_none(),
        "dry_base must be dropped when dry_multiplier is None"
    );
}

#[test]
fn apply_sampling_preserves_manual_dry_windows_and_omission() {
    for window in [None, Some(0), Some(512)] {
        let mut body = json!({});
        apply_sampling(&mut body, None, Some(1.0), None, None, window);
        assert_eq!(
            body.get("dry_penalty_last_n"),
            window.map(|n| json!(n)).as_ref(),
            "manual windows and omission must remain unchanged"
        );
    }
}

/// Model the managed server's strict HTTP range check: the legacy -1 client
/// alias must succeed through both real request builders, including SSE.
#[test]
fn dry_whole_context_alias_is_accepted_by_strict_http_server() {
    use std::io::{BufRead, BufReader, Read, Write};

    assert_eq!(LOCAL_CONTEXT_SIZE, 32768);
    for streaming in [false, true] {
        // Also exercise the stub's rejection path so it cannot silently accept
        // the negative wire value that caused the runtime failure.
        for window in [-2, -1] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let stub = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().expect("accept request");
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    let line = line.trim_end_matches(['\r', '\n']);
                    if line.is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = n.trim().parse::<usize>().unwrap();
                    }
                }
                let mut bytes = vec![0; content_length];
                reader.read_exact(&mut bytes).unwrap();
                let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                let valid_window = body["dry_penalty_last_n"]
                    .as_i64()
                    .is_some_and(|n| (0..=i64::from(i32::MAX)).contains(&n));
                let (status, content_type, response) = if !valid_window {
                    (
                        "400 Bad Request",
                        "application/json",
                        r#"{"error":{"message":"dry_penalty_last_n must be in range 0..2147483647"}}"#,
                    )
                } else if streaming {
                    (
                        "200 OK",
                        "text/event-stream",
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                    )
                } else {
                    (
                        "200 OK",
                        "application/json",
                        r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#,
                    )
                };
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .unwrap();
                body
            });

            let mut tokens = String::new();
            let result = if streaming {
                ocr_via_stream(
                    &base_url,
                    None,
                    None,
                    "p",
                    "data:image/png;base64,AAAA",
                    64,
                    0.0,
                    Some(1.3),
                    Some(1.0),
                    None,
                    Some(2),
                    Some(window),
                    &mut |token| {
                        tokens.push_str(token);
                        true
                    },
                    &|| false,
                )
            } else {
                ocr_via(
                    &base_url,
                    None,
                    None,
                    "p",
                    "data:image/png;base64,AAAA",
                    64,
                    0.0,
                    Some(1.3),
                    Some(1.0),
                    None,
                    Some(2),
                    Some(window),
                )
            };
            let body = stub.join().expect("strict HTTP stub");
            if window == -2 {
                let error = result.expect_err("stub must reject negative wire windows");
                assert!(error.to_string().contains("400"), "{error}");
                assert_eq!(body["dry_penalty_last_n"], json!(-2));
                continue;
            }
            let completion = result.expect("whole-context client alias must be accepted");
            assert_eq!(completion.text, "ok");
            assert!(!completion.truncated);
            if streaming {
                assert_eq!(tokens, "ok");
                assert_eq!(body["stream"], json!(true));
            }
            assert_eq!(body["dry_penalty_last_n"], json!(32768));
            assert_eq!(body["dry_allowed_length"], json!(2));
            assert_eq!(body["dry_multiplier"], json!(1.0f32));
            assert_eq!(body["repeat_penalty"], json!(1.3f32));
        }
    }
}

/// The local request builder sends DRY fields only when enabled. Remote
/// isolation is independently exercised through RemoteEndpoint above.
#[test]
fn ocr_image_sends_dry_fields_only_when_set() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    fn run_once(dry_multiplier: Option<f32>, dry_base: Option<f32>) -> serde_json::Value {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();
        let resp_body = json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let Ok(s) = listener.incoming().next().unwrap() else {
                return;
            };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut writer = s;
            let mut content_length = 0usize;
            let mut first = String::new();
            reader.read_line(&mut first).ok();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(http_response.as_bytes());
            let _ = tx.send(body);
        });

        ocr_via(
            &format!("http://127.0.0.1:{port}"),
            None,
            None,
            "p",
            "data:image/png;base64,AAAA",
            64,
            0.0,
            None,
            dry_multiplier,
            dry_base,
            None,
            None,
        )
        .expect("ocr");
        let body = rx.recv().expect("stub recorded body");
        serde_json::from_slice(&body).expect("body is json")
    }

    let with = run_once(Some(0.8), Some(2.0));
    assert_eq!(with["messages"][0]["content"][2]["text"], json!("p"));
    assert_eq!(with["dry_multiplier"], json!(0.8f32));
    assert_eq!(with["dry_allowed_length"], json!(4));
    assert_eq!(with["dry_base"], json!(2.0f32));

    let without = run_once(None, None);
    assert!(
        without.get("dry_multiplier").is_none() && without.get("dry_allowed_length").is_none(),
        "DRY fields must be absent when unset"
    );

    // Critical gating case, exercised end-to-end through the stub HTTP server:
    // dry_base supplied but dry_multiplier absent must not reach the wire.
    let base_only = run_once(None, Some(2.0));
    assert!(
        base_only.get("dry_base").is_none(),
        "dry_base must be absent when dry_multiplier is None"
    );
}

/// `temperature` must always be present on the wire (unlike the Option-gated
/// DRY/repeat-penalty knobs): `None` resolves to 0 (the historical hardcoded
/// default), and an explicit value is sent verbatim. Stub HTTP server captures
/// the request body for both transports, including the v2 page/image/instruction
/// order and the default output budget.
#[test]
fn ocr_image_resolves_temperature_default_and_override() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    fn run_once(temperature: Option<f32>, stream: bool) -> serde_json::Value {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();
        let resp_body = json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let Ok(s) = listener.incoming().next().unwrap() else {
                return;
            };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut writer = s;
            let mut content_length = 0usize;
            let mut first = String::new();
            reader.read_line(&mut first).ok();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(http_response.as_bytes());
            let _ = tx.send(body);
        });

        let ep = RemoteEndpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            model: None,
        };
        let opts = crate::OcrOptions::default();
        if stream {
            ep.ocr_image_stream(
                &opts.prompt,
                "data:image/png;base64,AAAA",
                opts.max_tokens,
                temperature,
                None,
                None,
                None,
                None,
                None,
                &mut |_| true,
                &|| false,
            )
            .expect("streaming ocr");
        } else {
            ep.ocr_image(
                &opts.prompt,
                "data:image/png;base64,AAAA",
                opts.max_tokens,
                temperature,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("ocr");
        }
        let body = rx.recv().expect("stub recorded body");
        serde_json::from_slice(&body).expect("body is json")
    }

    for stream in [false, true] {
        let default = run_once(None, stream);
        assert_eq!(default["temperature"], json!(0.0f32));
        assert_eq!(default["max_tokens"], json!(8000));
        assert_eq!(
            default["messages"],
            json!([{
                "role": "user",
                "content": [
                    { "type": "text", "text": "<PAGE>" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
                    { "type": "text", "text": "Multi page parsing." }
                ]
            }])
        );
        assert_eq!(
            default.get("stream").and_then(serde_json::Value::as_bool),
            stream.then_some(true)
        );

        let explicit = run_once(Some(0.7), stream);
        assert_eq!(explicit["temperature"], json!(0.7f32));
    }
}

/// `provider_error` extracts the message from the OpenAI/vLLM/SGLang error
/// object and returns None for a normal completion body. Pure, no network.
#[test]
fn provider_error_extracts_message() {
    assert_eq!(
        provider_error(&json!({"error": {"message": "boom", "code": 400}})),
        Some("boom".to_string())
    );
    assert_eq!(
        provider_error(&json!({"choices": [{"message": {"content": "ok"}}]})),
        None
    );
    assert_eq!(provider_error(&json!({})), None);
}

/// `finish_reason` extracts `choices[0].finish_reason` and returns None when
/// absent (e.g. a mid-stream delta chunk that hasn't finished yet). Pure, no
/// network. `OcrResult::truncated` is built from this returning `"length"`.
#[test]
fn finish_reason_extracts_value() {
    assert_eq!(
        finish_reason(&json!({"choices": [{"finish_reason": "length"}]})),
        Some("length".to_string())
    );
    assert_eq!(
        finish_reason(&json!({"choices": [{"finish_reason": "stop"}]})),
        Some("stop".to_string())
    );
    assert_eq!(
        finish_reason(&json!({"choices": [{"delta": {"content": "x"}}]})),
        None
    );
    assert_eq!(finish_reason(&json!({})), None);
}

/// A non-streaming completion whose `finish_reason` is `"length"` must be
/// flagged `truncated: true` (the model hit `max_tokens` without a natural
/// stop, the strongest available signal of a repetition loop); `"stop"` must
/// be `false`. Stub HTTP server, mirrors `remote_endpoint_sends_bearer_and_parses`.
#[test]
fn ocr_image_flags_truncated_from_finish_reason() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    fn run_once(finish_reason: &str) -> OcrResult {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();
        let resp_body = json!({
            "choices": [{ "message": { "content": "ok" }, "finish_reason": finish_reason }]
        })
        .to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let Ok(s) = listener.incoming().next().unwrap() else {
                return;
            };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut writer = s;
            let mut content_length = 0usize;
            let mut first = String::new();
            reader.read_line(&mut first).ok();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(http_response.as_bytes());
            let _ = tx.send(());
        });

        let ep = RemoteEndpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            model: None,
        };
        let out = ep
            .ocr_image(
                "p",
                "data:image/png;base64,AAAA",
                64,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("ocr");
        rx.recv().expect("stub served the request");
        out
    }

    assert!(
        run_once("length").truncated,
        "finish_reason length -> truncated"
    );
    assert!(
        !run_once("stop").truncated,
        "finish_reason stop -> not truncated"
    );
}

/// Same signal over the streaming path: `finish_reason: "length"` on the
/// terminal SSE chunk must set `OcrResult::truncated`. Mirrors
/// `sse_streaming_fires_on_token`'s stub server.
#[test]
fn sse_streaming_flags_truncated_from_finish_reason() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"loop\"}}]}\r\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\r\n",
        "data: [DONE]\r\n",
    );
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
        sse_body.len(),
        sse_body,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let http_resp_clone = http_response.clone();
    std::thread::spawn(move || {
        if let Ok(stream) = listener.accept() {
            let (sock, _) = stream;
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut writer = sock;
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if t.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = t.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = Read::read_exact(&mut reader, &mut body);
            let _ = writer.write_all(http_resp_clone.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let result = ocr_via_stream(
        &base_url,
        None,
        None,
        "test prompt",
        "data:image/png;base64,AAAA",
        64,
        0.0,
        None,
        None,
        None,
        None,
        None,
        &mut |_chunk: &str| true,
        &|| false,
    );

    let out = result.expect("ocr_via_stream failed");
    assert_eq!(out.text, "loop");
    assert!(
        out.truncated,
        "finish_reason length on the terminal chunk must set truncated"
    );
}
