use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use mlua::{Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::events::{spawn_task, Stop, TaskGuard};
use crate::engine::VmScheduler;
use crate::error::LehuaError;

fn header_pairs(t: &Table) -> mlua::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in t.pairs::<String, Value>() {
        let (k, v) = entry?;
        let value = match v {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            other => {
                return Err(LehuaError::msg(format!(
                    "header '{k}' must be a string or number, got {}",
                    other.type_name()
                ))
                .into())
            }
        };
        out.push((k, value));
    }
    Ok(out)
}

fn headers_to_table(lua: &Lua, headers: &HeaderMap) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (name, value) in headers {
        let key = name.as_str();
        let val = String::from_utf8_lossy(value.as_bytes()).into_owned();
        match t.get::<Option<String>>(key)? {
            Some(existing) => t.set(key, format!("{existing}, {val}"))?,
            None => t.set(key, val)?,
        }
    }
    Ok(t)
}

async fn do_request(lua: Lua, method: String, url: String, opts: Option<Table>) -> mlua::Result<Table> {
    let method = Method::from_bytes(method.to_uppercase().as_bytes())
        .map_err(|_| LehuaError::msg(format!("invalid HTTP method '{method}'")))?;

    let mut builder = Client::builder();
    let mut timeout: Option<f64> = None;
    if let Some(o) = &opts {
        if let Some(insecure) = o.get::<Option<bool>>("insecure")? {
            builder = builder.danger_accept_invalid_certs(insecure);
        }
        match o.get::<Value>("redirects")? {
            Value::Boolean(false) => {
                builder = builder.redirect(reqwest::redirect::Policy::none());
            }
            Value::Integer(n) => {
                builder = builder.redirect(reqwest::redirect::Policy::limited(n.max(0) as usize));
            }
            _ => {}
        }
        timeout = o.get::<Option<f64>>("timeout")?;
    }
    let client = builder.build().map_err(mlua::Error::external)?;

    let mut req = client.request(method, &url);

    if let Some(o) = &opts {
        if let Some(headers) = o.get::<Option<Table>>("headers")? {
            let mut map = HeaderMap::new();
            for (k, v) in header_pairs(&headers)? {
                let name = HeaderName::from_bytes(k.as_bytes())
                    .map_err(|_| LehuaError::msg(format!("invalid header name '{k}'")))?;
                let value = HeaderValue::from_str(&v)
                    .map_err(|_| LehuaError::msg(format!("invalid header value for '{k}'")))?;
                map.append(name, value);
            }
            req = req.headers(map);
        }
        if let Some(query) = o.get::<Option<Table>>("query")? {
            req = req.query(&header_pairs(&query)?);
        }
        if let Some(form) = o.get::<Option<Table>>("form")? {
            req = req.form(&header_pairs(&form)?);
        }
        match o.get::<Value>("json")? {
            Value::Nil => {}
            json => {
                let bytes = serde_json::to_vec(&json).map_err(mlua::Error::external)?;
                req = req
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(bytes);
            }
        }
        match o.get::<Value>("body")? {
            Value::String(s) => req = req.body(s.as_bytes().to_vec()),
            Value::Buffer(b) => req = req.body(b.to_vec()),
            _ => {}
        }
        if let Some(auth) = o.get::<Option<Table>>("auth")? {
            if let Some(bearer) = auth.get::<Option<String>>("bearer")? {
                req = req.bearer_auth(bearer);
            } else if let Some(user) = auth.get::<Option<String>>("username")? {
                req = req.basic_auth(user, auth.get::<Option<String>>("password")?);
            }
        }
    }
    if let Some(secs) = timeout {
        req = req.timeout(Duration::from_secs_f64(secs.max(0.0)));
    }

    let resp = req.send().await.map_err(mlua::Error::external)?;
    let status = resp.status();
    let final_url = resp.url().to_string();
    let headers = headers_to_table(&lua, resp.headers())?;
    let body = resp.bytes().await.map_err(mlua::Error::external)?;

    let out = lua.create_table()?;
    out.set("ok", status.is_success())?;
    out.set("status", status.as_u16())?;
    out.set(
        "statusText",
        status.canonical_reason().unwrap_or("").to_string(),
    )?;
    out.set("headers", headers)?;
    out.set("body", lua.create_string(&body)?)?;
    out.set("url", final_url)?;
    Ok(out)
}

