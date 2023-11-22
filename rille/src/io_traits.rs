//! A reimplementation of `std::io`'s traits for use in the kernel.
//!
//! Most of this code is stolen from std's code.

use core::cmp;

/// A trait for streams which can be read.
pub trait Read {
    /// Read bytes into the provided vector, returning the number of
    /// bytes read.
    ///
    /// # Errors
    ///
    /// This function return an error if an I/O error occured.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError>;

    /// TODO: docs
    ///
    /// # Errors
    ///
    /// TODO
    fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<(), IoError> {
        while !buf.is_empty() {
            match self.read(buf) {
                Ok(0) => break,
                Ok(n) => {
                    let tmp = buf;
                    buf = &mut tmp[n..];
                }
                Err(e) => return Err(e),
            }
        }
        if buf.is_empty() {
            Ok(())
        } else {
            Err(IoError)
        }
    }
}

impl<'a> Read for &'a [u8] {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let len = cmp::min(buf.len(), self.len());
        let (read_slice, rest) = self.split_at(len);

        buf[..len].copy_from_slice(read_slice);

        *self = rest;
        Ok(len)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), IoError> {
        if buf.len() > self.len() {
            return Err(IoError);
        }
        let (read_slice, rest) = self.split_at(buf.len());

        buf.copy_from_slice(read_slice);

        *self = rest;
        Ok(())
    }
}

/// A trait for streams with an internal cursor, which can be moved at
/// will.
pub trait Seek {
    /// Seek to the provided offset in a stream.
    ///
    /// If successful, the new position is returned.
    ///
    /// # Errors
    ///
    /// This function will return an error if an I/O error occurred.
    fn seek(&mut self, from: SeekFrom) -> Result<u64, IoError>;
}

/// Abstract error type for any I/O error.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct IoError;

/// Description of where to seek from and by how many bytes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SeekFrom {
    /// Seek some amount of bytes from the start of the stream.
    Start(u64),
    /// Seek some amount of bytes from the end of the stream.
    End(i64),
    /// Seek some amount of bytes from the current cursor position.
    Current(i64),
}

/// A [`Read`]- and [`Seek`]-able cursor over a byte buffer.
pub struct Cursor<T: AsRef<[u8]>> {
    inner: T,
    position: usize,
}

impl<T: AsRef<[u8]>> Cursor<T> {
    /// Get a slice to the rest of the buffer.
    pub fn rest(&self) -> &[u8] {
        &self.inner.as_ref()[self.position..]
    }

    /// Create a `Cursor` from a buffer.
    pub fn new(inner: T) -> Self {
        Self { inner, position: 0 }
    }

    /// Turn a `Cursor` back into its inner buffer, unchanged.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: AsRef<[u8]>> Read for Cursor<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let n = self.rest().read(buf)?;
        self.position += n;
        Ok(n)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), IoError> {
        self.rest().read_exact(buf)?;
        self.position += buf.len();
        Ok(())
    }
}

impl<T: AsRef<[u8]>> Seek for Cursor<T> {
    fn seek(&mut self, from: SeekFrom) -> Result<u64, IoError> {
        let (base_pos, offset) = match from {
            SeekFrom::Start(n) => {
                self.position = n as usize;
                return Ok(n);
            }
            SeekFrom::End(n) => (self.inner.as_ref().len() as u64, n),
            SeekFrom::Current(n) => (self.position as u64, n),
        };
        match base_pos.checked_add_signed(offset) {
            Some(n) => {
                self.position = n as usize;
                Ok(self.position as u64)
            }
            None => Err(IoError),
        }
    }
}
