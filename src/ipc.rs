use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    os::unix::{ffi::OsStrExt, fs::FileTypeExt},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
/// One newline-delimited JSON command from the Neovim plugin.
pub enum Message {
    Add {
        pdf: PathBuf,
        nvim: String,
    },
    Register {
        pdf: PathBuf,
        nvim: String,
    },
    Forward {
        pdf: PathBuf,
        source: PathBuf,
        line: u32,
        column: u32,
        nvim: String,
    },
    Control {
        pdf: PathBuf,
        nvim: String,
        #[serde(flatten)]
        command: Control,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Control {
    Move { page: u32 },
    Next { count: u32 },
    Prev { count: u32 },
    First,
    Last,
    Zoom { percent: u16 },
    ZoomIn,
    ZoomOut,
    ZoomReset,
    View { mode: ViewMode },
    Reload,
    Center,
    Clear,
    Switch,
    NextPdf,
    PrevPdf,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    Single,
    Scroll,
    Toggle,
}

fn normalize_pdf(message: &mut Message, viewer_pdf: Option<&Path>) -> Result<(), &'static str> {
    // The socket name alone is insufficient: reject commands addressed to a
    // different PDF and normalize accepted paths before the app compares them.
    let is_add = matches!(message, Message::Add { .. });
    let request_pdf = match message {
        Message::Add { pdf, .. }
        | Message::Register { pdf, .. }
        | Message::Forward { pdf, .. }
        | Message::Control { pdf, .. } => pdf,
    };
    let canonical =
        fs::canonicalize(&*request_pdf).map_err(|_| "PDF path does not match viewer")?;
    if canonical
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("pdf")
    {
        return Err("expected a .pdf file");
    }
    let allowed = if is_add {
        viewer_pdf.is_none()
    } else {
        viewer_pdf == Some(canonical.as_path())
    };
    if !allowed {
        return Err("PDF path does not match viewer");
    }
    *request_pdf = canonical;
    Ok(())
}

/// Owns the Unix socket and its PDF metadata file for the viewer lifetime.
pub struct Socket {
    path: PathBuf,
    metadata_path: PathBuf,
}

pub fn socket_path(pdf: &Path) -> PathBuf {
    // FNV-1a gives the same ID for the same canonical PDF path across processes.
    // Callers must canonicalize first; hashing a relative path changes the ID.
    let hash = pdf
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    std::env::temp_dir().join(format!("pdfty-{hash:016x}.sock"))
}

pub fn control_socket_path() -> PathBuf {
    std::env::temp_dir().join(format!("pdfty-control-{}.sock", std::process::id()))
}

pub fn active_sockets() -> Result<Vec<(PathBuf, Option<PathBuf>)>, String> {
    // Connecting filters out stale socket files left by crashed viewers. The
    // adjacent JSON file lets --list show which PDF owns each live socket.
    let mut active = Vec::new();
    for entry in fs::read_dir(std::env::temp_dir())
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("pdfty-") || !name.ends_with(".sock") {
            continue;
        }
        if !entry
            .file_type()
            .map(|kind| kind.is_socket())
            .unwrap_or(false)
        {
            continue;
        }
        let path = entry.path();
        if UnixStream::connect(&path).is_err() {
            continue;
        }
        let pdf = fs::read(metadata_path(&path))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        active.push((path, pdf));
    }
    active.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(active)
}

