//! Insertion-ordered HTTP header map.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers
{
    pub fn new() -> Self
    {
        Self(Vec::new())
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>)
    {
        self.0.push((name.into(), value.into()));
    }

    /// First occurrence (matched case-insensitively).
    pub fn get(&self, name: &str) -> Option<&str>
    {
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str>
    {
        self.0
            .iter()
            .filter(move |(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn contains(&self, name: &str) -> bool
    {
        self.get(name).is_some()
    }

    pub fn is_empty(&self) -> bool
    {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize
    {
        self.0.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>
    {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// True when `name` exists and one of its comma-separated values equals
    /// `token` (case-insensitive, surrounding whitespace ignored).
    pub fn contains_token(&self, name: &str, token: &str) -> bool
    {
        self.get_all(name).any(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        })
    }
}
