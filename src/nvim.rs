//! Talk to Neovim directly over its msgpack-RPC socket (replaces `nvr`), and
//! focus the terminal pane or window that hosts it.

use rmpv::Value;
use std::{
    env, fs,
    io::{BufReader, Read, Write},
    net::TcpStream,
    os::unix::net::UnixStream,
    path::Path,
    process::Command,
    time::Duration,
};

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

fn connect(server: &str) -> Result<Box<dyn Stream>, String> {
    let timeout = Some(Duration::from_secs(3));
    if Path::new(server).exists() || server.starts_with('/') {
        let stream = UnixStream::connect(server).map_err(|e| format!("connect {server}: {e}"))?;
        stream.set_read_timeout(timeout).ok();
        stream.set_write_timeout(timeout).ok();
        Ok(Box::new(stream))
    } else {
        let stream = TcpStream::connect(server).map_err(|e| format!("connect {server}: {e}"))?;
        stream.set_read_timeout(timeout).ok();
        stream.set_write_timeout(timeout).ok();
        Ok(Box::new(stream))
    }
}

/// Send one request and wait for its response.
pub fn request(server: &str, method: &str, params: Vec<Value>) -> Result<Value, String> {
    let mut stream = connect(server)?;
    let message = Value::Array(vec![0.into(), 1.into(), method.into(), Value::Array(params)]);
    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, &message).map_err(|e| e.to_string())?;
    stream.write_all(&bytes).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    loop {
        let value = rmpv::decode::read_value(&mut reader).map_err(|e| format!("read response: {e}"))?;
        let Value::Array(items) = value else { continue };
        // Responses are [1, msgid, error, result]; skip notifications.
        if items.len() == 4 && items[0].as_u64() == Some(1) && items[1].as_u64() == Some(1) {
            if !items[2].is_nil() {
                return Err(error_text(&items[2]));
            }
            return Ok(items[3].clone());
        }
    }
}

/// A persistent connection for writing to the editor's terminal through
/// `nvim_ui_send` (Neovim 0.12+). The editor's UI writes the bytes between its
/// own redraws, so they can never interleave with them mid-sequence the way
/// a second writer on the same tty can.
pub struct UiSender {
    server: String,
    stream: std::sync::Mutex<Option<Box<dyn Stream>>>,
}

impl UiSender {
    pub fn connect(server: &str) -> Result<Self, String> {
        let stream = connect(server)?;
        Ok(Self { server: server.to_owned(), stream: std::sync::Mutex::new(Some(stream)) })
    }

    /// Fire-and-forget: a msgpack-RPC notification needs no response.
    pub fn send(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let message = Value::Array(vec![2.into(), "nvim_ui_send".into(), Value::Array(vec![Value::Binary(bytes.to_vec())])]);
        let mut encoded = Vec::with_capacity(bytes.len() + 32);
        if rmpv::encode::write_value(&mut encoded, &message).is_err() {
            return;
        }
        let mut guard = self.stream.lock().unwrap();
        for _ in 0..2 {
            if guard.is_none() {
                *guard = connect(&self.server).ok();
            }
            let Some(stream) = guard.as_mut() else { return };
            if stream.write_all(&encoded).and_then(|_| stream.flush()).is_ok() {
                return;
            }
            *guard = None;
        }
    }
}

fn error_text(error: &Value) -> String {
    match error {
        Value::Array(parts) => parts
            .get(1)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string()),
        other => other.to_string(),
    }
}

/// The paired editor, or the VimTeX server file as a fallback.
pub fn resolve_server(registered: Option<&str>) -> Result<String, String> {
    if let Some(server) = registered.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(server.to_owned());
    }
    let server = fs::read_to_string("/tmp/vimtexserver.txt")
        .map_err(|e| format!("no paired Neovim and /tmp/vimtexserver.txt unreadable: {e}"))?;
    let server = server.trim();
    if server.is_empty() {
        return Err("/tmp/vimtexserver.txt is empty".into());
    }
    Ok(server.to_owned())
}

const OPEN_SOURCE: &str = r#"
local file, line, focus = ...
-- The pdfty plugin knows its side panel and picks the editor window itself.
local ok, pdfty = pcall(require, "pdfty")
if ok and type(pdfty.open_source) == "function" then
  return pdfty.open_source(file, line, focus) == true
