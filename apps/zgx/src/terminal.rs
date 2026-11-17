//! A session's terminals, for the panel's terminal tabs: a shell on the
//! session's device, in its folder, through the engine's terminal calls (the
//! shipping app's, crates/ui/src/terminal/panel.rs), drawn by zgx's own
//! terminal (`ui.term` / `ui.terminal`).
//!
//! zgx's terminal runs a process on a pseudoterminal of its own, so the
//! process it runs is this app's bridge (`zeron-zgx terminal-bridge …`):
//! it attaches to the engine's terminal and passes bytes both ways, its own
//! pseudoterminal in raw mode, and tells the engine its size as zgx sizes
//! it. The engine routes the calls to the session's device, as the file
//! calls are. zgx keeps the session (and so the bridge) across tab switches
//! and reloads; the engine's terminal outlives the bridge until it's closed.
//!
//! A new terminal is opened by its bridge, at the size zgx gave the bridge's
//! pseudoterminal: a shell started at another size draws its first prompt
//! for that width (zsh's end-of-line mark, a lone `%`, is left on the first
//! line of a narrower one). The bridge hands the engine's id back through a
//! claim file, which the app reads (`terminal.claimed`).
//!
//! The bridge sleeps until there's something to pass on: what's typed, what
//! the engine sent, or a new size (SIGWINCH). It once looked every 15 ms,
//! each terminal's bridge waking some 70 times a second while nothing
//! happened.
//!
//! The calls (`ui.call`, each a promise):
//!   terminal.start { target }                a new terminal's bridge:
//!                                            { command, claim }
//!   terminal.claimed { claim }               its engine terminal, once the
//!                                            bridge has opened it: the id
//!   terminal.command { id, device? }         the bridge's command line
//!   terminal.status { id, device? }          { busy, exited }: whether
//!                                            closing ends a program
//!   terminal.close { id, device? }           the engine's terminal ended
//!   terminal.write { id, device?, text }     typed into it, as if from the
//!                                            keyboard (an agent's sign-in
//!                                            command, run for you)
//! (`target` as src/files.rs's: { chat?, space?, device?, root? }).

use std::{
    io::Write,
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    time::Duration,
};
#[cfg(not(unix))]
use std::{io::Read, sync::mpsc};

use base64::Engine as _;
use serde_json::{Map, Value as Json, json};
use zgx_desktop::WindowContext;
use zgx_script::{ScriptHost, Value};

use crate::engine::{self, rpc};

/// The bridge commands the host itself made (`terminal.start`,
/// `terminal.command`): a terminal Zeron started shows while a template is a
/// preview, which can't start one (preview.rs `guard`).
static ISSUED: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

pub(crate) fn note_issued(line: &str) {
    ISSUED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(line.to_string());
}

pub(crate) fn issued(line: &str) -> bool {
    ISSUED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).contains(line)
}

pub fn install(cx: &mut WindowContext, host: &ScriptHost) {
    engine::install(cx);
    host.async_action("terminal.start", |arg| async move { crate::plugins::preview::refuse("terminal.start")?; start(arg).await });
    host.async_action("terminal.claimed", |arg| async move { claimed(arg).await });
    host.async_action("terminal.command", |arg| async move { crate::plugins::preview::refuse("terminal.command")?;
        let device = crate::files::route_to(opt_text(arg.field("device"))).await;
        command(arg, device)
    });
    host.async_action("terminal.status", |arg| async move {
        let reply = call_for("TerminalStatus", &arg, Map::new()).await?;
        Ok(Value::record([
            ("busy", Value::Bool(reply["busy"].as_bool().unwrap_or(false))),
            ("exited", Value::Bool(reply["exited"].as_bool().unwrap_or(false))),
        ]))
    });
    host.async_action("terminal.close", |arg| async move {
        call_for("CloseTerminal", &arg, Map::new()).await?;
        // (Linux fork) The shell its bridge opened in its place, closed too.
        forget_moved(&text(arg.field("id")));
        Ok(Value::Nil)
    });
    host.async_action("terminal.write", |arg| async move { crate::plugins::preview::refuse("terminal.write")?;
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(text(arg.field("text")));
        let mut params = Map::new();
        params.insert("data".into(), Json::String(data));
        call_for("WriteTerminal", &arg, params).await?;
        Ok(Value::Nil)
    });
}

fn text(value: Option<&Value>) -> String {
    value.map(crate::auth::text).unwrap_or_default()
}

fn opt_text(value: Option<&Value>) -> Option<String> {
    let t = text(value);
    (!t.is_empty()).then_some(t)
}

/// A word for the screen from an engine error: "offline" when it couldn't
/// be reached, what to do when it's from before terminals, else what it
/// said.
fn said(error: String) -> String {
    if engine::outcome(&error) == "offline" {
        "offline".into()
    } else if error.to_ascii_lowercase().contains("unknown method") {
        "Update Zeron to use terminals here.".into()
    } else {
        error
    }
}

/// A call about terminal `arg.id`, routed to `arg.device`.
async fn call_for(method: &str, arg: &Value, mut params: Map<String, Json>) -> Result<Json, String> {
    // (Linux fork) The shell running in the tab now: the one its bridge
    // opened in place of the one the app was given, if it has.
    params.insert("terminalId".into(), Json::String(current(text(arg.field("id")))));
    if let Some(device) = crate::files::route_to(opt_text(arg.field("device"))).await {
        params.insert("targetDeviceId".into(), Json::String(device));
    }
    engine::get().call(method, Json::Object(params)).await.map_err(said)
}

/// OpenTerminal's parameters for `target`, but its size.
async fn open_params(target: &Value) -> Result<Map<String, Json>, String> {
    let mut params = Map::new();
    // A session's terminal: its chat. A project with none open: its canvas,
    // in its folder (the engine's `space-canvas:{id}`).
    if let Some(chat) = opt_text(target.field("chat")) {
        params.insert("chatId".into(), Json::String(chat));
    } else if let Some(space) = opt_text(target.field("space")) {
        params.insert("chatId".into(), Json::String(format!("space-canvas:{space}")));
        if let Some(root) = opt_text(target.field("root")) {
            params.insert("cwd".into(), Json::String(root));
        }
    } else {
        return Err("no session or project".into());
    }
    if let Some(device) = crate::files::route_to(opt_text(target.field("device"))).await {
        params.insert("targetDeviceId".into(), Json::String(device));
    }
    Ok(params)
}

