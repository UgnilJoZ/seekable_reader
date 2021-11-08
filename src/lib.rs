use core::cmp::min;
use std::io::{Read, Result, Seek, SeekFrom};
use std::mem;

#[derive(Debug)]
enum Position {
    FrontBuffer(usize),
    BackBuffer(usize),
}

/// A reader adapter that allows to seek a little bit
///
/// The PreservingReader will wrap around a Read instance and can be read normally.
/// The core feature is to provide `Seek`, even if the underlying Reader does not.
/// It achieves this by holding a cache of the read data, which can be read again.
pub struct PreservingReader<R: Read> {
    pub inner: R,
    pub keep_size: usize,
    // TODO migrate to arrayvec
    current_buffer: Vec<u8>,
    older_buffer: Vec<u8>,
    pos: Position,
    /// Bytes read from `inner`
    pub read_bytes: usize,
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
            keep_size,
            current_buffer: Vec::with_capacity(keep_size),
            older_buffer: Vec::with_capacity(keep_size),
            pos: Position::FrontBuffer(0),
            read_bytes: 0,
            buffer_begins_at_pos: 0,
        }
    }

    // Returns the number of bytes which can be read from inner before the next buffer swap.
    fn remaining_current_buffer_capacity(&self) -> usize {
        dbg!(self.keep_size, self.current_buffer.len());
        self.keep_size - self.current_buffer.len()
    }

    /// Returns the size of the buffered data.
    /// Attempts to seek further back will result an Error.
    pub fn buffered_size(&self) -> usize {
        self.current_buffer.len() + self.older_buffer.len()
    }

    /// Reads more data from `inner` into `buf` and puts them into the cache
    /// 
    /// After this operation, the stream position will be at the end of all read data.
    /// 
    /// If buf is long enough, the caches will be flushed.
    fn read_inner(&mut self, buf: &mut [u8]) -> Result<usize> {
        let buf = buf;
        let read_bytes = self.inner.read(buf)?;
        let cache_capacity = 2 * self.keep_size;
        if read_bytes >= cache_capacity {
            // Flush cache and read everything out of the buffer
            let skip = cache_capacity * (read_bytes % cache_capacity);
            let (to_older, to_current) = (&buf[skip..]).split_at(self.keep_size);
            self.older_buffer.resize(self.keep_size, 0);
            self.older_buffer.as_mut_slice().copy_from_slice(to_older);
            self.current_buffer.resize(to_current.len(), 0);
            self.current_buffer.copy_from_slice(to_current);
        } else if read_bytes > self.remaining_current_buffer_capacity() {
            println!("Will swap buffers now.");
            mem::swap(&mut self.older_buffer, &mut self.current_buffer);
            let (to_older, to_current) = buf.split_at(self.remaining_current_buffer_capacity());
            self.older_buffer.extend_from_slice(to_older);
            self.current_buffer.resize(to_current.len(), 0);
            self.current_buffer.copy_from_slice(to_current);
        } else {
            self.current_buffer.extend_from_slice(buf);
        }
        if self.current_buffer.len() == self.keep_size {
            println!("Will swap buffers again.");
            mem::swap(&mut self.older_buffer, &mut self.current_buffer);
            self.current_buffer.clear();
        }
        self.pos = Position::FrontBuffer(self.current_buffer.len());
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
            let shift = min(shift, pos);
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

/// A PreservingReader can be read just normally:
///  ```
/// use std::io::Read;
/// use seekable_reader::PreservingReader;
/// 
/// fn onebyte_buffer_readthrough() {
///     let source = vec![1, 2, 3, 4, 5];
///     let reader = PreservingReader::new(source.as_slice(), 1);
///     let bytes: Vec<_> = reader.bytes().map(|b| b.unwrap()).collect();
///     assert_eq!(&source, &bytes);
/// }
/// ```
impl<R: Read> Read for PreservingReader<R> {
    /// Read something from this source and write it into buffer, returning how many bytes were read.
    ///
    /// `read` will never read more than `buf.len()` from the underlying reader. But it may have read less
    /// than it returns, in case the user seeked backwards before, causing the cache to be used.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self.pos {
            Position::FrontBuffer(pos) => {
                let cached = &self.current_buffer[pos..];
                let (from_cache, from_inner) = buf.split_at_mut(min(cached.len(), buf.len()));
                from_cache.copy_from_slice(&cached[..from_cache.len()]);
                self.pos = Position::FrontBuffer(pos + from_cache.len());
                if from_inner.len() > 0 {
                    Ok(cached.len() + self.read_inner(from_inner)?)
                } else {
                    Ok(cached.len())
                }
            }
            Position::BackBuffer(pos) => {
                let cached = &self.older_buffer[pos..];
                let cached = &cached[..min(cached.len(), buf.len())];
                let (from_cache, other) = buf.split_at_mut(cached.len());
                from_cache.copy_from_slice(cached);
                if other.len() > 0 {
                    self.pos = Position::FrontBuffer(0);
                    Ok(cached.len() + self.read(other)?)
                } else {
                    self.pos = Position::BackBuffer(pos + cached.len());
                    Ok(cached.len())
                }
            }
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
    use std::io::{Read, Seek, SeekFrom};

    #[test]
    fn readthrough_1byte_reserve() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = PreservingReader::new(source.as_slice(), 1);
        let mut buffer = [0; 1];
        let mut dest = vec!();
        while reader.read(&mut buffer).unwrap() != 0 {
            dest.push(buffer[0]);
        }
        assert_eq!(dest, source);
    }

    #[test]
    fn readthrough_2byte_reserve() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = PreservingReader::new(source.as_slice(), 2);
        let mut buffer = [0; 1];
        let mut dest = vec!();
        while reader.read(&mut buffer).unwrap() != 0 {
            dest.push(buffer[0]);
        }
        assert_eq!(dest, source);
    }

    #[test]
    fn readall_5byte() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = PreservingReader::new(source.as_slice(), 5);
        let mut dest = [0; 5];
        reader.read(&mut dest).unwrap();
        assert_eq!(reader.older_buffer.len(), 5);
        assert_eq!(reader.current_buffer.len(), 0);
    }

    #[test]
    fn seek_small_reserve() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = PreservingReader::new(source.as_slice(), 1);
        let mut dest = vec![];
        let mut buffer = [0; 1];
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_backwards(1).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_forwards(1).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        assert_eq!(dest, [1,1,3]);
    }

    #[test]
    fn seek_2bytes_reserve() {
        let source = vec![1, 2, 3, 4, 5];
        let mut reader = PreservingReader::new(source.as_slice(), 2);
        let mut dest = vec![];
        let mut buffer = [0; 1];
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_backwards(2).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_forwards(2).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        reader.seek_backwards(1).unwrap();
        reader.read(&mut buffer).unwrap();
        dest.push(buffer[0]);
        assert_eq!(dest, [1,2,1,4,4]);
    }

    #[test]
    fn bigger_test() {
        let source: Vec<u8> = (0..1536).map(|n| (n % 256) as u8).collect();
        let mut reader = PreservingReader::new(source.as_slice(), 1024);
        let mut buffer = [0; 1536];
        reader.read(&mut buffer).unwrap();
        assert_eq!(source.len(), buffer.len());
        assert_eq!(source, buffer);
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read(&mut buffer).unwrap();
        assert_eq!(source, buffer);
        reader.seek(SeekFrom::Current(-1024)).unwrap();
        reader.seek(SeekFrom::Current(-512)).unwrap();
        reader.read(&mut buffer).unwrap();
        assert_eq!(source, buffer);
        reader.seek(SeekFrom::End(-1536)).unwrap();
        reader.read(&mut buffer).unwrap();
        assert_eq!(source, buffer);
    }
}