fn metadata_path(socket: &Path) -> PathBuf {
    socket.with_extension("json")
}
impl Socket {
    pub fn start(
        path: PathBuf,
        viewer_pdf: Option<PathBuf>,
        tx: Sender<Message>,
    ) -> Result<Self, String> {
        // A live viewer with the same PDF already owns this deterministic path.
        // Only unlink an old socket when connecting to it fails.
        let metadata_path = metadata_path(&path);
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                return Err(format!("socket already in use: {}", path.display()));
            }
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        let metadata = serde_json::to_vec(&viewer_pdf).map_err(|e| e.to_string())?;
        if let Err(error) = fs::write(&metadata_path, metadata) {
            let _ = fs::remove_file(&path);
            return Err(error.to_string());
        }
        thread::spawn(move || {
            // The listener only validates and queues commands. Rendering and
            // Neovim navigation stay on the app loop, which owns viewer state.
            for stream in listener.incoming().flatten() {
                let Ok(copy) = stream.try_clone() else {
                    continue;
                };
                let mut stream = stream;
                let mut line = String::new();
                if BufReader::new(copy).read_line(&mut line).is_ok() {
                    let response = match serde_json::from_str::<Message>(&line) {
                        Ok(mut message) => match normalize_pdf(&mut message, viewer_pdf.as_deref())
                        {
                            Ok(()) => match tx.send(message) {
                                Ok(()) => "ok\n",
                                Err(_) => "viewer stopped\n",
                            },
                            Err(_) => "PDF path does not match viewer\n",
                        },
                        Err(_) => "invalid request\n",
                    };
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        });
        Ok(Self {
            path,
            metadata_path,
        })
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.metadata_path);
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_pdf, socket_path, Control, Message, Socket};
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        sync::mpsc,
        time::Duration,
    };

    #[test]
    fn registration_checks_and_normalizes_pdf_path() {
        let dir = std::env::temp_dir().join(format!("pdfty-ipc-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("main.pdf");
        let other = dir.join("other.pdf");
        fs::write(&pdf, []).unwrap();
        fs::write(&other, []).unwrap();
        let expected = fs::canonicalize(&pdf).unwrap();
        assert_eq!(
            socket_path(&expected),
            socket_path(&fs::canonicalize(dir.join("./main.pdf")).unwrap())
        );
        assert_ne!(
            socket_path(&expected),
            socket_path(&fs::canonicalize(&other).unwrap())
        );
        let mut mismatch = Message::Register {
            pdf: other,
            nvim: "test-server".into(),
        };
        assert_eq!(
            normalize_pdf(&mut mismatch, Some(&expected)),
            Err("PDF path does not match viewer")
        );
        let mut message = Message::Register {
            pdf: dir.join("./main.pdf"),
            nvim: "test-server".into(),
        };
        assert_eq!(normalize_pdf(&mut message, Some(&expected)), Ok(()));
        match message {
            Message::Register { pdf: received, .. } => {
                assert_eq!(received, expected)
            }
            _ => panic!("expected registration"),
        }
        let mut add = Message::Add {
            pdf: pdf.clone(),
            nvim: "test-server".into(),
        };
        assert_eq!(normalize_pdf(&mut add, None), Ok(()));
        assert_eq!(
            normalize_pdf(&mut add, Some(&expected)),
            Err("PDF path does not match viewer")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn blank_viewer_control_socket_accepts_add() {
        let dir = std::env::temp_dir().join(format!("pdfty-add-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("chapter.pdf");
        fs::write(&pdf, []).unwrap();
        let socket_path = dir.join("control.sock");
        let (tx, rx) = mpsc::channel();
        let socket = Socket::start(socket_path.clone(), None, tx).unwrap();
        let mut stream = UnixStream::connect(&socket_path).unwrap();
        let request = serde_json::to_string(&Message::Add {
            pdf: pdf.clone(),
            nvim: "editor-socket".into(),
        })
        .unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert_eq!(reply, "ok\n");
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Message::Add { pdf: received, .. } if received == fs::canonicalize(&pdf).unwrap())
        );
        drop(socket);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn document_socket_accepts_control_and_rejects_wrong_pdf_or_invalid_command() {
        let dir = std::env::temp_dir().join(format!("pdfty-control-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("main.pdf");
        let other = dir.join("other.pdf");
        fs::write(&pdf, []).unwrap();
        fs::write(&other, []).unwrap();
        let pdf = fs::canonicalize(pdf).unwrap();
        let path = dir.join("document.sock");
        let (tx, rx) = mpsc::channel();
        let socket = Socket::start(path.clone(), Some(pdf.clone()), tx).unwrap();
        let send = |request: serde_json::Value| {
            let mut stream = UnixStream::connect(&path).unwrap();
            writeln!(stream, "{request}").unwrap();
            let mut reply = String::new();
            BufReader::new(stream).read_line(&mut reply).unwrap();
            reply
        };
        assert_eq!(
            send(
                serde_json::json!({"action": "control", "pdf": pdf, "nvim": "editor", "command": "move", "page": 2})
            ),
            "ok\n"
        );
        assert!(matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Message::Control { command: Control::Move { page: 2 }, pdf: received, .. } if received == pdf));
        assert_eq!(
            send(
                serde_json::json!({"action": "control", "pdf": other, "nvim": "editor", "command": "last"})
            ),
            "PDF path does not match viewer\n"
        );
        assert_eq!(
            send(
                serde_json::json!({"action": "control", "pdf": pdf, "nvim": "editor", "command": "move", "page": -1})
            ),
            "invalid request\n"
        );
        assert_eq!(
            send(
                serde_json::json!({"action": "control", "pdf": pdf, "nvim": "editor", "command": "view", "mode": "unknown"})
            ),
            "invalid request\n"
        );
        assert!(rx.try_recv().is_err());
        drop(socket);
        fs::remove_dir_all(dir).unwrap();
    }
}