/// Where new terminals' bridges leave their ids.
fn claims_dir() -> PathBuf {
    std::env::temp_dir().join("zeron-zgx-terminals")
}

/// A new terminal's bridge line: it opens the engine's terminal itself, at
/// its own size, and leaves the id in `claim`.
async fn start(arg: Value) -> Result<Value, String> {
    let target = arg.field("target").ok_or("no target")?;
    let params = open_params(target).await?;
    let (port, data) = engine::get().ipc_endpoint().ok_or("Zeron isn't available here")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = claims_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let claim = dir.join(format!("{}-{}", std::process::id(), operation_nonce()));
    // (In base64: no quote inside it for a shell to read as its own, `runnable`.)
    let open = base64::engine::general_purpose::STANDARD.encode(Json::Object(params).to_string());
    let line = runnable(format!(
        "{} terminal-bridge --port {port} --data {} --open {} --claim {}",
        quote(&exe.to_string_lossy()),
        quote(&data.to_string_lossy()),
        quote(&open),
        quote(&claim.to_string_lossy()),
    ));
    note_issued(&line);
    Ok(Value::record([("command", Value::from(line.as_str())), ("claim", Value::from(claim.to_string_lossy().as_ref()))]))
}

/// A number unique to this run.
fn operation_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64);
    now ^ (NEXT.fetch_add(1, Ordering::Relaxed) << 48)
}

/// The engine terminal a bridge opened, once it has (15 s at most). The
/// claim is the app's own file in its own folder, read once.
async fn claimed(arg: Value) -> Result<Value, String> {
    let claim = PathBuf::from(text(arg.field("claim")));
    if claim.parent() != Some(claims_dir().as_path()) {
        return Err("not a terminal's claim".into());
    }
    // (Watched for off this thread, the task woken by what's found.)
    let (tx, mut rx) = zgx::task::latest::<Result<String, String>>();
    std::thread::spawn(move || {
        tx.send(await_claim(&claim, CLAIM_WAIT));
    });
    let id = rx.recv().await.unwrap_or_else(|| Err("The terminal didn't open.".into()))?;
    // Opened while a template ran: closed if safe mode starts (preview.rs).
    crate::plugins::preview::terminal_opened(&id, None);
    Ok(Value::from(id.as_str()))
}

/// How long a bridge has to open its terminal: it says why itself when it
/// can't, so only a bridge gone without a word takes this long.
const CLAIM_WAIT: Duration = Duration::from_secs(120);

