use std::io::{self, BufRead, Read, Write};

use serde::{Serialize, de::DeserializeOwned};

pub fn encode(message: &impl Serialize) -> serde_json::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(message)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn write(writer: &mut impl Write, message: &impl Serialize) -> anyhow::Result<()> {
    writer.write_all(&encode(message)?)?;
    Ok(())
}

pub fn read(reader: &mut impl BufRead, maximum: usize) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let count = reader
        .take((maximum + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message exceeded the size limit",
        ));
    }
    Ok((count > 0).then_some(bytes))
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> serde_json::Result<T> {
    serde_json::from_slice(bytes)
}

#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8], maximum: usize) -> anyhow::Result<()> {
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() > maximum {
            anyhow::bail!("input message exceeds limit");
        }
        Ok(())
    }

    pub fn decode_next<T: DeserializeOwned>(&mut self) -> serde_json::Result<Option<T>> {
        let Some(end) = self.bytes.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let line = self.bytes.drain(..=end).collect::<Vec<_>>();
        decode(&line).map(Some)
    }
}