struct HttpRequest {
    method: String,
    target: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    peer: String,
    responder: std::cell::RefCell<Option<oneshot::Sender<ResponseData>>>,
}

struct ResponseData {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    keep_alive: bool,
}

impl UserData for HttpRequest {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("method", |_, this, ()| Ok(this.method.clone()));
        m.add_method("path", |_, this, ()| Ok(this.path.clone()));
        m.add_method("target", |_, this, ()| Ok(this.target.clone()));
        m.add_method("query", |_, this, ()| Ok(this.query.clone()));
        m.add_method("peerAddress", |_, this, ()| Ok(this.peer.clone()));
        m.add_method("body", |lua, this, ()| lua.create_string(&this.body));
        m.add_method("headers", |lua, this, ()| {
            let t = lua.create_table()?;
            for (k, v) in &this.headers {
                t.set(k.to_ascii_lowercase(), v.clone())?;
            }
            Ok(t)
        });
        m.add_method("header", |_, this, name: String| {
            let name = name.to_ascii_lowercase();
            Ok(this
                .headers
                .iter()
                .find(|(k, _)| k.to_ascii_lowercase() == name)
                .map(|(_, v)| v.clone()))
        });

        m.add_method("respond", |_, this, res: Value| {
            let sender = this.responder.borrow_mut().take().ok_or_else(|| {
                LehuaError::msg("this request has already been answered")
            })?;
            let data = build_response(res)?;
            let _ = sender.send(data);
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("HttpRequest({} {})", this.method, this.path))
        });
    }
}

fn build_response(res: Value) -> mlua::Result<ResponseData> {
    match res {
        Value::String(s) => Ok(ResponseData {
            status: 200,
            headers: Vec::new(),
            body: s.as_bytes().to_vec(),
            keep_alive: true,
        }),
        Value::Table(t) => {
            let status = t.get::<Option<u16>>("status")?.unwrap_or(200);
            let mut headers = Vec::new();
            if let Some(h) = t.get::<Option<Table>>("headers")? {
                headers = header_pairs(&h)?;
            }
            let body = match t.get::<Value>("body")? {
                Value::String(s) => s.as_bytes().to_vec(),
                Value::Buffer(b) => b.to_vec(),
                Value::Nil => Vec::new(),
                other => {
                    return Err(LehuaError::msg(format!(
                        "response body must be a string or buffer, got {}",
                        other.type_name()
                    ))
                    .into())
                }
            };
            let body = if let Some(json) = opt_json(&t)? {
                if !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    headers.push(("Content-Type".into(), "application/json".into()));
                }
                json
            } else {
                body
            };
            Ok(ResponseData {
                status,
                headers,
                body,
                keep_alive: true,
            })
        }
        other => Err(LehuaError::msg(format!(
            "respond expects a string or a table, got {}",
            other.type_name()
        ))
        .into()),
    }
}

fn opt_json(t: &Table) -> mlua::Result<Option<Vec<u8>>> {
    match t.get::<Value>("json")? {
        Value::Nil => Ok(None),
        json => Ok(Some(serde_json::to_vec(&json).map_err(mlua::Error::external)?)),
    }
}

type IncomingChannel = async_channel::Receiver<HttpRequest>;

pub struct HttpServer {
    incoming: IncomingChannel,
    local: String,
    stop: Arc<Stop>,
    sched: Rc<VmScheduler>,
    req_bound: Cell<bool>,
}

fn error_response(status: u16, body: &str) -> ResponseData {
    ResponseData {
        status,
        headers: Vec::new(),
        body: body.as_bytes().to_vec(),
        keep_alive: false,
    }
}