/// What a claim holds: the bridge's terminal id, or why it couldn't open
/// one. A bridge that hasn't claimed within `wait` is told the app has gone
/// (the claim taken first, `GONE`, which its own can't then replace): it
/// closes what it opens. Either side takes the claim with an operation that
/// fails if the other has, so a terminal is never left with no one's.
fn await_claim(claim: &std::path::Path, wait: Duration) -> Result<String, String> {
    let until = std::time::Instant::now() + wait;
    loop {
        if let Ok(text) = std::fs::read_to_string(claim) {
            let _ = std::fs::remove_file(claim);
            let text = text.trim();
            if let Some(why) = text.strip_prefix("error: ") {
                return Err(why.to_owned());
            }
            if !text.is_empty() {
                return Ok(text.to_owned());
            }
        }
        if std::time::Instant::now() >= until {
            use std::io::Write as _;
            match std::fs::OpenOptions::new().write(true).create_new(true).open(claim) {
                // Taken first: a bridge that claims later closes its own.
                Ok(mut file) => {
                    let _ = file.write_all(GONE.as_bytes());
                    return Err("The terminal didn't open.".into());
                }
                // Claimed in the meantime: read on the next turn.
                Err(_) => continue,
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What an app that stopped waiting leaves in a claim.
const GONE: &str = "gone";

/// A word of the bridge's command line, quoted for how zgx's terminal runs
/// it: through `/bin/sh -c` on Unix; on Windows the whole line is
/// CreateProcess's command line (zgx-terminal's `shell()`, no shell between),
/// so its words are read back by the C runtime's rules (CommandLineToArgvW,
/// Rust's own `args`). A POSIX `'…'` word there names a program whose name
/// starts with a quote, and the terminal couldn't start ("cannot find the
/// file specified").
fn quote(word: &str) -> String {
    if cfg!(windows) { quote_windows(word) } else { quote_posix(word) }
}

/// A word for `/bin/sh -c`.
fn quote_posix(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// A word for a Windows command line: in double quotes, a quote escaped as
/// `\"`, and the backslashes before a quote (or the closing one) doubled, so
/// a folder's trailing `\` doesn't escape the closing quote.
fn quote_windows(word: &str) -> String {
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for c in word.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        let escapes = if c == '"' { backslashes * 2 + 1 } else { backslashes };
        quoted.extend(std::iter::repeat_n('\\', escapes));
        backslashes = 0;
        quoted.push(c);
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// The bridge's command line for terminal `arg.id` (on `device`, another
/// device's, routed there).
pub(crate) fn command(arg: Value, device: Option<String>) -> Result<Value, String> {
    let (port, data) = engine::get().ipc_endpoint().ok_or("Zeron isn't available here")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let id = text(arg.field("id"));
    if id.is_empty() {
        return Err("no terminal".into());
    }
    let mut line = format!(
        "{} terminal-bridge --port {port} --data {} --terminal {}",
        quote(&exe.to_string_lossy()),
        quote(&data.to_string_lossy()),
        quote(&id),
    );
    if let Some(device) = device {
        line.push_str(&format!(" --device {}", quote(&device)));
    }
    let line = runnable(line);
    note_issued(&line);
    Ok(Value::from(line.as_str()))
}

/// The line zgx's terminal runs for the bridge's `line`. The Windows beta is
/// a GUI program (main.rs `windows_subsystem`), and Windows attaches none to
/// the pseudoconsole it's started on: the bridge would have no console, its
/// shell opened and nothing shown. So cmd.exe, a console program, is started
/// on it, waits for the bridge, and the bridge attaches to its console
/// (`attach_console`). Each word is in quotes with none inside (the open
/// parameters are base64), so cmd takes nothing in them for its own syntax.
#[cfg(all(windows, feature = "beta"))]
fn runnable(line: String) -> String {
    let cmd = std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    format!("{} /d /s /c \"{line}\"", quote(&cmd))
}
#[cfg(not(all(windows, feature = "beta")))]
fn runnable(line: String) -> String {
    line
}

/// The console of the cmd.exe that started this bridge (`runnable`), when
/// it has none of its own (a GUI program's).
#[cfg(all(windows, feature = "beta"))]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE};
    // SAFETY: the process's own standard handle, and a mode for it to fill.
    unsafe {
        let mut mode = 0;
        if GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) == 0 {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

// ---------------------------------------------------------------------------
// The bridge: its own process, run by zgx's terminal.

struct Args {
    port: u16,
    data: PathBuf,
    /// The engine terminal to attach to; or, for a new one, what to open
    /// (OpenTerminal's parameters, but its size) and where to leave its id.
    terminal: Option<String>,
    open: Option<Map<String, Json>>,
    claim: Option<PathBuf>,
    device: Option<String>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut port = None;
    let mut data = None;
    let mut terminal = None;
    let mut open = None;
    let mut claim = None;
    let mut device = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--port" => port = value.parse().ok(),
            "--data" => data = Some(PathBuf::from(value)),
            "--terminal" => terminal = Some(value.clone()),
            // (As JSON, or JSON in base64: `start`'s.)
            "--open" => {
                let decoded = base64::engine::general_purpose::STANDARD.decode(value).ok().and_then(|bytes| String::from_utf8(bytes).ok());
                match serde_json::from_str::<Json>(decoded.as_deref().unwrap_or(value)) {
                    Ok(Json::Object(params)) => open = Some(params),
                    _ => return Err("--open takes OpenTerminal's parameters".into()),
                }
            }
            "--claim" => claim = Some(PathBuf::from(value)),
            "--device" => device = Some(value.clone()),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if terminal.is_none() && (open.is_none() || claim.is_none()) {
        return Err("--terminal, or --open with --claim, is needed".into());
    }
    Ok(Args {
        port: port.ok_or("--port is needed")?,
        data: data.ok_or("--data is needed")?,
        terminal,
        open,
        claim,
        device,
    })
}

type Socket = tungstenite::WebSocket<TcpStream>;

fn dial(port: u16, token: Option<&str>) -> Result<Socket, String> {
    use tungstenite::client::IntoClientRequest;
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);
    let mut request = format!("ws://127.0.0.1:{port}").into_client_request().map_err(|e| e.to_string())?;
    if let Some(token) = token {
        let value = tungstenite::http::HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| e.to_string())?;
        request.headers_mut().insert(tungstenite::http::header::AUTHORIZATION, value);
    }
    let (socket, _) = tungstenite::client::client(request, stream).map_err(|e| e.to_string())?;
    // (Opening a terminal waits on its answer with short reads; passing
    // bytes, the socket doesn't block at all: see `bridge`.)
    let _ = socket.get_ref().set_read_timeout(Some(Duration::from_millis(15)));
    Ok(socket)
}

/// The terminal's size as zgx gave this process's pseudoterminal.
#[cfg(unix)]
fn window_size() -> Option<(u16, u16)> {
    // SAFETY: TIOCGWINSZ fills the zeroed `winsize` for fd 0, its own terminal.
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        (libc::ioctl(0, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0).then_some((size.ws_col, size.ws_row))
    }
}
/// The visible window of the console this bridge writes to: the
/// pseudoconsole zgx's terminal sized (cmd.exe's, `attach_console`), which
/// follows the tab as zgx resizes it. Windows says nothing as it changes:
/// the bridge looks on each of its turns (`bridge`), as cheap as a call.
#[cfg(windows)]
fn window_size() -> Option<(u16, u16)> {
    use windows_sys::Win32::System::Console::{CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: the process's own standard handle, and a struct for it to fill.
    let info = unsafe {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        (GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) != 0).then_some(info)
    }?;
    console_window_size(info.srWindow.Left, info.srWindow.Top, info.srWindow.Right, info.srWindow.Bottom)
}

/// Columns and rows of a console window's inclusive rectangle, if it has any.
#[cfg(any(windows, test))]
fn console_window_size(left: i16, top: i16, right: i16, bottom: i16) -> Option<(u16, u16)> {
    let cols = i32::from(right) - i32::from(left) + 1;
    let rows = i32::from(bottom) - i32::from(top) + 1;
    (cols > 0 && rows > 0).then(|| (cols.min(i32::from(u16::MAX)) as u16, rows.min(i32::from(u16::MAX)) as u16))
}

#[cfg(not(any(unix, windows)))]
fn window_size() -> Option<(u16, u16)> {
    None
}

/// Its size once zgx has given it one and left it (two reads 40 ms apart
/// agree), half a second at most: a shell started then draws for it.
fn settled_size() -> (u16, u16) {
    let mut last = window_size();
    for _ in 0..12 {
        std::thread::sleep(Duration::from_millis(40));
        let now = window_size();
        if now.is_some() && now == last {
            break;
        }
        last = now;
    }
    last.unwrap_or((80, 24))
}

/// Writes `text` to `claim` whole (a file made aside, then linked into
/// place, which fails if the claim is there already). False when the app
/// stopped waiting first (its `GONE` there): no one will take what's
/// claimed, and the claim is cleared.
fn leave_claim(claim: &std::path::Path, text: &str) -> bool {
    let aside = claim.with_extension("part");
    if std::fs::write(&aside, text).is_err() {
        return true;
    }
    let linked = std::fs::hard_link(&aside, claim).is_ok();
    let _ = std::fs::remove_file(&aside);
    if !linked {
        let _ = std::fs::remove_file(claim);
    }
    linked
}

/// Opens the engine's terminal at `size` (what `open` says, and the size);
/// its id and whether it runs in a sandbox container (Linux fork), or what
/// went wrong.
fn open_terminal(socket: &mut Socket, open: &Map<String, Json>, size: (u16, u16)) -> Result<(String, bool), String> {
    const OPEN_ID: u64 = 1_000_000;
    let mut params = open.clone();
    params.insert("cols".into(), json!(size.0));
    params.insert("rows".into(), json!(size.1));
    // (Routed as `open` says: its targetDeviceId is in it already.)
    send(socket, OPEN_ID, "OpenTerminal", params, &None).map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        match socket.read() {
            Ok(tungstenite::Message::Text(frame)) => {
                for line in frame.as_str().lines().filter(|l| !l.trim().is_empty()) {
                    let Ok(frame) = serde_json::from_str::<Json>(line) else {
                        continue;
                    };
                    if frame["id"].as_u64() != Some(OPEN_ID) {
                        continue;
                    }
                    if let Some(error) = frame["err"].as_str() {
                        return Err(said(error.to_owned()));
                    }
                    let sandboxed = frame["ok"]["sandboxed"].as_bool().unwrap_or(false);
                    return frame["ok"]["id"].as_str().map(|id| (id.to_owned(), sandboxed)).ok_or_else(|| "no terminal".into());
                }
            }
            Ok(tungstenite::Message::Close(_)) => return Err("offline".into()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("The terminal didn't open in time.".into())
}

/// Its pseudoterminal in raw mode (what's typed goes on as it is, echoed by
/// the shell at the other end), put back as the bridge ends.
#[cfg(unix)]
struct Raw(Option<libc::termios>);
#[cfg(unix)]
impl Raw {
    fn new() -> Self {
        // SAFETY: tcgetattr/tcsetattr on fd 0 with a termios read from it.
        unsafe {
            let mut was: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut was) != 0 {
                return Raw(None);
            }
            let mut raw = was;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(0, libc::TCSANOW, &raw);
            Raw(Some(was))
        }
    }
}
#[cfg(unix)]
impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(was) = self.0 {
            // SAFETY: restores what tcgetattr gave for fd 0.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &was);
            }
        }
    }
}

/// The console's modes and code pages as found, put back as the bridge ends.
#[cfg(windows)]
struct Raw(Option<RawWas>);
#[cfg(windows)]
struct RawWas {
    input: u32,
    output: u32,
    input_cp: u32,
    output_cp: u32,
}
#[cfg(windows)]
impl Raw {
    /// Its console in raw mode, as `cfmakeraw` puts a Unix one: a cooked
    /// console echoed each line before the shell at the other end did (every
    /// line shown twice), ended it with a carriage return and a line feed
    /// (PowerShell read the second as a continued line, `>>`), and took
    /// Ctrl+C as a signal to every process on it, cmd.exe and the bridge
    /// included, leaving the tab with nothing behind it. Raw, what's typed goes
    /// on as it is (Ctrl+C as its byte, which the shell's own console turns
    /// into its interrupt), in the terminal's sequences; what the shell wrote
    /// comes out as it is, in UTF-8.
    fn new() -> Self {
        use windows_sys::Win32::System::Console::{
            DISABLE_NEWLINE_AUTO_RETURN, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetStdHandle,
            STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
        };
        // SAFETY: the process's own standard handles, modes read from them
        // and changed; nothing if either isn't a console.
        unsafe {
            let (stdin, stdout) = (GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE));
            let (mut input, mut output) = (0, 0);
            if GetConsoleMode(stdin, &mut input) == 0 || GetConsoleMode(stdout, &mut output) == 0 {
                return Raw(None);
            }
            let was = RawWas { input, output, input_cp: GetConsoleCP(), output_cp: GetConsoleOutputCP() };
            SetConsoleMode(stdin, raw_input_mode(input));
            SetConsoleMode(stdout, output | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN);
            const UTF8: u32 = 65001;
            SetConsoleCP(UTF8);
            SetConsoleOutputCP(UTF8);
            Raw(Some(was))
        }
    }
}
#[cfg(windows)]
impl Drop for Raw {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP};
        if let Some(was) = &self.0 {
            // SAFETY: restores what `new` read from the same handles.
            unsafe {
                SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), was.input);
                SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), was.output);
                SetConsoleCP(was.input_cp);
                SetConsoleOutputCP(was.output_cp);
            }
        }
    }
}

