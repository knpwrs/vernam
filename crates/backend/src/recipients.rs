//! Starting chats with people who aren't in the directory: by Signal
//! username (`name.12`) or by phone number, looked up on Signal's servers.

use presage::libsignal_service::prelude::phonenumber::{self, PhoneNumber};

/// A Signal username in `query` (`@name.12` or `name.12`): a nickname of
/// 3–32 letters, digits or underscores (not starting with a digit), a dot,
/// and at least two digits.
pub fn username_in(query: &str) -> Option<String> {
    let name = query.trim().trim_start_matches('@');
    let (nickname, discriminator) = name.rsplit_once('.')?;
    let nickname_ok = (3..=32).contains(&nickname.len())
        && nickname.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !nickname.starts_with(|c: char| c.is_ascii_digit());
    let discriminator_ok = discriminator.len() >= 2 && discriminator.chars().all(|c| c.is_ascii_digit());
    (nickname_ok && discriminator_ok).then(|| name.to_owned())
}

/// Why looking someone up found no one, with what to tell the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindError {
    /// No one on Signal has that username or number.
    NotFound(String),
    /// It isn't a username, number or link that can be looked up.
    Invalid(String),
    /// The lookup itself failed (no connection, the server).
    Failed(String),
}

impl FindError {
    /// What to tell the user.
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(message) | Self::Invalid(message) | Self::Failed(message) => message,
        }
    }
}

/// A number as Desktop's `parseAndFormatPhoneNumber` reads one: in E.164,
/// and whether it's a valid number. Numbers without a country code are
/// read as in `region`'s country, given as a number there (your own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPhone {
    pub e164: String,
    pub valid: bool,
}

/// Desktop's `parseAndFormatPhoneNumber` (`ts/util/libphonenumberInstance.std.ts`):
/// anything libphonenumber parses, valid or not, as long as it has a digit.
pub fn parse_phone(query: &str, home: Option<&str>) -> Option<ParsedPhone> {
    let query = query.trim();
    if !query.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let home: Option<PhoneNumber> = home.and_then(|home| phonenumber::parse(None, home).ok());
    let region = home.and_then(|home| home.country().id());
    let number = phonenumber::parse(region, query).ok()?;
    Some(ParsedPhone { e164: number.format().mode(phonenumber::Mode::E164).to_string(), valid: phonenumber::is_valid(&number) })
}

/// A phone number in `query`, in E.164 (`+15551234567`). Numbers without a
/// country code are read as being in `home`'s country (your own number).
pub fn phone_in(query: &str, home: Option<&str>) -> Option<String> {
    let query = query.trim();
    let plausible = query.chars().all(|c| c.is_ascii_digit() || " +-().".contains(c))
        && query.chars().filter(char::is_ascii_digit).count() >= 7;
    if !plausible {
        return None;
    }
    let home: Option<PhoneNumber> = home.and_then(|home| phonenumber::parse(None, home).ok());
    let region = home.and_then(|home| home.country().id());
    let number = phonenumber::parse(region, query).ok()?;
    phonenumber::is_valid(&number).then(|| number.format().mode(phonenumber::Mode::E164).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        assert_eq!(username_in("@jon.42"), Some("jon.42".into()));
        assert_eq!(username_in("jon_smith.007"), Some("jon_smith.007".into()));
        for query in ["jon", "jon.4", "1jon.42", "jo.42", "jon.ab", "signal.org"] {
            assert_eq!(username_in(query), None, "{query}");
        }
    }

    #[test]
    fn numbers_are_read_valid_or_not_as_desktop_reads_them() {
        let home = Some("+12025550123");
        assert_eq!(parse_phone("(202) 555-0187", home), Some(ParsedPhone { e164: "+12025550187".into(), valid: true }));
        // Parsed, though not a valid number: Desktop offers it and then says so.
        assert_eq!(parse_phone("555", home).map(|p| p.valid), Some(false));
        for text in ["bob", "", "  "] {
            assert_eq!(parse_phone(text, home), None, "{text}");
        }
    }

    #[test]
    fn phone_numbers_use_your_country_by_default() {
        let home = Some("+12025550123");
        assert_eq!(phone_in("(202) 555-0187", home), Some("+12025550187".into()));
        assert_eq!(phone_in("+44 20 7946 0958", home), Some("+442079460958".into()));
        assert_eq!(phone_in("555", home), None);
        assert_eq!(phone_in("call me", home), None);
    }
}
