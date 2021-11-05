use core::cmp::min;
use std::io::{Read, Result, Seek, SeekFrom};
use std::mem;

enum Position {
    FrontBuffer(usize),
    BackBuffer(usize),
}

/// A reader adapter that allows to seek a little bit
///
/// The PreservingReader will wrap around a Read instance and can be read normally.
/// The core feature is to provide `Seek`, even if the underlying Reader does not.
/// It achieves this by holding a cache of the read data, which can be read again.
///
///
/// ```
/// fn onebyte_buffer_readthrough() {
///     dbg!("Hallo Welt!");
///     let source = vec![1, 2, 3, 4, 5];
///     let reader = PreservingReader::new(source.as_slice(), 1);
///     let bytes: Vec<_> = reader.bytes().map(|b| b.unwrap()).collect();
///     assert_eq!(&source, &bytes);
/// }
/// ```
pub struct PreservingReader<R: Read> {
    pub inner: R,
    current_buffer: Vec<u8>,
    older_buffer: Vec<u8>,
    pos: Position,
    /// Bytes read from `inner`
    read_bytes: usize,
    buffer_begins_at_pos: usize,
}

impl<R: Read> PreservingReader<R> {
    /// Create a new instance of a PreservingReader.
    ///
    /// It wraps around `inner` and allows seeking backwards by
    /// keeping at least `keep_size` bytes of already read data,
    /// if this amount of data is already read.
    ///
    /// At most, `2 * keep_size` bytes are kept.
    pub fn new(inner: R, keep_size: usize) -> PreservingReader<R> {
        PreservingReader {
            inner,
            current_buffer: Vec::with_capacity(keep_size),
            older_buffer: Vec::with_capacity(keep_size),
            pos: Position::FrontBuffer(0),
            read_bytes: 0,
            buffer_begins_at_pos: 0,
        }
    }

    // Returns the number of bytes which can be read from inner before the next buffer swap.
    fn remaining_current_buffer_capacity(&self) -> usize {
        self.current_buffer.capacity() - self.current_buffer.len()
    }

    /// Returns the size of the buffered data.
    /// Attempts to seek further back will result an Error.
    pub fn buffered_size(&self) -> usize {
        self.current_buffer.len() + self.older_buffer.len()
    }

    /// Reads more data from `inner` into `buf` and puts them into the cache
    fn read_inner(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut buf = buf;
        let read_bytes = self.inner.read(buf)?;
        let to_cache_bytes = read_bytes;
        while to_cache_bytes >= self.remaining_current_buffer_capacity() {
            self.current_buffer
                .extend_from_slice(&buf[..self.remaining_current_buffer_capacity()]);
            buf = &mut buf[self.remaining_current_buffer_capacity()..];
            self.older_buffer.clear();
            self.buffer_begins_at_pos += self.older_buffer.capacity();
            mem::swap(&mut self.current_buffer, &mut self.older_buffer);
            self.pos = Position::FrontBuffer(0);
        }
        self.current_buffer.extend_from_slice(&buf);
        self.read_bytes += read_bytes;
        Ok(read_bytes)
    }

    fn get_stream_position(&self) -> usize {
        match self.pos {
            Position::FrontBuffer(pos) => self.buffer_begins_at_pos + self.older_buffer.len() + pos,
            Position::BackBuffer(pos) => self.buffer_begins_at_pos + pos,
        }
    }

    fn seek_backwards(&mut self, shift: usize) -> Result<u64> {
        let mut shift = shift;
        if let Position::FrontBuffer(pos) = self.pos {
            if shift > pos {
                self.pos = Position::BackBuffer(self.older_buffer.len() - 1);
                shift -= pos + 1;
            } else {
                self.pos = Position::FrontBuffer(pos - shift);
            }
        }
        
        if let Position::BackBuffer(pos) = self.pos {
            let shift = min(shift, self.older_buffer.len());
            let newpos = self.buffer_begins_at_pos + pos - shift;
            self.pos = Position::BackBuffer(newpos);
        }

        Ok(self.get_stream_position() as u64)
    }

