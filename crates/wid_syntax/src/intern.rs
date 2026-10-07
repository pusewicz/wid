//! A global string interner for identifiers.

use std::collections::HashMap;
use std::fmt;
use std::sync::{LazyLock, RwLock};

/// An interned identifier. Comparing names is a cheap integer comparison.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(u32);

struct Interner {
    map: HashMap<&'static str, Name>,
    strings: Vec<&'static str>,
}

static INTERNER: LazyLock<RwLock<Interner>> =
    LazyLock::new(|| RwLock::new(Interner { map: HashMap::new(), strings: Vec::new() }));

impl Name {
    /// Interns `text` and returns its name.
    pub fn new(text: &str) -> Name {
        if let Some(&name) = INTERNER.read().expect("interner lock").map.get(text) {
            return name;
        }
        let mut interner = INTERNER.write().expect("interner lock");
        if let Some(&name) = interner.map.get(text) {
            return name;
        }
        let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
        let name = Name(interner.strings.len() as u32);
        interner.strings.push(leaked);
        interner.map.insert(leaked, name);
        name
    }

    /// Returns the text of the name.
    pub fn as_str(self) -> &'static str {
        INTERNER.read().expect("interner lock").strings[self.0 as usize]
    }

    /// The name's number in the interner. It is the same for the same text
    /// for the life of the process, so compile-time code can hold a name as
    /// an integer (a `Symbol` value).
    pub fn index(self) -> u32 {
        self.0
    }

    /// The name with this number, or `None` when no name has it.
    pub fn from_index(index: u32) -> Option<Name> {
        let count = INTERNER.read().expect("interner lock").strings.len();
        ((index as usize) < count).then_some(Name(index))
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Name {
    fn from(text: &str) -> Self {
        Name::new(text)
    }
}
