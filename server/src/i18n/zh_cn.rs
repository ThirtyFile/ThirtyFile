//! Simplified Chinese texts of the server: one entry per text of texts.rs (the English source and what each one is),
//! with the same parameters, written like zh_tw.rs. Words as in web/src/lib/i18n/glossary.md.
//!
//! Not translated yet: until a text has an entry here, it is sent in English. Once every text has one, list the
//! language in `COMPLETE` (texts.rs), so a new text can't be added without it.

use super::texts::Text;

pub(super) const TEXTS: &[(Text, &str)] = &[];
