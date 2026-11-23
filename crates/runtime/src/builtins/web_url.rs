//! Website references can fill data within a fixed HTTP(S) destination, not its authority.
use reqwest::Url;
use std::fmt::Write;

fn split_url(input: &str) -> Option<(&str, &str)> {
    let scheme = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))?;
    let boundary = scheme.find(['/', '?', '#']).unwrap_or(scheme.len());
    let end = input.len() - scheme.len() + boundary;
    Some(input.split_at(end))
}

fn fixed_origin(input: &str) -> Result<Url, String> {
    let (authority, _) = split_url(input).ok_or_else(template_error)?;
    if authority.contains(['$', '{', '}', '@', '\\'])
        || input.contains('\\')
        || input.chars().any(char::is_control)
    {
        return Err(template_error());
    }
    let origin = Url::parse(authority).map_err(|_| template_error())?;
    if origin.host().is_none() || !origin.username().is_empty() || origin.password().is_some() {
        return Err(template_error());
    }
    Ok(origin)
}

fn template_error() -> String {
    "Vault references in URLs require a fixed HTTP(S) address; put them in the path, query, or fragment."
        .into()
}

pub(crate) fn validate_template(input: &str) -> Result<(), String> {
    fixed_origin(input).map(|_| ())
}

pub(super) fn encode_value(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing into a String succeeds");
        }
    }
    encoded
}

pub(super) fn validate_resolved(template: &str, rendered: &str) -> Result<(), String> {
    let expected = fixed_origin(template)?;
    let actual = fixed_origin(rendered)?;
    let (_, tail) = split_url(rendered).ok_or_else(template_error)?;
    let path = tail.split(['?', '#']).next().unwrap_or_default();
    // Browsers treat even percent-encoded dot segments as path navigation
    if expected.origin() != actual.origin()
        || path.split('/').any(|segment| {
            matches!(
                segment.to_ascii_lowercase().as_str(),
                "." | ".." | "%2e" | ".%2e" | "%2e." | "%2e%2e"
            )
        })
    {
        return Err(template_error());
    }
    Url::parse(rendered).map_err(|_| template_error())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_fields_keep_a_fixed_origin_and_encode_vault_values_as_data() {
        let template = "https://example.org/${vault.id}?q=${vault.other}";
        assert!(validate_template(template).is_ok());
        let value = encode_value("a/b?x=2# café");
        assert_eq!(value, "a%2Fb%3Fx%3D2%23%20caf%C3%A9");
        assert!(
            validate_resolved(template, &format!("https://example.org/{value}?q={value}")).is_ok()
        );
        assert!(validate_resolved(template, "https://example.org/?q=").is_ok());
        assert!(validate_resolved(template, "https://example.org/.%2E?q=").is_err());
        assert!(validate_resolved(template, "https://other.example/?q=").is_err());
        for invalid in [
            "${vault.id}",
            "https://${vault.id}/",
            "https:///${vault.id}",
            "https://example.org:${vault.id}/",
            "https://user@example.org/${vault.id}",
            "custom:${vault.id}",
        ] {
            assert!(validate_template(invalid).is_err());
        }
    }
}