end
vim.cmd("normal! m'")
vim.cmd("drop " .. vim.fn.fnameescape(file))
line = math.max(1, math.min(line, vim.api.nvim_buf_line_count(0)))
vim.api.nvim_win_set_cursor(0, { line, 0 })
vim.cmd("normal! zvzz")
return false
"#;

/// Show `file` at `line` in the editor, reusing a window that already has it.
/// Returns true when Neovim also took care of focus (pdfty in its side panel).
pub fn open_source(server: &str, file: &Path, line: u32, focus: bool) -> Result<bool, String> {
    let file = file.to_string_lossy().into_owned();
    request(
        server,
        "nvim_exec_lua",
        vec![OPEN_SOURCE.into(), Value::Array(vec![file.into(), line.into(), focus.into()])],
    )
    .map(|value| value.as_bool() == Some(true))
}

/// Focus the tmux pane and/or Kitty window running the editor.
pub fn focus(server: &str, log: &mut impl FnMut(String)) -> Result<(), String> {
    let expression = "json_encode({'tmux': $TMUX, 'pane': $TMUX_PANE, 'kitty': $KITTY_WINDOW_ID, 'listen': $KITTY_LISTEN_ON})";
    let value = request(server, "nvim_eval", vec![expression.into()])?;
    let json = value.as_str().ok_or("unexpected nvim_eval result")?;
    let editor: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let field = |name: &str| editor[name].as_str().unwrap_or("").to_owned();
    let (tmux, pane, kitty, listen) = (field("tmux"), field("pane"), field("kitty"), field("listen"));
    let mut focused = false;
    if !tmux.is_empty() && !pane.is_empty() {
        let socket = tmux_socket(&tmux).ok_or("invalid Neovim tmux address")?;
        log(format!("tmux -S {socket} switch-client -t {pane}"));
        run(Command::new("tmux").args(["-S", socket, "switch-client", "-t", &pane]))?;
        focused = true;
    }
    if !kitty.is_empty() && (!focused || env::var("KITTY_WINDOW_ID").ok().as_deref() != Some(&kitty)) {
        let id: u64 = kitty.parse().map_err(|_| "invalid Neovim Kitty window ID")?;
        let mut command = Command::new("kitten");
        command.arg("@");
        if !listen.is_empty() {
            command.args(["--to", &listen]);
        }
        let target = format!("id:{id}");
        command.args(["focus-window", "--match", &target]);
        log(format!("kitten @ focus-window --match {target}"));
        run(&mut command)?;
        focused = true;
    }
    if focused {
        Ok(())
    } else {
        Err("Neovim has no tmux pane or Kitty window ID".into())
    }
}

/// TMUX is `socket,pid,session`; the socket path itself may contain commas.
pub fn tmux_socket(address: &str) -> Option<&str> {
    address.rsplitn(3, ',').nth(2).filter(|socket| !socket.is_empty())
}

fn run(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn tmux_address_keeps_commas_in_socket_path() {
        assert_eq!(tmux_socket("/tmp/tmux-501/default,123,0"), Some("/tmp/tmux-501/default"));
        assert_eq!(tmux_socket("/tmp/a,b/socket,123,0"), Some("/tmp/a,b/socket"));
        assert_eq!(tmux_socket("invalid"), None);
    }

    #[test]
    fn request_round_trips_through_a_fake_server() {
        let path = std::env::temp_dir().join(format!("pdfty-nvim-test-{}.sock", std::process::id()));
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = rmpv::decode::read_value(&mut stream).unwrap();
            assert_eq!(request[2].as_str(), Some("nvim_eval"));
            // A notification first, then the response.
            let note = Value::Array(vec![2.into(), "redraw".into(), Value::Array(vec![])]);
            rmpv::encode::write_value(&mut stream, &note).unwrap();
            let reply = Value::Array(vec![1.into(), 1.into(), Value::Nil, "ok".into()]);
            rmpv::encode::write_value(&mut stream, &reply).unwrap();
        });
        let result = request(path.to_str().unwrap(), "nvim_eval", vec!["1".into()]).unwrap();
        assert_eq!(result.as_str(), Some("ok"));
        server.join().unwrap();
        let _ = fs::remove_file(path);
    }
}
