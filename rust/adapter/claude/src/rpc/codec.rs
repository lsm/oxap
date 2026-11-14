use std::io::{BufRead, ErrorKind, Write};

use super::{parse_message, Error, Message};

pub const DEFAULT_FRAME_LIMIT: usize = 8 << 20;

pub struct Decoder<R> {
    reader: R,
    limit: usize,
}

impl<R: BufRead> Decoder<R> {
    pub fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            limit: if limit == 0 {
                DEFAULT_FRAME_LIMIT
            } else {
                limit
            },
        }
    }

    pub fn decode(&mut self) -> Result<Message, Error> {
        let frame = self.read_frame()?;
        parse_message(&frame)
    }

    fn read_frame(&mut self) -> Result<Vec<u8>, Error> {
        let mut frame: Vec<u8> = Vec::with_capacity(self.limit.min(4096));
        loop {
            let available = match self.reader.fill_buf() {
                Ok(available) => available,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::io(error)),
            };
            if available.is_empty() {
                if frame.is_empty() {
                    return Err(Error::end_of_stream());
                }
                return Err(Error::invalid("unterminated frame"));
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |at| at + 1);
            if frame.len() + take > self.limit + 1 {
                return Err(Error::frame_too_large());
            }
            frame.extend_from_slice(&available[..take]);
            self.reader.consume(take);
            if newline.is_none() {
                continue;
            }
            frame.pop();
            if frame.contains(&b'\r') {
                return Err(Error::invalid("carriage return is not valid framing"));
            }
            if frame.is_empty() {
                return Err(Error::invalid("empty frame"));
            }
            if std::str::from_utf8(&frame).is_err() {
                return Err(Error::invalid("frame is not UTF-8"));
            }
            return Ok(frame);
        }
    }
}

pub struct Encoder<W> {
    writer: W,
    limit: usize,
}

impl<W: Write> Encoder<W> {
    pub fn new(writer: W, limit: usize) -> Self {
        Self {
            writer,
            limit: if limit == 0 {
                DEFAULT_FRAME_LIMIT
            } else {
                limit
            },
        }
    }

    pub fn encode(&mut self, message: &Message) -> Result<(), Error> {
        let mut data = message.marshal_json()?.into_bytes();
        if data.len() > self.limit {
            return Err(Error::frame_too_large());
        }
        data.push(b'\n');
        self.writer.write_all(&data).map_err(Error::io)
    }
}