/// A console input mode made raw: no echo, no lines, no Ctrl+C signal;
/// keys as the terminal's sequences. (Its other flags are left as found.)
#[cfg(any(windows, test))]
fn raw_input_mode(mode: u32) -> u32 {
    // (windows-sys's values, here so the rule is tested everywhere.)
    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    (mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT)) | ENABLE_VIRTUAL_TERMINAL_INPUT
}

/// Sends a call. On a socket that doesn't block, what doesn't go at once
/// waits in tungstenite's buffer (`rpc::would_block`) for the socket to
/// have room.
fn send(socket: &mut Socket, id: u64, method: &str, mut params: Map<String, Json>, device: &Option<String>) -> tungstenite::Result<()> {
    if let Some(device) = device {
        params.insert("targetDeviceId".into(), Json::String(device.clone()));
    }
    let frame = json!({ "id": id, "method": method, "params": Json::Object(params) });
    socket.send(tungstenite::Message::text(frame.to_string()))
}

/// Whether a send failed for good (not merely waiting for room).
fn failed(sent: tungstenite::Result<()>) -> bool {
    sent.is_err_and(|e| !rpc::would_block(&e))
}

/// The write end of the pipe SIGWINCH is told down (-1: none).
#[cfg(unix)]
static WINCH: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

#[cfg(unix)]
extern "C" fn on_winch(_: libc::c_int) {
    let fd = WINCH.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write is async-signal-safe; one byte from a static buffer
        // to the pipe's non-blocking write end, which lives as long as the
        // process.
        unsafe {
            libc::write(fd, [1u8].as_ptr().cast(), 1);
        }
    }
}

/// A pipe a byte goes down each time zgx resizes this pseudoterminal
/// (SIGWINCH), for the bridge to wait on: its read end.
#[cfg(unix)]
fn resizes() -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::IntoRawFd;
    let (heard, told) = rpc::pipe().ok()?;
    // Kept for the process's life: the handler may write to it any time.
    WINCH.store(told.into_raw_fd(), std::sync::atomic::Ordering::Relaxed);
    // SAFETY: installs a handler that only writes a byte to a pipe.
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
    }
    Some(heard)
}

