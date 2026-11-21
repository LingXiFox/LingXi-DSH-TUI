//! Bounded newline-delimited frames for the official SDK stdio transport.

use std::io::{self, BufRead, Read, Write};

use crate::protocol::{ClientMethod, Frame, InitializeParams, JsonRpcId, Request};

/// Maximum encoded frame size accepted by the host, including its newline.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Read one SDK frame, returning `None` on a clean stream close.
pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Frame>> {
    let mut line = Vec::new();
    loop {
        line.clear();
        (&mut *reader)
            .take((MAX_FRAME_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if line.is_empty() {
            return Ok(None);
        }
        if line.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SDK frame exceeds size limit",
            ));
        }
        if !line.ends_with(b"\n") {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "SDK frame ended before newline",
            ));
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        return serde_json::from_slice(&line)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    }
}

/// Write and flush exactly one SDK frame.
pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, frame).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Construct the official initialization request.
pub fn initialize_request(id: JsonRpcId, params: &InitializeParams) -> Frame {
    Frame::Request(Request {
        jsonrpc: "2.0".into(),
        id,
        method: ClientMethod::Initialize.as_str().into(),
        // Every field in InitializeParams has an infallible JSON representation.
        params: Some(serde_json::json!(params)),
    })
}

/// Construct the official process shutdown request.
pub fn shutdown_request(id: JsonRpcId) -> Frame {
    Frame::Request(Request {
        jsonrpc: "2.0".into(),
        id,
        method: ClientMethod::Shutdown.as_str().into(),
        params: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_fragmented_frames_blank_lines_and_clean_eof() {
        let bytes = b"\n\r\n{\"jsonrpc\":\"2.0\",\"id\":\"a\",\"result\":null}\n";
        let mut reader = io::BufReader::with_capacity(3, Cursor::new(bytes));
        let frame = read_frame(&mut reader).unwrap().unwrap();
        let mut output = Vec::new();
        write_frame(&mut output, &frame).unwrap();
        assert_eq!(read_frame(&mut Cursor::new(output)).unwrap(), Some(frame));
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn rejects_noise_truncated_oversized_and_invalid_envelopes() {
        for bytes in [
            b"diagnostic on stdout\n".to_vec(),
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}".to_vec(),
            b"{\"jsonrpc\":\"1.0\",\"id\":1,\"result\":{}}\n".to_vec(),
            b"{\"jsonrpc\":\"2.0\",\"id\":1}\n".to_vec(),
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{},\"error\":{\"code\":1,\"message\":\"x\"}}\n".to_vec(),
            vec![b' '; MAX_FRAME_BYTES + 1],
        ] {
            assert!(read_frame(&mut Cursor::new(bytes)).is_err());
        }
    }
}
