use std::collections::BTreeMap;

use crate::utils::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct Attributes {
    pub fields: BTreeMap<String, Vec<u8>>,
    pub records: Vec<(String, Vec<u8>)>,
}

impl Attributes {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.fields.get(key).map(|v| v.as_slice())
    }

    pub fn text(&self, key: &str) -> Option<String> {
        self.get(key).map(|v| String::from_utf8_lossy(v).into_owned())
    }

    pub fn number(&self, key: &str) -> Option<u64> {
        self.text(key).and_then(|v| v.parse().ok())
    }

    pub fn seconds(&self, key: &str) -> Option<i64> {
        let text = self.text(key)?;
        let whole = text.split_once('.').map_or(text.as_str(), |(head, _)| head);
        whole.parse().ok()
    }

    pub fn set(&mut self, key: &str, value: impl Into<Vec<u8>>) {
        let value = value.into();
        self.records.push((key.to_owned(), value.clone()));
        self.fields.insert(key.to_owned(), value);
    }

    /// Every value recorded for `key`, in the order they appeared.
    ///
    /// PAX normally lets a later record replace an earlier one, but the 0.0
    /// sparse format repeats its keys and means all of them.
    pub fn all(&self, key: &str) -> impl Iterator<Item = &[u8]> {
        self.records.iter().filter(move |(k, _)| k == key).map(|(_, v)| v.as_slice())
    }

    pub fn merge(&mut self, other: &Attributes) {
        self.records.extend(other.records.iter().cloned());
        for (key, value) in &other.fields {
            self.fields.insert(key.clone(), value.clone());
        }
    }
}

pub fn parse(data: &[u8]) -> Result<Attributes> {
    let mut attributes = Attributes::default();
    let mut at = 0usize;

    while at < data.len() {
        let space = data[at..].iter().position(|&b| b == b' ').ok_or_else(|| Error::malformed("pax record has no length separator"))?;

        let digits = std::str::from_utf8(&data[at..at + space]).map_err(|_| Error::malformed("pax record length is not ascii"))?;
        let length: usize = digits.parse().map_err(|_| Error::malformed(format!("pax record length {digits:?} is not a number")))?;

        if length <= space + 1 || at + length > data.len() {
            return Err(Error::malformed("pax record length is out of range"));
        }

        let body = &data[at + space + 1..at + length];
        let body = body.strip_suffix(b"\n").unwrap_or(body);

        let equals = body.iter().position(|&b| b == b'=').ok_or_else(|| Error::malformed("pax record has no '='"))?;
        let key = String::from_utf8_lossy(&body[..equals]).into_owned();
        let value = body[equals + 1..].to_vec();
        attributes.records.push((key.clone(), value.clone()));
        attributes.fields.insert(key, value);

        at += length;
    }

    Ok(attributes)
}

pub fn encode(attributes: &Attributes) -> Vec<u8> {
    let mut out = Vec::new();

    for (key, value) in &attributes.records {
        let payload = key.len() + value.len() + 3;
        let mut length = payload + 1;
        while length.to_string().len() + payload != length {
            length += 1;
        }

        out.extend_from_slice(length.to_string().as_bytes());
        out.push(b' ');
        out.extend_from_slice(key.as_bytes());
        out.push(b'=');
        out.extend_from_slice(value);
        out.push(b'\n');
    }

    out
}