/// Runs the bridge; returns the process's exit code.
pub fn bridge(args: &[String]) -> i32 {
    #[cfg(all(windows, feature = "beta"))]
    attach_console();
    let args = match parse(args) {
        Ok(args) => args,
        Err(why) => {
            eprintln!("terminal-bridge: {why}");
            return 2;
        }
    };
    let token = rpc::read_token(&args.data);
    let mut socket = match dial(args.port, token.as_deref()) {
        Ok(socket) => socket,
        Err(why) => {
            print!("\r\nZeron can't be reached right now ({why}).\r\n");
            let _ = std::io::stdout().flush();
            return 1;
        }
    };
    // (A device given is another's: the app only passes it then.)
    let mut device = args.device.clone();
    #[cfg(any(unix, windows))]
    let _raw = Raw::new();
    let mut size = None;
    // A new one: opened here, at the size zgx gave this pseudoterminal, so
    // the shell's first prompt is drawn for it; its id left for the app.
    let (id, sandboxed) = match (&args.terminal, &args.open, &args.claim) {
        (Some(id), _, _) => (id.clone(), false),
        (None, Some(open), Some(claim)) => {
            let now = settled_size();
            match open_terminal(&mut socket, open, now) {
                Ok((id, sandboxed)) => {
                    device = open.get("targetDeviceId").and_then(Json::as_str).map(str::to_owned);
                    if !leave_claim(claim, &id) {
                        // (The app stopped waiting: no tab knows this one, so
                        // it ends here rather than run on with no one's.)
                        let mut params = Map::new();
                        params.insert("terminalId".into(), Json::String(id));
                        let _ = send(&mut socket, 1, "CloseTerminal", params, &device);
                        print!("\r\nThe terminal took too long to open. Open another.\r\n");
                        let _ = std::io::stdout().flush();
                        return 1;
                    }
                    size = Some(now);
                    (id, sandboxed)
                }
                Err(why) => {
                    leave_claim(claim, &format!("error: {why}"));
                    let shown = if why == "offline" { "Zeron can't reach this session's device right now.".to_owned() } else { why };
                    print!("\r\n{shown}\r\n");
                    let _ = std::io::stdout().flush();
                    return 1;
                }
            }
        }
        _ => return 2,
    };
    // (Linux fork) A shell it opened itself: what it was opened with, so
    // another opens in its place as it ends (`next_shell`).
    let shell = match (&args.terminal, &args.open) {
        (None, Some(open)) if cfg!(unix) => Some(Shell { open: open.clone(), first: id.clone(), sandboxed }),
        _ => None,
    };
    let mut terminal = Json::String(id);
    // (Linux fork) Its stream's call (another for each shell opened in its
    // place), when it was attached, the last output it passed on, and when
    // something was last typed.
    let mut stream = 1u64;
    let mut attached = std::time::Instant::now();
    let mut seq = None;
    let mut typed_at: Option<std::time::Instant> = None;
    if failed(subscribe(&mut socket, stream, &terminal, None, &device)) {
        return 1;
    }
    let mut next_id = 2u64;
    // What's typed: read as the pseudoterminal has it (unix), else on a
    // thread of its own.
    #[cfg(unix)]
    let Some(resized) = resizes() else { return 1 };
    #[cfg(unix)]
    let _ = socket.get_ref().set_nonblocking(true);
    #[cfg(not(unix))]
    let rx = {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        rx
    };
    let mut stdout = std::io::stdout();
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut typed: Vec<Vec<u8>> = Vec::new();
    let mut look_at_size = true;
    loop {
        // Its size, told as it changes (zgx resizes its pseudoterminal, and
        // says so with SIGWINCH; on Windows, looked at each turn).
        let now = if look_at_size { window_size() } else { size };
        look_at_size = cfg!(not(unix));
        if now.is_some() && now != size {
            size = now;
            let (cols, rows) = now.unwrap_or((80, 24));
            let mut p = Map::new();
            p.insert("terminalId".into(), terminal.clone());
            p.insert("cols".into(), json!(cols));
            p.insert("rows".into(), json!(rows));
            if failed(send(&mut socket, next_id, "ResizeTerminal", p, &device)) {
                return 1;
            }
            next_id += 1;
        }
        // What's typed, on.
        #[cfg(not(unix))]
        loop {
            match rx.try_recv() {
                Ok(bytes) => {
                    typed.push(bytes);
                    typed_at = Some(std::time::Instant::now());
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // Its terminal closed (the tab): done.
                Err(mpsc::TryRecvError::Disconnected) => return 0,
            }
        }
        for bytes in typed.drain(..) {
            let mut p = Map::new();
            p.insert("terminalId".into(), terminal.clone());
            p.insert("data".into(), Json::String(b64.encode(bytes)));
            if failed(send(&mut socket, next_id, "WriteTerminal", p, &device)) {
                return 1;
            }
            next_id += 1;
        }
        // What didn't go yet waits for the socket to have room.
        let unsent = match socket.flush() {
            Ok(()) => false,
            Err(e) if rpc::would_block(&e) => true,
            Err(_) => return 1,
        };
        // What the shell wrote, out: all there is.
        let mut ended = None;
        'read: loop {
            match socket.read() {
                Ok(tungstenite::Message::Text(frame)) => {
                    for line in frame.as_str().lines().filter(|l| !l.trim().is_empty()) {
                        let Ok(frame) = serde_json::from_str::<Json>(line) else {
                            continue;
                        };
                        if frame["id"].as_u64() != Some(stream) {
                            continue;
                        }
                        if let Some(error) = frame["err"].as_str() {
                            ended = Some(End::Gone(error.to_owned()));
                            break 'read;
                        }
                        if frame.get("done").is_some() {
                            ended = Some(End::Closed);
                            break 'read;
                        }
                        let item = &frame["item"];
                        match item["type"].as_str() {
                            Some("data") => {
                                seq = item["seq"].as_u64().or(seq);
                                if let Some(bytes) = item["data"].as_str().and_then(|d| b64.decode(d).ok()) {
                                    let _ = stdout.write_all(&bytes);
                                    let _ = stdout.flush();
                                }
                            }
                            Some("exit") => {
                                ended = Some(End::Exit(item["exitCode"].as_i64().unwrap_or(0) as i32));
                                break 'read;
                            }
                            _ => {}
                        }
                    }
                }
                Ok(tungstenite::Message::Close(_)) => {
                    ended = Some(End::Lost);
                    break;
                }
                Ok(_) => {}
                Err(e) if rpc::would_block(&e) => break,
                Err(_) => {
                    ended = Some(End::Lost);
                    break;
                }
            }
        }
        // (Linux fork) Its shell ended, or its engine went: another shell in
        // its place, if it opened this one itself; else it ends as it did.
        if let Some(end) = ended {
            let Some(shell) = &shell else { return end.leave() };
            let idle = typed_at.map(|at| at.elapsed());
            match next_shell(&mut socket, &args, shell, &end, attached.elapsed(), idle) {
                Next::Done(code) => return code,
                // (Its connection back: the same shell, from where it was, if
                // its engine still has it; else it's Gone, and asked again.)
                Next::Same => {}
                Next::Shell(id, now) => {
                    if matches!(end, End::Exit(_)) {
                        let mut p = Map::new();
                        p.insert("terminalId".into(), terminal.clone());
                        let _ = send(&mut socket, next_id, "CloseTerminal", p, &device);
                        next_id += 1;
                    }
                    leave_moved(&shell.first, &id);
                    terminal = Json::String(id);
                    attached = std::time::Instant::now();
                    typed_at = None;
                    seq = None;
                    size = Some(now);
                }
            }
            stream = next_id;
            next_id += 1;
            if failed(subscribe(&mut socket, stream, &terminal, seq, &device)) {
                return 1;
            }
            #[cfg(unix)]
            let _ = socket.get_ref().set_nonblocking(true);
            continue;
        }
        // Asleep until one of them has something.
        #[cfg(unix)]
        {
            let out = if unsent { libc::POLLOUT } else { 0 };
            let stdin = std::io::stdin();
            let mut fds = [
                rpc::poll_fd(socket.get_ref(), libc::POLLIN | out),
                rpc::poll_fd(&stdin, libc::POLLIN),
                rpc::poll_fd(&resized, libc::POLLIN),
            ];
            rpc::wait(&mut fds);
            if fds[2].revents != 0 {
                rpc::drain(&resized);
                look_at_size = true;
            }
            if fds[1].revents != 0 {
                let mut buf = [0u8; 4096];
                // SAFETY: reads into a buffer of its own length from fd 0,
                // which poll says has something (or has closed).
                let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
                if n > 0 {
                    typed.push(buf[..n as usize].to_vec());
                    typed_at = Some(std::time::Instant::now());
                } else if n == 0 || !matches!(std::io::Error::last_os_error().kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) {
                    // Its terminal closed (the tab): done.
                    return 0;
                }
            }
        }
        #[cfg(not(unix))]
        let _ = unsent;
    }
}