    fn seek_forwards(&mut self, shift: usize) -> Result<u64> {
        let mut shift = shift;
        if let Position::BackBuffer(pos) = self.pos {
            let remaining_in_back_buffer = self.older_buffer.len() - pos;
            if shift >= remaining_in_back_buffer {
                self.pos = Position::FrontBuffer(0);
                shift -= remaining_in_back_buffer;
            } else {
                self.pos = Position::BackBuffer(pos + shift);
            }
        }
        
        if let Position::FrontBuffer(pos) = self.pos {
            let remaining_in_front_buffer = self.current_buffer.len() - pos;
            if shift > remaining_in_front_buffer {
                // We have to read additional data the user is not (yet) interested in
                shift -= remaining_in_front_buffer;
                self.pos = Position::FrontBuffer(self.current_buffer.len());
                let mut _discarded_data = vec![0; shift];
                self.read_inner(&mut _discarded_data)?;
            } else {
                self.pos = Position::FrontBuffer(pos + shift);
            }
        }

        Ok(self.get_stream_position() as u64)
    }
}

impl<R: Read> Read for PreservingReader<R> {
    /// Read something from this source and write it into buffer, returning how many bytes were read.
    ///
    /// `read` will never read more than `buf.len()` from the underlying reader. But it may have read less
    /// than it returns, in case the user seeked backwards before.
    /// ToDo Rewrite
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        // First, handle the older buffer / back buffer, if needed
        let front_pos = match self.pos {
            Position::FrontBuffer(pos) => pos,
            Position::BackBuffer(pos) => {
                let remaining_backbuffer = &self.older_buffer[pos..];
                if buf.len() < remaining_backbuffer.len() {
                    buf.copy_from_slice(&remaining_backbuffer[..buf.len()]);
                    self.pos = Position::BackBuffer(pos + buf.len());
                    return Ok(buf.len())
                } else {
                    let (backbuffer_cached, remainder) =
                        buf.split_at_mut(remaining_backbuffer.len());
                    backbuffer_cached.copy_from_slice(&remaining_backbuffer);
                    self.pos = Position::FrontBuffer(0);
                    0
                }
            }
        };
        // Now, we can read the rest (which may involve the front buffer)
        if front_pos < self.current_buffer.len() {
            let buffer_remainder = &self.current_buffer[front_pos..];
            let (cached_answer, new_read) = buf.split_at_mut(buffer_remainder.len());
            dbg!("Will copy {} bytes from cache", buffer_remainder.len());
            cached_answer.copy_from_slice(&buffer_remainder);
            let newly_read = self.read_inner(new_read)?;
            self.pos = Position::FrontBuffer(self.current_buffer.len());
            Ok(cached_answer.len() + newly_read)
        } else {
            self.read_inner(buf)
        }
    }
}

 impl<R: Read> Seek for PreservingReader<R> {
     fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
         let old_position = self.get_stream_position();
         match pos {
             SeekFrom::Start(pos) if pos > old_position as u64 => self.seek_forwards(pos as usize - old_position),
             SeekFrom::Start(pos) => self.seek_backwards(old_position - pos as usize),
             SeekFrom::End(shift) => self.seek(SeekFrom::Start((old_position as i64 + shift) as u64)),
             SeekFrom::Current(shift) if shift > 0 => self.seek_forwards(shift as usize),
             SeekFrom::Current(shift) => self.seek_backwards((-shift) as usize),
         }
     }
 }

#[cfg(test)]
mod tests {
    use crate::PreservingReader;
    use std::io::Read;

    #[test]
    fn onebyte_buffer_readthrough() {
        dbg!("Hallo Welt!");
        let source = vec![1, 2, 3, 4, 5];
        let reader = PreservingReader::new(source.as_slice(), 1);
        let bytes: Vec<_> = reader.bytes().map(|b| b.unwrap()).collect();
        assert_eq!(&source, &bytes);
    }
}
