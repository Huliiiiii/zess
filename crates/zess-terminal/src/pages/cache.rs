use std::{
    collections::VecDeque,
    io::{self, Write},
};

const MAX_ENTRIES: usize = 32;
const MAX_BYTES: usize = 8 * 1024 * 1024;

struct ByteCount(usize);

impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len();
        if count > MAX_BYTES - self.0 {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.0 += count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn payload_size(
    write: impl FnOnce(&mut dyn Write) -> serde_json::Result<()>,
) -> Option<usize> {
    let mut count = ByteCount(0);
    write(&mut count).ok()?;
    Some(count.0)
}

pub(super) struct Cache<K, V> {
    entries: VecDeque<(K, V, usize)>,
    bytes: usize,
}

impl<K, V> Default for Cache<K, V> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
        }
    }
}

impl<K: PartialEq, V> Cache<K, V> {
    pub(super) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(key, value, _)| (key, value))
    }

    pub(super) fn get(&mut self, key: &K) -> Option<&V> {
        let index = self.entries.iter().position(|(stored, ..)| stored == key)?;
        let entry = self.entries.remove(index).unwrap();
        self.entries.push_back(entry);
        self.entries.back().map(|(_, value, _)| value)
    }

    pub(super) fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        let bytes = &mut self.bytes;
        self.entries.retain(|(key, _, size)| {
            if keep(key) {
                true
            } else {
                *bytes -= size;
                false
            }
        });
    }

    pub(super) fn insert(&mut self, key: K, value: V, bytes: usize) -> bool {
        self.retain(|stored| stored != &key);
        if bytes > MAX_BYTES {
            return false;
        }
        while self.entries.len() >= MAX_ENTRIES || self.bytes + bytes > MAX_BYTES {
            let (_, _, size) = self.entries.pop_front().unwrap();
            self.bytes -= size;
        }
        self.entries.push_back((key, value, bytes));
        self.bytes += bytes;
        true
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}