fn dispatch_request(lua: &Lua, sched: &Rc<VmScheduler>, req: HttpRequest, cb: &Function) {
    let ud = match lua.create_userdata(req) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("lehua: net: http request handler error: {}", crate::error::pretty(&e));
            return;
        }
    };
    let thread = match lua.create_thread(cb.clone()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lehua: net: http request handler error: {}", crate::error::pretty(&e));
            return;
        }
    };
    let fut = match thread.into_async::<Value>(&ud) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("lehua: net: http request handler error: {}", crate::error::pretty(&e));
            return;
        }
    };
    let guard = TaskGuard::new(sched);
    tokio::task::spawn_local(async move {
        let _guard = guard;
        let result = crate::engine::catch_panics(fut).await;
        let pending = ud
            .borrow::<HttpRequest>()
            .ok()
            .and_then(|r| r.responder.borrow_mut().take());
        match result {
            Ok(Value::Nil) => {
                if let Some(sender) = pending {
                    let _ = sender.send(error_response(
                        500,
                        "internal error: the request handler did not respond",
                    ));
                }
            }
            Ok(v) => {
                if let Some(sender) = pending {
                    match build_response(v) {
                        Ok(data) => {
                            let _ = sender.send(data);
                        }
                        Err(e) => {
                            eprintln!("lehua: net: http request handler error: {}", crate::error::pretty(&e));
                            let _ = sender.send(error_response(500, "internal error"));
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("lehua: net: http request handler error: {}", crate::error::pretty(&e));
                if let Some(sender) = pending {
                    let _ = sender.send(error_response(500, "internal error"));
                }
            }
        }
    });
}

impl UserData for HttpServer {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("on", |lua, this, (event, cb): (String, Function)| {
            if event != "request" {
                return Err(LehuaError::msg(format!(
                    "unknown http server event '{event}' (expected \"request\")"
                ))
                .into());
            }
            if this.req_bound.get() {
                return Err(LehuaError::msg("this server already has a request receiver").into());
            }
            if this.stop.is_stopped() {
                return Err(LehuaError::msg("this server is closed").into());
            }
            this.req_bound.set(true);
            let lua = lua.clone();
            let sched = this.sched.clone();
            let incoming = this.incoming.clone();
            spawn_task(&this.sched, async move {
                while let Ok(req) = incoming.recv().await {
                    dispatch_request(&lua, &sched, req, &cb);
                }
            });
            Ok(())
        });

        m.add_async_method("accept", |lua, this, ()| {
            let incoming = this.incoming.clone();
            async move {
                match incoming.recv().await {
                    Ok(req) => Ok(Value::UserData(lua.create_userdata(req)?)),
                    Err(_) => Ok(Value::Nil),
                }
            }
        });

        m.add_method("address", |_, this, ()| Ok(this.local.clone()));

        m.add_method("close", |_, this, ()| {
            this.stop.stop();
            this.incoming.close();
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("HttpServer({})", this.local))
        });
    }
}

async fn read_headers(stream: &mut TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Ok(if buf.is_empty() { None } else { Some(buf) });
        }
        buf.push(byte[0]);
        if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
            return Ok(Some(buf));
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request header too large",
            ));
        }
    }
}

fn parse_head(raw: &[u8]) -> Option<(String, String, String, Vec<(String, String)>)> {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(colon) = line.find(':') {
            let name = line[..colon].trim().to_string();
            let value = line[colon + 1..].trim().to_string();
            headers.push((name, value));
        }
    }
    Some((method, target, version, headers))
}

async fn handle_connection(
    mut stream: TcpStream,
    peer: String,
    tx: async_channel::Sender<HttpRequest>,
    stop: Arc<Stop>,
) {
    loop {
        let head = tokio::select! {
            _ = stop.wait() => return,
            h = read_headers(&mut stream) => match h {
                Ok(Some(h)) => h,
                _ => return,
            },
        };
        let (method, target, version, headers) = match parse_head(&head) {
            Some(v) => v,
            None => return,
        };

        let content_length = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; content_length];
        if content_length > 0 && stream.read_exact(&mut body).await.is_err() {
            return;
        }

        let client_keep_alive = if version.contains("1.0") {
            headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("connection") && v.eq_ignore_ascii_case("keep-alive"))
        } else {
            !headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("connection") && v.eq_ignore_ascii_case("close"))
        };

        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };

        let (resp_tx, resp_rx) = oneshot::channel::<ResponseData>();
        let request = HttpRequest {
            method,
            target,
            path,
            query,
            headers,
            body,
            peer: peer.clone(),
            responder: std::cell::RefCell::new(Some(resp_tx)),
        };
        if tx.send(request).await.is_err() {
            return;
        }

        let resp = match resp_rx.await {
            Ok(r) => r,
            Err(_) => ResponseData {
                status: 500,
                headers: Vec::new(),
                body: b"internal error: request dropped without a response".to_vec(),
                keep_alive: false,
            },
        };

        let keep_alive = client_keep_alive && resp.keep_alive;
        if write_response(&mut stream, &resp, keep_alive).await.is_err() {
            return;
        }
        if !keep_alive {
            return;
        }
    }
}

