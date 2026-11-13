//! Kitty graphics protocol: image transmission, deletion and Unicode
//! placeholders. Every page tile is uploaded once and displayed through
//! placeholder cells, so scrolling only rewrites text and never pixels.

use std::fmt::Write as _;
use std::{fs::File, io::Write as _, path::PathBuf, sync::Arc};

/// The character Kitty replaces with image pixels.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column index diacritics from Kitty's rowcolumn-diacritics.txt.
/// A placement can therefore address at most 297 rows and columns.
pub const DIACRITICS: [char; 297] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}', '\u{0346}', '\u{034A}',
    '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}', '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}',
    '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}', '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}',
    '\u{0483}', '\u{0484}', '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}', '\u{05A8}', '\u{05A9}',
    '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}', '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}',
    '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}', '\u{0659}', '\u{065A}', '\u{065B}', '\u{065D}', '\u{065E}', '\u{06D6}',
    '\u{06D7}', '\u{06D8}', '\u{06D9}', '\u{06DA}', '\u{06DB}', '\u{06DC}', '\u{06DF}', '\u{06E0}', '\u{06E1}', '\u{06E2}',
    '\u{06E4}', '\u{06E7}', '\u{06E8}', '\u{06EB}', '\u{06EC}', '\u{0730}', '\u{0732}', '\u{0733}', '\u{0735}', '\u{0736}',
    '\u{073A}', '\u{073D}', '\u{073F}', '\u{0740}', '\u{0741}', '\u{0743}', '\u{0745}', '\u{0747}', '\u{0749}', '\u{074A}',
    '\u{07EB}', '\u{07EC}', '\u{07ED}', '\u{07EE}', '\u{07EF}', '\u{07F0}', '\u{07F1}', '\u{07F3}', '\u{0816}', '\u{0817}',
    '\u{0818}', '\u{0819}', '\u{081B}', '\u{081C}', '\u{081D}', '\u{081E}', '\u{081F}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082A}', '\u{082B}', '\u{082C}', '\u{082D}', '\u{0951}',
    '\u{0953}', '\u{0954}', '\u{0F82}', '\u{0F83}', '\u{0F86}', '\u{0F87}', '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}',
    '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}', '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}',
    '\u{1B6B}', '\u{1B6D}', '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}', '\u{1DC5}', '\u{1DC6}',
    '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}', '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}',
    '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}', '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}',
    '\u{1DE0}', '\u{1DE1}', '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}', '\u{20E9}', '\u{20F0}',
    '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}', '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}',
    '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}', '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}',
    '\u{2DF1}', '\u{2DF2}', '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}', '\u{A6F0}', '\u{A6F1}',
    '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}', '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}',
    '\u{A8EA}', '\u{A8EB}', '\u{A8EC}', '\u{A8ED}', '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}', '\u{AAB2}',
    '\u{AAB3}', '\u{AAB7}', '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}', '\u{FE20}', '\u{FE21}', '\u{FE22}', '\u{FE23}',
    '\u{FE24}', '\u{FE25}', '\u{FE26}', '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}', '\u{1D187}', '\u{1D188}', '\u{1D189}',
    '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}', '\u{1D242}', '\u{1D243}', '\u{1D244}',
];

/// How pixel data reaches the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    /// Raw pixels in a temporary file that Kitty reads and deletes. Fastest;
    /// requires the terminal to run on the same machine.
    File,
    /// zlib-compressed pixels sent inline in base64 chunks. Works over SSH.
    Direct,
}

impl Transfer {
    pub fn detect() -> Self {
        match std::env::var("PDFTY_TRANSFER").as_deref() {
            Ok("direct") => Transfer::Direct,
            Ok("file") => Transfer::File,
            _ if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() => {
                Transfer::Direct
            }
            _ => Transfer::File,
        }
    }
}

/// Encoded tile pixels, produced on a worker thread.
pub enum Payload {
    File(PathBuf),
    Direct(Vec<u8>),
}

/// Writes graphics commands, wrapping them for tmux passthrough when needed.
///
/// When pdfty runs inside a Neovim terminal, Neovim's terminal emulator drops
/// graphics commands but keeps placeholder text. Commands then go straight to
/// the terminal device Neovim itself draws on (`PDFTY_TTY`), while
/// placeholders travel through Neovim like any other text.
#[derive(Clone)]
pub struct Graphics {
    pub tmux: bool,
    tty: Option<Arc<File>>,
    /// Preferred over `tty` for writing: Neovim forwards the bytes itself.
    editor: Option<Arc<crate::nvim::UiSender>>,
}

impl Graphics {
    pub fn new(tmux: bool, tty: Option<Arc<File>>) -> Self {
        Self { tmux, tty, editor: None }
    }

    pub fn with_editor(mut self, editor: Option<Arc<crate::nvim::UiSender>>) -> Self {
        self.editor = editor;
        self
    }

    /// Hand over the commands collected for a frame. Through Neovim they go in
    /// one batch; otherwise they stay in `stdout` (written with the frame).
    pub fn deliver(&self, batch: &mut Vec<u8>, stdout: &mut Vec<u8>) {
        match &self.editor {
            Some(editor) => {
                editor.send(batch);
                batch.clear();
            }
            None => stdout.append(batch),
        }
    }

    pub fn tty(&self) -> Option<&File> {
        self.tty.as_deref()
    }