/// Asks for terminal `terminal`'s output on call `id`: what came after
/// `after` (what it passed on before its connection went), else all the
/// engine keeps.
fn subscribe(socket: &mut Socket, id: u64, terminal: &Json, after: Option<u64>, device: &Option<String>) -> tungstenite::Result<()> {
    let mut params = Map::new();
    params.insert("terminalId".into(), terminal.clone());
    // (zgx's terminal answers queries itself: the host mustn't too.)
    params.insert("hostReplies".into(), Json::Bool(false));
    if let Some(after) = after {
        params.insert("afterSeq".into(), json!(after));
    }
    send(socket, id, "SubscribeTerminal", params, device)
}

// ---------------------------------------------------------------------------
// (Linux fork) A shell the bridge opened is opened again in its tab as it
// ends, below what it showed (the desktop app's terminal/panel.rs
// `restart_tab`): by itself when the sandbox container it ran in stopped
// under it (a Rebuild, the container stopped or restarted, the engine
// restarted), else when Enter is pressed. The app keeps the id it claimed;
// the bridge leaves the one running now beside the claims (`moved-<id>`),
// and the app's calls about its id reach that one (`current`).

/// What a shell this bridge opened was opened with.
struct Shell {
    open: Map<String, Json>,
    /// The id the app claimed.
    first: String,
    sandboxed: bool,
}

/// How a terminal's stream ended.
enum End {
    /// Its shell ended, with this status.
    Exit(i32),
    /// The engine hasn't got it (it restarted), in its words.
    Gone(String),
    /// The connection to the engine ended.
    Lost,
    /// Its stream ended with no exit: it was closed, or its engine is
    /// shutting down (which ends every terminal's stream first).
    Closed,
}

impl End {
    /// The bridge's status as it ends on it, opening nothing in its place.
    fn leave(self) -> i32 {
        let said = match self {
            End::Exit(code) => return code,
            End::Closed => return 0,
            End::Gone(error) => format!("This terminal isn't there any more ({error})."),
            End::Lost => "Zeron's connection ended.".to_owned(),
        };
        print!("\r\n{said}\r\n");
        let _ = std::io::stdout().flush();
        1
    }
}

/// What comes after a shell the bridge opened ended.
enum Next {
    /// The bridge ends, with this status.
    Done(i32),
    /// Its connection is back: the same terminal again.
    Same,
    /// A new shell, opened at this size.
    Shell(String, (u16, u16)),
}

/// Whether a shell opens again by itself: one the sandbox container's stop
/// killed while no one was typing (`TerminalSession::reconnects`), or one
/// its engine's restart ended.
fn by_itself(sandboxed: bool, end: &End, alive: Duration, idle: Option<Duration>) -> bool {
    match end {
        End::Exit(code) => zeron_proto::TerminalSession::reconnects(sandboxed, *code, alive, idle),
        // (Killed, as it were: not the person's doing.)
        End::Gone(_) => zeron_proto::TerminalSession::reconnects(sandboxed, 128 + 9, alive, None),
        End::Lost | End::Closed => false,
    }
}

#[cfg(unix)]
fn next_shell(socket: &mut Socket, args: &Args, shell: &Shell, end: &End, alive: Duration, idle: Option<Duration>) -> Next {
    // (Closed by someone else: as before. The tab's own close ends the
    // bridge first.)
    if matches!(end, End::Closed) && !connection_ends(socket) {
        return Next::Done(0);
    }
    let say = |text: &str| {
        print!("\x1b[90m[{text}]\x1b[0m\r\n");
        let _ = std::io::stdout().flush();
    };
    // (On a line of its own, below what the shell left; one that follows
    // the connection's coming back is below that line already.)
    if !matches!(end, End::Gone(_)) {
        print!("\r\n");
    }
    match end {
        End::Exit(code) => say(&format!("process exited {code}")),
        End::Gone(_) => say("the shell ended as Zeron restarted"),
        End::Lost | End::Closed => {
            say("Zeron's connection ended; waiting for it");
            loop {
                if !pause(1000) {
                    return Next::Done(0);
                }
                if let Ok(back) = dial(args.port, rpc::read_token(&args.data).as_deref()) {
                    *socket = back;
                    return Next::Same;
                }
            }
        }
    }
    if by_itself(shell.sandboxed, end, alive, idle) {
        say(if matches!(end, End::Exit(_)) { "the sandbox container stopped; opening a new shell in it" } else { "opening a new shell in the sandbox" });
        // (The container may still be coming back.)
        let mut why = String::new();
        for wait in [500, 1000, 2000, 4000] {
            if !pause(wait) {
                return Next::Done(0);
            }
            match open_again(socket, args, &shell.open) {
                Ok((id, size)) => return Next::Shell(id, size),
                Err(error) => why = error,
            }
        }
        say(&why);
    }
    loop {
        say("press Enter for a new shell");
        if !wait_for_enter() {
            return Next::Done(0);
        }
        match open_again(socket, args, &shell.open) {
            Ok((id, size)) => return Next::Shell(id, size),
            Err(why) => say(&why),
        }
    }
}