async fn write_response(
    stream: &mut TcpStream,
    resp: &ResponseData,
    keep_alive: bool,
) -> std::io::Result<()> {
    let reason = reason_phrase(resp.status);
    let mut head = format!("HTTP/1.1 {} {}\r\n", resp.status, reason);
    let mut has_type = false;
    for (k, v) in &resp.headers {
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("connection") {
            continue;
        }
        if k.eq_ignore_ascii_case("content-type") {
            has_type = true;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_type {
        head.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
    head.push_str(if keep_alive {
        "Connection: keep-alive\r\n"
    } else {
        "Connection: close\r\n"
    });
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&resp.body).await?;
    stream.flush().await
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        418 => "I'm a teapot",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

pub fn install(lua: &Lua, net: &Table, sched: Rc<VmScheduler>) -> mlua::Result<()> {
    let http = lua.create_table()?;

    http.set(
        "request",
        lua.create_async_function(|lua, args: (Value, Option<Table>)| async move {
            let (first, opts) = args;
            let (method, url, opts) = match first {
                Value::String(s) => {
                    let url = s.to_str()?.to_string();
                    let opts = opts.clone();
                    let method = opts
                        .as_ref()
                        .and_then(|o| o.get::<Option<String>>("method").ok().flatten())
                        .unwrap_or_else(|| "GET".to_string());
                    (method, url, opts)
                }
                Value::Table(t) => {
                    let url = t
                        .get::<Option<String>>("url")?
                        .ok_or_else(|| LehuaError::msg("http.request: 'url' is required"))?;
                    let method = t.get::<Option<String>>("method")?.unwrap_or_else(|| "GET".to_string());
                    (method, url, Some(t))
                }
                other => {
                    return Err(LehuaError::msg(format!(
                        "http.request expects a url string or an options table, got {}",
                        other.type_name()
                    ))
                    .into())
                }
            };
            do_request(lua, method, url, opts).await
        })?,
    )?;

    for method in ["get", "post", "put", "patch", "delete", "head", "options"] {
        let m = method.to_uppercase();
        http.set(
            method,
            lua.create_async_function(move |lua, (url, opts): (String, Option<Table>)| {
                let m = m.clone();
                async move { do_request(lua, m, url, opts).await }
            })?,
        )?;
    }

    http.set(
        "serve",
        lua.create_async_function(move |lua, opts: Value| {
            let sched = sched.clone();
            async move {
            let (host, port) = match opts {
                Value::Integer(p) => ("0.0.0.0".to_string(), p as u16),
                Value::Table(t) => {
                    let host = t.get::<Option<String>>("host")?.unwrap_or_else(|| "0.0.0.0".to_string());
                    let port = t
                        .get::<Option<u16>>("port")?
                        .ok_or_else(|| LehuaError::msg("http.serve: 'port' is required"))?;
                    (host, port)
                }
                other => {
                    return Err(LehuaError::msg(format!(
                        "http.serve expects a port number or an options table, got {}",
                        other.type_name()
                    ))
                    .into())
                }
            };
            let listener = TcpListener::bind((host.as_str(), port))
                .await
                .map_err(mlua::Error::external)?;
            let local = listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default();
            let (tx, rx) = async_channel::bounded::<HttpRequest>(256);
            let stop = Stop::new();
            {
                let stop = stop.clone();
                tokio::spawn(async move {
                    loop {
                        let accepted = tokio::select! {
                            _ = stop.wait() => break,
                            r = listener.accept() => r,
                        };
                        match accepted {
                            Ok((stream, addr)) => {
                                let tx = tx.clone();
                                let peer = addr.to_string();
                                tokio::spawn(handle_connection(stream, peer, tx, stop.clone()));
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
            let server = HttpServer {
                incoming: rx,
                local,
                stop,
                sched,
                req_bound: Cell::new(false),
            };
            Ok(Value::UserData(lua.create_userdata(server)?))
            }
        })?,
    )?;

    net.set("http", http)?;
    Ok(())
}
