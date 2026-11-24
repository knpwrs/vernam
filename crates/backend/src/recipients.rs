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

/// The region of `home`, a number in E.164 (your own: Desktop's
/// `regionCode`, kept from the number the account registered with).
pub fn region_of(home: &str) -> Option<String> {
    let number = phonenumber::parse(None, home).ok()?;
    number.metadata(&phonenumber::metadata::DATABASE).map(|m| m.id().to_owned())
}

/// [`parse_phone`] with a region rather than a number (Find by phone
/// number's country, Desktop's `selectedRegion || regionCode`). Without a
/// region, only a number with a `+` and its country code reads.
pub fn parse_phone_in(query: &str, region: Option<&str>) -> Option<ParsedPhone> {
    let query = query.trim();
    if !query.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let region = region.and_then(|r| r.parse::<phonenumber::country::Id>().ok());
    let number = phonenumber::parse(region, query).ok()?;
    Some(ParsedPhone { e164: number.format().mode(phonenumber::Mode::E164).to_string(), valid: phonenumber::is_valid(&number) })
}

/// Desktop's typing in Find by phone number (`SET_COMPOSE_SEARCH_TERM`):
/// text whose country code is another region's than `selected` (a typed
/// `+44 …`, or a number another region of the same code owns) switches to
/// that region, and the text becomes the national number. `None` keeps
/// both. (Desktop formats the number without its country code
/// "in original format", which with no region to format by is its national
/// digits.)
pub fn switch_country(text: &str, selected: Option<&str>) -> Option<(String, String)> {
    let parsed_region = selected.and_then(|r| r.parse::<phonenumber::country::Id>().ok());
    let number = phonenumber::parse(parsed_region, text.trim()).ok()?;
    let region = number.metadata(&phonenumber::metadata::DATABASE)?.id().to_owned();
    // A number of no country ("+800", region "001") stays as typed: Desktop
    // switches to a region it can't list or read the number in.
    if Some(region.as_str()) == selected || crate::countries::country(&region).is_none() {
        return None;
    }
    let national = number.national();
    let digits = format!("{}{}", "0".repeat(usize::from(national.zeros())), national.value());
    Some((region, digits))
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
    fn a_number_reads_with_the_country_picked() {
        assert_eq!(region_of("+442079460958").as_deref(), Some("GB"));
        assert_eq!(region_of("+12025550123").as_deref(), Some("US"));
        assert_eq!(parse_phone_in("020 7946 0958", Some("GB")).map(|p| (p.e164, p.valid)), Some(("+442079460958".into(), true)));
        assert_eq!(parse_phone_in("+44 20 7946 0958", None).map(|p| p.e164), Some("+442079460958".into()));
        // No region and no country code: nothing to read it by.
        assert_eq!(parse_phone_in("20 7946 0958", None), None);
        assert_eq!(parse_phone_in("bob", Some("GB")), None);
    }

    #[test]
    fn a_typed_country_code_switches_the_country() {
        // Desktop's own example: the United Kingdom, its national part kept.
        assert_eq!(switch_country("+44 20 7946 0000", Some("US")), Some(("GB".into(), "2079460000".into())));
        assert_eq!(switch_country("+44 20 7946 0000", None), Some(("GB".into(), "2079460000".into())));
        // The country already picked, or text that isn't a number: no change.
        assert_eq!(switch_country("20 7946 0000", Some("GB")), None);
        assert_eq!(switch_country("+44 20 7946 0000", Some("GB")), None);
        assert_eq!(switch_country("20 7946", None), None);
        assert_eq!(switch_country("abc", Some("GB")), None);
        // A number of no country keeps its "+".
        assert_eq!(switch_country("+800 1234 5678", Some("US")), None);
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