/// Whether its connection ends within a moment: its engine going away, not
/// one terminal closed.
#[cfg(unix)]
fn connection_ends(socket: &mut Socket) -> bool {
    // (Short reads: the read timeout `dial` set.)
    let _ = socket.get_ref().set_nonblocking(false);
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < until {
        match socket.read() {
            Ok(tungstenite::Message::Close(_)) => return true,
            Ok(_) => {}
            Err(e) if rpc::would_block(&e) => {}
            Err(_) => return true,
        }
    }
    false
}

#[cfg(not(unix))]
fn next_shell(_: &mut Socket, _: &Args, _: &Shell, end: &End, _: Duration, _: Option<Duration>) -> Next {
    Next::Done(if let End::Exit(code) = end { *code } else { 1 })
}

/// A new shell as `open` says, at the terminal's size now: its id and that
/// size. Its connection made again first if it went with its engine.
#[cfg(unix)]
fn open_again(socket: &mut Socket, args: &Args, open: &Map<String, Json>) -> Result<(String, (u16, u16)), String> {
    let size = window_size().unwrap_or((80, 24));
    // (`open_terminal` waits on short reads, not a socket that never blocks.)
    let _ = socket.get_ref().set_nonblocking(false);
    let opened = open_terminal(socket, open, size).or_else(|_| {
        let back = dial(args.port, rpc::read_token(&args.data).as_deref())?;
        *socket = back;
        open_terminal(socket, open, size)
    });
    opened.map(|(id, _)| (id, size))
}

/// What's typed within `ms` (-1: however long it takes), for no shell:
/// empty if nothing, None once its terminal has closed (the tab).
#[cfg(unix)]
fn read_typed(ms: i32) -> Option<Vec<u8>> {
    let stdin = std::io::stdin();
    let mut fds = [rpc::poll_fd(&stdin, libc::POLLIN)];
    // SAFETY: one valid pollfd, for its length.
    if unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) } <= 0 {
        return Some(Vec::new());
    }
    let mut buf = [0u8; 256];
    // SAFETY: reads into a buffer of its own length from fd 0, which poll
    // says has something (or has closed).
    let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
    if n > 0 {
        Some(buf[..n as usize].to_vec())
    } else if n < 0 && matches!(std::io::Error::last_os_error().kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) {
        Some(Vec::new())
    } else {
        None
    }
}

/// Waits `ms`: false if its terminal closed meanwhile.
#[cfg(unix)]
fn pause(ms: u64) -> bool {
    let until = std::time::Instant::now() + Duration::from_millis(ms);
    loop {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return true;
        }
        if read_typed(left.as_millis().max(1) as i32).is_none() {
            return false;
        }
    }
}

/// Waits for Enter: false if its terminal closed first.
#[cfg(unix)]
fn wait_for_enter() -> bool {
    loop {
        match read_typed(-1) {
            None => return false,
            Some(bytes) if bytes.iter().any(|b| matches!(b, b'\r' | b'\n')) => return true,
            Some(_) => {}
        }
    }
}

/// Where a bridge leaves the shell running in place of terminal `id` (an
/// engine's id: letters, digits and dashes).
fn moved_path(id: &str) -> Option<PathBuf> {
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then(|| claims_dir().join(format!("moved-{id}")))
}

/// The terminal running in place of `id` now: `id` itself, unless its
/// bridge opened another.
fn current(id: String) -> String {
    moved_path(&id)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|now| now.trim().to_owned())
        .filter(|now| moved_path(now).is_some())
        .unwrap_or(id)
}

fn leave_moved(first: &str, now: &str) {
    if let Some(path) = moved_path(first) {
        let _ = std::fs::create_dir_all(claims_dir());
        let _ = std::fs::write(path, now);
    }
}