    /// Send an escape sequence to the outer terminal. With a separate tty each
    /// sequence is written in one call so it never interleaves with Neovim's output.
    pub fn emit(&self, out: &mut Vec<u8>, sequence: &[u8]) {
        let wrapped;
        let sequence = if self.tmux {
            let mut buf = b"\x1bPtmux;".to_vec();
            for &byte in sequence {
                if byte == 0x1b {
                    buf.push(0x1b);
                }
                buf.push(byte);
            }
            buf.extend_from_slice(b"\x1b\\");
            wrapped = buf;
            &wrapped[..]
        } else {
            sequence
        };
        match (&self.editor, &self.tty) {
            (None, Some(tty)) => {
                let _ = (&**tty).write_all(sequence);
            }
            _ => out.extend_from_slice(sequence),
        }
    }

    fn command(&self, out: &mut Vec<u8>, control: &str, payload: &[u8]) {
        let mut apc = Vec::with_capacity(control.len() + payload.len() + 8);
        apc.extend_from_slice(b"\x1b_G");
        apc.extend_from_slice(control.as_bytes());
        if !payload.is_empty() {
            apc.push(b';');
            apc.extend_from_slice(payload);
        }
        apc.extend_from_slice(b"\x1b\\");
        self.emit(out, &apc);
    }

    /// Transmit 24-bit RGB pixels and create a virtual placement spanning
    /// `cols`×`rows` cells, ready to be shown by placeholder text.
    pub fn upload(&self, out: &mut Vec<u8>, id: u32, size: (u32, u32), cells: (u32, u32), payload: &Payload) {
        let (w, h) = size;
        let (c, r) = cells;
        match payload {
            Payload::File(path) => {
                let control = format!("a=T,U=1,f=24,t=t,s={w},v={h},i={id},c={c},r={r},q=2");
                let encoded = base64(path.as_os_str().as_encoded_bytes());
                self.command(out, &control, &encoded);
            }
            Payload::Direct(data) => {
                let encoded = base64(data);
                let chunks: Vec<&[u8]> = encoded.chunks(4096).collect();
                let last = chunks.len().saturating_sub(1);
                for (index, chunk) in chunks.iter().enumerate() {
                    let more = u8::from(index != last);
                    let control = if index == 0 {
                        format!("a=T,U=1,f=24,o=z,s={w},v={h},i={id},c={c},r={r},q=2,m={more}")
                    } else {
                        format!("q=2,m={more}")
                    };
                    self.command(out, &control, chunk);
                }
            }
        }
    }

    /// Free an image and its pixel data.
    pub fn delete(&self, out: &mut Vec<u8>, id: u32) {
        self.command(out, &format!("a=d,d=I,i={id},q=2"), b"");
    }
}

/// SGR sequence selecting an image through the 24-bit foreground color.
pub fn image_color(out: &mut String, id: u32) {
    let _ = write!(out, "\x1b[0;38;2;{};{};{}m", (id >> 16) & 0xff, (id >> 8) & 0xff, id & 0xff);
}

/// One placeholder cell with explicit row and column. Every cell carries both
/// diacritics so partial redraws by tmux can never shift image fragments.
pub fn placeholder(out: &mut String, row: u16, col: u16) {
    out.push(PLACEHOLDER);
    out.push(DIACRITICS[row as usize]);
    out.push(DIACRITICS[col as usize]);
}

pub fn base64(input: &[u8]) -> Vec<u8> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63]);
        out.push(TABLE[(n >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] } else { b'=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] } else { b'=' });
    }
    out
}

/// Image IDs live in the 24-bit foreground color. Start at a random offset so
/// several viewers in one Kitty window do not collide.
pub struct IdAllocator(u32);

impl IdAllocator {
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            ^ std::process::id().rotate_left(13);
        Self((seed & 0x00ff_ffff).max(1))
    }
    pub fn next(&mut self) -> u32 {
        self.0 = ((self.0 + 1) & 0x00ff_ffff).max(1);
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), b"");
        assert_eq!(base64(b"f"), b"Zg==");
        assert_eq!(base64(b"fo"), b"Zm8=");
        assert_eq!(base64(b"foo"), b"Zm9v");
        assert_eq!(base64(b"foobar"), b"Zm9vYmFy");
    }

    #[test]
    fn diacritics_are_distinct_combining_marks() {
        let mut sorted = DIACRITICS.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 297);
        assert_eq!(DIACRITICS[0], '\u{305}');
        assert_eq!(DIACRITICS[1], '\u{30d}');
    }

    #[test]
    fn tmux_wraps_and_escapes_commands() {
        let mut out = Vec::new();
        Graphics::new(true, None).delete(&mut out, 7);
        assert_eq!(out, b"\x1bPtmux;\x1b\x1b_Ga=d,d=I,i=7,q=2\x1b\x1b\\\x1b\\");
        let mut out = Vec::new();
        Graphics::new(false, None).delete(&mut out, 7);
        assert_eq!(out, b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
    }

    #[test]
    fn direct_upload_is_chunked() {
        let mut out = Vec::new();
        let data = vec![1u8; 9000];
        Graphics::new(false, None).upload(&mut out, 5, (10, 10), (1, 1), &Payload::Direct(data));
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b_Ga=T,U=1,f=24,o=z,s=10,v=10,i=5,c=1,r=1,q=2,m=1;"));
        assert_eq!(text.matches("\x1b_G").count(), 3);
        assert!(text.contains("\x1b_Gq=2,m=0;"));
    }

    #[test]
    fn placeholder_encodes_row_and_column() {
        let mut s = String::new();
        image_color(&mut s, 0x010203);
        placeholder(&mut s, 0, 1);
        assert_eq!(s, "\x1b[0;38;2;1;2;3m\u{10EEEE}\u{305}\u{30d}");
    }
}