fn forget_moved(id: &str) {
    if let Some(path) = moved_path(id) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, quote, quote_posix, quote_windows, said};
    #[cfg(windows)]
    use std::path::PathBuf;

    #[test]
    fn a_claim_changes_hands_once_and_a_late_one_is_never_left_with_no_one() {
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("zgx-claims-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The bridge claims in time: the app reads its id, and the claim goes.
        let claim = dir.join("one");
        assert!(super::leave_claim(&claim, "term-7"));
        assert_eq!(super::await_claim(&claim, Duration::from_millis(200)), Ok("term-7".into()));
        assert!(!claim.exists());
        // It couldn't open one: why, passed on.
        let claim = dir.join("two");
        assert!(super::leave_claim(&claim, "error: offline"));
        assert_eq!(super::await_claim(&claim, Duration::from_millis(200)), Err("offline".into()));
        // No claim in time: the app stops waiting, and a bridge claiming
        // after is told so (and closes its terminal); nothing is left.
        let claim = dir.join("three");
        assert!(super::await_claim(&claim, Duration::from_millis(100)).is_err());
        assert!(!super::leave_claim(&claim, "term-9"), "a late claim must fail");
        assert!(!claim.exists(), "the claim is cleared");
        assert!(!claim.with_extension("part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Windows console made raw: no echo, no lines, no Ctrl+C signal, keys
    /// as sequences, and whatever else it had kept.
    #[test]
    fn a_raw_console_neither_echoes_nor_cooks_nor_signals() {
        // A cooked console's defaults: processed, line, echo, insert, quick
        // edit, extended flags.
        let cooked = 0x0001 | 0x0002 | 0x0004 | 0x0020 | 0x0040 | 0x0080;
        assert_eq!(super::raw_input_mode(cooked), 0x0020 | 0x0040 | 0x0080 | 0x0200);
        assert_eq!(super::raw_input_mode(0x0200), 0x0200);
    }

    /// A console's size is its visible window's: inclusive edges.
    #[test]
    fn a_consoles_size_is_its_windows() {
        assert_eq!(super::console_window_size(0, 0, 79, 23), Some((80, 24)));
        // Scrolled down its buffer: still the window's size.
        assert_eq!(super::console_window_size(0, 900, 39, 947), Some((40, 48)));
        assert_eq!(super::console_window_size(5, 5, 4, 10), None);
    }

    /// (Linux fork) A shell the sandbox container's stop killed opens again
    /// by itself; one the person left, or one on this machine, waits for
    /// Enter. The app's id for it reaches the shell opened in its place.
    #[test]
    fn a_killed_sandboxed_shell_opens_again_and_the_apps_id_follows_it() {
        use super::{End, by_itself, current, forget_moved, leave_moved};
        use std::time::Duration;
        let minute = Duration::from_secs(60);
        assert!(by_itself(true, &End::Exit(137), minute, None));
        assert!(!by_itself(true, &End::Exit(0), minute, None));
        assert!(!by_itself(false, &End::Exit(137), minute, None));
        // Ctrl-C, then `exit`, a moment ago.
        assert!(!by_itself(true, &End::Exit(130), minute, Some(Duration::from_millis(200))));
        // Its engine restarted under it.
        assert!(by_itself(true, &End::Gone("Terminal not found".into()), minute, Some(Duration::ZERO)));
        assert!(!by_itself(false, &End::Gone("Terminal not found".into()), minute, None));
        // A container that dies as a shell opens isn't tried over and over.
        assert!(!by_itself(true, &End::Exit(137), Duration::from_secs(1), None));
        assert!(!by_itself(true, &End::Lost, minute, None));
        assert!(!by_itself(true, &End::Closed, minute, None));
        let first = format!("test-{}-first", std::process::id());
        assert_eq!(current(first.clone()), first);
        leave_moved(&first, "now-1");
        assert_eq!(current(first.clone()), "now-1");
        leave_moved(&first, "now-2");
        assert_eq!(current(first.clone()), "now-2");
        forget_moved(&first);
        assert_eq!(current(first.clone()), first);
        // Nothing outside the claims' folder is read for an odd id.
        assert_eq!(current("../etc/passwd".into()), "../etc/passwd");
    }

    #[test]
    fn an_older_engine_says_to_update() {
        assert_eq!(said("unknown method: TerminalStatus".into()), "Update Zeron to use terminals here.");
        assert_eq!(said("no such terminal".into()), "no such terminal");
    }

    #[test]
    fn quotes_and_parses_the_bridge_line() {
        assert_eq!(quote_posix("a b'c"), "'a b'\\''c'");
        // Paths with spaces, each platform's way (both checked everywhere).
        assert_eq!(quote_posix("/Users/Some One/Zeron.app/zeron-zgx"), "'/Users/Some One/Zeron.app/zeron-zgx'");
        assert_eq!(quote_windows(r"C:\Program Files\Zeron\zeron.exe"), r#""C:\Program Files\Zeron\zeron.exe""#);
        // A folder's trailing backslash must not escape the closing quote.
        assert_eq!(quote_windows(r"C:\Users\me\AppData\Local\Zeron\"), r#""C:\Users\me\AppData\Local\Zeron\\""#);
        assert_eq!(quote_windows(r#"{"chatId":"c1"}"#), r#""{\"chatId\":\"c1\"}""#);
        assert_eq!(quote_windows(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_windows(""), r#""""#);
        let args: Vec<String> = ["--port", "27897", "--data", "/tmp/d", "--terminal", "t1", "--device", "dev"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.port, 27897);
        assert_eq!(parsed.terminal.as_deref(), Some("t1"));
        assert_eq!(parsed.device.as_deref(), Some("dev"));
        assert!(parse(&["--port".to_string()]).is_err());
        // A new one: what to open and where to leave its id.
        let args: Vec<String> = ["--port", "1", "--data", "/d", "--open", r#"{"chatId":"c1"}"#, "--claim", "/tmp/x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse(&args).unwrap();
        assert!(parsed.terminal.is_none());
        assert_eq!(parsed.open.unwrap()["chatId"], "c1");
        let args: Vec<String> = ["--port", "1", "--data", "/d", "--open", "{}"].iter().map(|s| s.to_string()).collect();
        assert!(parse(&args).is_err(), "a new one needs somewhere to leave its id");
    }

    /// The bridge line as Windows itself splits it (CommandLineToArgvW, the
    /// rules a C runtime or Rust's `args` read a CreateProcess command line
    /// by): every word comes back whole, the program first, and the bridge's
    /// own parser reads the rest. Spaces, a trailing backslash, quotes and
    /// backslashes in the JSON.
    #[cfg(windows)]
    #[test]
    fn windows_reads_the_bridge_line_back_whole() {
        use windows_sys::Win32::{Foundation::LocalFree, UI::Shell::CommandLineToArgvW};
        fn split(line: &str) -> Vec<String> {
            let wide: Vec<u16> = line.encode_utf16().chain([0]).collect();
            let mut count = 0;
            // SAFETY: a NUL-terminated line; the array is freed below.
            let argv = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut count) };
            assert!(!argv.is_null());
            let words = (0..count as usize)
                .map(|i| {
                    // SAFETY: `count` NUL-terminated strings.
                    let word = unsafe { *argv.add(i) };
                    let len = (0..).take_while(|&j| unsafe { *word.add(j) } != 0).count();
                    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(word, len) })
                })
                .collect();
            // SAFETY: the array CommandLineToArgvW allocated.
            unsafe { LocalFree(argv.cast()) };
            words
        }
        let exe = r"C:\Users\Some One\AppData\Local\Programs\Zeron\zeron.exe";
        let data = r"C:\Users\Some One\AppData\Local\Zeron\";
        let open = r#"{"chatId":"c 1","root":"C:\\work\\a \"b\""}"#;
        let claim = r"C:\Temp\zeron-zgx-terminals\12-34";
        let line = format!(
            "{} terminal-bridge --port 27654 --data {} --open {} --claim {}",
            quote(exe),
            quote(data),
            quote(open),
            quote(claim)
        );
        let words = split(&line);
        assert_eq!(words, [exe, "terminal-bridge", "--port", "27654", "--data", data, "--open", open, "--claim", claim]);
        let parsed = parse(&words[2..]).unwrap();
        assert_eq!(parsed.data, PathBuf::from(data));
        assert_eq!(parsed.open.unwrap()["root"], r#"C:\work\a "b""#);
    }
}
